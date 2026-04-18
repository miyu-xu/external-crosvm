// Copyright 2017 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Linux-compatible `base::linux` API for macOS (Apple Silicon HVF bring-up).

#[macro_use]
#[path = "../linux/ioctl.rs"]
pub mod ioctl;

#[macro_use]
pub mod syslog {
    pub use crate::sys::macos::syslog::PlatformSyslog;
}

mod capabilities;
mod event;
mod file;
mod file_traits;
mod net;
mod notifiers;
#[path = "../linux/platform_timer_resolution.rs"]
mod platform_timer_resolution;
mod sched;
mod shm;
pub mod signal;
mod signalfd;
mod stubs;

use std::fs::remove_file;
use std::fs::File;
use std::fs::OpenOptions;
use std::mem;
use std::mem::MaybeUninit;
use std::ops::Deref;
use std::os::unix::io::FromRawFd;
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixDatagram;
use std::os::unix::net::UnixListener;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::ptr;
use std::time::Duration;

pub use capabilities::drop_capabilities;
pub use event::EventExt;
pub(crate) use crate::sys::macos::event::PlatformEvent;
pub use file::find_next_data;
pub use file::FileDataIterator;
pub(crate) use file_traits::lib::*;
pub use ioctl::*;
use libc::c_int;
use libc::fcntl;
use libc::waitpid;
use libc::EINVAL;
use libc::SIGKILL;
use libc::WNOHANG;
pub use crate::sys::macos::mmap::*;
pub(in crate::sys) use net::sendmsg_nosignal;
pub(in crate::sys) use net::sockaddr_un;
pub(in crate::sys) use net::sockaddrv4_to_lib_c;
pub(in crate::sys) use net::sockaddrv6_to_lib_c;
pub use crate::sys::macos::poll::EventContext;
pub use crate::sys::macos::fallocate;
pub use crate::sys::macos::FallocateMode;
pub(crate) use crate::sys::macos::file_punch_hole;
pub(crate) use crate::sys::macos::file_write_zeroes_at;
#[path = "../linux/priority.rs"]
mod priority;
pub use platform_timer_resolution::*;
pub use priority::*;
pub use sched::*;
pub use shm::MemfdSeals;
pub use shm::SharedMemoryLinux;
pub use signal::*;
pub use signalfd::Error as SignalFdError;
pub use signalfd::*;
pub use stubs::AcpiNotifyEvent;
pub use stubs::NetlinkGenericSocket;
#[path = "../linux/terminal.rs"]
mod terminal;
pub use terminal::*;

use log::warn;

use crate::descriptor::FromRawDescriptor;
use crate::descriptor::SafeDescriptor;
pub use crate::errno::Error;
pub use crate::errno::Result;
pub use crate::errno::*;
pub use crate::errno_result;
use crate::round_up_to_page_size;
pub use crate::sys::unix::descriptor::*;
use crate::syscall;
use crate::AsRawDescriptor;
use crate::Pid;

pub type Uid = libc::uid_t;
pub type Gid = libc::gid_t;
pub type Mode = libc::mode_t;

#[inline(always)]
pub fn getpid() -> Pid {
    // SAFETY: always successful on Darwin.
    unsafe { libc::getpid() }
}

#[inline(always)]
pub fn getppid() -> Pid {
    unsafe { libc::getppid() }
}

pub fn gettid() -> Pid {
    getpid()
}

#[inline(always)]
pub fn geteuid() -> Uid {
    // SAFETY: trivially safe
    unsafe { libc::geteuid() }
}

#[inline(always)]
pub fn getegid() -> Gid {
    // SAFETY: trivially safe
    unsafe { libc::getegid() }
}

pub enum FlockOperation {
    LockShared,
    LockExclusive,
    Unlock,
}

pub fn flock<F: AsRawDescriptor>(file: &F, op: FlockOperation, nonblocking: bool) -> Result<()> {
    let mut operation = match op {
        FlockOperation::LockShared => libc::LOCK_SH,
        FlockOperation::LockExclusive => libc::LOCK_EX,
        FlockOperation::Unlock => libc::LOCK_UN,
    };
    if nonblocking {
        operation |= libc::LOCK_NB;
    }
    syscall!(unsafe { libc::flock(file.as_raw_descriptor(), operation) }).map(|_| ())
}

