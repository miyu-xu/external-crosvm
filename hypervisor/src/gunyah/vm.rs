use super::*;

use libc::ENXIO;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::os::raw::c_ulong;
use std::sync::Arc;

use libc::{EFAULT, EINVAL, EIO, ENOENT, ENOSPC, EOVERFLOW, ENOTSUP};

use base::{
    errno_result, ioctl_with_ref, ioctl_with_val, pagesize, AsRawDescriptor, Error, Event,
    FromRawDescriptor, MappedRegion, MmapError, Protection, RawDescriptor, Result, SafeDescriptor,
    ioctl_with_mut_ref, info,
};
use sync::Mutex;
use vm_memory::{GuestAddress, GuestMemory};

use crate::{ClockState, Datamatch, DeviceKind, Hypervisor, IoEventAddress, MemSlot, Vm, VmCap};

use std::sync::atomic::AtomicU64;
use crate::Vcpu;
use std::os::raw::c_int;
use crate::VcpuRunHandle;
use crate::IoParams;
use crate::HypervHypercall;
use crate::VcpuExit;
use base::error;
use crate::{IoOperation};
use base::MemoryMapping;
use crate::gunyah::gh_vcpu_run;
use std::cell::RefCell;
use base::block_signal;
use base::unblock_signal;
use base::MemoryMappingBuilder;
use std::mem::ManuallyDrop;
use libc::EBUSY;
use base::ioctl;
use std::cmp::min;
use base::MemoryMappingBuilderUnix;
use crate::gunyah::GH_VCPU_RUN;
use crate::gunyah::{GH_SYSTEM_EVENT_SHUTDOWN, GH_SYSTEM_EVENT_CRASH, GH_EXIT_MMIO, GH_EXIT_SHUTDOWN, GH_EXIT_HLT, GH_EXIT_INTR, GH_EXIT_WATCHDOG, GH_EXIT_SYSTEM_EVENT};

use std::fs::File;
use std::os::unix::io::FromRawFd;
use vm_memory::MemoryRegion;


/// A wrapper around using a GUNYAH Vcpu.
pub struct GunyahVcpu {
    vm: SafeDescriptor,
    vcpu: SafeDescriptor,
    id: usize,
    run_mmap: MemoryMapping,
    vcpu_run_handle_fingerprint: Arc<AtomicU64>,
}

impl AsRawDescriptor for GunyahVcpu {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.vcpu.as_raw_descriptor()
    }
}

pub(super) struct VcpuThread {
    run: *mut gh_vcpu_run,
    signal_num: Option<c_int>,
}

thread_local!(static VCPU_THREAD: RefCell<Option<VcpuThread>> = RefCell::new(None));

// Represents a temporarily blocked signal. It will unblock the signal when dropped.
struct BlockedSignal {
    signal_num: c_int,
}

impl BlockedSignal {
    // Returns a `BlockedSignal` if the specified signal can be blocked, otherwise None.
    fn new(signal_num: c_int) -> Option<BlockedSignal> {
        if block_signal(signal_num).is_ok() {
            Some(BlockedSignal { signal_num })
        } else {
            None
        }
    }
}

impl Drop for BlockedSignal {
    fn drop(&mut self) {
        let _ = unblock_signal(self.signal_num).expect("failed to restore signal mask");
    }
}

impl Vcpu for GunyahVcpu {
    fn try_clone(&self) -> Result<Self> {
        let vm = self.vm.try_clone()?;
        let vcpu = self.vcpu.try_clone()?;
        let run_mmap = MemoryMappingBuilder::new(self.run_mmap.size())
            .from_descriptor(&vcpu)
            .build()
            .map_err(|_| Error::new(ENOSPC))?;
        let vcpu_run_handle_fingerprint = self.vcpu_run_handle_fingerprint.clone();

        Ok(GunyahVcpu {
            vm,
            vcpu,
            id: self.id,
            run_mmap,
            vcpu_run_handle_fingerprint,
        })
    }

    fn as_vcpu(&self) -> &dyn Vcpu {
        self
    }

    #[allow(clippy::cast_ptr_alignment)]
    fn take_run_handle(&self, signal_num: Option<c_int>) -> Result<VcpuRunHandle> {
        fn vcpu_run_handle_drop() {
            VCPU_THREAD.with(|v| {
                // This assumes that a failure in `BlockedSignal::new` means the signal is already
                // blocked and there it should not be unblocked on exit.
                let _blocked_signal = &(*v.borrow())
                    .as_ref()
                    .and_then(|state| state.signal_num)
                    .map(BlockedSignal::new);

                *v.borrow_mut() = None;
            });
        }

        // Prevent `vcpu_run_handle_drop` from being called until we actually setup the signal
        // blocking. The handle needs to be made now so that we can use the fingerprint.
        let vcpu_run_handle = ManuallyDrop::new(VcpuRunHandle::new(vcpu_run_handle_drop));

        // AcqRel ordering is sufficient to ensure only one thread gets to set its fingerprint to
        // this Vcpu and subsequent `run` calls will see the fingerprint.
        if self
            .vcpu_run_handle_fingerprint
            .compare_exchange(
                0,
                vcpu_run_handle.fingerprint().as_u64(),
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return Err(Error::new(EBUSY));
        }

        // Block signal while we add -- if a signal fires (very unlikely,
        // as this means something is trying to pause the vcpu before it has
        // even started) it'll try to grab the read lock while this write
        // lock is grabbed and cause a deadlock.
        // Assuming that a failure to block means it's already blocked.
        let _blocked_signal = signal_num.map(BlockedSignal::new);

        VCPU_THREAD.with(|v| {
            if v.borrow().is_none() {
                *v.borrow_mut() = Some(VcpuThread {
                    run: self.run_mmap.as_ptr() as *mut gh_vcpu_run,
                    signal_num,
                });
                Ok(())
            } else {
                Err(Error::new(EBUSY))
            }
        })?;

        Ok(ManuallyDrop::into_inner(vcpu_run_handle))
    }

