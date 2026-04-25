// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub(crate) mod main;

use anyhow::bail;

use crate::Config;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExitState {
    Reset,
    Stop,
    Crash,
    GuestPanic,
    WatchdogReset,
}

pub fn run_config(_cfg: Config) -> anyhow::Result<ExitState> {
    bail!("macOS HVF routing is isolated, but run_config is not wired yet");
}

#[cfg(not(feature = "crash-report"))]
pub(crate) fn set_panic_hook() {}
