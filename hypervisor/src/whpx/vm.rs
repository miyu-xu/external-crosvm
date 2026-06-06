// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::ffi::c_void;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::collections::BinaryHeap;
use std::convert::TryInto;
use std::sync::Arc;

use base::error;
use base::info;
use base::pagesize;
use base::AsRawDescriptor;
use base::Error;
use base::Event;
use base::MappedRegion;
use base::MmapError;
use base::Protection;
use base::RawDescriptor;
use base::Result;
use base::SafeDescriptor;
use base::SendTube;
use fnv::FnvHashMap;
use libc::EEXIST;
use libc::EFAULT;
use libc::EINVAL;
use libc::EIO;
use libc::ENODEV;
use libc::ENOENT;
use libc::ENOSPC;
use libc::ENOTSUP;
use libc::EOVERFLOW;
use sync::Mutex;
use vm_memory::GuestAddress;
use vm_memory::GuestMemory;
use winapi::shared::winerror::ERROR_BUSY;
use winapi::shared::winerror::ERROR_SUCCESS;
use winapi::um::memoryapi::OfferVirtualMemory;
use winapi::um::memoryapi::ReclaimVirtualMemory;
use winapi::um::memoryapi::VmOfferPriorityBelowNormal;
use winapi::um::winnt::RtlZeroMemory;

use super::types::*;
use super::*;
use crate::host_phys_addr_bits;
use crate::whpx::whpx_sys::*;
use crate::BalloonEvent;
use crate::ClockState;
use crate::Datamatch;
use crate::DeliveryMode;
use crate::DestinationMode;
use crate::DeviceKind;
use crate::IoEventAddress;
use crate::LapicState;
use crate::MemCacheType;
use crate::MemSlot;
use crate::TriggerMode;
use crate::VcpuX86_64;
use crate::Vm;
use crate::VmCap;
use crate::VmX86_64;

pub struct WhpxVm {
    whpx: Whpx,
    // reference counted, since we need to implement try_clone or some variation.
    // There is only ever 1 create/1 delete partition unlike dup/close handle variations.
    vm_partition: Arc<SafePartition>,
    guest_mem: GuestMemory,
    mem_regions: Arc<Mutex<BTreeMap<MemSlot, (GuestAddress, Box<dyn MappedRegion>)>>>,
    /// A min heap of MemSlot numbers that were used and then removed and can now be re-used
    mem_slot_gaps: Arc<Mutex<BinaryHeap<Reverse<MemSlot>>>>,
    // WHPX's implementation of ioevents makes several assumptions about how crosvm uses ioevents:
    //   1. All ioevents are registered during device setup, and thus can be cloned when the vm is
    //      cloned instead of locked in an Arc<Mutex<>>. This will make handling ioevents in each
    //      vcpu thread easier because no locks will need to be acquired.
    //   2. All ioevents use Datamatch::AnyLength. We don't bother checking the datamatch, which
    //      will make this faster.
    //   3. We only ever register one eventfd to each address. This simplifies our data structure.
    ioevents: FnvHashMap<IoEventAddress, Event>,
    // Tube to send events to control.
    vm_evt_wrtube: Option<SendTube>,
    /// Whether WHPX APIC emulation is enabled (X2Apic vs None).
    apic_emulation: bool,
}

