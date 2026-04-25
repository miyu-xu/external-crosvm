// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fs::File;
use std::io::Error;
use std::io::ErrorKind;
use std::io::Result;
use std::os::fd::AsRawFd;

use crate::FileAllocate;

impl FileAllocate for File {
    fn allocate(&self, offset: u64, len: u64) -> Result<()> {
        let end = offset
            .checked_add(len)
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?;

        let mut store = libc::fstore_t {
            fst_flags: libc::F_ALLOCATECONTIG,
            fst_posmode: libc::F_PEOFPOSMODE,
            fst_offset: offset as libc::off_t,
            fst_length: len as libc::off_t,
            fst_bytesalloc: 0,
        };

        // SAFETY: the file descriptor is valid for the duration of the call.
        let ret = unsafe { libc::fcntl(self.as_raw_fd(), libc::F_PREALLOCATE, &store) };
        if ret == -1 {
            store.fst_flags = libc::F_ALLOCATEALL;
            // SAFETY: the file descriptor is valid for the duration of the call.
            let ret = unsafe { libc::fcntl(self.as_raw_fd(), libc::F_PREALLOCATE, &store) };
            if ret == -1 {
                return Err(Error::last_os_error());
            }
        }

        if self.metadata()?.len() < end {
            self.set_len(end)?;
        }

        Ok(())
    }
}

// Shim module for file_traits bridge
pub mod lib {
    // Re-export libc types and functions needed for I/O operations
    pub use libc::c_int;
    pub use libc::c_void;
    pub use libc::iovec;
    pub use libc::size_t;

    // Shim implementations wrapping libc functions
    #[inline]
    pub unsafe fn read(fd: c_int, buf: *mut c_void, count: size_t) -> isize {
        libc::read(fd, buf, count)
    }

    #[inline]
    pub unsafe fn readv(fd: c_int, iov: *const iovec, iovcnt: c_int) -> isize {
        libc::readv(fd, iov, iovcnt)
    }

    #[inline]
    pub unsafe fn write(fd: c_int, buf: *const c_void, count: size_t) -> isize {
        libc::write(fd, buf, count)
    }

    #[inline]
    pub unsafe fn writev(fd: c_int, iov: *const iovec, iovcnt: c_int) -> isize {
        libc::writev(fd, iov, iovcnt)
    }
}
