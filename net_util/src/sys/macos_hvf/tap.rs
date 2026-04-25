// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Write;
use std::net;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::FromRawFd;
use std::os::unix::io::RawFd;

use base::volatile_impl;
use base::AsRawDescriptor;
use base::FileReadWriteVolatile;
use base::RawDescriptor;
use base::ReadNotifier;

use super::TapT;
use super::TapTLinux;

use crate::Error;
use crate::MacAddress;
use crate::Result;
use crate::TapTCommon;

fn enotsup() -> Error {
    Error::IoctlError(base::Error::new(libc::ENOTSUP))
}

#[derive(Debug)]
pub struct Tap(File);

volatile_impl!(Tap);

impl Read for Tap {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for Tap {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl AsRawFd for Tap {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl AsRawDescriptor for Tap {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.0.as_raw_descriptor()
    }
}

impl ReadNotifier for Tap {
    fn get_read_notifier(&self) -> &dyn AsRawDescriptor {
        self
    }
}

impl TapTCommon for Tap {
    fn new_with_name(_name: &[u8], _vnet_hdr: bool, _multi_vq: bool) -> Result<Self> {
        Err(enotsup())
    }

    fn new(_vnet_hdr: bool, _multi_vq: bool) -> Result<Self> {
        Err(enotsup())
    }

    fn into_mq_taps(self, _vq_pairs: u16) -> Result<Vec<Self>> {
        Err(enotsup())
    }

    fn ip_addr(&self) -> Result<net::Ipv4Addr> {
        Err(enotsup())
    }

    fn set_ip_addr(&self, _ip_addr: net::Ipv4Addr) -> Result<()> {
        Err(enotsup())
    }

    fn netmask(&self) -> Result<net::Ipv4Addr> {
        Err(enotsup())
    }

    fn set_netmask(&self, _netmask: net::Ipv4Addr) -> Result<()> {
        Err(enotsup())
    }

    fn mtu(&self) -> Result<u16> {
        Err(enotsup())
    }

    fn set_mtu(&self, _mtu: u16) -> Result<()> {
        Err(enotsup())
    }

    fn mac_address(&self) -> Result<MacAddress> {
        Err(enotsup())
    }

    fn set_mac_address(&self, _mac_addr: MacAddress) -> Result<()> {
        Err(enotsup())
    }

    fn set_offload(&self, _flags: libc::c_uint) -> Result<()> {
        Err(enotsup())
    }

    fn enable(&self) -> Result<()> {
        Err(enotsup())
    }

    fn try_clone(&self) -> Result<Self> {
        Err(enotsup())
    }

    unsafe fn from_raw_descriptor(descriptor: RawDescriptor) -> Result<Self> {
        Ok(Tap(File::from_raw_fd(descriptor)))
    }
}

impl TapTLinux for Tap {
    fn set_vnet_hdr_size(&self, _size: usize) -> Result<()> {
        Err(enotsup())
    }

    fn if_flags(&self) -> u32 {
        0
    }
}

impl TapT for Tap {}

pub mod fakes {
    use std::fs::remove_file;
    use std::fs::OpenOptions;
    use std::io::Result as IoResult;
    use std::os::raw::c_uint;
    use std::os::unix::io::AsRawFd;

    use base::volatile_impl;
    use base::AsRawDescriptor;
    use base::FileReadWriteVolatile;
    use base::RawDescriptor;
    use base::ReadNotifier;

    use super::TapT;
    use super::TapTLinux;

    use crate::Error;
    use crate::MacAddress;
    use crate::Result;
    use crate::TapTCommon;

    const TMP_FILE: &str = "/tmp/crosvm_tap_test_file";

    pub struct FakeTap {
        tap_file: std::fs::File,
    }

    impl TapTCommon for FakeTap {
        fn new(_vnet_hdr: bool, _multi_vq: bool) -> Result<Self> {
            Self::new_with_name(b"", false, false)
        }

        fn new_with_name(_: &[u8], _: bool, _: bool) -> Result<FakeTap> {
            Ok(FakeTap {
                tap_file: OpenOptions::new()
                    .read(true)
                    .append(true)
                    .create(true)
                    .open(TMP_FILE)
                    .unwrap(),
            })
        }

        fn into_mq_taps(self, _vq_pairs: u16) -> Result<Vec<FakeTap>> {
            Ok(Vec::new())
        }

        fn ip_addr(&self) -> Result<std::net::Ipv4Addr> {
            Ok(std::net::Ipv4Addr::new(1, 2, 3, 4))
        }

        fn set_ip_addr(&self, _: std::net::Ipv4Addr) -> Result<()> {
            Ok(())
        }

        fn netmask(&self) -> Result<std::net::Ipv4Addr> {
            Ok(std::net::Ipv4Addr::new(255, 255, 255, 252))
        }

        fn set_netmask(&self, _: std::net::Ipv4Addr) -> Result<()> {
            Ok(())
        }

        fn mtu(&self) -> Result<u16> {
            Ok(1500)
        }

        fn set_mtu(&self, _: u16) -> Result<()> {
            Ok(())
        }

        fn mac_address(&self) -> Result<MacAddress> {
            Ok("01:02:03:04:05:06".parse().unwrap())
        }

        fn set_mac_address(&self, _: MacAddress) -> Result<()> {
            Ok(())
        }

        fn set_offload(&self, _: c_uint) -> Result<()> {
            Ok(())
        }

        fn enable(&self) -> Result<()> {
            Ok(())
        }

        fn try_clone(&self) -> Result<Self> {
            Ok(FakeTap {
                tap_file: self.tap_file.try_clone().unwrap(),
            })
        }

        unsafe fn from_raw_descriptor(_descriptor: RawDescriptor) -> Result<Self> {
            unimplemented!()
        }
    }

    impl TapTLinux for FakeTap {
        fn set_vnet_hdr_size(&self, _: usize) -> Result<()> {
            Ok(())
        }

        fn if_flags(&self) -> u32 {
            2 // IFF_TAP (see Linux `if_tun.h`)
        }
    }

    impl Drop for FakeTap {
        fn drop(&mut self) {
            let _ = remove_file(TMP_FILE);
        }
    }

    impl std::io::Read for FakeTap {
        fn read(&mut self, _: &mut [u8]) -> IoResult<usize> {
            Ok(0)
        }
    }

    impl std::io::Write for FakeTap {
        fn write(&mut self, _: &[u8]) -> IoResult<usize> {
            Ok(0)
        }

        fn flush(&mut self) -> IoResult<()> {
            Ok(())
        }
    }

    impl AsRawFd for FakeTap {
        fn as_raw_fd(&self) -> libc::c_int {
            self.tap_file.as_raw_descriptor()
        }
    }

    impl AsRawDescriptor for FakeTap {
        fn as_raw_descriptor(&self) -> RawDescriptor {
            self.tap_file.as_raw_descriptor()
        }
    }

    impl ReadNotifier for FakeTap {
        fn get_read_notifier(&self) -> &dyn AsRawDescriptor {
            self
        }
    }

    impl TapT for FakeTap {}
    volatile_impl!(FakeTap);
}
