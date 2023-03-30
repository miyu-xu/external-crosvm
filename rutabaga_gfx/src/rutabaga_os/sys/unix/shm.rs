// Copyright 2017 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::convert::TryInto;
use std::ffi::CStr;
use std::os::unix::io::FromRawFd;
use std::os::unix::io::OwnedFd;

use libc::c_char;
use libc::c_int;
use libc::c_long;
use libc::c_uint;
use libc::off_t;
use nix::unistd::ftruncate;
use nix::unistd::sysconf;
use nix::unistd::SysconfVar;

use crate::rutabaga_os::descriptor::AsRawDescriptor;
use crate::rutabaga_os::descriptor::IntoRawDescriptor;
use crate::rutabaga_os::RawDescriptor;
use crate::rutabaga_utils::RutabagaError;
use crate::rutabaga_utils::RutabagaResult;

const MFD_CLOEXEC: c_uint = 0x0001;

unsafe fn memfd_create(name: *const c_char, flags: c_uint) -> c_int {
    libc::syscall(libc::SYS_memfd_create as c_long, name, flags) as c_int
}

pub struct SharedMemory {
    fd: OwnedFd,
    size: u64,
}

impl SharedMemory {
    /// Creates a new shared memory file descriptor with zero size.
    ///
    /// If a name is given, it will appear in `/proc/self/fd/<shm fd>` for the purposes of
    /// debugging. The name does not need to be unique.
    ///
    /// The file descriptor is opened with the close on exec flag and allows memfd sealing.
    pub fn new(debug_name: &CStr, size: u64) -> RutabagaResult<SharedMemory> {
        let raw_fd = unsafe { memfd_create(debug_name.as_ptr() as *const c_char, MFD_CLOEXEC) };
        if raw_fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let size_off_t: off_t = size.try_into()?;
        ftruncate(raw_fd, size_off_t)?;

        // Nix will transition to owned fd in future releases, do it locally here.
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        Ok(SharedMemory { fd, size })
    }

    /// Gets the size in bytes of the shared memory.
    ///
    /// The size returned here does not reflect changes by other interfaces or users of the shared
    /// memory file descriptor..
    pub fn size(&self) -> u64 {
        self.size
    }
}

impl AsRawDescriptor for SharedMemory {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.fd.as_raw_descriptor()
    }
}

impl IntoRawDescriptor for SharedMemory {
    fn into_raw_descriptor(self) -> RawDescriptor {
        self.fd.into_raw_descriptor()
    }
}

/// Uses the system's page size in bytes to round the given value up to the nearest page boundary.
pub fn round_up_to_page_size(v: u64) -> RutabagaResult<u64> {
    let page_size_opt = sysconf(SysconfVar::PAGE_SIZE)?;
    if let Some(page_size) = page_size_opt {
        let page_mask = (page_size - 1) as u64;
        let aligned_size = (v + page_mask) & !page_mask;
        Ok(aligned_size)
    } else {
        Err(RutabagaError::SpecViolation("no page size"))
    }
}