impl WhpxVm {
    pub fn new(
        whpx: &Whpx,
        cpu_count: usize,
        guest_mem: GuestMemory,
        cpuid: CpuId,
        apic_emulation: bool,
        vm_evt_wrtube: Option<SendTube>,
    ) -> WhpxResult<WhpxVm> {
        let mut partition = SafePartition::new()?;
        partition.processor_count = cpu_count as u32;
        let partition = Arc::new(partition);

        // === QEMU-aligned partition property order ===
        // ALL properties MUST be set BEFORE WHvSetupPartition.
        // Property value sizes match QEMU's exact type sizes, not the full union.

        // 1. ProcessorCount (uses full WHV_PARTITION_PROPERTY union like QEMU)
        let mut property: WHV_PARTITION_PROPERTY = Default::default();
        property.ProcessorCount = cpu_count as u32;
        check_whpx!(unsafe {
            WHvSetPartitionProperty(
                partition.partition,
                WHV_PARTITION_PROPERTY_CODE_WHvPartitionPropertyCodeProcessorCount,
                &property as *const _ as *const c_void,
                std::mem::size_of::<WHV_PARTITION_PROPERTY>() as UINT32,
            )
        })
        .map_err(WhpxError::SetProcessorCount)?;

        // 2. Check APIC emulation support (only required for X2Apic mode; skipped for None).
        // In kernel-irqchip=off mode (None), crosvm handles x2APIC MSRs in software
        // via handle_msr_read/write, so WHPX APIC support is not needed.

        // 3. LocalApicEmulationMode = None (QEMU kernel-irqchip=off)
        // WHPX does not emulate the APIC. x2APIC MSR accesses cause X64MsrExit
        // exits, handled by crosvm's MSR handlers. This allows intercepting
        // MSR 0x830 (ICR) writes for software INIT/SIPI delivery.
        // CPUID still reports x2APIC via IrqChipCap::X2Apic=true.
        // QEMU passes sizeof(mode) = 4 bytes, not the full union.
        let apic_mode: WHV_X64_LOCAL_APIC_EMULATION_MODE =
            WHV_X64_LOCAL_APIC_EMULATION_MODE_WHvX64LocalApicEmulationModeNone;
        check_whpx!(unsafe {
            WHvSetPartitionProperty(
                partition.partition,
                WHV_PARTITION_PROPERTY_CODE_WHvPartitionPropertyCodeLocalApicEmulationMode,
                &apic_mode as *const _ as *const c_void,
                std::mem::size_of::<WHV_X64_LOCAL_APIC_EMULATION_MODE>() as UINT32,
            )
        })
        .map_err(WhpxError::SetLocalApicEmulationMode)?;

        // 4. ProcessorFeaturesBanks (QEMU: get capability + set BEFORE SetupPartition)
        {
            let mut features: WHV_PROCESSOR_FEATURES_BANKS = Default::default();
            features.BanksCount = 2;
            let mut cap_size: UINT32 = 0;
            let cap_hr = unsafe {
                WHvGetCapability(
                    WHV_CAPABILITY_CODE_WHvCapabilityCodeProcessorFeaturesBanks,
                    &mut features as *mut _ as *mut c_void,
                    std::mem::size_of::<WHV_PROCESSOR_FEATURES_BANKS>() as u32,
                    &mut cap_size,
                )
            };
            if cap_hr == 0 {
                match check_whpx!(unsafe {
                    WHvSetPartitionProperty(
                        partition.partition,
                        WHV_PARTITION_PROPERTY_CODE_WHvPartitionPropertyCodeProcessorFeaturesBanks,
                        &features as *const _ as *const c_void,
                        std::mem::size_of::<WHV_PROCESSOR_FEATURES_BANKS>() as u32,
                    )
                }) {
                    Ok(()) => info!("whpx: ProcessorFeaturesBanks set ok banks=2"),
                    Err(e) => info!("whpx: ProcessorFeaturesBanks failed: {}", e),
                }

            } else {
                info!("whpx: WHvGetCapability(ProcessorFeaturesBanks) not supported, hr={:#x}", cap_hr as u32);
            }
        }

        // 6. SyntheticProcessorFeaturesBanks (QEMU: set BEFORE SetupPartition)
        {
            let mut synth = WHV_SYNTHETIC_PROCESSOR_FEATURES_BANKS::default();
            synth.BanksCount = 1;
            synth.Bank0 = (1u64 << 0)   // HypervisorPresent
                | (1u64 << 1)   // Hv1
                | (1u64 << 2)   // AccessVpRunTimeReg
                | (1u64 << 3)   // AccessPartitionReferenceCounter
                | (1u64 << 9)   // AccessPartitionReferenceTsc
                | (1u64 << 7)   // AccessHypercallRegs
                | (1u64 << 11)  // AccessFrequencyRegs
                | (1u64 << 15)  // EnableExtendedGvaRangesForFlushVirtualAddressList
                | (1u64 << 8)   // AccessVpIndex
                | (1u64 << 25); // TbFlushHypercalls
            match check_whpx!(unsafe {
                WHvSetPartitionProperty(
                    partition.partition,
                    WHV_PARTITION_PROPERTY_CODE_WHvPartitionPropertyCodeSyntheticProcessorFeaturesBanks,
                    &synth as *const _ as *const c_void,
                    std::mem::size_of::<WHV_SYNTHETIC_PROCESSOR_FEATURES_BANKS>() as u32,
                )
            }) {
                Ok(()) => info!("whpx: SyntheticProcessorFeatures set ok"),
                Err(e) => info!("whpx: SyntheticProcessorFeatures failed: {}", e),
            }
        }

        // 7. CPUID results (Hyper-V enlightenment)
        let mut cpuid_results: Vec<WHV_X64_CPUID_RESULT> = cpuid
            .cpu_id_entries
            .iter()
            .map(WHV_X64_CPUID_RESULT::from)
            .collect();
        cpuid_results.push(WHV_X64_CPUID_RESULT {
            Function: HYPERV_CPUID_VENDOR_AND_MAX_FUNCTIONS,
            Reserved: [0u32; 3],
            Eax: HYPERV_CPUID_MIN,
            Ebx: u32::from_le_bytes([b'M', b'i', b'c', b'r']),
            Ecx: u32::from_le_bytes([b'o', b's', b'o', b'f']),
            Edx: u32::from_le_bytes([b't', b' ', b'H', b'v']),
        });
        cpuid_results.push(WHV_X64_CPUID_RESULT {
            Function: HYPERV_CPUID_FEATURES,
            Reserved: [0u32; 3],
            Eax: HV_ACCESS_FREQUENCY_MSRS
                | HV_ACCESS_TSC_INVARIANT
                | HV_MSR_REFERENCE_TSC_AVAILABLE,
            Ebx: 0,
            Edx: HV_FEATURE_FREQUENCY_MSRS_AVAILABLE,
            Ecx: 0,
        });
        check_whpx!(unsafe {
            WHvSetPartitionProperty(
                partition.partition,
                WHV_PARTITION_PROPERTY_CODE_WHvPartitionPropertyCodeCpuidResultList,
                cpuid_results.as_ptr() as *const _ as *const c_void,
                (std::mem::size_of::<WHV_X64_CPUID_RESULT>() * cpuid_results.len()) as UINT32,
            )
        })
        .map_err(WhpxError::SetCpuidResultList)?;

        // 8. CPUID exit list
        let exit_list: Vec<u32> = vec![0x1, 0x4, 0xB, 0x1F, 0x15];
        check_whpx!(unsafe {
            WHvSetPartitionProperty(
                partition.partition,
                WHV_PARTITION_PROPERTY_CODE_WHvPartitionPropertyCodeCpuidExitList,
                exit_list.as_ptr() as *const _ as *const c_void,
                (std::mem::size_of::<u32>() * exit_list.len()) as UINT32,
            )
        })
        .map_err(WhpxError::SetCpuidExitList)?;

        // 9. ExtendedVmExits (X64CpuidExit + X64MsrExit)
        // QEMU sets only X64MsrExit; crosvm also needs X64CpuidExit.
        // X64ApicInitSipiExitTrap is NOT enabled: on Win10 25H2, WHPX deadlocks
        // the BSP inside WHvRunVirtualProcessor after the INIT trap.
        // INIT/SIPI delivery is handled via MSR 0x830 interception in
        // handle_msr_write (QEMU kernel-irqchip=off alignment).
        let mut property: WHV_PARTITION_PROPERTY = Default::default();
        unsafe {
            property.ExtendedVmExits.__bindgen_anon_1.set_X64CpuidExit(1);
            property.ExtendedVmExits.__bindgen_anon_1.set_X64MsrExit(1);
        }
        check_whpx!(unsafe {
            WHvSetPartitionProperty(
                partition.partition,
                WHV_PARTITION_PROPERTY_CODE_WHvPartitionPropertyCodeExtendedVmExits,
                &property as *const _ as *const c_void,
                std::mem::size_of::<WHV_PARTITION_PROPERTY>() as UINT32,
            )
        })
        .map_err(WhpxError::SetExtendedVmExits)?;

        // 10. WHvSetupPartition – MUST be LAST, after ALL properties are set (QEMU order)
        check_whpx!(unsafe { WHvSetupPartition(partition.partition) })
            .map_err(WhpxError::SetupPartition)?;

        for region in guest_mem.regions() {
            unsafe {
                // Safe because the guest regions are guaranteed not to overlap.
                set_user_memory_region(
                    &partition,
                    false, // read_only
                    false, // track dirty pages
                    region.guest_addr.offset(),
                    region.size as u64,
                    region.host_addr as *mut u8,
                )
            }
            .map_err(WhpxError::MapGpaRange)?;
        }

        Ok(WhpxVm {
            whpx: whpx.clone(),
            vm_partition: partition,
            guest_mem,
            mem_regions: Arc::new(Mutex::new(BTreeMap::new())),
            mem_slot_gaps: Arc::new(Mutex::new(BinaryHeap::new())),
            ioevents: FnvHashMap::default(),
            vm_evt_wrtube,
            apic_emulation: apic_mode
                != WHV_X64_LOCAL_APIC_EMULATION_MODE_WHvX64LocalApicEmulationModeNone,
        })
    }

