// Copyright 2017 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//! Bindings for the Linux KVM (Kernel Virtual Machine) API.

#![cfg(unix)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use base::ioctl_io_nr;
use base::ioctl_ior_nr;
use base::ioctl_iow_nr;
use base::ioctl_iowr_nr;

#[cfg(any(target_arch = "aarch64"))]
pub mod aarch64 {
    pub mod bindings;
    use base::ioctl_ior_nr;
    use base::ioctl_iow_nr;
    pub use bindings::*;
}

#[cfg(any(target_arch = "aarch64"))]
pub use aarch64::*;
