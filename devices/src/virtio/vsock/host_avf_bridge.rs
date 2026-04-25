// Copyright 2026 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host paths for bridging guest virtio-vsock to Android RPC Binder “vsock” on the host.
//!
//! Must stay in sync with:
//! - Windows: `frameworks/native/libs/binder/platform/namedpipe_vsock.h` (`NamedPipeVsockAddress`)
//! - macOS: `frameworks/native/libs/binder/platform/macos_uds_vsock_path.cpp` (`binderRpcVsockHostPath`)
//! - Rust virtmgr: `packages/modules/Virtualization/android/virtmgr/src/vsock_transport.rs`
//!
//! **Windows VMM**: `sys/windows/vsock.rs` connects guest-initiated vsock to the named pipe below
//! when `host_guid` is not set (AVF / Android Virtualization flow).
//!
//! **macOS VMM**: `sys/macos/vsock.rs` connects guest-initiated vsock to the UDS path below when
//! `host_guid` is not set (AVF / Android Virtualization flow).

/// `\\.\pipe\binder_rpc_vsock_{guest_cid}_{host_port}` — matches libbinder `NamedPipeVsockAddress`.
pub fn windows_binder_rpc_pipe_path(guest_cid: u64, host_port: u32) -> String {
    format!(
        r"\\.\pipe\binder_rpc_vsock_{}_{}",
        guest_cid as u32, host_port
    )
}

/// `/tmp/binder_rpc_vsock_{guest_cid}_{host_port}.sock` — matches `binderRpcVsockHostPath` on macOS.
pub fn macos_binder_rpc_uds_path(guest_cid: u64, host_port: u32) -> String {
    format!(
        "/tmp/binder_rpc_vsock_{}_{}.sock",
        guest_cid as u32, host_port
    )
}
