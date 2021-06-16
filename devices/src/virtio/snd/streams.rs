// Copyright 2021 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::VecDeque;
use std::ops::Deref;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::virtio::{DescriptorChain, Interrupt, Queue, Reader, Writer};
use base::{error, set_rt_prio_limit, set_rt_round_robin, warn};
use data_model::Le32;
use sync::Mutex;
use vm_memory::GuestMemory;

use super::constants::*;
use super::layout::*;
use super::vios_backend::Error as VioSError;
use super::vios_backend::*;
use super::{Result, SoundError};

/// Messages that the worker can send to the stream (thread).
pub enum StreamMsg {
    SetParams(DescriptorChain, virtio_snd_pcm_set_params),
    Prepare(DescriptorChain),
    Start(DescriptorChain),
    Stop(DescriptorChain),
    Release(DescriptorChain),
    Buffer(DescriptorChain),
    Break,
}

enum StreamState {
    New,
    ParamsSet,
    Prepared,
    Started,
    Stopped,
    Released,
}

pub struct Stream {
    stream_id: u32,
    receiver: Receiver<StreamMsg>,
    vios_client: Arc<VioSClient>,
    guest_memory: GuestMemory,
    control_queue: Arc<Mutex<Queue>>,
    io_queue: Arc<Mutex<Queue>>,
    interrupt: Arc<Mutex<Interrupt>>,
    capture: bool,
    current_state: StreamState,
    period: Duration,
    start_time: Instant,
    next_buffer: Duration,
    buffer_queue: VecDeque<DescriptorChain>,
}

impl Stream {
    /// Start a new stream thread and return its handler.
    pub fn try_new(
        stream_id: u32,
        vios_client: Arc<VioSClient>,
        guest_memory: GuestMemory,
        interrupt: Arc<Mutex<Interrupt>>,
        control_queue: Arc<Mutex<Queue>>,
        io_queue: Arc<Mutex<Queue>>,
        capture: bool,
    ) -> Result<StreamProxy> {
        let (sender, receiver): (Sender<StreamMsg>, Receiver<StreamMsg>) = channel();
        let thread = thread::Builder::new()
            .name(format!("virtio_snd stream {}", stream_id))
            .spawn(move || {
                try_set_real_time_priority();

                let mut stream = Stream {
                    stream_id,
                    receiver,
                    vios_client,
                    guest_memory,
                    control_queue,
                    io_queue,
                    interrupt,
                    capture,
                    current_state: StreamState::New,
                    period: Duration::from_millis(0),
                    start_time: Instant::now(),
                    next_buffer: Duration::from_millis(0),
                    buffer_queue: VecDeque::new(),
                };

                if let Err(e) = stream.stream_loop() {
                    error!("virtio-snd: Error in stream {}: {}", stream_id, e);
                }
            })
            .map_err(SoundError::CreateThread)?;
        Ok(StreamProxy {
            sender,
            thread: Some(thread),
        })
    }

    fn stream_loop(&mut self) -> Result<()> {
        loop {
            if !self.recv_msg()? {
                break;
            }
            self.maybe_process_queued_buffers()?;
        }
        Ok(())
    }

