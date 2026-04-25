// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! POSIX shared memory (`shm_open`) backing for [`crate::SharedMemory`] on macOS.

use std::ffi::CStr;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use libc::c_char;

use crate::errno_result;
use crate::shm::PlatformSharedMemory;
use crate::AsRawDescriptor;
use crate::FromRawDescriptor;
use crate::Result;
use crate::SafeDescriptor;
use crate::SharedMemory;

static SHM_NAME_SEQ: AtomicU64 = AtomicU64::new(0);

fn shm_object_name(_debug_name: &CStr) -> std::ffi::CString {
    let seq = SHM_NAME_SEQ.fetch_add(1, Ordering::Relaxed);
    // SAFETY: POSIX `getpid` is always successful.
    let pid = unsafe { libc::getpid() };
    std::ffi::CString::new(format!("/crosvm_{pid}_{seq}")).expect("shm name has no NUL bytes")
}

impl PlatformSharedMemory for SharedMemory {
    fn new(debug_name: &CStr, size: u64) -> Result<SharedMemory> {
        if size > libc::off_t::MAX as u64 {
            return Err(crate::Error::new(libc::EINVAL));
        }
        let name = shm_object_name(debug_name);
        let name_ptr = name.as_ptr() as *const c_char;
        // SAFETY: `name` is NUL-terminated and obeys `shm_open` naming rules.
        let fd =
            unsafe { libc::shm_open(name_ptr, libc::O_CREAT | libc::O_EXCL | libc::O_RDWR, 0o600) };
        if fd < 0 {
            return errno_result();
        }
        // SAFETY: `shm_unlink` only needs a valid name pointer; the object stays open via `fd`.
        unsafe {
            let _ = libc::shm_unlink(name_ptr);
        }
        // SAFETY: `fd` is a valid owned descriptor from `shm_open`.
        let descriptor = unsafe { SafeDescriptor::from_raw_descriptor(fd) };
        crate::unix::set_descriptor_cloexec(&descriptor)?;
        // SAFETY: `size` fits `off_t` and `descriptor` is valid.
        let ret = unsafe { libc::ftruncate(descriptor.as_raw_descriptor(), size as libc::off_t) };
        if ret < 0 {
            return errno_result();
        }
        Ok(SharedMemory { descriptor, size })
    }

    fn from_safe_descriptor(descriptor: SafeDescriptor, size: u64) -> Result<SharedMemory> {
        Ok(SharedMemory { descriptor, size })
    }
}
