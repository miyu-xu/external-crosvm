// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod config;
#[cfg(any(target_os = "android", target_os = "linux"))]
pub mod fork;
#[cfg(any(target_os = "android", target_os = "linux", target_os = "macos"))]
mod helpers;

pub use crate::config::JailConfig;
#[cfg(any(target_os = "android", target_os = "linux"))]
pub use crate::fork::fork_process;
#[cfg(any(target_os = "android", target_os = "linux", target_os = "macos"))]
pub use crate::helpers::*;

// TODO(b/268407006): We define Minijail as an empty struct as a stub for minijail::Minijail on
// Windows and macOS because the concept of jailing is baked into a bunch of places where it
// isn't easy to compile it out. In the long term, this should go away.
#[cfg(any(windows, target_os = "macos"))]
pub struct FakeMinijailStub {}

#[cfg(any(windows, target_os = "macos"))]
impl FakeMinijailStub {
    pub fn mount_bind(
        &mut self,
        _src: &std::path::Path,
        _dest: &std::path::Path,
        _writable: bool,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    pub fn mount(
        &mut self,
        _src: &std::path::Path,
        _dest: &std::path::Path,
        _fstype: &str,
        _flags: usize,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    pub fn uidmap(&mut self, _map: &str) -> anyhow::Result<()> {
        Ok(())
    }

    pub fn gidmap(&mut self, _map: &str) -> anyhow::Result<()> {
        Ok(())
    }

    pub fn change_uid(&mut self, _uid: u32) {}

    pub fn change_gid(&mut self, _gid: u32) {}

    pub fn parse_seccomp_bytes(&mut self, _bytes: &[u8]) -> anyhow::Result<()> {
        Ok(())
    }

    pub fn kill(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