pub fn fstat<F: AsRawDescriptor>(f: &F) -> Result<libc::stat> {
    let mut st = MaybeUninit::<libc::stat>::zeroed();
    syscall!(unsafe { libc::fstat(f.as_raw_descriptor(), st.as_mut_ptr()) })?;
    Ok(unsafe { st.assume_init() })
}

pub fn is_block_file<F: AsRawDescriptor>(file: &F) -> Result<bool> {
    let stat = fstat(file)?;
    Ok((stat.st_mode & libc::S_IFMT) == libc::S_IFBLK)
}

ioctl_io_nr!(BLKDISCARD, 0x12u32, 119);

pub fn discard_block<F: AsRawDescriptor>(_file: &F, _offset: u64, _len: u64) -> Result<()> {
    Err(Error::new(libc::ENOTSUP))
}

pub trait AsRawPid {
    fn as_raw_pid(&self) -> Pid;
}

impl AsRawPid for Pid {
    fn as_raw_pid(&self) -> Pid {
        *self
    }
}

impl AsRawPid for std::process::Child {
    fn as_raw_pid(&self) -> Pid {
        self.id() as Pid
    }
}

pub fn wait_for_pid<A: AsRawPid>(pid: A, options: c_int) -> Result<(Option<Pid>, ExitStatus)> {
    let pid = pid.as_raw_pid();
    let mut status: c_int = 1;
    let ret = unsafe { libc::waitpid(pid, &mut status, options) };
    if ret < 0 {
        return errno_result();
    }
    Ok((
        if ret == 0 { None } else { Some(ret) },
        ExitStatus::from_raw(status),
    ))
}

pub fn reap_child() -> Result<Pid> {
    let ret = unsafe { waitpid(-1, ptr::null_mut(), WNOHANG) };
    if ret == -1 {
        errno_result()
    } else {
        Ok(ret)
    }
}

pub fn kill_process_group() -> Result<()> {
    // SAFETY: `kill(0, SIGKILL)` targets the current process group.
    unsafe {
        libc::kill(0, SIGKILL);
    }
    unreachable!();
}

pub use crate::sys::macos::pipe;

pub fn set_pipe_size(fd: RawFd, size: usize) -> Result<usize> {
    syscall!(unsafe { fcntl(fd, libc::F_SETPIPE_SZ, size as c_int) }).map(|ret| ret as usize)
}

pub fn new_pipe_full() -> Result<(File, File)> {
    use std::io::Write;

    let (rx, mut tx) = pipe()?;
    let page_size = set_pipe_size(tx.as_raw_descriptor(), round_up_to_page_size(1))?;
    let buf = vec![0u8; page_size];
    tx.write_all(&buf)?;
    Ok((rx, tx))
}

pub struct UnlinkUnixDatagram(pub UnixDatagram);
impl AsRef<UnixDatagram> for UnlinkUnixDatagram {
    fn as_ref(&self) -> &UnixDatagram {
        &self.0
    }
}
impl Drop for UnlinkUnixDatagram {
    fn drop(&mut self) {
        if let Ok(addr) = self.0.local_addr() {
            if let Some(path) = addr.as_pathname() {
                if let Err(e) = remove_file(path) {
                    warn!("failed to remove control socket file: {}", e);
                }
            }
        }
    }
}

pub struct UnlinkUnixListener(pub UnixListener);

impl AsRef<UnixListener> for UnlinkUnixListener {
    fn as_ref(&self) -> &UnixListener {
        &self.0
    }
}

impl Deref for UnlinkUnixListener {
    type Target = UnixListener;

    fn deref(&self) -> &UnixListener {
        &self.0
    }
}

impl Drop for UnlinkUnixListener {
    fn drop(&mut self) {
        if let Ok(addr) = self.0.local_addr() {
            if let Some(path) = addr.as_pathname() {
                if let Err(e) = remove_file(path) {
                    warn!("failed to remove control socket file: {}", e);
                }
            }
        }
    }
}

pub fn validate_raw_descriptor(raw_descriptor: RawDescriptor) -> Result<RawDescriptor> {
    validate_raw_fd(&raw_descriptor)
}

