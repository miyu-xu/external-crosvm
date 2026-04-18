// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! memfd-style seals API without Linux memfd / `F_ADD_SEALS` (not available on macOS).

use std::fs::File;

use crate::Result;
use crate::SharedMemory;

/// A set of memfd seals (bitmask tracking only; the kernel does not enforce seals on macOS).
#[derive(Copy, Clone, Default)]
pub struct MemfdSeals(i32);

impl MemfdSeals {
    #[inline]
    pub fn new() -> MemfdSeals {
        MemfdSeals(0)
    }

    #[inline]
    pub fn bitmask(self) -> i32 {
        self.0
    }

    #[inline]
    pub fn grow_seal(self) -> bool {
        self.0 & 1 != 0
    }

    #[inline]
    pub fn set_grow_seal(&mut self) {
        self.0 |= 1;
    }

    #[inline]
    pub fn shrink_seal(self) -> bool {
        self.0 & 2 != 0
    }

    #[inline]
    pub fn set_shrink_seal(&mut self) {
        self.0 |= 2;
    }

    #[inline]
    pub fn write_seal(self) -> bool {
        self.0 & 4 != 0
    }

    #[inline]
    pub fn set_write_seal(&mut self) {
        self.0 |= 4;
    }

    #[inline]
    pub fn future_write_seal(self) -> bool {
        self.0 & 8 != 0
    }

    #[inline]
    pub fn set_future_write_seal(&mut self) {
        self.0 |= 8;
    }

    #[inline]
    pub fn seal_seal(self) -> bool {
        self.0 & 16 != 0
    }

    #[inline]
    pub fn set_seal_seal(&mut self) {
        self.0 |= 16;
    }
}

pub trait SharedMemoryLinux {
    fn from_file(file: File) -> Result<SharedMemory>;
    fn get_seals(&self) -> Result<MemfdSeals>;
    fn add_seals(&mut self, seals: MemfdSeals) -> Result<()>;
}

impl SharedMemoryLinux for SharedMemory {
    fn from_file(mut file: File) -> Result<SharedMemory> {
        use std::io::Seek;
        use std::io::SeekFrom;

        let file_size = file.seek(SeekFrom::End(0))?;
        Ok(SharedMemory {
            descriptor: file.into(),
            size: file_size,
        })
    }

    fn get_seals(&self) -> Result<MemfdSeals> {
        Ok(MemfdSeals::new())
    }

    fn add_seals(&mut self, _seals: MemfdSeals) -> Result<()> {
        Ok(())
    }
}