    /// Get the current state of the specified VCPU's local APIC.
    /// Uses WHvGetVirtualProcessorInterruptControllerState2 (QEMU-equivalent, non-deprecated).
    /// When APIC emulation is off, returns a default (zero) state.
    pub fn get_vcpu_lapic_state(&self, vcpu_id: usize) -> Result<LapicState> {
        if !self.apic_emulation {
            // No WHPX APIC — return default state
            return Ok(LapicState { regs: [0; 64] });
        }
        let buffer = WhpxLapicState { regs: [0u32; 1024] };
        let mut written_size = 0u32;
        let size = std::mem::size_of::<WhpxLapicState>();

        check_whpx!(unsafe {
            WHvGetVirtualProcessorInterruptControllerState2(
                self.vm_partition.partition,
                vcpu_id as u32,
                buffer.regs.as_ptr() as *mut c_void,
                size as u32,
                &mut written_size,
            )
        })?;

        Ok(LapicState::from(&buffer))
    }

    /// Set the current state of the specified VCPU's local APIC.
    /// Uses WHvSetVirtualProcessorInterruptControllerState2 (QEMU-equivalent, non-deprecated).
    /// When APIC emulation is off, this is a no-op.
    pub fn set_vcpu_lapic_state(&mut self, vcpu_id: usize, state: &LapicState) -> Result<()> {
        if !self.apic_emulation {
            return Ok(());
        }
        let buffer = WhpxLapicState::from(state);
        check_whpx!(unsafe {
            WHvSetVirtualProcessorInterruptControllerState2(
                self.vm_partition.partition,
                vcpu_id as u32,
                buffer.regs.as_ptr() as *mut c_void,
                std::mem::size_of::<WhpxLapicState>() as u32,
            )
        })?;
        Ok(())
    }

