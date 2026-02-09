// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
mod aarch64;

mod gunyah_sys;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::collections::BinaryHeap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::hash_map::Entry;
use std::ffi::CString;
use std::fs;
use std::fs::File;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::mem::size_of;
use std::os::raw::c_ulong;
use std::os::unix::prelude::OsStrExt;
use std::os::unix::fs::FileExt;
use std::os::fd::FromRawFd;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use anyhow::Context;
use base::errno_result;
use base::error;
use base::info;
use base::ioctl;
use base::ioctl_with_ref;
use base::ioctl_with_val;
use base::pagesize;
use base::warn;
use base::Error;
use base::FromRawDescriptor;
use base::MemoryMapping;
use base::MemoryMappingBuilder;
use base::MmapError;
use base::RawDescriptor;
use base::Protection;
use gunyah_sys::*;
use libc::open;
use libc::EFAULT;
use libc::EINVAL;
use libc::EIO;
use libc::ENOENT;
use libc::ENOSPC;
use libc::ENOTSUP;
use libc::EOVERFLOW;
use libc::O_CLOEXEC;
use libc::O_RDWR;
use sync::Mutex;
use vm_memory::MemoryRegionPurpose;
use vm_memory::guest_memory::BackingObject;
use vm_memory::guest_memory::MemoryRegionInformation;

use crate::*;

const AARCH64_PHYS_MEM_START: u64 = 0x80000000;
const VM_SSRDUMP_PATH: &str = "/data/vendor/vm_ssrdump";

pub struct Gunyah {
    gunyah: SafeDescriptor,
}

impl AsRawDescriptor for Gunyah {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.gunyah.as_raw_descriptor()
    }
}

impl Gunyah {
    pub fn new_with_path(device_path: &Path) -> Result<Gunyah> {
        let c_path = CString::new(device_path.as_os_str().as_bytes()).unwrap();
        // SAFETY:
        // Open calls are safe because we give a nul-terminated string and verify the result.
        let ret = unsafe { open(c_path.as_ptr(), O_RDWR | O_CLOEXEC) };
        if ret < 0 {
            return errno_result();
        }
        Ok(Gunyah {
            // SAFETY:
            // Safe because we verify that ret is valid and we own the fd.
            gunyah: unsafe { SafeDescriptor::from_raw_descriptor(ret) },
        })
    }

    pub fn new() -> Result<Gunyah> {
        Gunyah::new_with_path(&PathBuf::from("/dev/gunyah"))
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
        match cap {
            HypervisorCap::UserMemory => true,
            HypervisorCap::ArmPmuV3 => false,
            HypervisorCap::ImmediateExit => true,
            HypervisorCap::StaticSwiotlbAllocationRequired => true,
            HypervisorCap::HypervisorInitializedBootContext => true,
            HypervisorCap::S390UserSigp | HypervisorCap::TscDeadlineTimer => false,
            #[cfg(target_arch = "x86_64")]
            HypervisorCap::Xcrs | HypervisorCap::CalibratedTscLeafRequired => false,
        }
    }
}

unsafe fn android_lend_user_memory_region(
    vm: &SafeDescriptor,
    slot: MemSlot,
    read_only: bool,
    guest_addr: u64,
    memory_size: u64,
    userspace_addr: *mut u8,
) -> Result<()> {
    let mut flags = 0;

    flags |= GH_MEM_ALLOW_READ | GH_MEM_ALLOW_EXEC;
    if !read_only {
        flags |= GH_MEM_ALLOW_WRITE;
    }

    let region = gh_userspace_memory_region {
        label: slot,
        flags,
        guest_phys_addr: guest_addr,
        memory_size,
        userspace_addr: userspace_addr as u64,
    };

    let ret = ioctl_with_ref(vm, GH_VM_ANDROID_LEND_USER_MEM, &region);
    if ret == 0 {
        Ok(())
    } else {
        errno_result()
    }
}

// Wrapper around GH_SET_USER_MEMORY_REGION ioctl, which creates, modifies, or deletes a mapping
// from guest physical to host user pages.
//
// SAFETY:
// Safe when the guest regions are guaranteed not to overlap.
unsafe fn set_user_memory_region(
    vm: &SafeDescriptor,
    slot: MemSlot,
    read_only: bool,
    guest_addr: u64,
    memory_size: u64,
    userspace_addr: *mut u8,
) -> Result<()> {
    let mut flags = 0;

    flags |= GH_MEM_ALLOW_READ | GH_MEM_ALLOW_EXEC;
    if !read_only {
        flags |= GH_MEM_ALLOW_WRITE;
    }

    let region = gh_userspace_memory_region {
        label: slot,
        flags,
        guest_phys_addr: guest_addr,
        memory_size,
        userspace_addr: userspace_addr as u64,
    };

    let ret = ioctl_with_ref(vm, GH_VM_SET_USER_MEM_REGION, &region);
    if ret == 0 {
        Ok(())
    } else {
        errno_result()
    }
}

