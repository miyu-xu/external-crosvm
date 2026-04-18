// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! macOS TAP stubs for `--features hvf` builds.

pub mod tap;
use base::FileReadWriteVolatile;
pub use tap::Tap;

use crate::TapTCommon;

/// Linux-specific TAP functions (mirrors `sys/linux.rs`).
pub trait TapTLinux {
    fn set_vnet_hdr_size(&self, size: usize) -> Result<(), crate::Error>;
    fn if_flags(&self) -> u32;
}

pub trait TapT: FileReadWriteVolatile + TapTCommon + TapTLinux {}

pub mod fakes {
    pub use super::tap::fakes::FakeTap;
}
