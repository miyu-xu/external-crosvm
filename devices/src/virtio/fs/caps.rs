// Copyright 2021 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io;

pub type cap_value_t = u32;

#[repr(u32)]
pub enum Capability {
    Chown = 0,
    DacOverride = 1,
    DacReadSearch = 2,
    Fowner = 3,
    Fsetid = 4,
    Kill = 5,
    Setgid = 6,
    Setuid = 7,
    Setpcap = 8,
    LinuxImmutable = 9,
    NetBindService = 10,
    NetBroadcast = 11,
    NetAdmin = 12,
    NetRaw = 13,
    IpcLock = 14,
    IpcOwner = 15,
    SysModule = 16,
    SysRawio = 17,
    SysChroot = 18,
    SysPtrace = 19,
    SysPacct = 20,
    SysAdmin = 21,
    SysBoot = 22,
    SysNice = 23,
    SysResource = 24,
    SysTime = 25,
    SysTtyConfig = 26,
    Mknod = 27,
    Lease = 28,
    AuditWrite = 29,
    AuditControl = 30,
    Setfcap = 31,
    MacOverride = 32,
    MacAdmin = 33,
    Syslog = 34,
    WakeAlarm = 35,
    BlockSuspend = 36,
    AuditRead = 37,
    Last,
}

impl From<Capability> for cap_value_t {
    fn from(c: Capability) -> cap_value_t {
        c as cap_value_t
    }
}

#[repr(u32)]
pub enum Set {
    Effective = 0,
    Permitted = 1,
    Inheritable = 2,
}

#[repr(i32)]
pub enum Value {
    Clear = 0,
    Set = 1,
}

pub struct Caps;

impl Caps {
    pub fn for_current_thread() -> io::Result<Caps> {
        Ok(Caps)
    }

    pub fn update(&mut self, _caps: &[Capability], _set: Set, _value: Value) -> io::Result<()> {
        Ok(())
    }

    pub fn apply(&self) -> io::Result<()> {
        Ok(())
    }
}