pub fn validate_raw_fd(raw_fd: &RawFd) -> Result<RawFd> {
    let flags = unsafe { libc::fcntl(*raw_fd, libc::F_GETFD) };
    if flags < 0 || (flags & libc::FD_CLOEXEC) != 0 {
        return Err(Error::new(libc::EBADF));
    }
    let dup_fd = unsafe { libc::fcntl(*raw_fd, libc::F_DUPFD_CLOEXEC, 0) };
    if dup_fd < 0 {
        return Err(Error::last());
    }
    Ok(dup_fd as RawFd)
}

pub fn poll_in<F: AsRawDescriptor>(fd: &F) -> bool {
    let mut fds = libc::pollfd {
        fd: fd.as_raw_descriptor(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ret = unsafe { libc::poll(&mut fds, 1, 0) };
    if ret == -1 {
        return false;
    }
    fds.revents & libc::POLLIN != 0
}

pub fn max_timeout() -> Duration {
    Duration::new(libc::time_t::MAX as u64, 999999999)
}

pub fn safe_descriptor_from_path<P: AsRef<Path>>(_path: P) -> Result<Option<SafeDescriptor>> {
    Ok(None)
}

pub fn safe_descriptor_from_cmdline_fd(fd: &RawFd) -> Result<SafeDescriptor> {
    let validated_fd = validate_raw_fd(fd)?;
    Ok(unsafe { SafeDescriptor::from_raw_descriptor(validated_fd) })
}

pub fn open_file_or_duplicate<P: AsRef<Path>>(path: P, options: &OpenOptions) -> Result<File> {
    options.open(path.as_ref()).map_err(|e| Error::new(e.raw_os_error().unwrap_or(EINVAL)))
}

pub fn max_open_files() -> Result<libc::rlimit64> {
    let mut r = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let res = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) };
    if res == 0 {
        Ok(libc::rlimit64 {
            rlim_cur: r.rlim_cur.into(),
            rlim_max: r.rlim_max.into(),
        })
    } else {
        errno_result()
    }
}

pub fn call_with_extended_max_files<T, E>(
    callback: impl FnOnce() -> std::result::Result<T, E>,
) -> Result<std::result::Result<T, E>> {
    let cur_limit = max_open_files()?;
    let new_limit = libc::rlimit64 {
        rlim_cur: cur_limit.rlim_max,
        ..cur_limit
    };
    let needs_extension = cur_limit.rlim_cur < new_limit.rlim_cur;
    if needs_extension {
        set_max_open_files(new_limit)?;
    }
    let r = callback();
    if needs_extension {
        set_max_open_files(cur_limit)?;
    }
    Ok(r)
}

fn set_max_open_files(limit: libc::rlimit64) -> Result<()> {
    let r = libc::rlimit {
        rlim_cur: limit.rlim_cur.try_into().map_err(|_| Error::new(EINVAL))?,
        rlim_max: limit.rlim_max.try_into().map_err(|_| Error::new(EINVAL))?,
    };
    let res = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &r) };
    if res == 0 {
        Ok(())
    } else {
        errno_result()
    }
}

pub fn move_to_cgroup(_cgroup_path: PathBuf, _id_to_write: Pid, _cgroup_file: &str) -> Result<()> {
    Err(Error::new(libc::ENOTSUP))
}

pub fn move_task_to_cgroup(cgroup_path: PathBuf, thread_id: Pid) -> Result<()> {
    move_to_cgroup(cgroup_path, thread_id, "tasks")
}

pub fn move_proc_to_cgroup(cgroup_path: PathBuf, process_id: Pid) -> Result<()> {
    move_to_cgroup(cgroup_path, process_id, "cgroup.procs")
}

pub fn logical_core_frequencies_khz(_cpu_id: usize) -> Result<Vec<u32>> {
    Ok(vec![])
}

pub fn logical_core_capacity(cpu_id: usize) -> Result<u32> {
    let _ = cpu_id;
    Ok(1024)
}

pub fn logical_core_cluster_id(cpu_id: usize) -> Result<u32> {
    Ok(cpu_id as u32)
}

pub fn logical_core_max_freq_khz(_cpu_id: usize) -> Result<u32> {
    Ok(0)
}
