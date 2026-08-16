// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Real macOS vmnet.framework TAP implementation using the C shim.

use std::ffi::CStr;
use std::ffi::CString;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::net;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixStream;

use base::pipe;
use base::warn;
use base::AsRawDescriptor;
use base::FileReadWriteVolatile;
use base::RawDescriptor;
use base::ReadNotifier;
use base::Result as BaseResult;
use base::VolatileSlice;

use super::TapT;
use super::TapTLinux;
use crate::Error;
use crate::MacAddress;
use crate::Result;
use crate::TapTCommon;

// --- FFI to vmnet_shim C functions ---

#[repr(C)]
struct vmnet_shim_interface {
    _private: [u8; 0],
}

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
const VMNET_SUCCESS: std::os::raw::c_int = 1000;
const VIRTIO_NET_HDR_LEN: usize = 12;

/// macOS vmnet.framework TAP interface.
pub struct VmnetTap {
    iface: *mut vmnet_shim_interface,
    notify_read: File,
    _notify_write: File,
    offline_mac: Option<MacAddress>,
    socket_vmnet: Option<UnixStream>,
    socket_rx_header: [u8; 4],
    socket_rx_header_read: usize,
    socket_rx_frame: Vec<u8>,
    socket_rx_frame_read: usize,
    io_buffer: Vec<u8>,
}

impl fmt::Debug for VmnetTap {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("VmnetTap")
            .field("iface", &self.iface)
            .field(
                "offline",
                &(self.iface.is_null() && self.socket_vmnet.is_none()),
            )
            .field("socket_vmnet", &self.socket_vmnet.is_some())
            .finish()
    }
}

impl VmnetTap {
    fn set_nonblocking(file: &File) -> Result<()> {
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
        {
            return Err(Error::CreateTap(base::Error::new(libc::EIO)));
        }
        Ok(())
    }

    /// Create a new vmnet interface in shared (NAT) mode.
    ///
    /// vmnet.framework does not expose a true TAP device; instead it provides
    /// a user-space network interface via XPC.  `new_with_name` is the common
    /// entry point per `TapTCommon`; the `name` parameter is ignored on macOS
    /// (vmnet assigns the interface automatically).
    fn create(mode: u32, mac_addr: Option<&MacAddress>, mtu: u64) -> Result<Self> {
        let (notify_read, notify_write) = pipe().map_err(Error::CreateTap)?;
        Self::set_nonblocking(&notify_read)?;
        Self::set_nonblocking(&notify_write)?;

        let mac_cstr = mac_addr.map(|m| {
            let s = format!(
                "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                m.octets()[0],
                m.octets()[1],
                m.octets()[2],
                m.octets()[3],
                m.octets()[4],
                m.octets()[5]
            );
            CString::new(s).unwrap()
        });
        let mac_ptr = mac_cstr
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null());

        let mut error_code = 0i32;
        // SAFETY: FFI call to C shim; passes valid parameters.
        let iface = unsafe {
            vmnet_shim_start(
                mode,
                mac_ptr,
                mtu,
                notify_write.as_raw_fd(),
                &mut error_code,
            )
        };

        if iface.is_null() {
            warn!("vmnet_start_interface failed with status {}", error_code);
            return Err(Error::CreateTap(base::Error::new(libc::EIO)));
        }