fn map_cma_region(
    vm: &SafeDescriptor,
    slot: MemSlot,
    lend: bool,
    read_only: bool,
    guest_addr: u64,
    guest_mem_fd: u32,
    size: u64,
    offset: u64,
) -> Result<()> {
    let mut flags = 0;
    flags |= GUNYAH_MEM_ALLOW_READ | GUNYAH_MEM_ALLOW_EXEC;
    if !read_only {
        flags |= GUNYAH_MEM_ALLOW_WRITE;
    }
    if lend {
        flags |= GUNYAH_MEM_FORCE_LEND;
    }
    else {
        flags |= GUNYAH_MEM_FORCE_SHARE;
    }
    let region = gunyah_map_cma_mem_args {
        label: slot,
        guest_addr,
        flags,
        guest_mem_fd,
        offset,
        size,
    };
    // SAFETY: safe because the return value is checked.
    let ret = unsafe { ioctl_with_ref(vm, GH_VM_ANDROID_MAP_CMA_MEM, &region) };
    if ret == 0 {
        Ok(())
    } else {
        errno_result()
    }
}

#[derive(PartialEq, Eq, Hash)]
pub struct GunyahIrqRoute {
    irq: u32,
    level: bool,
}

pub struct GunyahVm {
    gh: Gunyah,
    vm: SafeDescriptor,
    vm_id: Option<u16>,
    pas_id: Option<u32>,
    vm_dump: Option<String>,
    guest_mem: GuestMemory,
    mem_regions: Arc<Mutex<BTreeMap<MemSlot, (Box<dyn MappedRegion>, GuestAddress)>>>,
    /// A min heap of MemSlot numbers that were used and then removed and can now be re-used
    mem_slot_gaps: Arc<Mutex<BinaryHeap<Reverse<MemSlot>>>>,
    routes: Arc<Mutex<HashSet<GunyahIrqRoute>>>,
    hv_cfg: crate::Config,
    minidump_regions: Arc<Mutex<Option<Arc<Mutex<HashMap<String, MinidumpRegion>>>>>>,
    minidump_collected: Arc<AtomicBool>,
    vm_descriptor_closed: Arc<AtomicBool>,
}

impl AsRawDescriptor for GunyahVm {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.vm.as_raw_descriptor()
    }
}