    /// Request an interrupt be delivered to one or more virtualized interrupt controllers. This
    /// should only be used with ApicEmulationModeXApic or ApicEmulationModeX2Apic.
    pub fn request_interrupt(
        &self,
        vector: u8,
        dest_id: u8,
        dest_mode: DestinationMode,
        trigger: TriggerMode,
        delivery: DeliveryMode,
    ) -> Result<()> {
        // The WHV_INTERRUPT_CONTROL does not seem to support the dest_shorthand
        let mut interrupt = WHV_INTERRUPT_CONTROL {
            Destination: dest_id as u32,
            Vector: vector as u32,
            ..Default::default()
        };
        interrupt.set_DestinationMode(match dest_mode {
            DestinationMode::Physical => {
                WHV_INTERRUPT_DESTINATION_MODE_WHvX64InterruptDestinationModePhysical
            }
            DestinationMode::Logical => {
                WHV_INTERRUPT_DESTINATION_MODE_WHvX64InterruptDestinationModeLogical
            }
        } as u64);
        interrupt.set_TriggerMode(match trigger {
            TriggerMode::Edge => WHV_INTERRUPT_TRIGGER_MODE_WHvX64InterruptTriggerModeEdge,
            TriggerMode::Level => WHV_INTERRUPT_TRIGGER_MODE_WHvX64InterruptTriggerModeLevel,
        } as u64);
        interrupt.set_Type(match delivery {
            DeliveryMode::Fixed => WHV_INTERRUPT_TYPE_WHvX64InterruptTypeFixed,
            DeliveryMode::Lowest => WHV_INTERRUPT_TYPE_WHvX64InterruptTypeLowestPriority,
            DeliveryMode::SMI => {
                error!("WHPX does not support requesting an SMI");
                return Err(Error::new(ENOTSUP));
            }
            DeliveryMode::RemoteRead => {
                // This is also no longer supported by intel.
                error!("Remote Read interrupts are not supported by WHPX");
                return Err(Error::new(ENOTSUP));
            }
            DeliveryMode::NMI => WHV_INTERRUPT_TYPE_WHvX64InterruptTypeNmi,
            DeliveryMode::Init => WHV_INTERRUPT_TYPE_WHvX64InterruptTypeInit,
            DeliveryMode::Startup => WHV_INTERRUPT_TYPE_WHvX64InterruptTypeSipi,
            DeliveryMode::External => {
                error!("WHPX does not support requesting an external interrupt");
                return Err(Error::new(ENOTSUP));
            }
        } as u64);

        check_whpx!(unsafe {
            WHvRequestInterrupt(
                self.vm_partition.partition,
                &interrupt,
                std::mem::size_of::<WHV_INTERRUPT_CONTROL>() as u32,
            )
        })
    }

    /// Cancel WHvRunVirtualProcessor on all vCPUs so pending interrupts are delivered promptly.
    pub fn kick_all_virtual_processors(&self) {
        for i in 0..self.vm_partition.processor_count {
            // SAFETY: we own the partition and `i` is a valid processor index.
            unsafe {
                WHvCancelRunVirtualProcessor(self.vm_partition.partition, i, 0);
            }
        }
    }

    /// Unmap a GPA subrange so WHPX traps guest accesses as MMIO exits.
    /// This is needed for device BARs that fall within RAM-backed GPA windows — without
    /// unmapping, WHPX serves reads/writes from the RAM backing, bypassing the mmio_bus.
    pub fn unmap_gpa_range(&self, guest_addr: GuestAddress, len: u64) -> Result<()> {
        // SAFETY: WHPX validates GPA and size for WHvUnmapGpaRange.
        unsafe {
            check_whpx!(WHvUnmapGpaRange(
                self.vm_partition.partition,
                guest_addr.offset(),
                len
            ))
        }
    }

    /// Unmap and remap a GPA subrange so WHPX vCPUs observe recent host-side guest RAM writes.
    ///
    /// FlushViewOfFile alone is not always sufficient for virtio ring updates on WHPX; remapping
    /// forces the hypervisor to reload the backing host pages.
    pub fn refresh_gpa_range(&self, guest_addr: GuestAddress, len: usize) -> Result<()> {
        if len == 0 {
            return Ok(());
        }

        let ps = pagesize();
        let gpa = guest_addr.offset();
        let sync_start = gpa & !(ps as u64 - 1);
        let end = gpa
            .checked_add(len as u64)
            .ok_or(Error::new(EOVERFLOW))?;
        let sync_end = (end + ps as u64 - 1) & !(ps as u64 - 1);
        let sync_len = sync_end - sync_start;

        for region in self.guest_mem.regions() {
            let slot_start = region.guest_addr.offset();
            let slot_end = slot_start + region.size as u64;
            if sync_start >= slot_start && sync_end <= slot_end {
                let offset_in_slot = (sync_start - slot_start) as usize;
                let host_ptr = (region.host_addr + offset_in_slot) as *mut u8;
                // Remap in place (no WHvUnmapGpaRange): unmap+remap can leave stale guest TLB
                // entries while OVMF polls the used ring, so the guest never sees used.idx update.
                // SAFETY: WHPX validates GPA and host pointer for WHvMapGpaRange.
                unsafe {
                    set_user_memory_region(
                        &self.vm_partition,
                        false,
                        false,
                        sync_start,
                        sync_len,
                        host_ptr,
                    )?;
                }
                base::debug!(
                    "whpx: refreshed GPA range {:#x} len={}",
                    sync_start, sync_len
                );
                return Ok(());
            }
        }

        Err(Error::new(ENOENT))
    }

