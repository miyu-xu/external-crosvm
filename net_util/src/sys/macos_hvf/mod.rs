// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! macOS vmnet.framework TAP interface for `--features hvf` builds.
//!
//! Uses vmnet.framework (macOS >= 10.15) via a C shim (`vmnet_shim.c`) that
//! bridges the block-based vmnet API to plain C for Rust FFI.
//!
//! Two implementations are available:
//! - [`VmnetTap`]: real vmnet.framework interface (default).
//! - [`tap::Tap`] / [`fakes::FakeTap`]: retained for fallback / testing.

pub mod net;
pub mod tap;
use base::FileReadWriteVolatile;
pub use net::VmnetTap;

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
