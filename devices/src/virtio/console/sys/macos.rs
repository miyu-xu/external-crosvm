// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::VecDeque;
use std::sync::Arc;

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
    input: InStreamType,
    _in_avail_evt: Event,
    _input_buffer: Arc<Mutex<VecDeque<u8>>>,
) -> WorkerThread<InStreamType> {
    // macOS does not currently support the Linux-style WaitContext-based console stdin path
    // used by crosvm. Keep console output working and leave stdin disabled for now.
    WorkerThread::start("v_console_input", move |_kill_evt| input)
}
