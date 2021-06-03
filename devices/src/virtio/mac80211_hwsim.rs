// Copyright 2021 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{self, Read, Write};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::thread;

use vm_memory::GuestMemory;

use thiserror::Error as ThisError;

use base::Error as SysError;
use base::{error, warn};
use base::{Event, EventType, PollToken, RawDescriptor, WaitContext};

use super::{
    DescriptorError, Interrupt, Queue, Reader, SignalableInterrupt, VirtioDevice, Writer,
    TYPE_MAC80211_HWSIM, VIRTIO_F_VERSION_1,
};

const QUEUE_SIZE: u16 = 256;

#[derive(ThisError, Debug)]
pub enum Mac80211HwsimError {
    #[error("failed to create mac80211_hwsim device: {0}")]
    CreateMac80211Hwsim(SysError),
    #[error("failed to create reader of tx_queue: {0}")]
    CreateReadQueue(DescriptorError),
    #[error("failed to create waitcontext: {0}")]
    CreateWaitContext(SysError),
    #[error("failed to read event: {0}")]
    ReadEvent(SysError),
    #[error("failed to read tx_queue: {0}")]
    ReadQueue(io::Error),
    #[error("rx queue for guest device is exhausted")]
    RxQueueExhausted,
    #[error("failed to disable waitcontext: {0}")]
    WaitContextDisable(SysError),
    #[error("failed to wait on waitcontext: {0}")]
    WaitOnContext(SysError),
}

pub struct Mac80211Hwsim {
    avail_features: u64,
    queue_sizes: [u16; 2],
}

impl Mac80211Hwsim {
    pub fn new(base_features: u64) -> Result<Mac80211Hwsim, Mac80211HwsimError> {
        let features = base_features | 1 << VIRTIO_F_VERSION_1;

        Ok(Mac80211Hwsim {
            queue_sizes: [QUEUE_SIZE, QUEUE_SIZE],
            avail_features: features,
        })
    }
}

struct Worker {
    mem: GuestMemory,
    interrupt: Interrupt,
    tx_queue: Queue,
    rx_queue: Queue,
    frame_send_channel: Sender<Vec<u8>>,
    frame_recv_channel: Receiver<Vec<u8>>,
    frame_channel_evt: Event,
}

impl Worker {
    fn new(
        mem: GuestMemory,
        interrupt: Interrupt,
        tx_queue: Queue,
        rx_queue: Queue,
    ) -> Result<Worker, Mac80211HwsimError> {
        let (frame_send_channel, frame_recv_channel) = channel();
        let frame_channel_evt = Event::new().map_err(Mac80211HwsimError::CreateMac80211Hwsim)?;

        Ok(Worker {
            mem,
            interrupt,
            tx_queue,
            rx_queue,
            frame_send_channel,
            frame_recv_channel,
            frame_channel_evt,
        })
    }

    // Guest OS's mac80211_hwsim driver had sent a data
    fn process_tx_queue(&mut self) -> Result<(), Mac80211HwsimError> {
        while let Some(desc_chain) = self.tx_queue.pop(&self.mem) {
            let index = desc_chain.index;
            let mut reader = Reader::new(self.mem.clone(), desc_chain)
                .map_err(Mac80211HwsimError::CreateReadQueue)?;

            let expected_count = reader.available_bytes();
            let mut buf = vec![0; expected_count];
            let read_count = reader
                .read(&mut buf)
                .map_err(Mac80211HwsimError::ReadQueue)?;

            self.tx_queue.add_used(&self.mem, index, 0);
        }

        self.interrupt.signal_used_queue(self.tx_queue.vector);

        Ok(())
    }

    // Function that sends data frame to Guest OS
    fn send_frame_to_guest(&mut self, data: &[u8]) {
        self.frame_send_channel.send(data.to_vec());
        self.frame_channel_evt.write(1);
    }

    fn process_frame_channel(&mut self) -> Result<(), Mac80211HwsimError> {
        let mut exhausted_queue = false;
        let mut needs_interrupt = false;

        loop {
            let desc_chain = match self.rx_queue.peek(&self.mem) {
                Some(desc) => desc,
                None => {
                    exhausted_queue = true;
                    break;
                }
            };

            let index = desc_chain.index;

            let data = match self.frame_recv_channel.try_recv() {
                Ok(data) => data,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    break;
                }
            };

            let mut writer = Writer::new(self.mem.clone(), desc_chain).unwrap();
            let bytes_written = writer.write(&data).unwrap() as u32;

            if bytes_written > 0 {
                self.rx_queue.pop_peeked(&self.mem);
                self.rx_queue.add_used(&self.mem, index, bytes_written);
                needs_interrupt = true;
            }
        }

