// The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Manages system resources that can be allocated to VMs and their devices.

#[cfg(feature = "wl-dmabuf")]
extern crate gpu_buffer;
extern crate libc;
extern crate msg_socket;
extern crate sys_util;

use msg_socket::MsgOnSocket;
use std::fmt::Display;

pub use crate::address_allocator::AddressAllocator;
pub use crate::gpu_allocator::{
    GpuAllocatorError, GpuMemoryAllocator, GpuMemoryDesc, GpuMemoryPlaneDesc,
};
pub use crate::system_allocator::{MmioType, SystemAllocator};

mod address_allocator;
mod gpu_allocator;
mod system_allocator;

/// Used to tag SystemAllocator allocations.
#[derive(Debug, Eq, PartialEq, Hash, MsgOnSocket, Copy, Clone)]
pub enum Alloc {
    /// An anonymous resource allocation.
    /// Should only be instantiated through `SystemAllocator::get_anon_alloc()`.
    /// Avoid using these. Instead, use / create a more descriptive Alloc variant.
    Anon(usize),
    /// A PCI BAR region with associated bus, device, function and bar numbers.
    PciBar { bus: u8, dev: u8, func: u8, bar: u8 },
    /// GPU render node region.
    GpuRenderNode,
    /// Pmem device region with associated device index.
    PmemDevice(usize),
    /// pstore region.
    Pstore,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Error {
    AllocSizeZero,
    BadAlignment,
    CreateGpuAllocator(GpuAllocatorError),
    ExistingAlloc(Alloc),
    InvalidAlloc(Alloc),
    MissingHighMMIOAddresses,
    MissingLowMMIOAddresses,
    NoIoAllocator,
    OutOfSpace,
    OutOfBounds,
    PoolOverflow { base: u64, size: u64 },
    PoolSizeZero,
    InvalidAddress,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        use self::Error::*;
        match self {
            AllocSizeZero => write!(f, "Allocation cannot have size of 0"),
            BadAlignment => write!(f, "Pool alignment must be a power of 2"),
            CreateGpuAllocator(e) => write!(f, "Failed to create GPU allocator: {:?}", e),
            ExistingAlloc(tag) => write!(f, "Alloc already exists: {:?}", tag),
            InvalidAlloc(tag) => write!(f, "Invalid Alloc: {:?}", tag),
            MissingHighMMIOAddresses => write!(f, "High MMIO address range not specified"),
            MissingLowMMIOAddresses => write!(f, "Low MMIO address range not specified"),
            NoIoAllocator => write!(f, "No IO address range specified"),
            OutOfSpace => write!(f, "Out of space"),
            OutOfBounds => write!(f, "Out of bounds"),
            PoolOverflow { base, size } => write!(f, "base={} + size={} overflows", base, size),
            PoolSizeZero => write!(f, "Pool cannot have size of 0"),
            InvalidAddress => write!(f, "Invalid address"),
        }
    }
}

/// NonMappedDeviceMemory represents an existing buffer in the current address space for purposes
/// of sharing device memory to the guest where the device memory is not compatible with the mmap
/// interface, such as Vulkan VkDeviceMemory in the non-exportable case or when exported as an
/// opaque fd.
///
/// It makes the critical assumption that the buffer outlives it. It is only used to pass to the
/// hypervisor so that the memory can be shared to the guest.

/// NonMappedDeviceMemory uses a trait object `NonMappedDeviceMemoryInfo` that performs device
/// specific setup and cleanup operations (via the concrete object's new and drop methods)
/// and also implements the method to get the host address.
pub trait NonMappedDeviceMemoryInfo {
    fn as_ptr(&self) -> *mut u8;
    fn size(&self) -> u64;
}

pub struct NonMappedDeviceMemory {
    info: Box<dyn NonMappedDeviceMemoryInfo + Send>,
}

impl NonMappedDeviceMemory {
    /// Creates a non-mapped device memory object. This is marked unsafe because it is not
    /// guaranteed that the address and size don't alias some other memory in Rust.
    pub unsafe fn new(
        info: Box<dyn NonMappedDeviceMemoryInfo + Send>,
    ) -> Result<NonMappedDeviceMemory> {
        if info.as_ptr() == std::ptr::null_mut() {
            return Err(Error::InvalidAddress);
        }
        if info.size() == 0 {
            return Err(Error::InvalidAddress);
        }
        Ok(NonMappedDeviceMemory { info })
    }

    /// Returns a pointer to the beginning of the memory region. Should only be
    /// used for passing this region to ioctls for setting guest memory.
    pub fn as_ptr(&self) -> *mut u8 {
        self.info.as_ptr()
    }

    /// Returns the size of the memory region in bytes.
    pub fn size(&self) -> u64 {
        self.info.size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MemoryInfoForTest {
        hva: u64,
        size: u64,
    }

    impl NonMappedDeviceMemoryInfo for MemoryInfoForTest {
        fn as_ptr(&self) -> *mut u8 {
            self.hva as *mut u8
        }
        fn size(&self) -> u64 {
            self.size
        }
    }

    #[test]
    fn non_mapped_memory_invalid_address() {
        let res =
            unsafe { NonMappedDeviceMemory::new(Box::new(MemoryInfoForTest { hva: 0, size: 1 })) };
        match res {
            Err(Error::InvalidAddress) => (),
            Err(e) => panic!("Unexpected error: {}", e),
            Ok(_) => {
                panic!("Should not be able to create NonMappedDeviceMemory with null host address")
            }
        }
    }

    #[test]
    fn non_mapped_memory_invalid_size() {
        let res =
            unsafe { NonMappedDeviceMemory::new(Box::new(MemoryInfoForTest { hva: 1, size: 0 })) };
        match res {
            Err(Error::InvalidAddress) => (),
            Err(e) => panic!("Unexpected error: {}", e),
            Ok(_) => panic!("Should not be able to create NonMappedDeviceMemory with zero size"),
        }
    }

    #[test]
    fn non_mapped_memory_valid_parameters() {
        let res =
            unsafe { NonMappedDeviceMemory::new(Box::new(MemoryInfoForTest { hva: 1, size: 1 })) };
        match res {
            Ok(_) => (),
            Err(e) => panic!("Unexpected error: {}", e),
        }
    }

    #[test]
    fn non_mapped_memory_get_address() {
        let address = 1234 as *mut u8;
        let res = unsafe {
            NonMappedDeviceMemory::new(Box::new(MemoryInfoForTest {
                hva: address as u64,
                size: 1,
            }))
        };
        match res {
            Ok(mem) => {
                assert_eq!(mem.as_ptr(), address);
            }
            Err(e) => panic!("Unexpected error: {}", e),
        }
    }

    #[test]
    fn non_mapped_memory_get_address_with_size() {
        let address = 1234 as *mut u8;
        let size = 5678 as u64;
        let res = unsafe {
            NonMappedDeviceMemory::new(Box::new(MemoryInfoForTest {
                hva: address as u64,
                size,
            }))
        };
        match res {
            Ok(mem) => {
                assert_eq!(mem.size(), size);
            }
            Err(e) => panic!("Unexpected error: {}", e),
        }
    }
}
