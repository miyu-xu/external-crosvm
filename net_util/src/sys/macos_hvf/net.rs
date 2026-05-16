// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Real macOS vmnet.framework TAP implementation using the C shim.

use std::ffi::CStr;
use std::ffi::CString;
use std::fmt;
use std::fs::File;
use std::io;
use std::io::Read;
use std::io::Write;
use std::net;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::FromRawFd;
use std::os::unix::io::RawFd;

use base::pipe;
use base::volatile_impl;
use base::warn;
use base::AsRawDescriptor;
use base::FileReadWriteVolatile;
use base::RawDescriptor;
use base::ReadNotifier;
use base::Result as BaseResult;

use super::TapT;
use super::TapTLinux;
use crate::Error;
use crate::MacAddress;
use crate::Result;
use crate::TapTCommon;

// --- FFI to vmnet_shim C functions ---

#[repr(C)]
struct vmnet_shim_interface; // Opaque handle from C side.

extern "C" {
    fn vmnet_shim_start(
        mode: u32,
        mac_addr: *const std::ffi::c_char,
        mtu: u64,
        notify_fd: std::os::raw::c_int,
        error_code: *mut std::os::raw::c_int,
    ) -> *mut vmnet_shim_interface;

    fn vmnet_shim_read(
        iface: *mut vmnet_shim_interface,
        buf: *mut std::ffi::c_void,
        buf_size: usize,
        bytes_read: *mut usize,
    ) -> std::os::raw::c_int;

    fn vmnet_shim_write(
        iface: *mut vmnet_shim_interface,
        buf: *const std::ffi::c_void,
        size: usize,
    ) -> std::os::raw::c_int;

    fn vmnet_shim_max_packet_size(iface: *mut vmnet_shim_interface) -> usize;

    fn vmnet_shim_mac_address(iface: *mut vmnet_shim_interface) -> *const std::ffi::c_char;

    fn vmnet_shim_stop(iface: *mut vmnet_shim_interface);
}

const VMNET_SHARED_MODE: u32 = 1001;
const VMNET_HOST_MODE: u32 = 1000;
const VMNET_SUCCESS: std::os::raw::c_int = 1000;

/// macOS vmnet.framework TAP interface.
pub struct VmnetTap {
    iface: *mut vmnet_shim_interface,
    notify_fd: File,
}

impl fmt::Debug for VmnetTap {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("VmnetTap")
            .field("iface", &self.iface)
            .finish()
    }
}

impl VmnetTap {
    /// Create a new vmnet interface in shared (NAT) mode.
    ///
    /// vmnet.framework does not expose a true TAP device; instead it provides
    /// a user-space network interface via XPC.  `new_with_name` is the common
    /// entry point per `TapTCommon`; the `name` parameter is ignored on macOS
    /// (vmnet assigns the interface automatically).
    fn create(mode: u32, mac_addr: Option<&MacAddress>, mtu: u64) -> Result<Self> {
        let (notify_fd, _) = pipe().map_err(|e| Error::CreateTap(e))?;

        let mac_cstr = mac_addr.map(|m| {
            let s = format!("{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                m.octets()[0], m.octets()[1], m.octets()[2],
                m.octets()[3], m.octets()[4], m.octets()[5]);
            CString::new(s).unwrap()
        });
        let mac_ptr = mac_cstr.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());

        let mut error_code = 0i32;
        // SAFETY: FFI call to C shim; passes valid parameters.
        let iface = unsafe {
            vmnet_shim_start(
                mode,
                mac_ptr,
                mtu,
                notify_fd.as_raw_fd(),
                &mut error_code,
            )
        };

        if iface.is_null() {
            return Err(Error::CreateTap(base::Error::new(libc::EIO)));
        }

        Ok(VmnetTap { iface, notify_fd })
    }
}

impl Drop for VmnetTap {
    fn drop(&mut self) {
        if !self.iface.is_null() {
            // SAFETY: FFI call to C shim; self.iface is valid here.
            unsafe { vmnet_shim_stop(self.iface) };
            self.iface = std::ptr::null_mut();
        }
    }
}

// --- Read / Write ---

impl Read for VmnetTap {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut bytes_read = 0usize;
        // SAFETY: FFI call to C shim; buf is writable and valid.
        let ret = unsafe {
            vmnet_shim_read(
                self.iface,
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                buf.len(),
                &mut bytes_read,
            )
        };
        if ret == VMNET_SUCCESS {
            Ok(bytes_read)
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "vmnet read failed"))
        }
    }
}

