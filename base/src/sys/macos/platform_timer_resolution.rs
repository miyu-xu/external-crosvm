// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::EnabledHighResTimer;
use crate::Result;

/// No-op on macOS (no programmable global timer resolution like Windows `timeBeginPeriod`).
pub struct UnixSetTimerResolution {}

impl EnabledHighResTimer for UnixSetTimerResolution {}

pub fn enable_high_res_timers() -> Result<Box<dyn EnabledHighResTimer>> {
    Ok(Box::new(UnixSetTimerResolution {}))
}