    fn id(&self) -> usize {
        self.id
    }

    #[allow(clippy::cast_ptr_alignment)]
    fn set_immediate_exit(&self, exit: bool) {
        // Safe because we know we mapped enough memory to hold the gh_vcpu_run struct because the
        // kernel told us how large it was. The pointer is page aligned so casting to a different
        // type is well defined, hence the clippy allow attribute.
        let run = unsafe { &mut *(self.run_mmap.as_ptr() as *mut gh_vcpu_run) };
        run.immediate_exit = if exit { 1 } else { 0 };
    }

    fn set_local_immediate_exit(exit: bool) {
        VCPU_THREAD.with(|v| {
            if let Some(state) = &(*v.borrow()) {
                unsafe {
                    (*state.run).immediate_exit = if exit { 1 } else { 0 };
                };
            }
        });
    }

    fn set_local_immediate_exit_fn(&self) -> extern "C" fn() {
        extern "C" fn f() {
            GunyahVcpu::set_local_immediate_exit(true);
        }
        f
    }

    fn pvclock_ctrl(&self) -> Result<()> {
        Err(Error::new(libc::ENXIO))
    }

    fn set_signal_mask(&self, _signals: &[c_int]) -> Result<()> {
        Err(Error::new(libc::ENXIO))
    }

    unsafe fn enable_raw_capability(&self, _cap: u32, _args: &[u64; 4]) -> Result<()> {
        Err(Error::new(libc::ENXIO))
    }

    #[allow(clippy::cast_ptr_alignment)]
    // The pointer is page aligned so casting to a different type is well defined, hence the clippy
    // allow attribute.
    fn run(&mut self, run_handle: &VcpuRunHandle) -> Result<VcpuExit> {
        // Acquire is used to ensure this check is ordered after the `compare_exchange` in `run`.
        if self
            .vcpu_run_handle_fingerprint
            .load(std::sync::atomic::Ordering::Acquire)
            != run_handle.fingerprint().as_u64()
        {
            panic!("invalid VcpuRunHandle used to run Vcpu");
        }

        // Safe because we know that our file is a VCPU fd and we verify the return result.
        let ret = unsafe { ioctl(self, GH_VCPU_RUN()) };
        if ret != 0 {
            return errno_result();
        }

        // Safe because we know we mapped enough memory to hold the gh_vcpu_run struct because the
        // kernel told us how large it was.
        let run = unsafe { &mut *(self.run_mmap.as_ptr() as *mut gh_vcpu_run) };
        match run.exit_reason {
            GH_EXIT_MMIO => Ok(VcpuExit::Mmio),
            GH_EXIT_SHUTDOWN => Ok(VcpuExit::Shutdown),
            GH_EXIT_HLT => Ok(VcpuExit::Hlt),
            GH_EXIT_INTR => Ok(VcpuExit::Intr),
            GH_EXIT_WATCHDOG => Ok(VcpuExit::Watchdog),
            GH_EXIT_SYSTEM_EVENT => {
                // Safe because we know the exit reason told us this union
                // field is valid
                let event_type = unsafe { run.__bindgen_anon_1.system_event.type_ };
                let event_flags = unsafe { run.__bindgen_anon_1.system_event.__bindgen_anon_1.flags };
                match event_type {
                    GH_SYSTEM_EVENT_SHUTDOWN => Ok(VcpuExit::SystemEventShutdown),
                    //GH_SYSTEM_EVENT_RESET => self.system_event_reset(event_flags),
                    GH_SYSTEM_EVENT_CRASH => Ok(VcpuExit::SystemEventCrash),
                    _ => {
                        error!(
                            "Unknown GH system event {} with flags {}",
                            event_type, event_flags
                        );
                        Err(Error::new(EINVAL))
                    }
                }
            }
            r => panic!("unknown gh exit reason: {}", r),
        }
    }

    fn handle_mmio(&self, handle_fn: &mut dyn FnMut(IoParams) -> Option<[u8; 8]>) -> Result<()> {
        // Safe because we know we mapped enough memory to hold the gh_vcpu_run struct because the
        // kernel told us how large it was. The pointer is page aligned so casting to a different
        // type is well defined, hence the clippy allow attribute.
        let run = unsafe { &mut *(self.run_mmap.as_ptr() as *mut gh_vcpu_run) };
        // Verify that the handler is called in the right context.
        assert!(run.exit_reason == GH_EXIT_MMIO);
        // Safe because the exit_reason (which comes from the kernel) told us which
        // union field to use.
        let mmio = unsafe { &mut run.__bindgen_anon_1.mmio };
        let address = mmio.phys_addr;
        let size = min(mmio.len as usize, mmio.data.len());
        if mmio.is_write != 0 {
            handle_fn(IoParams {
                address,
                size,
                operation: IoOperation::Write { data: mmio.data },
            });
            Ok(())
        } else if let Some(data) = handle_fn(IoParams {
            address,
            size,
            operation: IoOperation::Read,
        }) {
            mmio.data[..size].copy_from_slice(&data[..size]);
            Ok(())
        } else {
            Err(Error::new(EINVAL))
        }
    }

