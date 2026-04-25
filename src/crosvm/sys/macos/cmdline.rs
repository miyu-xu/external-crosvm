// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;

#[derive(FromArgs)]
#[argh(subcommand, name = "devices")]
/// Placeholder for unsupported macOS-only sys device command group.
pub struct DevicesCommand {
    #[argh(subcommand)]
    pub command: DeviceSubcommand,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub enum DeviceSubcommand {
    Unsupported(UnsupportedDeviceCommand),
}

#[derive(FromArgs)]
#[argh(subcommand, name = "unsupported")]
/// Placeholder for unsupported macOS-only device-process commands.
pub struct UnsupportedDeviceCommand {}

#[derive(FromArgs)]
#[argh(subcommand)]
pub enum Commands {
    Unsupported(UnsupportedCommand),
}

#[derive(FromArgs)]
#[argh(subcommand, name = "unsupported")]
/// Placeholder for unsupported macOS-only sys commands.
pub struct UnsupportedCommand {}