        Ok(VmnetTap {
            iface,
            notify_read,
            _notify_write: notify_write,
            offline_mac: None,
            socket_vmnet: None,
            socket_rx_header: [0; 4],
            socket_rx_header_read: 0,
            socket_rx_frame: Vec::new(),
            socket_rx_frame_read: 0,
            io_buffer: Vec::new(),
        })
    }

    /// Create a disconnected Ethernet device for HD's explicit offline network profile.
    ///
    /// The retained write end keeps the nonblocking read descriptor idle, while guest
    /// transmissions are accepted and discarded. This exposes a stable virtio-net device
    /// without claiming host uplink or requiring Apple's restricted vmnet entitlement.
    fn create_offline(mac_addr: Option<&MacAddress>) -> Result<Self> {
        let (notify_read, notify_write) = pipe().map_err(Error::CreateTap)?;
        Self::set_nonblocking(&notify_read)?;
        Self::set_nonblocking(&notify_write)?;
        Ok(Self {
            iface: std::ptr::null_mut(),
            notify_read,
            _notify_write: notify_write,
            offline_mac: mac_addr.copied(),
            socket_vmnet: None,
            socket_rx_header: [0; 4],
            socket_rx_header_read: 0,
            socket_rx_frame: Vec::new(),
            socket_rx_frame_read: 0,
            io_buffer: Vec::new(),
        })
    }

    /// Connect to a root-owned socket_vmnet daemon.
    ///
    /// socket_vmnet uses QEMU's stream framing: a four-byte big-endian frame
    /// length followed by one raw Ethernet frame. The privileged daemon owns
    /// vmnet.framework while crosvm remains an unprivileged process.
    fn create_socket_vmnet(path: &str, mac_addr: Option<&MacAddress>) -> Result<Self> {
        let stream = UnixStream::connect(path).map_err(|error| {
            Error::CreateTap(base::Error::new(error.raw_os_error().unwrap_or(libc::EIO)))
        })?;
        stream.set_nonblocking(true).map_err(|error| {
            Error::CreateTap(base::Error::new(error.raw_os_error().unwrap_or(libc::EIO)))
        })?;
        let (notify_read, notify_write) = pipe().map_err(Error::CreateTap)?;
        Ok(Self {
            iface: std::ptr::null_mut(),
            notify_read,
            _notify_write: notify_write,
            offline_mac: mac_addr.copied(),
            socket_vmnet: Some(stream),
            socket_rx_header: [0; 4],
            socket_rx_header_read: 0,
            socket_rx_frame: Vec::new(),
            socket_rx_frame_read: 0,
            io_buffer: Vec::new(),
        })
    }

    /// Create a shared vmnet interface with the guest-visible MAC selected up front.
    ///
    /// vmnet does not allow changing the MAC after the interface has started, so
    /// the generic `set_mac_address` flow cannot be used on macOS.
    pub fn new_with_name_and_mac(
        name: &[u8],
        mac_addr: Option<&MacAddress>,
        multi_vq: bool,
    ) -> Result<Self> {
        if multi_vq {
            return Err(Error::IoctlError(base::Error::new(libc::ENOTSUP)));
        }
        if name.starts_with(b"hd-offline-") {
            return Self::create_offline(mac_addr);
        }
        if let Some(path) = name.strip_prefix(b"hd-socket-vmnet:") {
            let path = std::str::from_utf8(path)
                .map_err(|_| Error::CreateTap(base::Error::new(libc::EINVAL)))?;
            return Self::create_socket_vmnet(path, mac_addr);
        }
        Self::create(VMNET_SHARED_MODE, mac_addr, 0)
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
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(stream) = &mut self.socket_vmnet {
            if buf.len() < VIRTIO_NET_HDR_LEN {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "virtio-net receive buffer is too small",
                ));
            }

            while self.socket_rx_header_read < self.socket_rx_header.len() {
                match stream.read(&mut self.socket_rx_header[self.socket_rx_header_read..]) {
                    Ok(0) => {
                        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
                    }
                    Ok(count) => self.socket_rx_header_read += count,
                    Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
                    }
                    Err(ref error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }

            let frame_len = u32::from_be_bytes(self.socket_rx_header) as usize;
            if frame_len == 0 || frame_len > 65_536 {
                self.socket_rx_header_read = 0;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid socket_vmnet frame length {frame_len}"),
                ));
            }
            self.socket_rx_frame.resize(frame_len, 0);
            while self.socket_rx_frame_read < frame_len {
                match stream.read(&mut self.socket_rx_frame[self.socket_rx_frame_read..]) {
                    Ok(0) => {
                        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
                    }
                    Ok(count) => self.socket_rx_frame_read += count,
                    Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
                    }
                    Err(ref error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }

            self.socket_rx_header_read = 0;
            self.socket_rx_frame_read = 0;
            if frame_len + VIRTIO_NET_HDR_LEN > buf.len() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("socket_vmnet frame length {frame_len} exceeds receive buffer"),
                ));
            }
            buf[..VIRTIO_NET_HDR_LEN].fill(0);
            buf[VIRTIO_NET_HDR_LEN..VIRTIO_NET_HDR_LEN + frame_len]
                .copy_from_slice(&self.socket_rx_frame);
            return Ok(VIRTIO_NET_HDR_LEN + frame_len);
        }
        if self.iface.is_null() {
            return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
        }
        if buf.len() < VIRTIO_NET_HDR_LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "virtio-net receive buffer is too small",
            ));
        }

        // Clear all edge notifications before draining vmnet. The pipe is
        // nonblocking, so subsequent reads can continue until vmnet is empty.
        let mut notifications = [0u8; 64];
        loop {
            match self.notify_read.read(&mut notifications) {
                Ok(0) => break,
                Ok(_) => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }

        let mut bytes_read = 0usize;
        // SAFETY: FFI call to C shim; buf is writable and valid.
        let ret = unsafe {
            vmnet_shim_read(
                self.iface,
                buf[VIRTIO_NET_HDR_LEN..].as_mut_ptr() as *mut std::ffi::c_void,
                buf.len() - VIRTIO_NET_HDR_LEN,
                &mut bytes_read,
            )
        };
        if ret == VMNET_SUCCESS && bytes_read > 0 {
            buf[..VIRTIO_NET_HDR_LEN].fill(0);
            Ok(bytes_read + VIRTIO_NET_HDR_LEN)
        } else if ret == VMNET_SUCCESS {
            Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
        } else {
            Err(std::io::Error::other(format!("vmnet read failed: {ret}")))
        }
    }
}

