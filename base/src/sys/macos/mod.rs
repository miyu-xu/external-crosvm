// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cmp::min;
use std::fs::File;
use std::io;
use std::path::Path;

use crate::descriptor::FromRawDescriptor;
use crate::sys::unix::RawDescriptor;
use crate::unix::set_descriptor_cloexec;
use crate::unix::Pid;

pub(crate) mod event;
pub mod ioctl;
pub(in crate::sys::macos) mod kqueue;
pub(crate) mod mmap;
#[cfg(not(feature = "hvf"))]
mod net;
pub mod platform_timer_resolution;
pub(crate) mod poll;
mod shm;
mod timer;

pub(crate) use event::PlatformEvent;
pub use ioctl::*;
pub(in crate::sys) use libc::sendmsg;
pub use mmap::*;
#[cfg(not(feature = "hvf"))]
pub(in crate::sys) use net::sockaddr_un;
#[cfg(not(feature = "hvf"))]
pub(in crate::sys) use net::sockaddrv4_to_lib_c;
#[cfg(not(feature = "hvf"))]
pub(in crate::sys) use net::sockaddrv6_to_lib_c;
pub use platform_timer_resolution::*;
pub use poll::EventContext;

pub fn get_cpu_affinity() -> crate::errno::Result<Vec<usize>> {
    let n = crate::number_of_logical_cores()?;
    Ok((0..n).collect())
}

pub fn getpid() -> Pid {
    // SAFETY: `getpid` is always successful on Darwin.
    unsafe { libc::getpid() }
}

pub fn open_file_or_duplicate<P: AsRef<Path>>(
    path: P,
    options: &std::fs::OpenOptions,
) -> crate::Result<File> {
    Ok(options.open(path.as_ref())?)
}

pub fn set_cpu_affinity<I: IntoIterator<Item = usize>>(_cpus: I) -> crate::errno::Result<()> {
    // CPU affinity for the current thread is not exposed like Linux `sched_setaffinity`.
    Ok(())
}

/// The operation to perform with `fallocate` (not supported on macOS; returns `ENOTSUP`).
pub enum FallocateMode {
    PunchHole,
    ZeroRange,
    Allocate,
}

impl From<FallocateMode> for i32 {
    fn from(_value: FallocateMode) -> Self {
        0
    }
}

impl From<FallocateMode> for u32 {
    fn from(value: FallocateMode) -> Self {
        Into::<i32>::into(value) as u32
    }
}

/// macOS has no `posix_fallocate` in the same form; callers that need sparse files use Linux.
pub fn fallocate<F: crate::AsRawDescriptor>(
    _file: &F,
    _mode: FallocateMode,
    _offset: u64,
    _len: u64,
) -> crate::errno::Result<()> {
    Err(crate::Error::new(libc::ENOTSUP))
}

pub fn file_punch_hole(_file: &File, _offset: u64, _length: u64) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOTSUP))
}

pub fn file_write_zeroes_at(file: &File, offset: u64, length: usize) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;

    let buf_size = min(length, 0x10000);
    let buf = vec![0u8; buf_size];
    let mut nwritten: usize = 0;
    while nwritten < length {
        let remaining = length - nwritten;
        let write_size = min(remaining, buf_size);
        nwritten += file.write_at(&buf[0..write_size], offset + nwritten as u64)?;
    }
    Ok(length)
}

pub mod syslog {
    pub struct PlatformSyslog {}

    impl crate::syslog::Syslog for PlatformSyslog {
        fn new(
            _proc_name: String,
            _facility: crate::syslog::Facility,
        ) -> Result<
            (
                Option<Box<dyn crate::syslog::Log + Send>>,
                Option<crate::RawDescriptor>,
            ),
            crate::syslog::Error,
        > {
            Ok((None, None))
        }
    }
}

impl PartialEq for crate::SafeDescriptor {
    fn eq(&self, other: &Self) -> bool {
        if self.descriptor == other.descriptor {
            return true;
        }
        let mut sa = std::mem::MaybeUninit::<libc::stat>::uninit();
        let mut sb = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `fstat` writes only on success; we check return values.
        unsafe {
            if libc::fstat(self.descriptor, sa.as_mut_ptr()) != 0 {
                return false;
            }
            if libc::fstat(other.descriptor, sb.as_mut_ptr()) != 0 {
                return false;
            }
            let sa = sa.assume_init();
            let sb = sb.assume_init();
            sa.st_dev == sb.st_dev && sa.st_ino == sb.st_ino
        }
    }
}

pub(crate) use libc::off_t;
pub(crate) use libc::pread;
pub(crate) use libc::preadv;
pub(crate) use libc::pwrite;
pub(crate) use libc::pwritev;

/// Spawns a pipe pair where the first pipe is the read end and the second pipe is the write end.
///
/// The `O_CLOEXEC` flag will be applied after pipe creation.
pub fn pipe() -> crate::errno::Result<(File, File)> {
    let mut pipe_fds = [-1; 2];
    // SAFETY:
    // Safe because pipe will only write 2 element array of i32 to the given pointer, and we check
    // for error.
    let ret = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
    if ret == -1 {
        return crate::errno::errno_result();
    }

    // SAFETY:
    // Safe because both fds must be valid for pipe to have returned sucessfully and we have
    // exclusive ownership of them.
    let pipes = unsafe {
        (
            File::from_raw_descriptor(pipe_fds[0]),
            File::from_raw_descriptor(pipe_fds[1]),
        )
    };

    set_descriptor_cloexec(&pipes.0)?;
    set_descriptor_cloexec(&pipes.1)?;

    Ok(pipes)
}