    fn recv_msg(&mut self) -> Result<bool> {
        let msg = self.receiver.recv().map_err(SoundError::StreamThreadRecv)?;
        match msg {
            StreamMsg::SetParams(desc, params) => {
                let code = match self.vios_client.set_stream_parameters_raw(params) {
                    Ok(()) => {
                        let frame_rate = rate_from_virtio_constant(params.rate);
                        self.period = Duration::from_millis(
                            (params.period_bytes.to_native() as u64 * 1000u64)
                                / frame_rate
                                / params.channels as u64
                                / bytes_per_sample(params.format) as u64,
                        );
                        VIRTIO_SND_S_OK
                    }
                    Err(e) => {
                        error!(
                            "virtio-snd: Error setting parameters for stream {}: {}",
                            self.stream_id, e
                        );
                        vios_error_to_status_code(e)
                    }
                };
                reply_control_op_status(
                    code,
                    desc,
                    &self.guest_memory,
                    &self.control_queue,
                    &self.interrupt,
                )?;
                self.current_state = StreamState::ParamsSet;
            }
            StreamMsg::Prepare(desc) => {
                let code = match self.vios_client.prepare_stream(self.stream_id) {
                    Ok(()) => VIRTIO_SND_S_OK,
                    Err(e) => {
                        error!(
                            "virtio-snd: Failed to prepare stream {}: {}",
                            self.stream_id, e
                        );
                        vios_error_to_status_code(e)
                    }
                };
                reply_control_op_status(
                    code,
                    desc,
                    &self.guest_memory,
                    &self.control_queue,
                    &self.interrupt,
                )?;
                self.current_state = StreamState::Prepared;
            }
            StreamMsg::Start(desc) => {
                let code = match self.vios_client.start_stream(self.stream_id) {
                    Ok(()) => VIRTIO_SND_S_OK,
                    Err(e) => {
                        error!(
                            "virtio-snd: Failed to start stream {}: {}",
                            self.stream_id, e
                        );
                        vios_error_to_status_code(e)
                    }
                };
                reply_control_op_status(
                    code,
                    desc,
                    &self.guest_memory,
                    &self.control_queue,
                    &self.interrupt,
                )?;
                self.start_time = Instant::now();
                self.next_buffer = Duration::from_millis(0);
                self.current_state = StreamState::Started;
            }
            StreamMsg::Stop(desc) => {
                let code = match self.vios_client.stop_stream(self.stream_id) {
                    Ok(()) => VIRTIO_SND_S_OK,
                    Err(e) => {
                        error!(
                            "virtio-snd: Failed to stop stream {}: {}",
                            self.stream_id, e
                        );
                        vios_error_to_status_code(e)
                    }
                };
                reply_control_op_status(
                    code,
                    desc,
                    &self.guest_memory,
                    &self.control_queue,
                    &self.interrupt,
                )?;
                self.current_state = StreamState::Stopped;
            }
            StreamMsg::Release(desc) => {
                let code = match self.vios_client.release_stream(self.stream_id) {
                    Ok(()) => VIRTIO_SND_S_OK,
                    Err(e) => {
                        error!(
                            "virtio-snd: Failed to release stream {}: {}",
                            self.stream_id, e
                        );
                        vios_error_to_status_code(e)
                    }
                };
                reply_control_op_status(
                    code,
                    desc,
                    &self.guest_memory,
                    &self.control_queue,
                    &self.interrupt,
                )?;
                self.current_state = StreamState::Released;
            }
            StreamMsg::Buffer(d) => {
                // Buffers may arrive while in several states:
                // - Prepared: Buffer should be queued and played when start cmd arrives
                // - Started: Buffer should be processed immediately
                // - Stopped: Buffer should be returned to the guest immediately
                // Because we may need to wait to process the buffer, we always queue it and
                // decide what to do with queued buffers after every message.
                self.buffer_queue.push_back(d);
            }
            StreamMsg::Break => {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn maybe_process_queued_buffers(&mut self) -> Result<()> {
        match self.current_state {
            StreamState::Started => {
                while let Some(desc) = self.buffer_queue.pop_front() {
                    let mut reader = Reader::new(self.guest_memory.clone(), desc.clone())
                        .map_err(SoundError::CreateReader)?;
                    // Ignore the first buffer, it was already read by the time this thread
                    // receives the descriptor
                    reader.consume(std::mem::size_of::<virtio_snd_pcm_xfer>());
                    let desc_index = desc.index;
                    let mut writer = Writer::new(self.guest_memory.clone(), desc)
                        .map_err(SoundError::CreateWriter)?;
                    let io_res = if self.capture {
                        let buffer_size =
                            writer.available_bytes() - std::mem::size_of::<virtio_snd_pcm_status>();
                        let request_res = self.vios_client.request_audio_data(
                            self.stream_id,
                            buffer_size,
                            |vslice| {
                                let mut src_slice = *vslice;
                                for dst_slice in writer.get_remaining() {
                                    src_slice.copy_to_volatile_slice(dst_slice);
                                    src_slice = match src_slice.offset(dst_slice.size()) {
                                        // Err here means vslice is fully consumed
                                        Err(_) => return (),
                                        Ok(s) => s,
                                    };
                                }
                            },
                        );
                        // Consume the entire buffer regardless of request outcome to ensure
                        // the status is written at the end of the writable area
                        writer.consume_bytes(buffer_size);
                        request_res
                    } else {
                        let inject_res = self.vios_client.inject_audio_data(
                            self.stream_id,
                            reader.available_bytes(),
                            |vslice| {
                                let mut dst_slice = vslice;
                                for src_slice in reader.get_remaining() {
                                    src_slice.copy_to_volatile_slice(dst_slice);
                                    dst_slice = match dst_slice.offset(src_slice.size()) {
                                        // Err here means vslice is full
                                        Err(_) => return (),
                                        Ok(s) => s,
                                    };
                                }
                            },
                        );
                        reader.consume(reader.available_bytes());
                        inject_res
                    };
                    let (code, latency) = match io_res {
                        Ok((latency, ())) => (VIRTIO_SND_S_OK, latency),
                        Err(e) => {
                            error!(
                                "virtio-snd: Failed IO operation in stream {}: {}",
                                self.stream_id, e
                            );
                            (VIRTIO_SND_S_IO_ERR, 0)
                        }
                    };
                    if let Err(e) = writer.write_obj(virtio_snd_pcm_status {
                        status: Le32::from(code),
                        latency_bytes: Le32::from(latency),
                    }) {
                        error!(
                            "virtio-snd: Failed to write pcm status from stream {} thread: {}",
                            self.stream_id, e
                        );
                    }

                    self.next_buffer += self.period;
                    let elapsed = self.start_time.elapsed();
                    if elapsed < self.next_buffer {
                        // Completing an IO request can be considered an elapsed period
                        // notification by the driver, so we must wait the right amount of time to
                        // release the buffer if the sound server client returned too soon.
                        std::thread::sleep(self.next_buffer - elapsed);
                    }
                    {
                        let mut io_queue_lock = self.io_queue.lock();
                        io_queue_lock.add_used(
                            &self.guest_memory,
                            desc_index,
                            writer.bytes_written() as u32,
                        );
                        let interrupt_lock = self.interrupt.lock();
                        io_queue_lock.trigger_interrupt(&self.guest_memory, interrupt_lock.deref());
                    }
                }
            }
            StreamState::Stopped | StreamState::Released => {
                // For some reason playback buffers can arrive after stop and release (maybe because
                // buffer-ready notifications arrive over eventfds and those are processed in
                // random order?). The spec requires the device to not confirm the release of a
                // stream until all IO buffers have been released, but that's impossible to
                // guarantee if a buffer arrives after release is requested. Luckily it seems to
                // work fine if the buffer is released after the release command is completed.
                while let Some(desc) = self.buffer_queue.pop_front() {
                    reply_pcm_buffer_status(
                        VIRTIO_SND_S_OK,
                        0,
                        desc,
                        &self.guest_memory,
                        &self.io_queue,
                        &self.interrupt,
                    )?;
                }
            }
            StreamState::Prepared => {} // Do nothing, any buffers will be processed after start
            _ => {
                if self.buffer_queue.len() > 0 {
                    warn!("virtio-snd: Buffers received while in unexpected state");
                }
            }
        }
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // Try to stop and release the stream in case it was playing, these operations will fail if the
        // stream is already released, just ignore that failure
        let _ = self.vios_client.stop_stream(self.stream_id);
        let _ = self.vios_client.release_stream(self.stream_id);

        // Also release any pending buffer
        while let Some(desc) = self.buffer_queue.pop_front() {
            if let Err(e) = reply_pcm_buffer_status(
                VIRTIO_SND_S_IO_ERR,
                0,
                desc,
                &self.guest_memory,
                &self.io_queue,
                &self.interrupt,
            ) {
                error!(
                    "virtio-snd: Failed to reply buffer on stream {}: {}",
                    self.stream_id, e
                );
            }
        }
    }
}

/// Basically a proxy to the thread handling a particular stream.
pub struct StreamProxy {
    sender: Sender<StreamMsg>,
    thread: Option<thread::JoinHandle<()>>,
}

impl StreamProxy {
    /// Access the underlying sender to clone it or send messages
    pub fn msg_sender<'a>(&'a self) -> &'a Sender<StreamMsg> {
        &self.sender
    }

    /// Send a message to the stream thread on the other side of this sender
    pub fn send_msg(sender: &Sender<StreamMsg>, msg: StreamMsg) -> Result<()> {
        sender.send(msg).map_err(SoundError::StreamThreadSend)
    }

    /// Convenience function to send a message to this stream's thread
    pub fn send(&self, msg: StreamMsg) -> Result<()> {
        Self::send_msg(&self.sender, msg)
    }

    fn stop_thread(&mut self) {
        if let Err(e) = self.send(StreamMsg::Break) {
            error!(
                "virtio-snd: Failed to send Break msg to stream thread: {}",
                e
            );
        }
        if let Some(th) = self.thread.take() {
            if let Err(e) = th.join() {
                error!("virtio-snd: Panic detected on stream thread: {:?}", e);
            }
        }
    }
}

impl Drop for StreamProxy {
    fn drop(&mut self) {
        self.stop_thread();
    }
}

/// Attempts to set the current thread's priority to a value hight enough to handle audio IO. This
/// may fail due to insuficient permissions.
pub fn try_set_real_time_priority() {
    const AUDIO_THREAD_RTPRIO: u16 = 10; // Matches other cros audio clients.
    if let Err(e) = set_rt_prio_limit(u64::from(AUDIO_THREAD_RTPRIO))
        .and_then(|_| set_rt_round_robin(i32::from(AUDIO_THREAD_RTPRIO)))
    {
        warn!("Failed to set audio stream thread to real time: {}", e);
    }
}

fn vios_error_to_status_code(e: VioSError) -> u32 {
    match e {
        VioSError::ServerIOError(_) => VIRTIO_SND_S_IO_ERR,
        _ => VIRTIO_SND_S_NOT_SUPP,
    }
}

/// Encapsulates sending the virtio_snd_hdr struct back to the driver.
pub fn reply_control_op_status(
    code: u32,
    desc: DescriptorChain,
    guest_memory: &GuestMemory,
    queue: &Arc<Mutex<Queue>>,
    interrupt: &Arc<Mutex<Interrupt>>,
) -> Result<()> {
    let desc_index = desc.index;
    let mut writer = Writer::new(guest_memory.clone(), desc).map_err(SoundError::Descriptor)?;
    writer
        .write_obj(virtio_snd_hdr {
            code: Le32::from(code),
        })
        .map_err(SoundError::QueueIO)?;
    {
        // Lock queue first, then the interrupt
        let mut queue_lock = queue.lock();
        let interrupt_lock = interrupt.lock();
        queue_lock.add_used(guest_memory, desc_index, writer.bytes_written() as u32);
        queue_lock.trigger_interrupt(guest_memory, interrupt_lock.deref());
    }
    Ok(())
}

/// Encapsulates sending the virtio_snd_pcm_status struct back to the driver.
pub fn reply_pcm_buffer_status(
    status: u32,
    latency_bytes: u32,
    desc: DescriptorChain,
    guest_memory: &GuestMemory,
    queue: &Arc<Mutex<Queue>>,
    interrupt: &Arc<Mutex<Interrupt>>,
) -> Result<()> {
    let desc_index = desc.index;
    let mut writer = Writer::new(guest_memory.clone(), desc).map_err(SoundError::Descriptor)?;
    if writer.available_bytes() > std::mem::size_of::<virtio_snd_pcm_status>() {
        writer
            .consume_bytes(writer.available_bytes() - std::mem::size_of::<virtio_snd_pcm_status>());
    }
    writer
        .write_obj(virtio_snd_pcm_status {
            status: Le32::from(status),
            latency_bytes: Le32::from(latency_bytes),
        })
        .map_err(SoundError::QueueIO)?;
    {
        // Lock queue first, then the interrupt
        let mut queue_lock = queue.lock();
        let interrupt_lock = interrupt.lock();
        queue_lock.add_used(guest_memory, desc_index, writer.bytes_written() as u32);
        queue_lock.trigger_interrupt(guest_memory, interrupt_lock.deref());
    }
    Ok(())
}

fn bytes_per_sample(format: u8) -> usize {
    match format {
        VIRTIO_SND_PCM_FMT_IMA_ADPCM => 1usize,
        VIRTIO_SND_PCM_FMT_MU_LAW => 1usize,
        VIRTIO_SND_PCM_FMT_A_LAW => 1usize,
        VIRTIO_SND_PCM_FMT_S8 => 1usize,
        VIRTIO_SND_PCM_FMT_U8 => 1usize,
        VIRTIO_SND_PCM_FMT_S16 => 2usize,
        VIRTIO_SND_PCM_FMT_U16 => 2usize,
        VIRTIO_SND_PCM_FMT_S32 => 4usize,
        VIRTIO_SND_PCM_FMT_U32 => 4usize,
        VIRTIO_SND_PCM_FMT_FLOAT => 4usize,
        VIRTIO_SND_PCM_FMT_FLOAT64 => 8usize,
        // VIRTIO_SND_PCM_FMT_DSD_U8
        // VIRTIO_SND_PCM_FMT_DSD_U16
        // VIRTIO_SND_PCM_FMT_DSD_U32
        // VIRTIO_SND_PCM_FMT_IEC958_SUBFRAME
        // VIRTIO_SND_PCM_FMT_S18_3
        // VIRTIO_SND_PCM_FMT_U18_3
        // VIRTIO_SND_PCM_FMT_S20_3
        // VIRTIO_SND_PCM_FMT_U20_3
        // VIRTIO_SND_PCM_FMT_S24_3
        // VIRTIO_SND_PCM_FMT_U24_3
        // VIRTIO_SND_PCM_FMT_S20
        // VIRTIO_SND_PCM_FMT_U20
        // VIRTIO_SND_PCM_FMT_S24
        // VIRTIO_SND_PCM_FMT_U24
        _ => {
            // Some of these formats are not consistently stored in a particular size (24bits is
            // sometimes stored in a 32bit word) while others are of variable size.
            // The size per sample estimated here is designed to greatly underestimate the time it
            // takes to play a buffer and depend instead on timings provided by the sound server if
            // it supports these formats.
            warn!(
                "Unknown sample size for format {}, depending on sound server timing instead.",
                format
            );
            1000usize
        }
    }
}

fn rate_from_virtio_constant(frame_rate: u8) -> u64 {
    match frame_rate {
        VIRTIO_SND_PCM_RATE_5512 => 5512u64,
        VIRTIO_SND_PCM_RATE_8000 => 8000u64,
        VIRTIO_SND_PCM_RATE_11025 => 11025u64,
        VIRTIO_SND_PCM_RATE_16000 => 16000u64,
        VIRTIO_SND_PCM_RATE_22050 => 22050u64,
        VIRTIO_SND_PCM_RATE_32000 => 32000u64,
        VIRTIO_SND_PCM_RATE_44100 => 44100u64,
        VIRTIO_SND_PCM_RATE_48000 => 48000u64,
        VIRTIO_SND_PCM_RATE_64000 => 64000u64,
        VIRTIO_SND_PCM_RATE_88200 => 88200u64,
        VIRTIO_SND_PCM_RATE_96000 => 96000u64,
        VIRTIO_SND_PCM_RATE_176400 => 176400u64,
        VIRTIO_SND_PCM_RATE_192000 => 192000u64,
        VIRTIO_SND_PCM_RATE_384000 => 384000u64,
        _ => 0u64,
    }
}