impl Write for VmnetTap {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() < VIRTIO_NET_HDR_LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "virtio-net transmit frame is missing its header",
            ));
        }
        if let Some(stream) = &mut self.socket_vmnet {
            let frame = &buf[VIRTIO_NET_HDR_LEN..];
            let frame_len = u32::try_from(frame.len())
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
            stream.write_all(&frame_len.to_be_bytes())?;
            stream.write_all(frame)?;
            return Ok(buf.len());
        }
        if self.iface.is_null() {
            return Ok(buf.len());
        }
        let frame = &buf[VIRTIO_NET_HDR_LEN..];
        // SAFETY: FFI call to C shim; frame is readable and valid.
        let ret = unsafe {
            vmnet_shim_write(
                self.iface,
                frame.as_ptr() as *const std::ffi::c_void,
                frame.len(),
            )
        };
        if ret == VMNET_SUCCESS {
            Ok(buf.len())
        } else {
            Err(std::io::Error::other(format!("vmnet write failed: {ret}")))
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl AsRawFd for VmnetTap {
    fn as_raw_fd(&self) -> RawFd {
        self.socket_vmnet
            .as_ref()
            .map_or_else(|| self.notify_read.as_raw_fd(), AsRawFd::as_raw_fd)
    }
}

impl AsRawDescriptor for VmnetTap {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.as_raw_fd()
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

    fn into_mq_taps(self, vq_pairs: u16) -> Result<Vec<Self>> {
        if vq_pairs == 1 {
            Ok(vec![self])
        } else {
            Err(Error::IoctlError(base::Error::new(libc::ENOTSUP)))
        }
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
        if self.iface.is_null() {
            return Ok(1500);
        }
        // SAFETY: FFI call; iface is valid.
        let max_pkt = unsafe { vmnet_shim_max_packet_size(self.iface) };
        if max_pkt > 0 {
            Ok((max_pkt.saturating_sub(14).min(u16::MAX as usize)) as u16)
        } else {
            Ok(1500)
        }
    }

    fn set_mtu(&self, _mtu: u16) -> Result<()> {
        // MTU is set at interface creation time; cannot change after start.
        Err(Error::IoctlError(base::Error::new(libc::ENOTSUP)))
    }

    fn mac_address(&self) -> Result<MacAddress> {
        if let Some(mac) = self.offline_mac {
            return Ok(mac);
        }
        // SAFETY: FFI call; returns a pointer to a C string "xx:xx:xx:xx:xx:xx".
        let ptr = unsafe { vmnet_shim_mac_address(self.iface) };
        if ptr.is_null() {
            return Err(Error::IoctlError(base::Error::new(libc::ENODEV)));
        }
        // SAFETY: ptr is a valid NUL-terminated string from C.
        let cstr = unsafe { CStr::from_ptr(ptr) };
        let s = cstr
            .to_str()
            .map_err(|_| Error::IoctlError(base::Error::new(libc::EINVAL)))?;
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

impl FileReadWriteVolatile for VmnetTap {
    fn read_volatile(&mut self, slice: VolatileSlice) -> std::io::Result<usize> {
        // SAFETY: `VolatileSlice` guarantees that its pointer is valid for `size` writable bytes
        // for the duration of this call. `Read::read` does not retain the borrowed slice.
        let buffer =
            unsafe { std::slice::from_raw_parts_mut(slice.as_mut_ptr(), slice.size() as usize) };
        self.read(buffer)
    }

    fn read_vectored_volatile(&mut self, bufs: &[VolatileSlice]) -> std::io::Result<usize> {
        let buffer_size = bufs.iter().map(VolatileSlice::size).sum();
        if buffer_size == 0 {
            return Ok(0);
        }

        let mut io_buffer = std::mem::take(&mut self.io_buffer);
        io_buffer.resize(buffer_size, 0);
        let result = self.read(&mut io_buffer);
        if let Ok(bytes_read) = &result {
            let mut copied = 0;
            for slice in bufs {
                let count = slice.size().min(*bytes_read - copied);
                if count == 0 {
                    break;
                }
                // SAFETY: Both buffers are valid for `count` bytes and cannot overlap because the
                // source is the tap-owned aggregation buffer while the destination is guest memory.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        io_buffer.as_ptr().add(copied),
                        slice.as_mut_ptr(),
                        count,
                    );
                }
                copied += count;
            }
        }
        self.io_buffer = io_buffer;
        result
    }

    fn write_volatile(&mut self, slice: VolatileSlice) -> std::io::Result<usize> {
        // SAFETY: `VolatileSlice` guarantees that its pointer is valid for `size` readable bytes
        // for the duration of this call. `Write::write` does not retain the borrowed slice.
        let buffer = unsafe { std::slice::from_raw_parts(slice.as_ptr(), slice.size() as usize) };
        self.write(buffer)
    }

    fn write_vectored_volatile(&mut self, bufs: &[VolatileSlice]) -> std::io::Result<usize> {
        let buffer_size = bufs.iter().map(VolatileSlice::size).sum();
        if buffer_size == 0 {
            return Ok(0);
        }

        let mut io_buffer = std::mem::take(&mut self.io_buffer);
        io_buffer.clear();
        io_buffer.reserve(buffer_size);
        for slice in bufs {
            // SAFETY: `VolatileSlice` guarantees that its pointer is valid for `size` readable
            // bytes for the duration of this call. `extend_from_slice` copies those bytes.
            let buffer =
                unsafe { std::slice::from_raw_parts(slice.as_ptr(), slice.size() as usize) };
            io_buffer.extend_from_slice(buffer);
        }
        let result = self.write(&io_buffer);
        self.io_buffer = io_buffer;
        result
    }
}

impl TapT for VmnetTap {}

// SAFETY: VmnetTap's inner pointer is only accessed from a single thread
// (vmnet.framework XPC connection is not shared across threads).
unsafe impl Send for VmnetTap {}
