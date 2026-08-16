// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::VecDeque;
use std::io;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use base::error;
use base::AsRawDescriptor;
use base::Event;
use base::FileSync;
use base::RawDescriptor;
use base::WorkerThread;
use sync::Mutex;

use crate::serial::sys::InStreamType;
use crate::serial_device::SerialInput;
use crate::serial_device::SerialOptions;
use crate::virtio::console::device::ConsoleDevice;
use crate::virtio::console::port::ConsolePort;
use crate::virtio::console::port::ConsolePortInfo;
use crate::virtio::console::Console;
use crate::virtio::ProtectionType;
use crate::SerialDevice;

impl SerialDevice for Console {
    fn new(
        protection_type: ProtectionType,
        _event: Event,
        input: Option<Box<dyn SerialInput>>,
        output: Option<Box<dyn std::io::Write + Send>>,
        _sync: Option<Box<dyn FileSync + Send>>,
        options: SerialOptions,
        keep_rds: Vec<RawDescriptor>,
    ) -> Console {
        Console::new(
            protection_type,
            input,
            output,
            keep_rds,
            options.pci_address,
        )
    }
}

impl SerialDevice for ConsoleDevice {
    fn new(
        protection_type: ProtectionType,
        _event: Event,
        input: Option<Box<dyn SerialInput>>,
        output: Option<Box<dyn std::io::Write + Send>>,
        _sync: Option<Box<dyn FileSync + Send>>,
        options: SerialOptions,
        keep_rds: Vec<RawDescriptor>,
    ) -> ConsoleDevice {
        let info = ConsolePortInfo {
            name: options.name,
            console: options.console,
        };
        let port = ConsolePort::new(input, output, Some(info), keep_rds);
        ConsoleDevice::new_single_port(protection_type, port)
    }
}

impl SerialDevice for ConsolePort {
    fn new(
        _protection_type: ProtectionType,
        _event: Event,
        input: Option<Box<dyn SerialInput>>,
        output: Option<Box<dyn std::io::Write + Send>>,
        _sync: Option<Box<dyn FileSync + Send>>,
        options: SerialOptions,
        keep_rds: Vec<RawDescriptor>,
    ) -> ConsolePort {
        let info = ConsolePortInfo {
            name: options.name,
            console: options.console,
        };
        ConsolePort::new(input, output, Some(info), keep_rds)
    }
}

pub(in crate::virtio::console) fn spawn_input_thread(
    mut input: InStreamType,
    in_avail_evt: Event,
    input_buffer: Arc<Mutex<VecDeque<u8>>>,
) -> WorkerThread<InStreamType> {
    WorkerThread::start("v_console_input", move |kill_evt| {
        if !input_buffer.lock().is_empty() {
            if let Err(e) = in_avail_evt.signal() {
                error!("failed to signal initial console input: {:#}", e);
            }
        }
        if let Err(e) = read_input(&mut input, &in_avail_evt, input_buffer, &kill_evt) {
            error!("console input thread exited with error: {:#}", e);
        }
        input
    })
}

/// Reads host console input on macOS and wakes the virtio-console worker.
///
/// `WaitContext` is Linux-only in this crosvm branch. Both the serial input FD
/// and macOS `Event` are pollable descriptors, so `poll(2)` provides the same
/// wake-on-input / wake-on-kill behavior without periodic busy waiting.
fn read_input(
    input: &mut InStreamType,
    in_avail_evt: &Event,
    input_buffer: Arc<Mutex<VecDeque<u8>>>,
    kill_evt: &Event,
) -> io::Result<()> {
    let mut poll_fds = [
        libc::pollfd {
            fd: input.get_read_notifier().as_raw_descriptor(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: kill_evt.as_raw_descriptor(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let mut bytes = [0u8; 4096];

    loop {
        // SAFETY: poll_fds is a valid, writable two-element pollfd array.
        let result = unsafe { libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as u32, -1) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }

        if poll_fds[1].revents != 0 {
            return Ok(());
        }
        if poll_fds[0].revents & (libc::POLLIN | libc::POLLHUP) == 0 {
            continue;
        }

        match input.read(&mut bytes) {
            Ok(0) => {
                // A temporarily writer-less FIFO reports HUP continuously.
                // Avoid spinning while still allowing a later bridge to attach.
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(count) => {
                input_buffer.lock().extend(&bytes[..count]);
                in_avail_evt.signal().map_err(|e| {
                    io::Error::other(format!("failed to signal console input: {e:#}"))
                })?;
            }
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
}
