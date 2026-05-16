// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Stub for macOS: the vhost-user GPU device backend is Linux-only.

use argh::FromArgs;

use crate::virtio::GpuParameters;
use crate::virtio::Interrupt;
use crate::virtio::vhost::user::device::gpu::GpuBackend;

#[derive(FromArgs)]
/// GPU device (stub on macOS)
#[argh(subcommand, name = "gpu")]
pub struct Options {
    #[argh(option, arg_name = "PATH")]
    /// path to bind a listening vhost-user socket
    socket: String,
    #[argh(option, default = "Default::default()", arg_name = "JSON")]
    /// a JSON object of virtio-gpu parameters
    params: GpuParameters,
}

impl GpuBackend {
    pub fn start_platform_workers(&mut self, _interrupt: Interrupt) -> anyhow::Result<()> {
        anyhow::bail!("vhost-user GPU device is not supported on macOS");
    }
}

pub fn run_gpu_device(_opts: Options) -> anyhow::Result<()> {
    anyhow::bail!("vhost-user GPU device is not supported on macOS");
}
