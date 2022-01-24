// Copyright (c) 2022 Qualcomm Innovation Center, Inc. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    fs::File,
    os::unix::prelude::AsRawFd,
    path::{Path, PathBuf},
};

use base::{
    errno_result, ioctl, AsRawDescriptor, Event, FromRawDescriptor, MappedRegion, RawDescriptor,
    Result, SafeDescriptor,
};

use gunyah_sys::*;
use vm_memory::{GuestAddress, GuestMemory};

use crate::{
    ClockState, Datamatch, DeviceKind, Hypervisor, HypervisorCap, IoEventAddress, MemSlot, Vm,
    VmCap,
};

pub struct Gunyah {
    gunyah: SafeDescriptor,
}

impl Gunyah {
    pub fn new_with_path(device_path: &Path) -> Result<Gunyah> {
        Ok(Gunyah {
            gunyah: SafeDescriptor::from(File::open(device_path)?),
        })
    }

    pub fn new() -> Result<Gunyah> {
        Gunyah::new_with_path(&PathBuf::from("/dev/gunyah"))
    }
}

impl AsRawDescriptor for Gunyah {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.gunyah.as_raw_descriptor()
    }
}

impl Hypervisor for Gunyah {
    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        Ok(Gunyah {
            gunyah: self.gunyah.try_clone()?,
        })
    }

    fn check_capability(&self, cap: HypervisorCap) -> bool {
        todo!()
    }
}

pub struct GunyahVm {
    gunyah: Gunyah,
    vm: SafeDescriptor,
}

impl GunyahVm {
    pub fn new(gunyah: &Gunyah) -> Result<GunyahVm> {
        // Safe because we know gunyah is a real gunyah fd as this module is the only one that can
        // make Gunyah objects.
        let ret = unsafe { ioctl(gunyah, GH_CREATE_VM()) };
        if ret < 0 {
            return errno_result();
        }
        // Safe because we verify that ret is valid and we own the fd.
        let vm_descriptor = unsafe { SafeDescriptor::from_raw_descriptor(ret) };
        Ok(GunyahVm {
            gunyah: gunyah.try_clone()?,
            vm: vm_descriptor,
        })
    }
}

impl Vm for GunyahVm {
    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        Ok(GunyahVm {
            gunyah: self.gunyah.try_clone()?,
            vm: self.vm.try_clone()?,
        })
    }

    fn check_capability(&self, c: VmCap) -> bool {
        todo!()
    }

    fn get_memory(&self) -> &GuestMemory {
        todo!()
    }

    fn add_memory_region(
        &mut self,
        guest_addr: GuestAddress,
        mem_region: Box<dyn MappedRegion>,
        read_only: bool,
        log_dirty_pages: bool,
    ) -> Result<MemSlot> {
        todo!()
    }

    fn msync_memory_region(&mut self, slot: MemSlot, offset: usize, size: usize) -> Result<()> {
        todo!()
    }

    fn remove_memory_region(&mut self, slot: MemSlot) -> Result<Box<dyn base::MappedRegion>> {
        todo!()
    }

    fn create_device(&self, kind: DeviceKind) -> Result<SafeDescriptor> {
        todo!()
    }

    fn get_dirty_log(&self, slot: MemSlot, dirty_log: &mut [u8]) -> Result<()> {
        todo!()
    }

    fn register_ioevent(
        &mut self,
        evt: &Event,
        addr: IoEventAddress,
        datamatch: Datamatch,
    ) -> Result<()> {
        todo!()
    }

    fn unregister_ioevent(
        &mut self,
        evt: &Event,
        addr: IoEventAddress,
        datamatch: Datamatch,
    ) -> Result<()> {
        todo!()
    }

    fn handle_io_events(&self, addr: IoEventAddress, data: &[u8]) -> Result<()> {
        todo!()
    }

    fn get_pvclock(&self) -> Result<ClockState> {
        todo!()
    }

    fn set_pvclock(&self, state: &ClockState) -> Result<()> {
        todo!()
    }

    fn add_fd_mapping(
        &mut self,
        slot: u32,
        offset: usize,
        size: usize,
        fd: &dyn AsRawFd,
        fd_offset: u64,
        prot: base::Protection,
    ) -> Result<()> {
        todo!()
    }

    fn remove_mapping(&mut self, slot: u32, offset: usize, size: usize) -> Result<()> {
        todo!()
    }

    fn get_guest_phys_addr_bits(&self) -> u8 {
        todo!()
    }
}

impl AsRawDescriptor for GunyahVm {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.vm.as_raw_descriptor()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gunyah_exists() -> bool {
        PathBuf::from("/dev/gunyah").exists()
    }

    #[test]
    fn new() {
        if !gunyah_exists() {
            return;
        }

        Gunyah::new().unwrap();
    }
}