    /// In order to fully unmap a memory range such that the host can reclaim the memory,
    /// we unmap it from the hypervisor partition, and then mark crosvm's process as uninterested
    /// in the memory.
    ///
    /// This will make crosvm unable to access the memory, and allow Windows to reclaim it for other
    /// uses when memory is in demand.
    fn handle_inflate(&mut self, guest_address: GuestAddress, size: u64) -> Result<()> {
        info!(
            "Balloon: Requested WHPX unmap of addr: {:?}, size: {:?}",
            guest_address, size
        );
        // Safe because WHPX does proper error checking, even if an out-of-bounds address is
        // provided.
        unsafe {
            check_whpx!(WHvUnmapGpaRange(
                self.vm_partition.partition,
                guest_address.offset(),
                size,
            ))?;
        }

        let host_address = self
            .guest_mem
            .get_host_address(guest_address)
            .map_err(|_| Error::new(1))? as *mut c_void;

        // Safe because we have just successfully unmapped this range from the
        // guest partition, so we know it's unused.
        let result =
            unsafe { OfferVirtualMemory(host_address, size as usize, VmOfferPriorityBelowNormal) };

        if result != ERROR_SUCCESS {
            let err = Error::new(result);
            error!("Freeing memory failed with error: {}", err);
            return Err(err);
        }
        Ok(())
    }

    /// Remap memory that has previously been unmapped with #handle_inflate. Note
    /// that attempts to remap pages that were not previously unmapped, or addresses that are not
    /// page-aligned, will result in failure.
    ///
    /// To do this, reclaim the memory from Windows first, then remap it into the hypervisor
    /// partition. Remapped memory has no guarantee of content, and the guest should not expect
    /// it to.
    fn handle_deflate(&mut self, guest_address: GuestAddress, size: u64) -> Result<()> {
        info!(
            "Balloon: Requested WHPX unmap of addr: {:?}, size: {:?}",
            guest_address, size
        );

        let host_address = self
            .guest_mem
            .get_host_address(guest_address)
            .map_err(|_| Error::new(1))? as *const c_void;

        // Note that we aren't doing any validation here that this range was previously unmapped.
        // However, we can avoid that expensive validation by relying on Windows error checking for
        // ReclaimVirtualMemory. The call will fail if:
        // - If the range is not currently "offered"
        // - The range is outside of current guest mem (GuestMemory will fail to convert the
        //   address)
        // In short, security is guaranteed by ensuring the guest can never reclaim ranges it
        // hadn't previously forfeited (and even then, the contents will be zeroed).
        //
        // Safe because the memory ranges in question are managed by Windows, not Rust.
        // Also, ReclaimVirtualMemory has built-in error checking for bad parameters.
        let result = unsafe { ReclaimVirtualMemory(host_address, size as usize) };

        if result == ERROR_BUSY || result == ERROR_SUCCESS {
            // In either of these cases, the contents of the reclaimed memory
            // are preserved or undefined. Regardless, zero the memory
            // to ensure no unintentional memory contents are shared.
            //
            // Safe because we just reclaimed the region in question and haven't yet remapped
            // it to the guest partition, so we know it's unused.
            unsafe { RtlZeroMemory(host_address as RawDescriptor, size as usize) };
        } else {
            let err = Error::new(result);
            error!("Reclaiming memory failed with error: {}", err);
            return Err(err);
        }

        // Safe because no-overlap is guaranteed by the success of ReclaimVirtualMemory,
        // Which would fail if it was called on areas which were not unmapped.
        unsafe {
            set_user_memory_region(
                &self.vm_partition,
                false, // read_only
                false, // track dirty pages
                guest_address.offset(),
                size,
                host_address as *mut u8,
            )
        }
    }
}

// Wrapper around WHvMapGpaRange, which creates, modifies, or deletes a mapping
// from guest physical to host user pages.
//
// Safe when the guest regions are guaranteed not to overlap.
unsafe fn set_user_memory_region(
    partition: &SafePartition,
    read_only: bool,
    track_dirty_pages: bool,
    guest_addr: u64,
    memory_size: u64,
    userspace_addr: *mut u8,
) -> Result<()> {
    let mut flags = WHV_MAP_GPA_RANGE_FLAGS_WHvMapGpaRangeFlagRead
        | WHV_MAP_GPA_RANGE_FLAGS_WHvMapGpaRangeFlagExecute;
    if !read_only {
        flags |= WHV_MAP_GPA_RANGE_FLAGS_WHvMapGpaRangeFlagWrite
    }
    if track_dirty_pages {
        flags |= WHV_MAP_GPA_RANGE_FLAGS_WHvMapGpaRangeFlagTrackDirtyPages;
    }

    let ret = WHvMapGpaRange(
        partition.partition,
        userspace_addr as *mut c_void,
        guest_addr,
        memory_size,
        flags,
    );
    check_whpx!(ret)
}

/// Helper function to determine the size in bytes of a dirty log bitmap for the given memory region
/// size.
///
/// # Arguments
///
/// * `size` - Number of bytes in the memory region being queried.
pub fn dirty_log_bitmap_size(size: usize) -> usize {
    let page_size = pagesize();
    (((size + page_size - 1) / page_size) + 7) / 8
}