    fn handle_io(&self, _handle_fn: &mut dyn FnMut(IoParams) -> Option<[u8; 8]>) -> Result<()> {
            Err(Error::new(EINVAL))
    }

    fn handle_hyperv_hypercall(
        &self,
        _handle_fn: &mut dyn FnMut(HypervHypercall) -> u64,
    ) -> Result<()> {
            Err(Error::new(EINVAL))
    }

    fn handle_rdmsr(&self, _data: u64) -> Result<()> {
            Err(Error::new(EINVAL))
    }

    fn handle_wrmsr(&self) {
    }
}

fn map_guest_mem(ranges: &[(GuestAddress, u64)], file: File) -> std::result::Result<GuestMemory, Error> {
    assert_eq!(ranges.len(), 1);
    let offset = 0;
    let guest_base = ranges[0].0;
    let mut size = ranges[0].1;

    let metadata = file.metadata()
                        .map_err(|e| {
                        error!("{}", format!("failed to metadata of file {:?} err {}", file, e));
                        }).unwrap();
    if size != metadata.len() {
        size = metadata.len();
        info!("as fixed memory is selected, memory size is restricted to {}", size);
    }
    if size % pagesize() as u64 != 0 {
        error!("size {} is not page aligned", size);
        return Err(Error::new(EINVAL));
    }

    let memory_region = MemoryRegion::new_from_file(size, guest_base, offset, Arc::new(file))
                        .map_err(|e| {
			error!("{}", format!("failed to create mem region, addr:{}, size:{}. Err: {}", guest_base, size, e));
                        }).unwrap();
    let mut regions = Vec::<MemoryRegion>::new();
    regions.push(memory_region);

    let guest_mem = GuestMemory::from_regions(regions)
                    .map_err(|e| {
                    error!("{}", format!("failed to create GuestMemory from the regions provided {}", e));
                    }).unwrap();
    Ok(guest_mem)
}

/// A wrapper around creating and using a GUNYAH VM.
pub struct GunyahVm {
    gunyah: Gunyah,
    vm: SafeDescriptor,
    guest_mem: GuestMemory,
    mem_regions: Arc<Mutex<BTreeMap<MemSlot, Box<dyn MappedRegion>>>>,
    /// A min heap of MemSlot numbers that were used and then removed and can now be re-used
    mem_slot_gaps: Arc<Mutex<BinaryHeap<Reverse<MemSlot>>>>,
}

impl GunyahVm {
    /// Constructs a new `GunyahVm` using the given `Gunyah` instance.
    pub fn new(
        gunyah: &Gunyah,
        guest_mem: GuestMemory,
        protection_type: ProtectionType,
    ) -> Result<GunyahVm> {
        // Safe because we know descriptor is a real gunyah descriptor as this module is the only
        // one that can make Gunyah objects.
        let ret = unsafe {
            ioctl_with_val(
                gunyah,
                GH_CREATE_VM(),
                gunyah.get_vm_type(protection_type)? as c_ulong,
            )
        };

        if ret < 0 {
            return errno_result();
        }
        // Safe because we verify that ret is valid and we own the fd.
        let vm_descriptor = unsafe { SafeDescriptor::from_raw_descriptor(ret) };

        guest_mem.with_regions(|index, guest_addr, size, host_addr, _, _| {
            unsafe {
                // Safe because the guest regions are guaranteed not to overlap.
                set_user_memory_region(
                    &vm_descriptor,
                    index as MemSlot,
                    false,
                    false,
                    guest_addr.offset(),
                    size as u64,
                    host_addr as *mut u8,
                )
            }
        })?;

        Ok(GunyahVm {
            gunyah: gunyah.try_clone()?,
            vm: vm_descriptor,
            guest_mem,
            mem_regions: Arc::new(Mutex::new(BTreeMap::new())),
            mem_slot_gaps: Arc::new(Mutex::new(BinaryHeap::new())),
        })
    }

    pub fn new_guestmem_with_fixed_memory(
        gunyah: &Gunyah,
        guest_mem_layout: &[(GuestAddress, u64)],
        vm_name: &str,
        protection_type: ProtectionType,
    ) -> Result<GunyahVm> {
        // Safe because we know descriptor is a real gunyah descriptor as this module is the only
        // one that can make Gunyah objects.
        let ret = unsafe {
            ioctl_with_val(
                gunyah,
                GH_CREATE_VM(),
                gunyah.get_vm_type(protection_type)? as c_ulong,
            )
        };

        if ret < 0 {
            return errno_result();
        }
        // Safe because we verify that ret is valid and we own the fd.
        let vm_descriptor = unsafe { SafeDescriptor::from_raw_descriptor(ret) };

        let mut fw_name = fw_name {_name: [0; 16],};
        fw_name._name.get_mut(..vm_name.len()).ok_or_else(|| Error::new(EINVAL))?;
        fw_name._name[..vm_name.len()].copy_from_slice(vm_name.as_bytes());

        let ret = unsafe {
            ioctl_with_ref(
                &vm_descriptor,
                GH_SET_VM_NAME(),
                &fw_name,
            )
        };

        if ret < 0 {
            return errno_result();
        }

        let file = unsafe { File::from_raw_fd(vm_descriptor.as_raw_descriptor()) };
        let guest_mem = match self::map_guest_mem(&guest_mem_layout, file) {
            Ok(guest_mem) => guest_mem,
            Err(e) => {
                error!("failed to mmap the guest memory");
                return Err(Error::new(EINVAL));
            },
        };

        Ok(GunyahVm {
            gunyah: gunyah.try_clone()?,
            vm: vm_descriptor,
            guest_mem: guest_mem,
            mem_regions: Arc::new(Mutex::new(BTreeMap::new())),
            mem_slot_gaps: Arc::new(Mutex::new(BinaryHeap::new())),
        })
    }