        if needs_interrupt {
            self.interrupt.signal_used_queue(self.rx_queue.vector);
        }

        if exhausted_queue {
            Err(Mac80211HwsimError::RxQueueExhausted)
        } else {
            Ok(())
        }
    }

    fn run(&mut self, tx_queue_evt: Event, rx_queue_evt: Event) -> Result<(), Mac80211HwsimError> {
        #[derive(PollToken)]
        enum Token {
            RxQueue,
            TxQueue,
            FrameToGuest,
            InterruptResample,
        }

        let wait_ctx: WaitContext<Token> = WaitContext::build_with(&[
            (&tx_queue_evt, Token::TxQueue),
            (&rx_queue_evt, Token::RxQueue),
            (&self.frame_channel_evt, Token::FrameToGuest),
        ])
        .map_err(Mac80211HwsimError::CreateWaitContext)?;

        if let Some(resample_evt) = self.interrupt.get_resample_evt() {
            wait_ctx
                .add(resample_evt, Token::InterruptResample)
                .map_err(Mac80211HwsimError::CreateWaitContext)?;
        }

        let mut poll_frame_channel_enabled = true;

        'wait: loop {
            let events = wait_ctx.wait().map_err(Mac80211HwsimError::WaitOnContext)?;

            for event in events.iter().filter(|e| e.is_readable) {
                match event.token {
                    Token::TxQueue => {
                        tx_queue_evt.read().map_err(Mac80211HwsimError::ReadEvent)?;
                        self.process_tx_queue()?;
                    }
                    Token::RxQueue => {
                        rx_queue_evt.read().map_err(Mac80211HwsimError::ReadEvent)?;
                        if !poll_frame_channel_enabled {
                            wait_ctx
                                .modify(
                                    &self.frame_channel_evt,
                                    EventType::Read,
                                    Token::FrameToGuest,
                                )
                                .map_err(Mac80211HwsimError::WaitContextDisable)?;
                            poll_frame_channel_enabled = true;
                        }
                    }
                    Token::FrameToGuest => match self.process_frame_channel() {
                        Ok(()) => {
                            self.frame_channel_evt
                                .read()
                                .map_err(Mac80211HwsimError::ReadEvent)?;
                        }
                        Err(Mac80211HwsimError::RxQueueExhausted) => {
                            wait_ctx
                                .modify(
                                    &self.frame_channel_evt,
                                    EventType::None,
                                    Token::FrameToGuest,
                                )
                                .map_err(Mac80211HwsimError::WaitContextDisable)?;
                            poll_frame_channel_enabled = false;
                        }
                        Err(e) => {
                            error!("mac80211_hwsim: failed to process frame channel {}", e);
                        }
                    },
                    Token::InterruptResample => {
                        self.interrupt.interrupt_resample();
                    }
                }
            }
        }

        Ok(())
    }
}

impl VirtioDevice for Mac80211Hwsim {
    fn keep_rds(&self) -> Vec<RawDescriptor> {
        Vec::new()
    }

    fn features(&self) -> u64 {
        self.avail_features
    }

    fn device_type(&self) -> u32 {
        TYPE_MAC80211_HWSIM
    }

    fn queue_max_sizes(&self) -> &[u16] {
        &self.queue_sizes
    }

    fn activate(
        &mut self,
        mem: GuestMemory,
        interrupt: Interrupt,
        mut queues: Vec<Queue>,
        mut queue_evts: Vec<Event>,
    ) {
        let tx_queue = queues.remove(0); // queue for the data sent from guest
        let rx_queue = queues.remove(0); // queue for the data send to guest
        let tx_queue_evt = queue_evts.remove(0);
        let rx_queue_evt = queue_evts.remove(0);

        let worker_result = thread::Builder::new()
            .name("mac80211_hwsim virtio worker".to_string())
            .spawn(move || {
                let mut worker = Worker::new(mem, interrupt, tx_queue, rx_queue).unwrap();

                let result = worker.run(tx_queue_evt, rx_queue_evt);
                worker
            });
    }
}
