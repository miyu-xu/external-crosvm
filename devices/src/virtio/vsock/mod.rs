// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module implements the virtio vsock device.
//!
//! - **Linux/Android**: vhost-vsock (kernel).
//! - **Windows**: userspace virtio-vsock backed by named pipes; see [`host_avf_bridge`] for AVF
//!   paths when `host_guid` is unset.
//! - **macOS**: userspace virtio-vsock using Unix domain sockets; see [`host_avf_bridge`] for AVF
//!   paths that must match host libbinder.

mod host_avf_bridge;
mod sys;

pub use host_avf_bridge::macos_binder_rpc_uds_path;
pub use host_avf_bridge::windows_binder_rpc_pipe_path;
pub use sys::Vsock;
pub use sys::VsockConfig;
#[cfg(windows)]
pub use sys::VsockControlCommand;
#[cfg(windows)]
pub use sys::VsockControlResponse;