impl Vm for WhpxVm {
    /// Makes a shallow clone of this `Vm`.
    fn try_clone(&self) -> Result<Self> {
        let mut ioevents = FnvHashMap::default();
        for (addr, evt) in self.ioevents.iter() {
            ioevents.insert(*addr, evt.try_clone()?);
        }
        Ok(WhpxVm {
            whpx: self.whpx.try_clone()?,
            vm_partition: self.vm_partition.clone(),
            guest_mem: self.guest_mem.clone(),
            mem_regions: self.mem_regions.clone(),
            mem_slot_gaps: self.mem_slot_gaps.clone(),
            ioevents,
            vm_evt_wrtube: self
                .vm_evt_wrtube
                .as_ref()
                .map(|t| t.try_clone().expect("could not clone vm_evt_wrtube")),
            apic_emulation: self.apic_emulation,
        })
    }

    fn check_capability(&self, c: VmCap) -> bool {
        match c {
            VmCap::DirtyLog => Whpx::check_whpx_feature(WhpxFeature::DirtyPageTracking)
                .unwrap_or_else(|e| {
                    error!(
                        "failed to check whpx feature {:?}: {}",
                        WhpxFeature::DirtyPageTracking,
                        e
                    );
                    false
                }),
            // there is a pvclock like thing already done w/ hyperv, but we can't get the state.
            VmCap::PvClock => false,
            VmCap::Protected => false,
            // whpx initializes cpuid early during VM creation.
            VmCap::EarlyInitCpuid => true,
            #[cfg(target_arch = "x86_64")]
            VmCap::BusLockDetect => false,
            VmCap::ReadOnlyMemoryRegion => true,
            VmCap::MemNoncoherentDma => false,
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
        _cache: MemCacheType,
    ) -> Result<MemSlot> {
        let size = mem.size() as u64;
        let end_addr = guest_addr.checked_add(size).ok_or(Error::new(EOVERFLOW))?;
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
                &self.vm_partition,
                read_only,
                log_dirty_pages,
                guest_addr.offset(),
                size,
                mem.as_ptr(),
            )
        };

        if let Err(e) = res {
            gaps.push(Reverse(slot));
            return Err(e);
        }
        regions.insert(slot, (guest_addr, mem));
        Ok(slot)
    }

    fn msync_memory_region(&mut self, slot: MemSlot, offset: usize, size: usize) -> Result<()> {
        let mut regions = self.mem_regions.lock();
        let (_, mem) = regions.get_mut(&slot).ok_or(Error::new(ENOENT))?;

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
        if let Some((guest_addr, mem)) = regions.get(&slot) {
            // Safe because the slot is checked against the list of memory slots.
            unsafe {
                check_whpx!(WHvUnmapGpaRange(
                    self.vm_partition.partition,
                    guest_addr.offset(),
                    mem.size() as u64,
                ))?;
            }
            self.mem_slot_gaps.lock().push(Reverse(slot));
            Ok(regions.remove(&slot).unwrap().1)
        } else {
            Err(Error::new(ENOENT))
        }
    }

    fn unmap_gpa_range(&mut self, guest_addr: GuestAddress, len: u64) -> Result<()> {
        self.unmap_gpa_range(guest_addr, len)
    }

    fn create_device(&self, _kind: DeviceKind) -> Result<SafeDescriptor> {
        // Whpx does not support in-kernel devices
        Err(Error::new(libc::ENXIO))
    }

    fn get_dirty_log(&self, slot: u32, dirty_log: &mut [u8]) -> Result<()> {
        let regions = self.mem_regions.lock();
        if let Some((guest_addr, mem)) = regions.get(&slot) {
            // Ensures that there are as many bytes in dirty_log as there are pages in the mmap.
            if dirty_log_bitmap_size(mem.size()) > dirty_log.len() {
                return Err(Error::new(EINVAL));
            }
            let bitmap_size = if dirty_log.len() % 8 == 0 {
                dirty_log.len() / 8
            } else {
                dirty_log.len() / 8 + 1
            };
            let mut bitmap = vec![0u64; bitmap_size];
            check_whpx!(unsafe {
                WHvQueryGpaRangeDirtyBitmap(
                    self.vm_partition.partition,
                    guest_addr.offset(),
                    mem.size() as u64,
                    bitmap.as_mut_ptr() as *mut u64,
                    (bitmap.len() * 8) as u32,
                )
            })?;
            // safe because we have allocated a vec of u64, which we can cast to a u8 slice.
            let buffer = unsafe {
                std::slice::from_raw_parts(bitmap.as_ptr() as *const u8, bitmap.len() * 8)
            };
            dirty_log.copy_from_slice(&buffer[..dirty_log.len()]);
            Ok(())
        } else {
            Err(Error::new(ENOENT))
        }
    }

    fn register_ioevent(
        &mut self,
        evt: &Event,
        addr: IoEventAddress,
        datamatch: Datamatch,
    ) -> Result<()> {
        if datamatch != Datamatch::AnyLength {
            error!("WHPX currently only supports Datamatch::AnyLength");
            return Err(Error::new(ENOTSUP));
        }

        if self.ioevents.contains_key(&addr) {
            error!("WHPX does not support multiple ioevents for the same address");
            return Err(Error::new(EEXIST));
        }

        self.ioevents.insert(addr, evt.try_clone()?);

        Ok(())
    }

    fn unregister_ioevent(
        &mut self,
        evt: &Event,
        addr: IoEventAddress,
        datamatch: Datamatch,
    ) -> Result<()> {
        if datamatch != Datamatch::AnyLength {
            error!("WHPX only supports Datamatch::AnyLength");
            return Err(Error::new(ENOTSUP));
        }

        match self.ioevents.get(&addr) {
            Some(existing_evt) => {
                // evt should match the existing evt associated with addr
                if evt != existing_evt {
                    return Err(Error::new(ENOENT));
                }
                self.ioevents.remove(&addr);
            }

            None => {
                return Err(Error::new(ENOENT));
            }
        };
        Ok(())
    }

    /// Trigger any io events based on the memory mapped IO at `addr`.  If the hypervisor does
    /// in-kernel IO event delivery, this is a no-op.
    fn handle_io_events(&self, addr: IoEventAddress, _data: &[u8]) -> Result<()> {
        match self.ioevents.get(&addr) {
            None => {}
            Some(evt) => {
                evt.signal()?;
            }
        };
        Ok(())
    }

    fn get_pvclock(&self) -> Result<ClockState> {
        Err(Error::new(ENODEV))
    }

    fn set_pvclock(&self, _state: &ClockState) -> Result<()> {
        Err(Error::new(ENODEV))
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
        let (_, region) = regions.get_mut(&slot).ok_or(Error::new(EINVAL))?;

        match region.add_fd_mapping(offset, size, fd, fd_offset, prot) {
            Ok(()) => Ok(()),
            Err(MmapError::SystemCallFailed(e)) => Err(e),
            Err(_) => Err(Error::new(EIO)),
        }
    }

    fn remove_mapping(&mut self, slot: u32, offset: usize, size: usize) -> Result<()> {
        let mut regions = self.mem_regions.lock();
        let (_, region) = regions.get_mut(&slot).ok_or(Error::new(EINVAL))?;

        match region.remove_mapping(offset, size) {
            Ok(()) => Ok(()),
            Err(MmapError::SystemCallFailed(e)) => Err(e),
            Err(_) => Err(Error::new(EIO)),
        }
    }

    fn handle_balloon_event(&mut self, event: BalloonEvent) -> Result<()> {
        match event {
            BalloonEvent::Inflate(m) => self.handle_inflate(m.guest_address, m.size),
            BalloonEvent::Deflate(m) => self.handle_deflate(m.guest_address, m.size),
            BalloonEvent::BalloonTargetReached(_) => Ok(()),
        }
    }

    fn get_guest_phys_addr_bits(&self) -> u8 {
        // Assume the guest physical address size is the same as the host.
        host_phys_addr_bits()
    }
}

