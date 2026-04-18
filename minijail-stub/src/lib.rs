// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Stub [`Minijail`] for hosts without libminijail (macOS). All jail operations
//! are inert or fail at runtime; crosvm on macOS is expected to run without
//! `--sandbox` / device jails.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_ulong, c_ushort};
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::Path;

pub type rlim_t = u64;

#[derive(Debug, Clone)]
pub enum Error {
    Unsupported,
    StrToCString(String),
    OpenDevNull(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported => write!(f, "minijail is not available on this host"),
            Error::StrToCString(s) => write!(f, "CString: {s}"),
            Error::OpenDevNull(e) => write!(f, "open /dev/null: {e}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Minijail;

impl Minijail {
    pub fn new() -> Result<Self> {
        Ok(Minijail)
    }

    pub fn try_clone(&self) -> Result<Self> {
        Ok(Minijail)
    }

    pub fn change_uid(&mut self, _uid: libc::uid_t) {}

    pub fn change_gid(&mut self, _gid: libc::gid_t) {}

    pub fn change_user(&mut self, _user: &str) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn change_group(&mut self, _group: &str) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn set_supplementary_gids(&mut self, _ids: &[libc::gid_t]) {}

    pub fn keep_supplementary_gids(&mut self) {}

    pub fn set_rlimit(&mut self, _kind: c_int, _cur: rlim_t, _max: rlim_t) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn use_seccomp(&mut self) {}

    pub fn no_new_privs(&mut self) {}

    pub fn use_seccomp_filter(&mut self) {}

    pub fn set_seccomp_filter_tsync(&mut self) {}

    pub fn parse_seccomp_program<P: AsRef<Path>>(&mut self, _path: P) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn parse_seccomp_bytes(&mut self, _buffer: &[u8]) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn parse_seccomp_filters<P: AsRef<Path>>(&mut self, _path: P) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn log_seccomp_filter_failures(&mut self) {}

    pub fn use_caps(&mut self, _capmask: u64) {}

    pub fn capbset_drop(&mut self, _capmask: u64) {}

    pub fn set_ambient_caps(&mut self) {}

    pub fn reset_signal_mask(&mut self) {}

    pub fn run_as_init(&mut self) {}

    pub fn namespace_pids(&mut self) {}

    pub fn namespace_user(&mut self) {}

    pub fn namespace_user_disable_setgroups(&mut self) {}

    pub fn namespace_vfs(&mut self) {}

    pub fn new_session_keyring(&mut self) {}

    pub fn skip_remount_private(&mut self) {}

    pub fn namespace_ipc(&mut self) {}

    pub fn namespace_net(&mut self) {}

    pub fn namespace_cgroups(&mut self) {}

    pub fn remount_proc_readonly(&mut self) {}

    pub fn set_remount_mode(&mut self, _mode: c_ulong) {}

    pub fn uidmap(&mut self, uid_map: &str) -> Result<()> {
        let _ = CString::new(uid_map).map_err(|_| Error::StrToCString(uid_map.to_owned()))?;
        Err(Error::Unsupported)
    }

    pub fn gidmap(&mut self, gid_map: &str) -> Result<()> {
        let _ = CString::new(gid_map).map_err(|_| Error::StrToCString(gid_map.to_owned()))?;
        Err(Error::Unsupported)
    }

    pub fn inherit_usergroups(&mut self) {}

    pub fn use_alt_syscall(&mut self, _table_name: &str) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn enter_chroot<P: AsRef<Path>>(&mut self, _dir: P) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn enter_pivot_root<P: AsRef<Path>>(&mut self, _dir: P) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn mount<P1: AsRef<Path>, P2: AsRef<Path>>(
        &mut self,
        _src: P1,
        _dest: P2,
        _fstype: &str,
        _flags: usize,
    ) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn mount_with_data<P1: AsRef<Path>, P2: AsRef<Path>>(
        &mut self,
        _src: P1,
        _dest: P2,
        _fstype: &str,
        _flags: usize,
        _data: &str,
    ) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn mount_dev(&mut self) {}

    pub fn mount_tmp(&mut self) {}

    pub fn mount_tmp_size(&mut self, _size: usize) {}

    pub fn mount_bind<P1: AsRef<Path>, P2: AsRef<Path>>(
        &mut self,
        _src: P1,
        _dest: P2,
        _writable: bool,
    ) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn run<P: AsRef<Path>, S: AsRef<str>>(
        &self,
        _cmd: P,
        _inheritable_fds: &[RawFd],
        _args: &[S],
    ) -> Result<libc::pid_t> {
        Err(Error::Unsupported)
    }

    pub fn run_remap<P: AsRef<Path>, S: AsRef<str>>(
        &self,
        _cmd: P,
        _inheritable_fds: &[(RawFd, RawFd)],
        _args: &[S],
    ) -> Result<libc::pid_t> {
        Err(Error::Unsupported)
    }

    pub fn run_fd<F: AsRawFd, S: AsRef<str>>(
        &self,
        _cmd: &F,
        _inheritable_fds: &[RawFd],
        _args: &[S],
    ) -> Result<libc::pid_t> {
        Err(Error::Unsupported)
    }

    pub fn run_fd_remap<F: AsRawFd, S: AsRef<str>>(
        &self,
        _cmd: &F,
        _inheritable_fds: &[(RawFd, RawFd)],
        _args: &[S],
    ) -> Result<libc::pid_t> {
        Err(Error::Unsupported)
    }

    pub fn run_command(&self, _cmd: Command) -> Result<libc::pid_t> {
        Err(Error::Unsupported)
    }

    pub unsafe fn fork(&self, _inheritable_fds: Option<&[RawFd]>) -> Result<libc::pid_t> {
        Err(Error::Unsupported)
    }

    pub unsafe fn fork_remap(&self, _inheritable_fds: &[(RawFd, RawFd)]) -> Result<libc::pid_t> {
        Err(Error::Unsupported)
    }

    pub fn wait(&self) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn kill(&self) -> Result<()> {
        Ok(())
    }
}

/// Minimal [`Command`] placeholder; virtio-gpu jails are disabled on macOS.
pub struct Command;

impl Command {
    pub fn new_for_path<P: AsRef<Path>, S: AsRef<str>, A: AsRef<str>>(
        _path: P,
        _keep_fds: &[RawFd],
        _args: &[S],
        _env_vars: Option<&[A]>,
    ) -> Result<Command> {
        Err(Error::Unsupported)
    }
}

#[repr(C)]
pub struct sock_filter {
    pub code: c_ushort,
    pub jt: libc::c_uchar,
    pub jf: libc::c_uchar,
    pub k: u32,
}
