// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::number_of_logical_cores;
use crate::errno::Error;
use crate::errno::Result;
use crate::Pid;
use libc::EINVAL;

/// Set CPU affinity (no-op on macOS; host policy is not exposed like Linux `sched_setaffinity`).
pub fn set_cpu_affinity<I: IntoIterator<Item = usize>>(_cpus: I) -> Result<()> {
    Ok(())
}

pub fn get_cpu_affinity() -> Result<Vec<usize>> {
    let n = number_of_logical_cores()?;
    Ok((0..n).collect())
}

pub fn enable_core_scheduling() -> Result<()> {
    Ok(())
}

#[repr(C)]
pub struct sched_attr {
    pub size: u32,
    pub sched_policy: u32,
    pub sched_flags: u64,
    pub sched_nice: i32,
    pub sched_priority: u32,
    pub sched_runtime: u64,
    pub sched_deadline: u64,
    pub sched_period: u64,
    pub sched_util_min: u32,
    pub sched_util_max: u32,
}

impl sched_attr {
    pub fn default() -> Self {
        Self {
            size: std::mem::size_of::<sched_attr>() as u32,
            sched_policy: 0,
            sched_flags: 0,
            sched_nice: 0,
            sched_priority: 0,
            sched_runtime: 0,
            sched_deadline: 0,
            sched_period: 0,
            sched_util_min: 0,
            sched_util_max: 0,
        }
    }
}

pub fn sched_setattr(_pid: Pid, _attr: &mut sched_attr, _flags: u32) -> Result<()> {
    Err(Error::new(EINVAL))
}