impl Write for VmnetTap {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // SAFETY: FFI call to C shim; buf is readable and valid.
        let ret = unsafe {
            vmnet_shim_write(self.iface, buf.as_ptr() as *const std::ffi::c_void, buf.len())
        };
        if ret == VMNET_SUCCESS {
            Ok(buf.len())
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "vmnet write failed"))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AsRawFd for VmnetTap {
    fn as_raw_fd(&self) -> RawFd {
        self.notify_fd.as_raw_fd()
    }
}

impl AsRawDescriptor for VmnetTap {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.notify_fd.as_raw_descriptor()
    }
}

impl ReadNotifier for VmnetTap {
    fn get_read_notifier(&self) -> &dyn AsRawDescriptor {
        self
    }
}

impl TapTCommon for VmnetTap {
    fn new_with_name(_name: &[u8], _vnet_hdr: bool, _multi_vq: bool) -> Result<Self> {
        Self::create(VMNET_SHARED_MODE, None, 0)
    }

    fn new(_vnet_hdr: bool, _multi_vq: bool) -> Result<Self> {
        Self::create(VMNET_SHARED_MODE, None, 0)
    }

    fn into_mq_taps(self, _vq_pairs: u16) -> Result<Vec<Self>> {
        warn!("vmnet: into_mq_taps not supported, returning single tap");
        Ok(vec![self])
    }

    fn ip_addr(&self) -> Result<net::Ipv4Addr> {
        // vmnet.framework assigns IP via DHCP internally; we cannot query it
        // directly from the shared interface handle.
        Err(Error::IoctlError(base::Error::new(libc::ENOTSUP)))
    }

    fn set_ip_addr(&self, _ip_addr: net::Ipv4Addr) -> Result<()> {
        Err(Error::IoctlError(base::Error::new(libc::ENOTSUP)))
    }

    fn netmask(&self) -> Result<net::Ipv4Addr> {
        Err(Error::IoctlError(base::Error::new(libc::ENOTSUP)))
    }

    fn set_netmask(&self, _netmask: net::Ipv4Addr) -> Result<()> {
        Err(Error::IoctlError(base::Error::new(libc::ENOTSUP)))
    }

    fn mtu(&self) -> Result<u16> {
        // SAFETY: FFI call; iface is valid.
        let max_pkt = unsafe { vmnet_shim_max_packet_size(self.iface) };
        if max_pkt > 0 {
            // Standard Ethernet MTU accounting: max packet size minus L2 overhead (~22 bytes).
            Ok((max_pkt.saturating_sub(22).min(u16::MAX as usize)) as u16)
        } else {
            Ok(1500)
        }
    }

    fn set_mtu(&self, _mtu: u16) -> Result<()> {
        // MTU is set at interface creation time; cannot change after start.
        Err(Error::IoctlError(base::Error::new(libc::ENOTSUP)))
    }

    fn mac_address(&self) -> Result<MacAddress> {
        // SAFETY: FFI call; returns a pointer to a C string "xx:xx:xx:xx:xx:xx".
        let ptr = unsafe { vmnet_shim_mac_address(self.iface) };
        if ptr.is_null() {
            return Err(Error::IoctlError(base::Error::new(libc::ENODEV)));
        }
        // SAFETY: ptr is a valid NUL-terminated string from C.
        let cstr = unsafe { CStr::from_ptr(ptr) };
        let s = cstr.to_str().map_err(|_| Error::IoctlError(base::Error::new(libc::EINVAL)))?;
        s.parse::<MacAddress>()
            .map_err(|_| Error::IoctlError(base::Error::new(libc::EINVAL)))
    }

    fn set_mac_address(&self, _mac_addr: MacAddress) -> Result<()> {
        // MAC can only be set at interface creation on vmnet.
        Err(Error::IoctlError(base::Error::new(libc::ENOTSUP)))
    }

    fn set_offload(&self, _flags: libc::c_uint) -> Result<()> {
        // vmnet handles offloading internally.
        Ok(())
    }

    fn enable(&self) -> Result<()> {
        // vmnet interface is active once created; nothing extra needed.
        Ok(())
    }

    fn try_clone(&self) -> Result<Self> {
        Err(Error::IoctlError(base::Error::new(libc::ENOTSUP)))
    }

    unsafe fn from_raw_descriptor(_descriptor: RawDescriptor) -> Result<Self> {
        Err(Error::IoctlError(base::Error::new(libc::ENOTSUP)))
    }
}

impl TapTLinux for VmnetTap {
    fn set_vnet_hdr_size(&self, _size: usize) -> Result<()> {
        // vmnet does not use a virtio-net header.
        Ok(())
    }

    fn if_flags(&self) -> u32 {
        2 // IFF_TAP mimic
    }
}

volatile_impl!(VmnetTap);

impl TapT for VmnetTap {}

// SAFETY: VmnetTap's inner pointer is only accessed from a single thread
// (vmnet.framework XPC connection is not shared across threads).
unsafe impl Send for VmnetTap {}
