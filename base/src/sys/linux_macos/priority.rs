// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::mem::MaybeUninit;

use super::errno_result;
use super::Result;

/// macOS does not expose Linux's RLIMIT_RTPRIO. Treat this as a no-op.
pub fn set_rt_prio_limit(_limit: u64) -> Result<()> {
    Ok(())
}

/// Sets the current thread to be scheduled using the round robin real time class with `priority`.
pub fn set_rt_round_robin(priority: i32) -> Result<()> {
    let mut sched_param: libc::sched_param = unsafe { MaybeUninit::zeroed().assume_init() };
    sched_param.sched_priority = priority;

    let res =
        unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_RR, &sched_param) };
    if res != 0 {
        errno_result()
    } else {
        Ok(())
    }
}
