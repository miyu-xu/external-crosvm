// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Apple Hypervisor.framework (**HVF**) backend for AArch64 hosts.
//!
//! Build with `--features hvf` on `aarch64-apple-darwin`. This uses the
//! [`applevisor_sys`](https://docs.rs/applevisor-sys) bindings.

mod vcpu;
mod vm;

pub use vcpu::HvfVcpu;
pub use vm::dirty_log_bitmap_size;
pub use vm::HvfHypervisor;
pub use vm::HvfVm;

/// Returns true when this crate was built for Apple Silicon with the `hvf` feature.
pub fn is_supported_target() -> bool {
    true
}