impl VmX86_64 for WhpxVm {
    fn get_hypervisor(&self) -> &dyn HypervisorX86_64 {
        &self.whpx
    }

    fn create_vcpu(&self, id: usize) -> Result<Box<dyn VcpuX86_64>> {
        Ok(Box::new(WhpxVcpu::new(
            self.vm_partition.clone(),
            id.try_into().unwrap(),
        )?))
    }

    /// Sets the address of the three-page region in the VM's address space.
    /// This function is only necessary for unrestricted_guest_mode=0, which we do not support for
    /// WHPX.
    fn set_tss_addr(&self, _addr: GuestAddress) -> Result<()> {
        Ok(())
    }

    /// Sets the address of a one-page region in the VM's address space.
    /// This function is only necessary for unrestricted_guest_mode=0, which we do not support for
    /// WHPX.
    fn set_identity_map_addr(&self, _addr: GuestAddress) -> Result<()> {
        Ok(())
    }
}

// NOTE: WHPX Tests need to be run serially as otherwise it barfs unless we map new regions of guest
// memory.
#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use base::EventWaitResult;
    use base::MemoryMappingBuilder;
    use base::SharedMemory;

    use super::*;

    fn new_vm(cpu_count: usize, mem: GuestMemory) -> WhpxVm {
        let whpx = Whpx::new().expect("failed to instantiate whpx");
        let local_apic_supported = Whpx::check_whpx_feature(WhpxFeature::LocalApicEmulation)
            .expect("failed to get whpx features");
        WhpxVm::new(
            &whpx,
            cpu_count,
            mem,
            CpuId::new(0),
            local_apic_supported,
            None,
        )
        .expect("failed to create whpx vm")
    }

    #[test]
    fn create_vm() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        new_vm(cpu_count, mem);
    }

    #[test]
    fn create_vcpu() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        vm.create_vcpu(0).expect("failed to create vcpu");
    }

    #[test]
    fn try_clone() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let _vm_clone = vm.try_clone().expect("failed to clone whpx vm");
    }

    #[test]
    fn send_vm() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        thread::spawn(move || {
            let _vm = vm;
        })
        .join()
        .unwrap();
    }

    #[test]
    fn check_vm_capability() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        assert!(vm.check_capability(VmCap::DirtyLog));
        assert!(!vm.check_capability(VmCap::PvClock));
    }

    #[test]
    fn dirty_log_size() {
        let page_size = pagesize();
        assert_eq!(dirty_log_bitmap_size(0), 0);
        assert_eq!(dirty_log_bitmap_size(page_size), 1);
        assert_eq!(dirty_log_bitmap_size(page_size * 8), 1);
        assert_eq!(dirty_log_bitmap_size(page_size * 8 + 1), 2);
        assert_eq!(dirty_log_bitmap_size(page_size * 100), 13);
    }

    #[test]
    fn register_ioevent() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let mut vm = new_vm(cpu_count, mem);
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
        .expect_err("WHPX should not allow you to register two events for the same address");

        vm.register_ioevent(
            &otherevt,
            IoEventAddress::Mmio(0x1000),
            Datamatch::U8(None),
        )
        .expect_err(
            "WHPX should not allow you to register ioevents with Datamatches other than AnyLength",
        );

        vm.register_ioevent(
            &otherevt,
            IoEventAddress::Mmio(0x1000),
            Datamatch::U32(Some(0xf6)),
        )
        .expect_err(
            "WHPX should not allow you to register ioevents with Datamatches other than AnyLength",
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
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let mut vm = new_vm(cpu_count, mem);
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
            evt.wait_timeout(Duration::from_millis(10))
                .expect("failed to read event"),
            EventWaitResult::TimedOut
        );
        assert_eq!(
            evt2.wait_timeout(Duration::from_millis(10))
                .expect("failed to read event"),
            EventWaitResult::TimedOut
        );
        // Check an mmio address
        vm.handle_io_events(IoEventAddress::Mmio(0x1000), &[])
            .expect("failed to handle_io_events");
        assert_eq!(
            evt.wait_timeout(Duration::from_millis(10))
                .expect("failed to read event"),
            EventWaitResult::TimedOut
        );
        assert_ne!(
            evt2.wait_timeout(Duration::from_millis(10))
                .expect("failed to read event"),
            EventWaitResult::TimedOut
        );

        // Check an address that does not match any registered ioevents
        vm.handle_io_events(IoEventAddress::Pio(0x1001), &[])
            .expect("failed to handle_io_events");
        assert_eq!(
            evt.wait_timeout(Duration::from_millis(10))
                .expect("failed to read event"),
            EventWaitResult::TimedOut
        );
        assert_eq!(
            evt2.wait_timeout(Duration::from_millis(10))
                .expect("failed to read event"),
            EventWaitResult::TimedOut
        );
    }

    #[test]
    fn add_memory_ro() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let mut vm = new_vm(cpu_count, mem);
        let mem_size = 0x1000;
        let shm = SharedMemory::new("test", mem_size as u64).unwrap();
        let mem = MemoryMappingBuilder::new(mem_size)
            .from_shared_memory(&shm)
            .build()
            .unwrap();
        vm.add_memory_region(
            GuestAddress(0x1000),
            Box::new(mem),
            true,
            false,
            MemCacheType::CacheCoherent,
        )
        .unwrap();
    }

    #[test]
    fn remove_memory() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let mut vm = new_vm(cpu_count, mem);
        let mem_size = 0x1000;
        let shm = SharedMemory::new("test", mem_size as u64).unwrap();
        let mem = MemoryMappingBuilder::new(mem_size)
            .from_shared_memory(&shm)
            .build()
            .unwrap();
        let mem_ptr = mem.as_ptr();
        let slot = vm
            .add_memory_region(
                GuestAddress(0x1000),
                Box::new(mem),
                false,
                false,
                MemCacheType::CacheCoherent,
            )
            .unwrap();
        let removed_mem = vm.remove_memory_region(slot).unwrap();
        assert_eq!(removed_mem.size(), mem_size);
        assert_eq!(removed_mem.as_ptr(), mem_ptr);
    }

    #[test]
    fn remove_invalid_memory() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let mut vm = new_vm(cpu_count, mem);
        assert!(vm.remove_memory_region(0).is_err());
    }

    #[test]
    fn overlap_memory() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x10000)]).expect("failed to create guest memory");
        let mut vm = new_vm(cpu_count, mem);
        let mem_size = 0x2000;
        let shm = SharedMemory::new("test", mem_size as u64).unwrap();
        let mem = MemoryMappingBuilder::new(mem_size)
            .from_shared_memory(&shm)
            .build()
            .unwrap();
        assert!(vm
            .add_memory_region(
                GuestAddress(0x2000),
                Box::new(mem),
                false,
                false,
                MemCacheType::CacheCoherent
            )
            .is_err());
    }

    #[test]
    fn sync_memory() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let mut vm = new_vm(cpu_count, mem);
        let mem_size = 0x1000;
        let shm = SharedMemory::new("test", mem_size as u64).unwrap();
        let mem = MemoryMappingBuilder::new(mem_size)
            .from_shared_memory(&shm)
            .build()
            .unwrap();
        let slot = vm
            .add_memory_region(
                GuestAddress(0x10000),
                Box::new(mem),
                false,
                false,
                MemCacheType::CacheCoherent,
            )
            .unwrap();
        vm.msync_memory_region(slot, mem_size - 1, 0).unwrap();
        vm.msync_memory_region(slot, 0, mem_size).unwrap();
        assert!(vm.msync_memory_region(slot, mem_size, 0).is_err());
        assert!(vm.msync_memory_region(slot + 1, mem_size, 0).is_err());
    }
}