    /// Checks whether a particular KVM-specific capability is available for this VM.
    fn check_raw_capability(&self, capability: GhCap) -> bool {
        // Safe because we know that our file is a GUNYAH fd, and if the cap is invalid Gunyah assumes
        // it's an unavailable extension and returns 0.
        unsafe { ioctl_with_val(self, GH_VM_CHECK_EXTENSION(), capability as c_ulong) == 1 }
    }

    fn get_device_params_arch(&self, kind: DeviceKind) -> Option<gh_create_device> {
        match kind {
            DeviceKind::ArmVgicV3 => Some(gh_create_device {
                type_: gh_device_type_GH_DEV_TYPE_ARM_VGIC_V3,
                fd: 0,
                flags: 0,
            }),
            _ => None,
        }
    }

    fn ioeventfd(
        &self,
        evt: &Event,
        addr: IoEventAddress,
        datamatch: Datamatch,
        deassign: bool,
    ) -> Result<()> {
        let (do_datamatch, datamatch_value, datamatch_len) = match datamatch {
            Datamatch::AnyLength => (false, 0, 0),
            Datamatch::U8(v) => match v {
                Some(u) => (true, u as u64, 1),
                None => (false, 0, 1),
            },
            Datamatch::U16(v) => match v {
                Some(u) => (true, u as u64, 2),
                None => (false, 0, 2),
            },
            Datamatch::U32(v) => match v {
                Some(u) => (true, u as u64, 4),
                None => (false, 0, 4),
            },
            Datamatch::U64(v) => match v {
                Some(u) => (true, u as u64, 8),
                None => (false, 0, 8),
            },
        };
        let mut flags = 0;
        if deassign {
            flags |= 1 << gh_ioeventfd_flag_nr_deassign;
        }
        if do_datamatch {
            flags |= 1 << gh_ioeventfd_flag_nr_datamatch;
        }
        if let IoEventAddress::Pio(_) = addr {
            // Fixme
            return errno_result();
        }
        let ioeventfd = gh_ioeventfd {
            datamatch: datamatch_value,
            len: datamatch_len,
            addr: match addr {
                IoEventAddress::Pio(p) => p as u64,
                IoEventAddress::Mmio(m) => m,
            },
            fd: evt.as_raw_descriptor(),
            flags,
            ..Default::default()
        };
        // Safe because we know that our file is a VM fd, we know the kernel will only read the
        // correct amount of memory from our pointer, and we verify the return result.
        let ret = unsafe { ioctl_with_ref(self, GH_VM_IOEVENTFD(), &ioeventfd) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    fn create_vcpu(&self, id: usize) -> Result<GunyahVcpu> {
        let run_mmap_size = self.gunyah.get_vcpu_mmap_size()?;

        // Safe because we know that our file is a VM fd and we verify the return result.
        let fd = unsafe { ioctl_with_val(self, GH_CREATE_VCPU(), c_ulong::try_from(id).unwrap()) };
        if fd < 0 {
            return errno_result();
        }

        // Wrap the vcpu now in case the following ? returns early. This is safe because we verified
        // the value of the fd and we own the fd.
        let vcpu = unsafe { SafeDescriptor::from_raw_descriptor(fd) };

        let run_mmap = MemoryMappingBuilder::new(run_mmap_size)
            .from_descriptor(&vcpu)
            .build()
            .map_err(|_| Error::new(ENOSPC))?;

        Ok(GunyahVcpu {
            vm: self.vm.try_clone()?,
            vcpu,
            id,
            run_mmap,
            vcpu_run_handle_fingerprint: Default::default(),
        })
    }

    /// Registers an event that will, when signalled, trigger the `gsi` irq, and `resample_evt`
    /// ( when not None ) will be triggered when the irqchip is resampled.
    pub fn register_irqfd(
        &self,
        gsi: u32,
        evt: &Event,
        resample_evt: Option<&Event>,
    ) -> Result<()> {
        let mut irqfd = gh_irqfd {
            fd: evt.as_raw_descriptor() as u32,
            label: gsi,
            ..Default::default()
        };

        if let Some(r_evt) = resample_evt {
            irqfd.flags = GH_IRQFD_FLAG_RESAMPLE;
            irqfd.resamplefd = r_evt.as_raw_descriptor() as u32;
        }

        // Safe because we know that our file is a VM fd, we know the kernel will only read the
        // correct amount of memory from our pointer, and we verify the return result.
        let ret = unsafe { ioctl_with_ref(self, GH_VM_IRQFD(), &irqfd) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    /// Unregisters an event that was previously registered with
    /// `register_irqfd`.
    ///
    /// The `evt` and `gsi` pair must be the same as the ones passed into
    /// `register_irqfd`.
    pub fn unregister_irqfd(&self, gsi: u32, evt: &Event) -> Result<()> {
        let irqfd = gh_irqfd {
            fd: evt.as_raw_descriptor() as u32,
            label: gsi,
            flags: GH_IRQFD_FLAG_DEASSIGN,
            ..Default::default()
        };
        // Safe because we know that our file is a VM fd, we know the kernel will only read the
        // correct amount of memory from our pointer, and we verify the return result.
        let ret = unsafe { ioctl_with_ref(self, GH_VM_IRQFD(), &irqfd) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

}

impl AsRawDescriptor for GunyahVm {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.vm.as_raw_descriptor()
    }
}

// Wrapper around GH_SET_USER_MEMORY_REGION ioctl, which creates, modifies, or deletes a mapping
// from guest physical to host user pages.
//
// Safe when the guest regions are guaranteed not to overlap.
unsafe fn set_user_memory_region(
    vm: &SafeDescriptor,
    slot: MemSlot,
    read_only: bool,
    log_dirty_pages: bool,
    guest_addr: u64,
    memory_size: u64,
    userspace_addr: *mut u8,
) -> Result<()> {
    let flags = 0;
    let region = gh_userspace_memory_region {
        slot,
        flags,
        guest_phys_addr: guest_addr,
        memory_size,
        userspace_addr: userspace_addr as u64,
    };

    if read_only || log_dirty_pages {
        return Err(Error::new(EINVAL));
    }

    let ret = ioctl_with_ref(vm, GH_VM_SET_USER_MEMORY_REGION(), &region);
    if ret == 0 {
        Ok(())
    } else {
        errno_result()
    }
}

impl Vm for GunyahVm {
    fn get_guest_phys_addr_bits(&self) -> u8 {
        match unsafe { ioctl_with_val(self, GH_VM_CHECK_EXTENSION(), GH_CAP_ARM_VM_IPA_SIZE.into()) } {
            // Default physical address size is 40 bits if the extension is not supported.
            ret if ret <= 0 => 40,
            ipa => ipa as u8,
        }
    }

    fn try_clone(&self) -> Result<Self> {
        Ok(GunyahVm {
            gunyah: self.gunyah.try_clone()?,
            vm: self.vm.try_clone()?,
            guest_mem: self.guest_mem.clone(),
            mem_regions: self.mem_regions.clone(),
            mem_slot_gaps: self.mem_slot_gaps.clone(),
        })
    }

    fn check_capability(&self, c: VmCap) -> bool {
        match c {
            VmCap::DirtyLog => false,
            VmCap::PvClock => false,
            VmCap::PvClockSuspend => false,
            VmCap::Protected => self.check_raw_capability(GhCap::ArmProtectedVm),
            VmCap::EarlyInitCpuid => false,
        }
    }

    fn get_memory(&self) -> &GuestMemory {
        &self.guest_mem
    }

    fn add_memory_region(
        &mut self,
        guest_addr: GuestAddress,
        mem: Box<dyn MappedRegion>,
        read_only: bool,
        log_dirty_pages: bool,
    ) -> Result<MemSlot> {
        let pgsz = pagesize() as u64;
        let size = (mem.size() as u64 + pgsz - 1) / pgsz * pgsz;
        let end_addr = guest_addr
            .checked_add(size)
            .ok_or_else(|| Error::new(EOVERFLOW))?;
        if self.guest_mem.range_overlap(guest_addr, end_addr) {
            return Err(Error::new(ENOSPC));
        }
        let mut regions = self.mem_regions.lock();
        let mut gaps = self.mem_slot_gaps.lock();
        let slot = match gaps.pop() {
            Some(gap) => gap.0,
            None => (regions.len() + self.guest_mem.num_regions() as usize) as MemSlot,
        };

        // Safe because we check that the given guest address is valid and has no overlaps. We also
        // know that the pointer and size are correct because the MemoryMapping interface ensures
        // this. We take ownership of the memory mapping so that it won't be unmapped until the slot
        // is removed.
        let res = unsafe {
            set_user_memory_region(
                &self.vm,
                slot,
                read_only,
                log_dirty_pages,
                guest_addr.offset() as u64,
                size,
                mem.as_ptr(),
            )
        };

        if let Err(e) = res {
            gaps.push(Reverse(slot));
            return Err(e);
        }
        regions.insert(slot, mem);
        Ok(slot)
    }

    fn msync_memory_region(&mut self, slot: MemSlot, offset: usize, size: usize) -> Result<()> {
        let mut regions = self.mem_regions.lock();
        let mem = regions.get_mut(&slot).ok_or_else(|| Error::new(ENOENT))?;

        mem.msync(offset, size).map_err(|err| match err {
            MmapError::InvalidAddress => Error::new(EFAULT),
            MmapError::NotPageAligned => Error::new(EINVAL),
            MmapError::SystemCallFailed(e) => e,
            _ => Error::new(EIO),
        })
    }

    fn remove_memory_region(&mut self, slot: MemSlot) -> Result<Box<dyn MappedRegion>> {
        let mut regions = self.mem_regions.lock();
        if !regions.contains_key(&slot) {
            return Err(Error::new(ENOENT));
        }
        // Safe because the slot is checked against the list of memory slots.
        unsafe {
            set_user_memory_region(&self.vm, slot, false, false, 0, 0, std::ptr::null_mut())?;
        }
        self.mem_slot_gaps.lock().push(Reverse(slot));
        // This remove will always succeed because of the contains_key check above.
        Ok(regions.remove(&slot).unwrap())
    }

    fn create_device(&self, kind: DeviceKind) -> Result<SafeDescriptor> {
        let device = if let Some(dev) = self.get_device_params_arch(kind) {
            dev
        } else {
            return Err(Error::new(libc::ENXIO));
        };

        // Safe because we know that our file is a VM fd, we know the kernel will only write correct
        // amount of memory to our pointer, and we verify the return result.
        let ret = unsafe { base::ioctl_with_ref(self, GH_VM_CREATE_DEVICE(), &device) };
        if ret == 0 {
            // Safe because we verify that ret is valid and we own the fd.
            Ok(unsafe { SafeDescriptor::from_raw_descriptor(device.fd as i32) })
        } else {
            errno_result()
        }
    }

    fn get_dirty_log(&self, _slot: MemSlot, _dirty_log: &mut [u8]) -> Result<()> {
        return Err(Error::new(EINVAL));
    }

    fn register_ioevent(
        &mut self,
        evt: &Event,
        addr: IoEventAddress,
        datamatch: Datamatch,
    ) -> Result<()> {
        self.ioeventfd(evt, addr, datamatch, false)
    }

    fn unregister_ioevent(
        &mut self,
        evt: &Event,
        addr: IoEventAddress,
        datamatch: Datamatch,
    ) -> Result<()> {
        self.ioeventfd(evt, addr, datamatch, true)
    }

    fn handle_io_events(&self, _addr: IoEventAddress, _data: &[u8]) -> Result<()> {
        // KVM delivers IO events in-kernel with ioeventfds, so this is a no-op
        Ok(())
    }

    // Fixme
    fn get_pvclock(&self) -> Result<ClockState> {
        // Fixme
        Err(Error::new(ENXIO))
    }

    fn set_pvclock(&self, _state: &ClockState) -> Result<()> {
        // Fixme
        Ok(())
    }

    fn add_fd_mapping(
        &mut self,
        slot: u32,
        offset: usize,
        size: usize,
        fd: &dyn AsRawDescriptor,
        fd_offset: u64,
        prot: Protection,
    ) -> Result<()> {
        let mut regions = self.mem_regions.lock();
        let region = regions.get_mut(&slot).ok_or_else(|| Error::new(EINVAL))?;

        match region.add_fd_mapping(offset, size, fd, fd_offset, prot) {
            Ok(()) => Ok(()),
            Err(MmapError::SystemCallFailed(e)) => Err(e),
            Err(_) => Err(Error::new(EIO)),
        }
    }

    fn remove_mapping(&mut self, slot: u32, offset: usize, size: usize) -> Result<()> {
        let mut regions = self.mem_regions.lock();
        let region = regions.get_mut(&slot).ok_or_else(|| Error::new(EINVAL))?;

        match region.remove_mapping(offset, size) {
            Ok(()) => Ok(()),
            Err(MmapError::SystemCallFailed(e)) => Err(e),
            Err(_) => Err(Error::new(EIO)),
        }
    }

    fn handle_inflate(&mut self, guest_address: GuestAddress, size: u64) -> Result<()> {
        match self.guest_mem.remove_range(guest_address, size) {
            Ok(_) => Ok(()),
            Err(vm_memory::Error::MemoryAccess(_, MmapError::SystemCallFailed(e))) => Err(e),
            Err(_) => Err(Error::new(EIO)),
        }
    }

    fn handle_deflate(&mut self, _guest_address: GuestAddress, _size: u64) -> Result<()> {
        // No-op, when the guest attempts to access the pages again, Linux/KVM will provide them.
        Ok(())
    }
}

impl VmAArch64 for GunyahVm {
    fn get_hypervisor(&self) -> &dyn Hypervisor {
        &self.gunyah
    }

    fn load_protected_vm_firmware(
        &mut self,
        _fw_addr: GuestAddress,
        _fw_max_size: u64,
    ) -> Result<()> {
            Err(Error::new(EINVAL))
    }

    fn create_vcpu(&self, id: usize) -> Result<Box<dyn VcpuAArch64>> {
        Ok(Box::new(GunyahVm::create_vcpu(self, id)?))
    }
}

struct GunyahVcpuRegister(u64);

#[macro_export]
macro_rules! gh_core_reg {
    ($reg: tt) => {{
        let off = (memoffset::offset_of!(gunyah_sys::user_pt_regs, $reg) / 4) as u64;
        gunyah_sys::GH_REG_ARM64
            | gunyah_sys::GH_REG_SIZE_U64
            | gunyah_sys::GH_REG_ARM_CORE as u64
            | off
    }};
    (regs, $x: literal) => {{
        let off = ((memoffset::offset_of!(gunyah_sys::user_pt_regs, regs)
            + ($x * ::std::mem::size_of::<u64>()))
            / 4) as u64;
        gunyah_sys::GH_REG_ARM64
            | gunyah_sys::GH_REG_SIZE_U64
            | gunyah_sys::GH_REG_ARM_CORE as u64
            | off
    }};
}

impl From<VcpuRegAArch64> for GunyahVcpuRegister {
    fn from(reg: VcpuRegAArch64) -> Self {
        match reg {
            VcpuRegAArch64::X0 => Self(gh_core_reg!(regs, 0)),
            VcpuRegAArch64::X1 => Self(gh_core_reg!(regs, 1)),
            VcpuRegAArch64::X2 => Self(gh_core_reg!(regs, 2)),
            VcpuRegAArch64::X3 => Self(gh_core_reg!(regs, 3)),
            VcpuRegAArch64::X4 => Self(gh_core_reg!(regs, 4)),
            VcpuRegAArch64::X5 => Self(gh_core_reg!(regs, 5)),
            VcpuRegAArch64::X6 => Self(gh_core_reg!(regs, 6)),
            VcpuRegAArch64::X7 => Self(gh_core_reg!(regs, 7)),
            VcpuRegAArch64::X8 => Self(gh_core_reg!(regs, 8)),
            VcpuRegAArch64::X9 => Self(gh_core_reg!(regs, 9)),
            VcpuRegAArch64::X10 => Self(gh_core_reg!(regs, 10)),
            VcpuRegAArch64::X11 => Self(gh_core_reg!(regs, 11)),
            VcpuRegAArch64::X12 => Self(gh_core_reg!(regs, 12)),
            VcpuRegAArch64::X13 => Self(gh_core_reg!(regs, 13)),
            VcpuRegAArch64::X14 => Self(gh_core_reg!(regs, 14)),
            VcpuRegAArch64::X15 => Self(gh_core_reg!(regs, 15)),
            VcpuRegAArch64::X16 => Self(gh_core_reg!(regs, 16)),
            VcpuRegAArch64::X17 => Self(gh_core_reg!(regs, 17)),
            VcpuRegAArch64::X18 => Self(gh_core_reg!(regs, 18)),
            VcpuRegAArch64::X19 => Self(gh_core_reg!(regs, 19)),
            VcpuRegAArch64::X20 => Self(gh_core_reg!(regs, 20)),
            VcpuRegAArch64::X21 => Self(gh_core_reg!(regs, 21)),
            VcpuRegAArch64::X22 => Self(gh_core_reg!(regs, 22)),
            VcpuRegAArch64::X23 => Self(gh_core_reg!(regs, 23)),
            VcpuRegAArch64::X24 => Self(gh_core_reg!(regs, 24)),
            VcpuRegAArch64::X25 => Self(gh_core_reg!(regs, 25)),
            VcpuRegAArch64::X26 => Self(gh_core_reg!(regs, 26)),
            VcpuRegAArch64::X27 => Self(gh_core_reg!(regs, 27)),
            VcpuRegAArch64::X28 => Self(gh_core_reg!(regs, 28)),
            VcpuRegAArch64::X29 => Self(gh_core_reg!(regs, 29)),
            VcpuRegAArch64::X30 => Self(gh_core_reg!(regs, 30)),
            VcpuRegAArch64::Sp => Self(gh_core_reg!(sp)),
            VcpuRegAArch64::Pc => Self(gh_core_reg!(pc)),
            VcpuRegAArch64::Pstate => Self(gh_core_reg!(pstate)),
        }
    }
}

impl GunyahVcpu {
    fn set_one_gunyah_reg(&self, gh_reg_id: GunyahVcpuRegister, data: u64) -> Result<()> {
        let data_ref = &data as *const u64;
        let onereg = gh_one_reg {
            id: gh_reg_id.0,
            addr: data_ref as u64,
        };
        // Safe because we allocated the struct and we know the kernel will read exactly the size of
        // the struct.
        let ret = unsafe { ioctl_with_ref(self, GH_SET_ONE_REG(), &onereg) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    fn get_one_gunyah_reg(&self, gh_reg_id: GunyahVcpuRegister) -> Result<u64> {
        let mut val: u64 = 0;
        let onereg = gh_one_reg {
            id: gh_reg_id.0,
            addr: (&mut val as *mut u64) as u64,
        };

        // Safe because we allocated the struct and we know the kernel will read exactly the size of
        // the struct.
        let ret = unsafe { ioctl_with_ref(self, GH_GET_ONE_REG(), &onereg) };
        if ret == 0 {
            Ok(val)
        } else {
            return errno_result();
        }
    }
}

impl VcpuAArch64 for GunyahVcpu {
    fn init(&self, features: &[VcpuFeature]) -> Result<()> {
        let mut gvi = gh_vcpu_init {
            target: GH_ARM_TARGET_GENERIC_V8,
            features: [0; 7],
        };

        // Safe because we allocated the struct and we know the kernel will write exactly the size
        // of the struct.
        let ret = unsafe { ioctl_with_mut_ref(&self.vm, GH_ARM_PREFERRED_TARGET(), &mut gvi) };
        if ret != 0 {
            return errno_result();
        }

        for f in features {
            let shift = match f {
                VcpuFeature::PsciV0_2 => GH_ARM_VCPU_PSCI_0_2,
                VcpuFeature::PmuV3 => GH_ARM_VCPU_PMU_V3,
                VcpuFeature::PowerOff => GH_ARM_VCPU_POWER_OFF,
            };
            gvi.features[0] |= 1 << shift;
        }

        // Safe because we allocated the struct and we know the kernel will read exactly the size of
        // the struct.
        let ret = unsafe { ioctl_with_ref(self, GH_ARM_VCPU_INIT(), &gvi) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    fn init_pmu(&self, _irq: u64) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    fn has_pvtime_support(&self) -> bool {
        return false;
    }

    fn init_pvtime(&self, _pvtime_ipa: u64) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    fn set_one_reg(&self, reg_id: VcpuRegAArch64, data: u64) -> Result<()> {
        self.set_one_gunyah_reg(GunyahVcpuRegister::from(reg_id), data)
    }

    fn get_one_reg(&self, reg_id: VcpuRegAArch64) -> Result<u64> {
        self.get_one_gunyah_reg(GunyahVcpuRegister::from(reg_id))
    }

    fn get_psci_version(&self) -> Result<PsciVersion> {
        Ok(PSCI_0_2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base::{EventReadResult, MemoryMappingBuilder, SharedMemory};
    use std::time::Duration;

    #[test]
    fn create_vm() {
        let gunyah = Gunyah::new().expect("failed to instantiate GUNYAH");
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        GunyahVm::new(&gunyah, mem, ProtectionType::Protected).expect("failed to create vm");
    }

    #[test]
    fn create_vcpu() {
        let gunyah = Gunyah::new().expect("failed to instantiate GUNYAH");
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm =
            GunyahVm::new(&gunyah, mem, ProtectionType::Protected).expect("failed to create vm");
        vm.create_vcpu(0).expect("failed to create vcpu");
    }

    #[test]
    fn register_ioevent() {
        let gunyah = Gunyah::new().expect("failed to create gunyah");
        let gm = GuestMemory::new(&[(GuestAddress(0), 0x10000)]).unwrap();
        let mut vm =
            GunyahVm::new(&gunyah, gm, ProtectionType::Protected).expect("failed to create vm");
        let evt = Event::new().expect("failed to create event");
        let otherevt = Event::new().expect("failed to create event");
        vm.register_ioevent(&evt, IoEventAddress::Pio(0xf4), Datamatch::AnyLength)
            .unwrap();
        vm.register_ioevent(&evt, IoEventAddress::Mmio(0x1000), Datamatch::AnyLength)
            .unwrap();

        vm.register_ioevent(
            &otherevt,
            IoEventAddress::Mmio(0x1000),
            Datamatch::AnyLength,
        )
        .expect_err("GUNYAH should not allow you to register two events for the same address");

        vm.register_ioevent(
            &otherevt,
            IoEventAddress::Mmio(0x1000),
            Datamatch::U8(None),
        )
        .expect_err(
            "GUNYAH should not allow you to register ioevents with Datamatches other than AnyLength",
        );

        vm.register_ioevent(
            &otherevt,
            IoEventAddress::Mmio(0x1000),
            Datamatch::U32(Some(0xf6)),
        )
        .expect_err(
            "GUNYAH should not allow you to register ioevents with Datamatches other than AnyLength",
        );

        vm.unregister_ioevent(&otherevt, IoEventAddress::Pio(0xf4), Datamatch::AnyLength)
            .expect_err("unregistering an unknown event should fail");
        vm.unregister_ioevent(&evt, IoEventAddress::Pio(0xf5), Datamatch::AnyLength)
            .expect_err("unregistering an unknown PIO address should fail");
        vm.unregister_ioevent(&evt, IoEventAddress::Pio(0x1000), Datamatch::AnyLength)
            .expect_err("unregistering an unknown PIO address should fail");
        vm.unregister_ioevent(&evt, IoEventAddress::Mmio(0xf4), Datamatch::AnyLength)
            .expect_err("unregistering an unknown MMIO address should fail");
        vm.unregister_ioevent(&evt, IoEventAddress::Pio(0xf4), Datamatch::AnyLength)
            .unwrap();
        vm.unregister_ioevent(&evt, IoEventAddress::Mmio(0x1000), Datamatch::AnyLength)
            .unwrap();
    }

    #[test]
    fn handle_io_events() {
        let gunyah = Gunyah::new().expect("failed to create gunyah");
        let gm = GuestMemory::new(&[(GuestAddress(0), 0x10000)]).unwrap();
        let mut vm =
            GunyahVm::new(&gunyah, gm, ProtectionType::Protected).expect("failed to create vm");
        let evt = Event::new().expect("failed to create event");
        let evt2 = Event::new().expect("failed to create event");
        vm.register_ioevent(&evt, IoEventAddress::Pio(0x1000), Datamatch::AnyLength)
            .unwrap();
        vm.register_ioevent(&evt2, IoEventAddress::Mmio(0x1000), Datamatch::AnyLength)
            .unwrap();

        // Check a pio address
        vm.handle_io_events(IoEventAddress::Pio(0x1000), &[])
            .expect("failed to handle_io_events");
        assert_ne!(
            evt.read_timeout(Duration::from_millis(10))
                .expect("failed to read event"),
            EventReadResult::Timeout
        );
        assert_eq!(
            evt2.read_timeout(Duration::from_millis(10))
                .expect("failed to read event"),
            EventReadResult::Timeout
        );
        // Check an mmio address
        vm.handle_io_events(IoEventAddress::Mmio(0x1000), &[])
            .expect("failed to handle_io_events");
        assert_eq!(
            evt.read_timeout(Duration::from_millis(10))
                .expect("failed to read event"),
            EventReadResult::Timeout
        );
        assert_ne!(
            evt2.read_timeout(Duration::from_millis(10))
                .expect("failed to read event"),
            EventReadResult::Timeout
        );

        // Check an address that does not match any registered ioevents
        vm.handle_io_events(IoEventAddress::Pio(0x1001), &[])
            .expect("failed to handle_io_events");
        assert_eq!(
            evt.read_timeout(Duration::from_millis(10))
                .expect("failed to read event"),
            EventReadResult::Timeout
        );
        assert_eq!(
            evt2.read_timeout(Duration::from_millis(10))
                .expect("failed to read event"),
            EventReadResult::Timeout
        );
    }

    #[test]
    fn remove_memory() {
        let gunyah = Gunyah::new().unwrap();
        let gm = GuestMemory::new(&[(GuestAddress(0), 0x1000)]).unwrap();
        let mut vm = GunyahVm::new(&gunyah, gm, ProtectionType::Protected).unwrap();
        let mem_size = 0x1000;
        let shm = SharedMemory::new("test", mem_size as u64).unwrap();
        let mem = MemoryMappingBuilder::new(mem_size)
            .from_shared_memory(&shm)
            .build()
            .unwrap();
        let mem_ptr = mem.as_ptr();
        let slot = vm
            .add_memory_region(GuestAddress(0x1000), Box::new(mem), false, false)
            .unwrap();
        let removed_mem = vm.remove_memory_region(slot).unwrap();
        assert_eq!(removed_mem.size(), mem_size);
        assert_eq!(removed_mem.as_ptr(), mem_ptr);
    }
}
