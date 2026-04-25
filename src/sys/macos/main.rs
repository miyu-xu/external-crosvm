// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use anyhow::Context;
use base::syslog;
use base::syslog::LogArgs;
use base::syslog::LogConfig;

use crate::crosvm::sys::cmdline::Commands;
use crate::crosvm::sys::cmdline::DeviceSubcommand;
use crate::CommandStatus;
use crate::Config;

pub(crate) fn start_device(command: DeviceSubcommand) -> anyhow::Result<()> {
    match command {
        DeviceSubcommand::Unsupported(_) => Err(anyhow!(
            "macOS does not support sys-managed device subprocess commands"
        )),
    }
}

pub(crate) fn cleanup() {}

pub fn get_library_watcher() -> std::io::Result<()> {
    Ok(())
}

pub(crate) fn run_command(command: Commands, _log_args: LogArgs) -> anyhow::Result<()> {
    match command {
        Commands::Unsupported(_) => Err(anyhow!("macOS does not support sys subcommands")),
    }
}

pub(crate) fn init_log(log_config: LogConfig, _cfg: &Config) -> anyhow::Result<()> {
    syslog::init_with(log_config).context("failed to initialize syslog")
}

pub(crate) fn error_to_exit_code(_res: &std::result::Result<CommandStatus, anyhow::Error>) -> i32 {
    1
}