impl GunyahVm {
    pub fn new(gh: &Gunyah, vm_id: Option<u16>, pas_id: Option<u32>, vm_dump: Option<String>, guest_mem: GuestMemory, cfg: Config) -> Result<GunyahVm> {
        // SAFETY:
        // Safe because we know gunyah is a real gunyah fd as this module is the only one that can
        // make Gunyah objects.
        let ret = unsafe { ioctl_with_val(gh, GH_CREATE_VM, 0 as c_ulong) };
        if ret < 0 {
            return errno_result();
        }

        // SAFETY:
        // Safe because we verify that ret is valid and we own the fd.
        let vm_descriptor = unsafe { SafeDescriptor::from_raw_descriptor(ret) };
        for region in guest_mem.regions() {
            let lend = if cfg.protection_type.isolates_memory() {
                match region.options.purpose {
                    MemoryRegionPurpose::Bios => true,
                    MemoryRegionPurpose::GuestMemoryRegion => true,
                    #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
                    MemoryRegionPurpose::ProtectedFirmwareRegion => true,
                    MemoryRegionPurpose::ReservedMemory => true,
                    #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
                    MemoryRegionPurpose::StaticSwiotlbRegion => false,
                }
            } else {
                false
            };
            if region.options.file_backed.is_some() {
                map_cma_region(
                        &vm_descriptor,
                        region.index as MemSlot,
                        lend,
                        !region.options.file_backed.unwrap().writable,
                        region.guest_addr.offset(),
                        region.shm.as_raw_descriptor().try_into().unwrap(),
                        region.size.try_into().unwrap(),
                        region.shm_offset,
                )?;
            } else if lend {
                // SAFETY:
                // Safe because the guest regions are guarnteed not to overlap.
                unsafe {
                    android_lend_user_memory_region(
                        &vm_descriptor,
                        region.index as MemSlot,
                        false,
                        region.guest_addr.offset(),
                        region.size.try_into().unwrap(),
                        region.host_addr as *mut u8,
                    )?;
                }
            } else {
                // SAFETY:
                // Safe because the guest regions are guarnteed not to overlap.
                unsafe {
                    set_user_memory_region(
                        &vm_descriptor,
                        region.index as MemSlot,
                        false,
                        region.guest_addr.offset(),
                        region.size.try_into().unwrap(),
                        region.host_addr as *mut u8,
                    )?;
                }
            }
        }

        Ok(GunyahVm {
            gh: gh.try_clone()?,
            vm: vm_descriptor,
            vm_id,
            pas_id,
            vm_dump,
            guest_mem,
            mem_regions: Arc::new(Mutex::new(BTreeMap::new())),
            mem_slot_gaps: Arc::new(Mutex::new(BinaryHeap::new())),
            routes: Arc::new(Mutex::new(HashSet::new())),
            hv_cfg: cfg,
            minidump_regions: Arc::new(Mutex::new(None)),
            minidump_collected: Arc::new(AtomicBool::new(false)),
            vm_descriptor_closed: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Safely close VM descriptor, ensuring it is only closed once
    /// Returns true if the descriptor was closed by this call, false if already closed
    fn close_vm_descriptor_once(&self) -> bool {
        // Try to atomically set vm_descriptor_closed from false to true
        if self.vm_descriptor_closed.compare_exchange(
            false,
            true,
            Ordering::AcqRel,
            Ordering::Acquire
        ).is_ok() {
            // We successfully changed it from false to true, so we should close it
            let close_result = unsafe { libc::close(self.vm.as_raw_descriptor()) };
            if close_result == 0 {
                info!("[VM] Successfully closed VM descriptor");
                true
            } else {
                let err = std::io::Error::last_os_error();
                warn!("[VM] Failed to close VM descriptor: {}", err);
                // Even if close failed, we mark it as closed to prevent retry
                true
            }
        } else {
            // Already closed by another function
            info!("[VM] VM descriptor already closed, skipping");
            false
        }
    }

    pub fn fulldump(&self) -> Result<()> {
        // Close VM descriptor before reading memory (safe from double-close)
        self.close_vm_descriptor_once();

        for guest_region in self.guest_mem.regions() {
            if guest_region.options.file_backed.is_some() {
                let cma_fd = guest_region.shm.as_raw_descriptor();
                let mut cma_file = unsafe { File::from_raw_fd(cma_fd as i32) };

                let dump_dir = format!(
                    "{}/fulldump_{}",
                    VM_SSRDUMP_PATH,
                    self.vm_id.map(|id| id.to_string()).unwrap_or_else(|| "unknown".to_string())
                );

                // Create directory, if failed log error and return
                if let Err(e) = fs::create_dir_all(&dump_dir) {
                    error!("Failed to create dump directory {}: {}", dump_dir, e);
                    return Err(e.into());
                }

                let mut dump_bin = File::create(format!("{}/fulldump.bin", dump_dir))?;
                let mut dump_info = File::create(format!("{}/dump_info.txt", dump_dir))?;

                // Write more metadata information
                writeln!(dump_info, "VM ID: {}", self.vm_id.unwrap_or(0))?;
                writeln!(dump_info, "PAS ID: {}", self.pas_id.unwrap_or(0))?;
                writeln!(dump_info, "IPA Address: 0x{:X}", AARCH64_PHYS_MEM_START)?;
                writeln!(dump_info, "Memory Size: 0x{:X}", guest_region.size)?;
                writeln!(dump_info, "Dump Path: {}", dump_dir)?;

                let mut buffer = [0u8; 4096];
                loop {
                    let bytes_read = cma_file.read(&mut buffer)?;
                    if bytes_read == 0 {
                        break;
                    }
                    dump_bin.write_all(&buffer[..bytes_read])?;
                }

                // Prevent closing the fd
                std::mem::forget(cma_file);

                // Break after processing the first file-backed region
                break;
            }
        }

        Ok(())
    }

    /// Collect minidump when VM crashes
    /// Uses regions registered via virtio minidump device
    pub fn minidump(&self) -> Result<()> {

        if self.minidump_collected.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            info!("[MINIDUMP] Minidump already collected (or in-progress); skipping ");
            return Ok(());
        }

        let regions_arc = match self.minidump_regions.lock().clone() {
            Some(r) => r,
            None => {
                warn!("[MINIDUMP] No minidump regions registered ");
                self.minidump_collected.store(false, Ordering::Release);
                return Err(Error::new(ENOENT));
            }
        };

        let regions = regions_arc.lock();
        if regions.is_empty() {
            warn!("[MINIDUMP] No regions to dump ");
            self.minidump_collected.store(false, Ordering::Release);
            return Ok(());
        }

        let regions_snapshot: Vec<MinidumpRegion> = regions.values().cloned().collect();
        drop(regions);

        let dump_dir = format!("{}/minidump_{}", VM_SSRDUMP_PATH, self.vm_id.map(|id| id.to_string()).unwrap_or_else(|| "unknown".to_string()));
        fs::create_dir_all(&dump_dir)?;

        // Close VM descriptor before reading memory
        self.close_vm_descriptor_once();

        let mut cma_file_opt: Option<File> = None;
        for guest_region in self.guest_mem.regions() {
            if guest_region.options.file_backed.is_some() {
                let cma_fd = guest_region.shm.as_raw_descriptor();
                let cma_file = unsafe { File::from_raw_fd(cma_fd as i32) };
                cma_file_opt = Some(cma_file);
                break;
            }
        }

        let mut cma_file = match cma_file_opt {
            Some(f) => f,
            None => {
                error!("[MINIDUMP] No file-backed guest memory region found");
                self.minidump_collected.store(false, Ordering::Release);
                return Err(Error::new(ENOENT));
            }
        };

        for region in regions_snapshot.iter() {
            let safe_name = region.name.trim().replace('/', "_");
            let file_path = format!("{}/{}.bin", dump_dir, safe_name);
            info!("[MINIDUMP] Dumping region '{}': phys_addr=0x{:x}, size=0x{:x}", safe_name, region.phys_addr, region.size);

            let offset = region.phys_addr.saturating_sub(AARCH64_PHYS_MEM_START);
            match cma_file.seek(SeekFrom::Start(offset)) {
                Ok(pos) => { /* seek succeeded */ },
                Err(e) => {
                    warn!("[MINIDUMP] Failed to seek to offset 0x{:x} for region '{}': {}", offset, safe_name, e);
                    continue;
                }
            }

            let size: usize = region.size.try_into().unwrap_or(0);
            let mut buffer = vec![0u8; size];
            match cma_file.read(&mut buffer) {
                Ok(bytes_read) => {
                    if bytes_read > 0 {
                        match File::create(&file_path) {
                            Ok(mut output_file) => {
                                if let Err(e) = output_file.write_all(&buffer[..bytes_read]) {
                                    warn!("[MINIDUMP] Failed to write region '{}': {:?}", safe_name, e);
                                    continue;
                                }
                                info!("[MINIDUMP] Successfully dumped {} ({} bytes)", file_path, bytes_read);
                            }
                            Err(e) => {
                                warn!("[MINIDUMP] Failed to create output file for '{}': {:?}", safe_name, e);
                                continue;
                            }
                        }
                    } else {
                        warn!("[MINIDUMP] Zero bytes read for region '{}'", safe_name);
                        continue;
                    }
                }
                Err(e) => {
                    warn!("[MINIDUMP] Failed to read region '{}': {:?}; skipping", safe_name, e);
                    continue;
                }
            }
        }

        std::mem::forget(cma_file);
        info!("[MINIDUMP] Minidump collection completed successfully");
        Ok(())
    }

    pub fn set_vm_auth_type_to_qcom_trusted_vm(&self, payload_start: GuestAddress, payload_size: u64) -> Result<()> {
        let gunyah_qtvm_auth_arg = gunyah_qtvm_auth_arg {
            vm_id: self.vm_id.expect("VM ID not specified for a QTVM"),
            pas_id: self.pas_id.expect("PAS ID not specified for a QTVM"),
            // QTVMs have the metadata needed for authentication at the start of the guest addrspace.
            guest_phys_addr: payload_start.offset(),
            size: payload_size,
        };
        let gunyah_auth_desc = gunyah_auth_desc {
            type_: gunyah_auth_type_GUNYAH_QCOM_TRUSTED_VM_TYPE,
            arg_size: size_of::<gunyah_qtvm_auth_arg>() as u32,
            arg: &gunyah_qtvm_auth_arg as *const gunyah_qtvm_auth_arg as u64,
        };
        // SAFETY: safe because the return value is checked.
        let ret = unsafe { ioctl_with_ref(self, GH_VM_ANDROID_SET_AUTH_TYPE, &gunyah_auth_desc) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    fn create_vcpu(&self, id: usize) -> Result<GunyahVcpu> {
        let gh_fn_vcpu_arg = gh_fn_vcpu_arg {
            id: id.try_into().unwrap(),
        };

        let function_desc = gh_fn_desc {
            type_: GH_FN_VCPU,
            arg_size: size_of::<gh_fn_vcpu_arg>() as u32,
            // Safe because kernel is expecting pointer with non-zero arg_size
            arg: &gh_fn_vcpu_arg as *const gh_fn_vcpu_arg as u64,
        };

        // SAFETY:
        // Safe because we know that our file is a VM fd and we verify the return result.
        let fd = unsafe { ioctl_with_ref(self, GH_VM_ADD_FUNCTION, &function_desc) };
        if fd < 0 {
            return errno_result();
        }

        // SAFETY:
        // Wrap the vcpu now in case the following ? returns early. This is safe because we verified
        // the value of the fd and we own the fd.
        let vcpu = unsafe { File::from_raw_descriptor(fd) };

        // SAFETY:
        // Safe because we know this is a Gunyah VCPU
        let res = unsafe { ioctl(&vcpu, GH_VCPU_MMAP_SIZE) };
        if res < 0 {
            return errno_result();
        }
        let run_mmap_size = res as usize;

        let run_mmap = MemoryMappingBuilder::new(run_mmap_size)
            .from_file(&vcpu)
            .build()
            .map_err(|_| Error::new(ENOSPC))?;

        Ok(GunyahVcpu {
            vm: self.vm.try_clone()?,
            vcpu,
            id,
            run_mmap: Arc::new(run_mmap),
            vm_ref: Some(Arc::new(self.try_clone()?)),
        })
    }

    pub fn register_irqfd(&self, label: u32, evt: &Event, level: bool) -> Result<()> {
        let gh_fn_irqfd_arg = gh_fn_irqfd_arg {
            fd: evt.as_raw_descriptor() as u32,
            label,
            flags: if level { GH_IRQFD_LEVEL } else { 0 },
            ..Default::default()
        };

        let function_desc = gh_fn_desc {
            type_: GH_FN_IRQFD,
            arg_size: size_of::<gh_fn_irqfd_arg>() as u32,
            // SAFETY:
            // Safe because kernel is expecting pointer with non-zero arg_size
            arg: &gh_fn_irqfd_arg as *const gh_fn_irqfd_arg as u64,
        };

        // SAFETY: safe because the return value is checked.
        let ret = unsafe { ioctl_with_ref(self, GH_VM_ADD_FUNCTION, &function_desc) };
        if ret == 0 {
            self.routes
                .lock()
                .insert(GunyahIrqRoute { irq: label, level });
            Ok(())
        } else {
            errno_result()
        }
    }

    pub fn unregister_irqfd(&self, label: u32, _evt: &Event) -> Result<()> {
        let gh_fn_irqfd_arg = gh_fn_irqfd_arg {
            label,
            ..Default::default()
        };

        let function_desc = gh_fn_desc {
            type_: GH_FN_IRQFD,
            arg_size: size_of::<gh_fn_irqfd_arg>() as u32,
            // Safe because kernel is expecting pointer with non-zero arg_size
            arg: &gh_fn_irqfd_arg as *const gh_fn_irqfd_arg as u64,
        };

        // SAFETY: safe because memory is not modified and the return value is checked.
        let ret = unsafe { ioctl_with_ref(self, GH_VM_REMOVE_FUNCTION, &function_desc) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    pub fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        Ok(GunyahVm {
            gh: self.gh.try_clone()?,
            vm: self.vm.try_clone()?,
            vm_id: self.vm_id,
            pas_id: self.pas_id,
            vm_dump: self.vm_dump.clone(),
            guest_mem: self.guest_mem.clone(),
            mem_regions: self.mem_regions.clone(),
            mem_slot_gaps: self.mem_slot_gaps.clone(),
            routes: self.routes.clone(),
            hv_cfg: self.hv_cfg,
            minidump_regions: self.minidump_regions.clone(),
            minidump_collected: self.minidump_collected.clone(),
            vm_descriptor_closed: self.vm_descriptor_closed.clone(),
        })
    }

    fn set_dtb_config(&self, fdt_address: GuestAddress, fdt_size: usize) -> Result<()> {
        let dtb_config = gh_vm_dtb_config {
            guest_phys_addr: fdt_address.offset(),
            size: fdt_size.try_into().unwrap(),
        };

        // SAFETY:
        // Safe because we know this is a Gunyah VM
        let ret = unsafe { ioctl_with_ref(self, GH_VM_SET_DTB_CONFIG, &dtb_config) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    fn set_protected_vm_firmware_ipa(&self, fw_addr: GuestAddress, fw_size: u64) -> Result<()> {
        let fw_config = gh_vm_firmware_config {
            guest_phys_addr: fw_addr.offset(),
            size: fw_size,
        };

        // SAFETY:
        // Safe because we know this is a Gunyah VM
        let ret = unsafe { ioctl_with_ref(self, GH_VM_ANDROID_SET_FW_CONFIG, &fw_config) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    fn set_boot_pc(&self, value: u64) -> Result<()> {
        self.set_boot_context(gh_vm_boot_context_reg::REG_SET_PC, 0, value)
    }

    // Sets the boot context for the Gunyah VM by specifying the register type, index, and value.
    fn set_boot_context(
        &self,
        reg_type: gh_vm_boot_context_reg::Type,
        reg_idx: u8,
        value: u64,
    ) -> Result<()> {
        let reg_id = boot_context_reg_id(reg_type, reg_idx);
        let boot_context = gh_vm_boot_context {
            reg: reg_id,
            value,
            ..Default::default()
        };

        // SAFETY: Safe because we ensure the boot_context is correctly initialized
        // and the ioctl call is checked.
        let ret = unsafe { ioctl_with_ref(self, GH_VM_SET_BOOT_CONTEXT, &boot_context) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    fn start(&self) -> Result<()> {
        // SAFETY: safe because memory is not modified and the return value is checked.
        let ret = unsafe { ioctl(self, GH_VM_START) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    fn handle_inflate(&self, guest_addr: GuestAddress, size: u64) -> Result<()> {
        let range = gunyah_address_range {
            guest_phys_addr: guest_addr.0,
            size,
        };

        // SAFETY: Safe because we know this is a Gunyah VM
        let ret = unsafe { ioctl_with_ref(self, GH_VM_RECLAIM_REGION, &range) };
        if ret != 0 {
            warn!("Gunyah failed to reclaim {:?}", range);
            return errno_result();
        }

        match self.guest_mem.remove_range(guest_addr, size) {
            Ok(_) => Ok(()),
            Err(vm_memory::Error::MemoryAccess(_, MmapError::SystemCallFailed(e))) => Err(e),
            Err(_) => Err(Error::new(EIO)),
        }
    }
}

impl Vm for GunyahVm {
    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        Ok(GunyahVm {
            gh: self.gh.try_clone()?,
            vm: self.vm.try_clone()?,
            vm_id: self.vm_id,
            pas_id: self.pas_id,
            vm_dump: self.vm_dump.clone(),
            guest_mem: self.guest_mem.clone(),
            mem_regions: self.mem_regions.clone(),
            mem_slot_gaps: self.mem_slot_gaps.clone(),
            routes: self.routes.clone(),
            hv_cfg: self.hv_cfg,
            minidump_regions: self.minidump_regions.clone(),
            minidump_collected: self.minidump_collected.clone(),
            vm_descriptor_closed: self.vm_descriptor_closed.clone(),
        })
    }

    fn try_clone_descriptor(&self) -> Result<SafeDescriptor> {
        error!("try_clone_descriptor hasn't been tested on gunyah, returning -ENOTSUP");
        Err(Error::new(ENOTSUP))
    }

    fn hypervisor_kind(&self) -> HypervisorKind {
        HypervisorKind::Gunyah
    }

    fn check_capability(&self, c: VmCap) -> bool {
        match c {
            VmCap::DirtyLog => false,
            // Strictly speaking, Gunyah supports pvclock, but Gunyah takes care
            // of it and crosvm doesn't need to do anything for it
            VmCap::PvClock => false,
            VmCap::Protected => true,
            VmCap::EarlyInitCpuid => false,
            #[cfg(target_arch = "x86_64")]
            VmCap::BusLockDetect => false,
            VmCap::ReadOnlyMemoryRegion => false,
            VmCap::MemNoncoherentDma => false,
            #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
            VmCap::Sve => false,
        }
    }

    fn get_guest_phys_addr_bits(&self) -> u8 {
        40
    }

    fn get_memory(&self) -> &GuestMemory {
        &self.guest_mem
    }

    fn add_memory_region(
        &mut self,
        guest_addr: GuestAddress,
        mem_region: Box<dyn MappedRegion>,
        read_only: bool,
        _log_dirty_pages: bool,
        _cache: MemCacheType,
    ) -> Result<MemSlot> {
        let pgsz = pagesize() as u64;
        // Gunyah require to set the user memory region with page size aligned size. Safe to extend
        // the mem.size() to be page size aligned because the mmap will round up the size to be
        // page size aligned if it is not.
        let size = (mem_region.size() as u64 + pgsz - 1) / pgsz * pgsz;
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

        // SAFETY: safe because memory is not modified and the return value is checked.
        let res = unsafe {
            set_user_memory_region(
                &self.vm,
                slot,
                read_only,
                guest_addr.offset(),
                size,
                mem_region.as_ptr(),
            )
        };

        if let Err(e) = res {
            gaps.push(Reverse(slot));
            return Err(e);
        }
        regions.insert(slot, (mem_region, guest_addr));
        Ok(slot)
    }

    fn msync_memory_region(&mut self, slot: MemSlot, offset: usize, size: usize) -> Result<()> {
        let mut regions = self.mem_regions.lock();
        let (mem, _) = regions.get_mut(&slot).ok_or_else(|| Error::new(ENOENT))?;

        mem.msync(offset, size).map_err(|err| match err {
            MmapError::InvalidAddress => Error::new(EFAULT),
            MmapError::NotPageAligned => Error::new(EINVAL),
            MmapError::SystemCallFailed(e) => e,
            _ => Error::new(EIO),
        })
    }

    fn madvise_pageout_memory_region(
        &mut self,
        _slot: MemSlot,
        _offset: usize,
        _size: usize,
    ) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    fn madvise_remove_memory_region(
        &mut self,
        _slot: MemSlot,
        _offset: usize,
        _size: usize,
    ) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    fn remove_memory_region(&mut self, _slot: MemSlot) -> Result<Box<dyn MappedRegion>> {
        unimplemented!()
    }

    fn create_device(&self, _kind: DeviceKind) -> Result<SafeDescriptor> {
        unimplemented!()
    }

    fn get_dirty_log(&self, _slot: MemSlot, _dirty_log: &mut [u8]) -> Result<()> {
        unimplemented!()
    }

    fn register_ioevent(
        &mut self,
        evt: &Event,
        addr: IoEventAddress,
        datamatch: Datamatch,
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
                Some(u) => (true, u, 8),
                None => (false, 0, 8),
            },
        };

        let mut flags = 0;
        if do_datamatch {
            flags |= 1 << GH_IOEVENTFD_DATAMATCH;
        }

        let maddr = if let IoEventAddress::Mmio(maddr) = addr {
            maddr
        } else {
            todo!()
        };

        let gh_fn_ioeventfd_arg = gh_fn_ioeventfd_arg {
            fd: evt.as_raw_descriptor(),
            datamatch: datamatch_value,
            len: datamatch_len,
            addr: maddr,
            flags,
            ..Default::default()
        };

        let function_desc = gh_fn_desc {
            type_: GH_FN_IOEVENTFD,
            arg_size: size_of::<gh_fn_ioeventfd_arg>() as u32,
            arg: &gh_fn_ioeventfd_arg as *const gh_fn_ioeventfd_arg as u64,
        };

        // SAFETY: safe because memory is not modified and the return value is checked.
        let ret = unsafe { ioctl_with_ref(self, GH_VM_ADD_FUNCTION, &function_desc) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    fn unregister_ioevent(
        &mut self,
        _evt: &Event,
        addr: IoEventAddress,
        _datamatch: Datamatch,
    ) -> Result<()> {
        let maddr = if let IoEventAddress::Mmio(maddr) = addr {
            maddr
        } else {
            todo!()
        };

        let gh_fn_ioeventfd_arg = gh_fn_ioeventfd_arg {
            addr: maddr,
            ..Default::default()
        };

        let function_desc = gh_fn_desc {
            type_: GH_FN_IOEVENTFD,
            arg_size: size_of::<gh_fn_ioeventfd_arg>() as u32,
            arg: &gh_fn_ioeventfd_arg as *const gh_fn_ioeventfd_arg as u64,
        };

        // SAFETY: safe because memory is not modified and the return value is checked.
        let ret = unsafe { ioctl_with_ref(self, GH_VM_REMOVE_FUNCTION, &function_desc) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    fn handle_io_events(&self, _addr: IoEventAddress, _data: &[u8]) -> Result<()> {
        Ok(())
    }

    fn get_pvclock(&self) -> Result<ClockState> {
        unimplemented!()
    }

    fn set_pvclock(&self, _state: &ClockState) -> Result<()> {
        unimplemented!()
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
        let (region, _) = regions.get_mut(&slot).ok_or_else(|| Error::new(EINVAL))?;

        match region.add_fd_mapping(offset, size, fd, fd_offset, prot) {
            Ok(()) => Ok(()),
            Err(MmapError::SystemCallFailed(e)) => Err(e),
            Err(_) => Err(Error::new(EIO)),
        }
    }

    fn remove_mapping(&mut self, slot: u32, offset: usize, size: usize) -> Result<()> {
        let mut regions = self.mem_regions.lock();
        let (region, _) = regions.get_mut(&slot).ok_or_else(|| Error::new(EINVAL))?;

        match region.remove_mapping(offset, size) {
            Ok(()) => Ok(()),
            Err(MmapError::SystemCallFailed(e)) => Err(e),
            Err(_) => Err(Error::new(EIO)),
        }
    }

    fn handle_balloon_event(&mut self, event: BalloonEvent) -> Result<()> {
        match event {
            BalloonEvent::Inflate(m) => self.handle_inflate(m.guest_address, m.size),
            BalloonEvent::Deflate(m) => Ok(()),
            BalloonEvent::BalloonTargetReached(_) => Ok(()),
        }
    }

    fn set_minidump_regions(&mut self, regions: Arc<Mutex<HashMap<String, MinidumpRegion>>>) -> Result<()> {
        *self.minidump_regions.lock() = Some(regions);
        Ok(())
    }
}

const GH_RM_EXIT_TYPE_VM_EXIT: u16 = 0;
const GH_RM_EXIT_TYPE_PSCI_POWER_OFF: u16 = 1;
const GH_RM_EXIT_TYPE_PSCI_SYSTEM_RESET: u16 = 2;
const GH_RM_EXIT_TYPE_PSCI_SYSTEM_RESET2: u16 = 3;
const GH_RM_EXIT_TYPE_WDT_BITE: u16 = 4;
const GH_RM_EXIT_TYPE_HYP_ERROR: u16 = 5;
const GH_RM_EXIT_TYPE_ASYNC_EXT_ABORT: u16 = 6;
const GH_RM_EXIT_TYPE_VM_FORCE_STOPPED: u16 = 7;

pub struct GunyahVcpu {
    vm: SafeDescriptor,
    vcpu: File,
    id: usize,
    run_mmap: Arc<MemoryMapping>,
    vm_ref: Option<Arc<GunyahVm>>,
}

struct GunyahVcpuSignalHandle {
    run_mmap: Arc<MemoryMapping>,
}

impl VcpuSignalHandleInner for GunyahVcpuSignalHandle {
    fn signal_immediate_exit(&self) {
        // SAFETY: we ensure `run_mmap` is a valid mapping of `kvm_run` at creation time, and the
        // `Arc` ensures the mapping still exists while we hold a reference to it.
        unsafe {
            let run = self.run_mmap.as_ptr() as *mut gh_vcpu_run;
            (*run).immediate_exit = 1;
        }
    }
}

impl AsRawDescriptor for GunyahVcpu {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.vcpu.as_raw_descriptor()
    }
}

impl Vcpu for GunyahVcpu {
    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        let vcpu = self.vcpu.try_clone()?;

        Ok(GunyahVcpu {
            vm: self.vm.try_clone()?,
            vcpu,
            id: self.id,
            run_mmap: self.run_mmap.clone(),
            vm_ref: self.vm_ref.clone(),
        })
    }

    fn as_vcpu(&self) -> &dyn Vcpu {
        self
    }

    fn run(&mut self) -> Result<VcpuExit> {
        // SAFETY:
        // Safe because we know our file is a VCPU fd and we verify the return result.
        let ret = unsafe { ioctl(self, GH_VCPU_RUN) };
        if ret != 0 {
            return errno_result();
        }

        // SAFETY:
        // Safe because we know we mapped enough memory to hold the gh_vcpu_run struct
        // because the kernel told us how large it is.
        let run = unsafe { &mut *(self.run_mmap.as_ptr() as *mut gh_vcpu_run) };
        match run.exit_reason {
            GH_VCPU_EXIT_MMIO => Ok(VcpuExit::Mmio),
            GH_VCPU_EXIT_STATUS => {
                // SAFETY:
                // Safe because the exit_reason (which comes from the kernel) told us which
                // union field to use.
                let status = unsafe { &mut run.__bindgen_anon_1.status };
                match status.status {
                    GH_VM_STATUS_GH_VM_STATUS_LOAD_FAILED => Ok(VcpuExit::FailEntry {
                        hardware_entry_failure_reason: 0,
                    }),
                    GH_VM_STATUS_GH_VM_STATUS_CRASHED => Ok(VcpuExit::SystemEventCrash),
                    GH_VM_STATUS_GH_VM_STATUS_EXITED => {
                        info!("exit type {}", status.exit_info.type_);
                        match status.exit_info.type_ {
                            GH_RM_EXIT_TYPE_VM_EXIT => Ok(VcpuExit::SystemEventShutdown),
                            GH_RM_EXIT_TYPE_PSCI_POWER_OFF => Ok(VcpuExit::SystemEventShutdown),
                            GH_RM_EXIT_TYPE_PSCI_SYSTEM_RESET => Ok(VcpuExit::SystemEventReset),
                            GH_RM_EXIT_TYPE_PSCI_SYSTEM_RESET2 => Ok(VcpuExit::SystemEventReset),
                            GH_RM_EXIT_TYPE_WDT_BITE => Ok(VcpuExit::SystemEventCrash),
                            GH_RM_EXIT_TYPE_HYP_ERROR => Ok(VcpuExit::SystemEventCrash),
                            GH_RM_EXIT_TYPE_ASYNC_EXT_ABORT => Ok(VcpuExit::SystemEventCrash),
                            GH_RM_EXIT_TYPE_VM_FORCE_STOPPED => Ok(VcpuExit::SystemEventShutdown),
                            r => {
                                warn!("Unknown exit type: {}", r);
                                Err(Error::new(EINVAL))
                            }
                        }
                    }
                    r => {
                        warn!("Unknown vm status: {}", r);
                        Err(Error::new(EINVAL))
                    }
                }
            }
            r => {
                warn!("unknown gh exit reason: {}", r);
                Err(Error::new(EINVAL))
            }
        }
    }

    fn id(&self) -> usize {
        self.id
    }

    fn set_immediate_exit(&self, exit: bool) {
        // SAFETY:
        // Safe because we know we mapped enough memory to hold the kvm_run struct because the
        // kernel told us how large it was. The pointer is page aligned so casting to a different
        // type is well defined, hence the clippy allow attribute.
        let run = unsafe { &mut *(self.run_mmap.as_ptr() as *mut gh_vcpu_run) };
        run.immediate_exit = exit.into();
    }

    fn signal_handle(&self) -> VcpuSignalHandle {
        VcpuSignalHandle {
            inner: Box::new(GunyahVcpuSignalHandle {
                run_mmap: self.run_mmap.clone(),
            }),
        }
    }

    fn handle_mmio(&self, handle_fn: &mut dyn FnMut(IoParams) -> Result<()>) -> Result<()> {
        // SAFETY:
        // Safe because we know we mapped enough memory to hold the gh_vcpu_run struct because the
        // kernel told us how large it was. The pointer is page aligned so casting to a different
        // type is well defined
        let run = unsafe { &mut *(self.run_mmap.as_ptr() as *mut gh_vcpu_run) };
        // Verify that the handler is called in the right context.
        assert!(run.exit_reason == GH_VCPU_EXIT_MMIO);
        // SAFETY:
        // Safe because the exit_reason (which comes from the kernel) told us which
        // union field to use.
        let mmio = unsafe { &mut run.__bindgen_anon_1.mmio };
        let address = mmio.phys_addr;
        let data = &mut mmio.data[..mmio.len as usize];
        if mmio.is_write != 0 {
            handle_fn(IoParams {
                address,
                operation: IoOperation::Write(data),
            })
        } else {
            handle_fn(IoParams {
                address,
                operation: IoOperation::Read(data),
            })
        }
    }

    fn handle_io(&self, _handle_fn: &mut dyn FnMut(IoParams)) -> Result<()> {
        unreachable!()
    }

    fn on_suspend(&self) -> Result<()> {
        Ok(())
    }

    unsafe fn enable_raw_capability(&self, _cap: u32, _args: &[u64; 4]) -> Result<()> {
        unimplemented!()
    }
}
