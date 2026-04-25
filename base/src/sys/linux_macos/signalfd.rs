// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! `signalfd(2)`-compatible API on macOS using a self-pipe and `sigaction`.

use std::collections::VecDeque;
use std::fs::File;
use std::mem;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::FromRawFd;
use std::os::unix::io::RawFd;
use std::os::unix::process::ExitStatusExt;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use libc::c_int;
use libc::c_void;
use libc::read;
use libc::sigaction;
use libc::siginfo_t;
use libc::waitpid;
use libc::EAGAIN;
use libc::EINTR;
use libc::WNOHANG;
use log::error;
use remain::sorted;
use thiserror::Error;

use super::signal;
use super::Error as ErrnoError;
use super::RawDescriptor;
use crate::descriptor::AsRawDescriptor;

/// Linux `signalfd_siginfo` layout (see Linux `signalfd.h`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct signalfd_siginfo {
    pub ssi_signo: u32,
    pub ssi_errno: i32,
    pub ssi_code: i32,
    pub ssi_pid: u32,
    pub ssi_uid: u32,
    pub ssi_fd: i32,
    pub ssi_tid: u32,
    pub ssi_band: u32,
    pub ssi_overrun: u32,
    pub ssi_trapno: u32,
    pub ssi_status: i32,
    pub ssi_int: i32,
    pub ssi_ptr: u64,
    pub ssi_utime: u64,
    pub ssi_stime: u64,
    pub ssi_addr: u64,
    pub ssi_addr_lsb: u16,
    _pad2: [u8; 6],
    pub ssi_syscall: i32,
    pub ssi_call_addr: u64,
    pub ssi_arch: u32,
    _pad: [u8; 28],
}

#[sorted]
#[derive(Error, Debug)]
pub enum Error {
    #[error("failed to block the signal when creating signalfd: {0}")]
    CreateBlockSignal(signal::Error),
    #[error("failed to create a new signalfd: {0}")]
    CreateSignalFd(ErrnoError),
    #[error("failed to construct sigset when creating signalfd: {0}")]
    CreateSigset(ErrnoError),
    #[error("signalfd failed to return a full siginfo struct, read only {0} bytes")]
    SignalFdPartialRead(usize),
    #[error("unable to read from signalfd: {0}")]
    SignalFdRead(ErrnoError),
}

pub type Result<T> = std::result::Result<T, Error>;

static RELAY_WRITE_FD: AtomicI32 = AtomicI32::new(-1);
static RELAY_LOCK: Mutex<()> = Mutex::new(());

unsafe extern "C" fn relay_signal(_sig: c_int, _info: *mut siginfo_t, _ctx: *mut c_void) {
    let w = RELAY_WRITE_FD.load(Ordering::SeqCst);
    if w >= 0 {
        let _ = libc::write(w, [0u8].as_ptr() as *const c_void, 1);
    }
}

fn siginfo_from_wait_status(signal: c_int, pid: i32, status: c_int) -> signalfd_siginfo {
    let mut siginfo: signalfd_siginfo = unsafe { mem::zeroed() };
    siginfo.ssi_signo = signal as u32;
    siginfo.ssi_pid = pid as u32;
    let es = std::process::ExitStatus::from_raw(status);
    if let Some(code) = es.code() {
        siginfo.ssi_code = libc::CLD_EXITED;
        siginfo.ssi_status = code;
    } else if let Some(sig) = es.signal() {
        siginfo.ssi_code = libc::CLD_KILLED;
        siginfo.ssi_status = sig;
    } else {
        siginfo.ssi_code = libc::CLD_EXITED;
        siginfo.ssi_status = 0;
    }
    siginfo
}

/// A safe wrapper around a Linux `signalfd`, approximated on macOS for `SIGCHLD` only.
pub struct SignalFd {
    read_end: File,
    _write_end: File,
    signal: c_int,
    old_action: libc::sigaction,
    pending: Mutex<VecDeque<signalfd_siginfo>>,
}

impl SignalFd {
    pub fn new(signal: c_int) -> Result<SignalFd> {
        if signal != libc::SIGCHLD {
            return Err(Error::CreateSignalFd(ErrnoError::new(libc::EINVAL)));
        }

        let _guard = RELAY_LOCK.lock().expect("signalfd init lock poisoned");

        let mut fds = [-1; 2];
        let ret = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if ret != 0 {
            return Err(Error::CreateSignalFd(ErrnoError::last()));
        }
        for fd in fds {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags >= 0 {
                let _ = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
            }
            let fl = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if fl >= 0 {
                let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK) };
            }
        }

        RELAY_WRITE_FD.store(fds[1], Ordering::SeqCst);

        signal::block_signal(signal).map_err(Error::CreateBlockSignal)?;

        let mut sa: libc::sigaction = unsafe { mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut sa.sa_mask);
        }
        sa.sa_sigaction = relay_signal as libc::sighandler_t;
        sa.sa_flags = libc::SA_RESTART | libc::SA_SIGINFO | libc::SA_NOCLDSTOP;

        let mut old: libc::sigaction = unsafe { mem::zeroed() };
        let ret = unsafe { sigaction(signal, &sa, &mut old) };
        if ret != 0 {
            RELAY_WRITE_FD.store(-1, Ordering::SeqCst);
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            let _ = signal::unblock_signal(signal);
            return Err(Error::CreateSignalFd(ErrnoError::last()));
        }

        Ok(SignalFd {
            read_end: unsafe { File::from_raw_fd(fds[0]) },
            _write_end: unsafe { File::from_raw_fd(fds[1]) },
            signal,
            old_action: old,
            pending: Mutex::new(VecDeque::new()),
        })
    }

    fn reap_available(&self) {
        let mut q = self.pending.lock().expect("pending queue poisoned");
        loop {
            let mut status: c_int = 0;
            let pid = unsafe { waitpid(-1, &mut status, WNOHANG) };
            if pid <= 0 {
                break;
            }
            q.push_back(siginfo_from_wait_status(self.signal, pid, status));
        }
    }

    pub fn read(&self) -> Result<Option<signalfd_siginfo>> {
        {
            let mut q = self.pending.lock().expect("pending queue poisoned");
            if let Some(si) = q.pop_front() {
                return Ok(Some(si));
            }
        }

        let mut byte = [0u8; 1];
        loop {
            let ret = unsafe {
                read(
                    self.read_end.as_raw_fd(),
                    byte.as_mut_ptr() as *mut c_void,
                    1,
                )
            };
            if ret == 1 {
                break;
            }
            if ret == 0 {
                return Ok(None);
            }
            let err = ErrnoError::last();
            match err.errno() {
                EAGAIN => return Ok(None),
                EINTR => continue,
                _ => return Err(Error::SignalFdRead(err)),
            }
        }

        self.reap_available();

        let mut q = self.pending.lock().expect("pending queue poisoned");
        Ok(q.pop_front())
    }
}

impl AsRawFd for SignalFd {
    fn as_raw_fd(&self) -> RawFd {
        self.read_end.as_raw_fd()
    }
}

impl AsRawDescriptor for SignalFd {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.read_end.as_raw_descriptor()
    }
}

impl Drop for SignalFd {
    fn drop(&mut self) {
        let _lock = RELAY_LOCK.lock();
        unsafe {
            sigaction(self.signal, &self.old_action, std::ptr::null_mut());
        }
        RELAY_WRITE_FD.store(-1, Ordering::SeqCst);
        let res = signal::unblock_signal(self.signal);
        if let Err(e) = res {
            error!("signalfd failed to unblock signal {}: {}", self.signal, e);
        }
    }
}
