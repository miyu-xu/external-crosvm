// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Serial bring-up for macOS hosts (no device process / minijail).

#![allow(dead_code)]

use base::RawDescriptor;
use devices::serial_device::SerialParameters;
use devices::BusDevice;
use devices::Serial;
use jail::FakeMinijailStub as Minijail;
use std::sync::Arc;
use sync::Mutex;

use crate::DeviceRegistrationError;

pub fn add_serial_device(
    com: Serial,
    _serial_params: &SerialParameters,
    serial_jail: Option<Minijail>,
    _preserved_descriptors: Vec<RawDescriptor>,
) -> std::result::Result<Arc<Mutex<dyn BusDevice>>, DeviceRegistrationError> {
    assert!(serial_jail.is_none());
    Ok(Arc::new(Mutex::new(com)))
}
