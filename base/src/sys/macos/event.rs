// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::mem;
use std::time::Duration;

use crate::errno::errno_result;
use crate::errno::Error;
use crate::errno::Result;
use crate::event::EventWaitResult;
use crate::sys::unix::RawDescriptor;
use crate::AsRawDescriptor;
use crate::FromRawDescriptor;
use crate::IntoRawDescriptor;
use crate::SafeDescriptor;

/// A pollable event that can be transferred with SCM_RIGHTS on macOS.
///
/// A kqueue descriptor cannot be transferred over an AF_UNIX socket on macOS.
/// Use one connected-to-self loopback datagram socket so crosvm can transfer
/// virtio queue and MSI events between its control threads.
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlatformEvent {
    socket: SafeDescriptor,
}

impl PlatformEvent {
    pub fn new() -> Result<PlatformEvent> {
        // SAFETY: no pointer arguments; the return value is checked.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if fd < 0 {
            return errno_result();
        }
        // SAFETY: fd is newly created and ownership is transferred.
        let socket = unsafe { SafeDescriptor::from_raw_descriptor(fd) };

        // SAFETY: fd is valid and the return value is checked.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return errno_result();
        }

        // SAFETY: sockaddr_in contains only integer fields.
        let mut address: libc::sockaddr_in = unsafe { mem::zeroed() };
        address.sin_len = mem::size_of::<libc::sockaddr_in>() as u8;
        address.sin_family = libc::AF_INET as libc::sa_family_t;
        address.sin_addr.s_addr = libc::htonl(libc::INADDR_LOOPBACK);

        // SAFETY: address is initialized and has the matching length.
        if unsafe {
            libc::bind(
                fd,
                &address as *const libc::sockaddr_in as *const libc::sockaddr,
                mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        } < 0
        {
            return errno_result();
        }

        let mut address_len = mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        // SAFETY: address and address_len are writable local variables.
        if unsafe {
            libc::getsockname(
                fd,
                &mut address as *mut libc::sockaddr_in as *mut libc::sockaddr,
                &mut address_len,
            )
        } < 0
        {
            return errno_result();
        }

        // A connected-to-self socket keeps Event's one-descriptor contract.
        // SAFETY: getsockname filled address and address_len.
        if unsafe {
            libc::connect(
                fd,
                &address as *const libc::sockaddr_in as *const libc::sockaddr,
                address_len,
            )
        } < 0
        {
            return errno_result();
        }

        Ok(PlatformEvent { socket })
    }

    pub fn signal(&self) -> Result<()> {
        let byte = [1u8];
        loop {
            // SAFETY: byte is readable and the descriptor is valid.
            let result = unsafe {
                libc::send(
                    self.as_raw_descriptor(),
                    byte.as_ptr() as *const libc::c_void,
                    byte.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if result >= 0 {
                return Ok(());
            }
            let error = Error::last();
            match error.errno() {
                libc::EINTR => continue,
                // A full buffer already represents a signaled event.
                libc::EAGAIN => return Ok(()),
                _ => return Err(error),
            }
        }
    }

    fn drain(&self) -> Result<()> {
        let mut bytes = [0u8; 64];
        loop {
            // SAFETY: bytes is writable and the descriptor is valid.
            let result = unsafe {
                libc::recv(
                    self.as_raw_descriptor(),
                    bytes.as_mut_ptr() as *mut libc::c_void,
                    bytes.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if result >= 0 {
                continue;
            }
            let error = Error::last();
            match error.errno() {
                libc::EINTR => continue,
                libc::EAGAIN => return Ok(()),
                _ => return Err(error),
            }
        }
    }

    fn wait_ms(&self, timeout_ms: i32) -> Result<EventWaitResult> {
        let mut poll_fd = libc::pollfd {
            fd: self.as_raw_descriptor(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // SAFETY: poll_fd is a valid one-element pollfd array.
            let result = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
            if result > 0 {
                self.drain()?;
                return Ok(EventWaitResult::Signaled);
            }
            if result == 0 {
                return Ok(EventWaitResult::TimedOut);
            }
            let error = Error::last();
            if error.errno() != libc::EINTR {
                return Err(error);
            }
        }
    }

    pub fn wait(&self) -> Result<()> {
        self.wait_ms(-1).map(|_| ())
    }

    pub fn wait_timeout(&self, timeout: Duration) -> Result<EventWaitResult> {
        let timeout_ms = if timeout.is_zero() {
            0
        } else {
            timeout.as_millis().max(1).min(i32::MAX as u128) as i32
        };
        self.wait_ms(timeout_ms)
    }

    pub fn reset(&self) -> Result<()> {
        self.drain()
    }

    pub fn try_clone(&self) -> Result<PlatformEvent> {
        self.socket
            .try_clone()
            .map(|socket| PlatformEvent { socket })
    }
}

impl AsRawDescriptor for PlatformEvent {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.socket.as_raw_descriptor()
    }
}

impl FromRawDescriptor for PlatformEvent {
    unsafe fn from_raw_descriptor(descriptor: RawDescriptor) -> Self {
        PlatformEvent {
            socket: SafeDescriptor::from_raw_descriptor(descriptor),
        }
    }
}

impl IntoRawDescriptor for PlatformEvent {
    fn into_raw_descriptor(self) -> RawDescriptor {
        self.socket.into_raw_descriptor()
    }
}

impl From<PlatformEvent> for SafeDescriptor {
    fn from(evt: PlatformEvent) -> Self {
        evt.socket
    }
}

impl From<SafeDescriptor> for PlatformEvent {
    fn from(socket: SafeDescriptor) -> Self {
        PlatformEvent { socket }
    }
}
