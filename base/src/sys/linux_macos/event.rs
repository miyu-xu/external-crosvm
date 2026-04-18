// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Linux `EventExt` (eventfd counters) approximated on top of macOS `Event` (kqueue / EVFILT_USER).

use crate::errno::Result;

/// Linux-specific extensions to `Event`.
pub trait EventExt {
    /// Adds `v` to the eventfd's count, blocking until this won't overflow the count.
    fn write_count(&self, v: u64) -> Result<()>;
    /// Blocks until the the eventfd's count is non-zero, then resets the count to zero.
    fn read_count(&self) -> Result<u64>;
}

impl EventExt for crate::Event {
    fn write_count(&self, mut v: u64) -> Result<()> {
        while v > 0 {
            self.signal()?;
            v -= 1;
        }
        Ok(())
    }

    fn read_count(&self) -> Result<u64> {
        self.wait()?;
        Ok(1)
    }
}
