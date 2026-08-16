// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::collections::BinaryHeap;
use std::collections::HashMap;
use std::ffi::c_char;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;

use applevisor_sys::hv_error_t;
use applevisor_sys::hv_ipa_t;
use applevisor_sys::hv_memory_flags_t;
use applevisor_sys::hv_return_t;
use applevisor_sys::hv_vm_config_create;
use applevisor_sys::hv_vm_config_set_ipa_size;
use applevisor_sys::hv_vm_create;
use applevisor_sys::hv_vm_destroy;
use applevisor_sys::hv_vm_map;
use applevisor_sys::hv_vm_protect;
use applevisor_sys::hv_vm_unmap;
use applevisor_sys::os_release;
use applevisor_sys::HV_MEMORY_EXEC;
use applevisor_sys::HV_MEMORY_READ;
use applevisor_sys::HV_MEMORY_WRITE;
use base::pagesize;
use base::warn;
use base::AsRawDescriptor;
use base::Error;
use base::Event;
use base::MappedRegion;
use base::MmapError;
use base::Protection;
use base::Result;
use base::SafeDescriptor;
use cros_fdt::Fdt;
use libc::EEXIST;
use libc::EFAULT;
use libc::EINVAL;
use libc::EIO;
use libc::ENOENT;
use libc::ENOSPC;
use libc::ENOSYS;
use libc::ENXIO;
use libc::EOVERFLOW;
use libc::RTLD_LAZY;
use libc::RTLD_LOCAL;
use vm_memory::GuestAddress;
use vm_memory::GuestMemory;

use super::vcpu::HvfVcpu;
use super::vcpu::VcpuPowerControl;
use crate::BalloonEvent;
use crate::ClockState;
use crate::Config;
use crate::Datamatch;
use crate::DeviceKind;
use crate::Hypervisor;
use crate::HypervisorCap;
use crate::IoEventAddress;
use crate::MemCacheType;
use crate::MemSlot;
use crate::ProtectionType;
use crate::Vm;
use crate::VmAArch64;
use crate::VmCap;

/// Same layout as devices `irqchip/kvm/aarch64` (guest GICv3 MMIO).
const AARCH64_AXI_BASE: u64 = 0x4000_0000;
const AARCH64_GIC_DIST_SIZE: u64 = 0x1_0000;
const AARCH64_GIC_CPUI_SIZE: u64 = 0x2_0000;
const AARCH64_GIC_REDIST_SIZE: u64 = 0x2_0000;

const AARCH64_GIC_DIST_BASE: u64 = AARCH64_AXI_BASE - AARCH64_GIC_DIST_SIZE;
const AARCH64_GIC_CPUI_BASE: u64 = AARCH64_GIC_DIST_BASE - AARCH64_GIC_CPUI_SIZE;

type HvGicConfigCreate = unsafe extern "C" fn() -> *mut c_void;
type HvGicConfigSetDistributorBase = unsafe extern "C" fn(*mut c_void, u64) -> hv_return_t;
type HvGicConfigSetRedistributorBase = unsafe extern "C" fn(*mut c_void, u64) -> hv_return_t;
type HvGicCreate = unsafe extern "C" fn(*mut c_void) -> hv_return_t;
type HvGicGetDistributorSize = unsafe extern "C" fn(*mut u64) -> hv_return_t;
type HvGicGetDistributorReg = unsafe extern "C" fn(u32, *mut u64) -> hv_return_t;
type HvGicGetRedistributorRegionSize = unsafe extern "C" fn(*mut u64) -> hv_return_t;
type HvGicSendMsi = unsafe extern "C" fn(hv_ipa_t, u32) -> hv_return_t;
type HvGicSetSpi = unsafe extern "C" fn(u32, bool) -> hv_return_t;

struct HvfGicSymbols {
    _framework: *mut c_void,
    config_create: HvGicConfigCreate,
    config_set_distributor_base: HvGicConfigSetDistributorBase,
    config_set_redistributor_base: HvGicConfigSetRedistributorBase,
    create: HvGicCreate,
    get_distributor_reg: HvGicGetDistributorReg,
    get_distributor_size: HvGicGetDistributorSize,
    get_redistributor_region_size: HvGicGetRedistributorRegionSize,
    send_msi: HvGicSendMsi,
    set_spi: HvGicSetSpi,
}

unsafe impl Send for HvfGicSymbols {}
unsafe impl Sync for HvfGicSymbols {}

fn hvf_gic_symbols() -> Option<&'static HvfGicSymbols> {
    static SYMBOLS: once_cell::sync::OnceCell<Option<HvfGicSymbols>> =
        once_cell::sync::OnceCell::new();
    SYMBOLS.get_or_init(load_hvf_gic_symbols).as_ref()
}

fn load_hvf_gic_symbols() -> Option<HvfGicSymbols> {
    unsafe {
        let framework = libc::dlopen(
            b"/System/Library/Frameworks/Hypervisor.framework/Hypervisor\0"
                .as_ptr()
                .cast::<c_char>(),
            RTLD_LAZY | RTLD_LOCAL,
        );
        if framework.is_null() {
            return None;
        }

        Some(HvfGicSymbols {
            _framework: framework,
            config_create: load_hvf_gic_symbol(framework, b"hv_gic_config_create\0")?,
            config_set_distributor_base: load_hvf_gic_symbol(
                framework,
                b"hv_gic_config_set_distributor_base\0",
            )?,
            config_set_redistributor_base: load_hvf_gic_symbol(
                framework,
                b"hv_gic_config_set_redistributor_base\0",
            )?,
            create: load_hvf_gic_symbol(framework, b"hv_gic_create\0")?,
            get_distributor_size: load_hvf_gic_symbol(framework, b"hv_gic_get_distributor_size\0")?,
            get_distributor_reg: load_hvf_gic_symbol(framework, b"hv_gic_get_distributor_reg\0")?,
            get_redistributor_region_size: load_hvf_gic_symbol(
                framework,
                b"hv_gic_get_redistributor_region_size\0",
            )?,
            send_msi: load_hvf_gic_symbol(framework, b"hv_gic_send_msi\0")?,
            set_spi: load_hvf_gic_symbol(framework, b"hv_gic_set_spi\0")?,
        })
    }
}

unsafe fn load_hvf_gic_symbol<T>(framework: *mut c_void, name: &[u8]) -> Option<T>
where
    T: Copy,
{
    let symbol = libc::dlsym(framework, name.as_ptr().cast::<c_char>());
    if symbol.is_null() {
        None
    } else {
        Some(std::mem::transmute_copy(&symbol))
    }
}

pub(crate) fn check_hv_quiet(r: hv_return_t) -> Result<()> {
    if r == hv_error_t::HV_SUCCESS as hv_return_t {
        Ok(())
    } else {
        Err(Error::new(EIO))
    }
}

pub(crate) fn check_hv(r: hv_return_t) -> Result<()> {
    if r != hv_error_t::HV_SUCCESS as hv_return_t {
        eprintln!("HVF call failed with code {}", r);
    }
    check_hv_quiet(r)
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    mutex.lock().map_err(|_| Error::new(EIO))
}

/// Bitmap size for dirty logging (same formula as KVM).
pub fn dirty_log_bitmap_size(size: usize) -> usize {
    let page_size = pagesize();
    (((size + page_size - 1) / page_size) + 7) / 8
}

/// Ensures `hv_vm_destroy` runs once when the last `HvfVm` clone drops.
struct HvfVmLife {
    _p: (),
}

impl Drop for HvfVmLife {
    fn drop(&mut self) {
        // SAFETY: Hypervisor.framework API; destroys the per-process VM.
        unsafe {
            let _ = hv_vm_destroy();
        }
    }
}

/// Lightweight handle for [`Hypervisor`] trait parity with KVM (`Kvm` does not own the VM fd).
#[derive(Clone)]
pub struct HvfHypervisor;

impl HvfHypervisor {
    pub fn new() -> Result<Self> {
        Ok(HvfHypervisor)
    }
}

impl Hypervisor for HvfHypervisor {
    fn try_clone(&self) -> Result<Self> {
        Ok(HvfHypervisor)
    }

    fn check_capability(&self, cap: HypervisorCap) -> bool {
        matches!(
            cap,
            HypervisorCap::UserMemory
                | HypervisorCap::ImmediateExit
                | HypervisorCap::VcpuRunThreadLocal
        )
    }
}

pub struct HvfVm {
    _hypervisor: HvfHypervisor,
    _vm_life: Arc<HvfVmLife>,
    guest_mem: GuestMemory,
    mem_regions: Arc<Mutex<BTreeMap<MemSlot, (GuestAddress, u64, Box<dyn MappedRegion>)>>>,
    mem_slot_gaps: Arc<Mutex<BinaryHeap<Reverse<MemSlot>>>>,
    ipa_bits: u8,
    gic_done: AtomicBool,
    ioevents: Arc<Mutex<HashMap<IoEventAddress, Event>>>,
    power_control: Arc<VcpuPowerControl>,
}

impl HvfVm {
    pub fn new(_hv: &HvfHypervisor, guest_mem: GuestMemory, cfg: Config) -> Result<HvfVm> {
        if cfg.protection_type != ProtectionType::Unprotected {
            return Err(Error::new(ENOSYS));
        }
        #[cfg(target_arch = "aarch64")]
        if cfg.mte {
            return Err(Error::new(ENOSYS));
        }

        let vm_life = Arc::new(HvfVmLife { _p: () });
        if let Ok(mut entry_bytes) =
            guest_mem.read_obj_from_addr::<[u8; 16]>(GuestAddress(0x8000_0000))
        {
            warn!("HVF guest entry bytes @0x80000000: {:02x?}", entry_bytes);
        }
        let vm_cfg = unsafe { hv_vm_config_create() };
        if !vm_cfg.is_null() {
            let r = unsafe { hv_vm_config_set_ipa_size(vm_cfg, 40) };
            if r == hv_error_t::HV_SUCCESS as hv_return_t {
                let r = unsafe { hv_vm_create(vm_cfg) };
                unsafe {
                    os_release(vm_cfg as *mut c_void);
                }
                if r == hv_error_t::HV_SUCCESS as hv_return_t {
                    for region in guest_mem.regions() {
                        warn!(
                            "HVF mapping guest region host=0x{:x} guest=0x{:x} size=0x{:x}",
                            region.host_addr as usize,
                            region.guest_addr.offset(),
                            region.size
                        );
                        let flags = HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC;
                        check_hv(unsafe {
                            hv_vm_map(
                                region.host_addr as *const c_void,
                                region.guest_addr.offset() as hv_ipa_t,
                                region.size,
                                flags,
                            )
                        })?;
                    }

                    return Ok(HvfVm {
                        _hypervisor: HvfHypervisor,
                        _vm_life: vm_life,
                        guest_mem,
                        mem_regions: Arc::new(Mutex::new(BTreeMap::new())),
                        mem_slot_gaps: Arc::new(Mutex::new(BinaryHeap::new())),
                        ipa_bits: 40,
                        gic_done: AtomicBool::new(false),
                        ioevents: Arc::new(Mutex::new(HashMap::new())),
                        power_control: Arc::new(VcpuPowerControl::default()),
                    });
                }
                eprintln!(
                    "HVF hv_vm_create(config) failed with code {}, retrying with default config",
                    r
                );
            } else {
                eprintln!(
                    "HVF hv_vm_config_set_ipa_size failed with code {}, retrying with default config",
                    r
                );
                unsafe {
                    os_release(vm_cfg as *mut c_void);
                }
            }
        }

        let r = unsafe { hv_vm_create(null_mut()) };
        check_hv(r)?;

        for region in guest_mem.regions() {
            warn!(
                "HVF mapping guest region host=0x{:x} guest=0x{:x} size=0x{:x}",
                region.host_addr as usize,
                region.guest_addr.offset(),
                region.size
            );
            let flags = HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC;
            check_hv(unsafe {
                hv_vm_map(
                    region.host_addr as *const c_void,
                    region.guest_addr.offset() as hv_ipa_t,
                    region.size,
                    flags,
                )
            })?;
        }

        Ok(HvfVm {
            _hypervisor: HvfHypervisor,
            _vm_life: vm_life,
            guest_mem,
            mem_regions: Arc::new(Mutex::new(BTreeMap::new())),
            mem_slot_gaps: Arc::new(Mutex::new(BinaryHeap::new())),
            ipa_bits: 40,
            gic_done: AtomicBool::new(false),
            ioevents: Arc::new(Mutex::new(HashMap::new())),
            power_control: Arc::new(VcpuPowerControl::default()),
        })
    }

    /// Installs the in-framework GICv3. Must run **before** any `hv_vcpu_create`, after guest RAM
    /// is mapped. Matches the guest physical layout used by `KvmKernelIrqChip` on AArch64.
    pub fn init_gic(&self, num_vcpus: usize) -> Result<()> {
        self.power_control.initialize(num_vcpus)?;
        let gic = hvf_gic_symbols().ok_or_else(|| Error::new(ENOSYS))?;
        if self
            .gic_done
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(());
        }

        let mut dist_size = 0u64;
        check_hv(unsafe { (gic.get_distributor_size)(&mut dist_size) })?;
        if dist_size == 0 {
            self.gic_done.store(false, Ordering::SeqCst);
            return Err(Error::new(EINVAL));
        }
        let mut redist_region_size = 0u64;
        check_hv(unsafe { (gic.get_redistributor_region_size)(&mut redist_region_size) })?;
        if redist_region_size == 0 {
            self.gic_done.store(false, Ordering::SeqCst);
            return Err(Error::new(EINVAL));
        }
        let redist_size = redist_region_size
            .checked_mul(num_vcpus as u64)
            .ok_or_else(|| Error::new(EOVERFLOW))?;
        let redist_addr = AARCH64_AXI_BASE
            .checked_sub(redist_size)
            .ok_or_else(|| Error::new(EOVERFLOW))?;
        let dist_if_addr = redist_addr
            .checked_sub(dist_size)
            .ok_or_else(|| Error::new(EOVERFLOW))?;
        warn!(
            "HVF GIC layout: dist=0x{:x} dist_size=0x{:x} redist=0x{:x} redist_size=0x{:x} vcpus={}",
            dist_if_addr, dist_size, redist_addr, redist_region_size, num_vcpus
        );

        let gic_cfg = unsafe { (gic.config_create)() };
        if gic_cfg.is_null() {
            self.gic_done.store(false, Ordering::SeqCst);
            return Err(Error::new(ENOSPC));
        }
        let mut ok = true;
        if ok {
            let r = unsafe { (gic.config_set_distributor_base)(gic_cfg, dist_if_addr) };
            ok = r == hv_error_t::HV_SUCCESS as hv_return_t;
        }
        if ok {
            let r = unsafe { (gic.config_set_redistributor_base)(gic_cfg, redist_addr) };
            ok = r == hv_error_t::HV_SUCCESS as hv_return_t;
        }
        let r = if ok {
            unsafe { (gic.create)(gic_cfg) }
        } else {
            hv_error_t::HV_BAD_ARGUMENT as hv_return_t
        };
        if !ok || r != hv_error_t::HV_SUCCESS as hv_return_t {
            self.gic_done.store(false, Ordering::SeqCst);
            return check_hv(r);
        }
        Ok(())
    }

    /// Assert or deassert a GICv3 SPI line (`intid` is the full interrupt ID, e.g. `32 + gsi` for
    /// the first guest SPI).
    pub fn set_gic_spi(&self, intid: u32, level: bool) -> Result<()> {
        let gic = hvf_gic_symbols().ok_or_else(|| Error::new(ENOSYS))?;
        {
            let r = unsafe { (gic.set_spi)(intid, level) };
            if r != hv_error_t::HV_SUCCESS as hv_return_t {
                eprintln!(
                    "HVF hv_gic_set_spi failed: intid={intid} level={level} code={r} ({r:#x})"
                );
            }
            check_hv(r)
        }
    }
    fn gic_spi_distributor_bit(&self, register_base: u32, intid: u32) -> Result<bool> {
        if intid < 32 {
            return Err(Error::new(EINVAL));
        }
        let gic = hvf_gic_symbols().ok_or_else(|| Error::new(ENOSYS))?;
        let reg = register_base + (intid / 32) * 4;
        let mut value = 0u64;
        let r = unsafe { (gic.get_distributor_reg)(reg, &mut value) };
        check_hv(r)?;
        Ok(value & (1u64 << (intid % 32)) != 0)
    }

    /// Returns whether an SPI is currently pending in the virtual GIC.
    pub fn gic_spi_pending(&self, intid: u32) -> Result<bool> {
        // GICD_ISPENDR<n> starts at offset 0x200 and has one bit per INTID.
        self.gic_spi_distributor_bit(0x200, intid)
    }

    /// Returns whether an SPI is currently active in the virtual GIC.
    pub fn gic_spi_active(&self, intid: u32) -> Result<bool> {
        // GICD_ISACTIVER<n> starts at offset 0x300 and has one bit per INTID.
        self.gic_spi_distributor_bit(0x300, intid)
    }

    pub fn send_gic_msi(&self, gpa: u64, intid: u32) -> Result<()> {
        let gic = hvf_gic_symbols().ok_or_else(|| Error::new(ENOSYS))?;
        {
            let r = unsafe { (gic.send_msi)(gpa as hv_ipa_t, intid) };
            if r != hv_error_t::HV_SUCCESS as hv_return_t {
                eprintln!(
                    "HVF hv_gic_send_msi failed: gpa={gpa:#x} intid={intid} code={r} ({r:#x})"
                );
            }
            check_hv(r)
        }
    }

    pub(crate) fn guest_pagesize() -> usize {
        applevisor_sys::PAGE_SIZE
    }

    pub(crate) fn fire_ioevents(&self, addr: IoEventAddress, data: &[u8]) -> Result<()> {
        let map = lock(&self.ioevents)?;
        if let Some(evt) = map.get(&addr) {
            evt.signal().map_err(|_| Error::new(EIO))?;
        }
        let _ = data;
        Ok(())
    }
}

impl Vm for HvfVm {
    fn try_clone(&self) -> Result<Self> {
        Ok(HvfVm {
            _hypervisor: self._hypervisor.clone(),
            _vm_life: self._vm_life.clone(),
            guest_mem: self.guest_mem.clone(),
            mem_regions: self.mem_regions.clone(),
            mem_slot_gaps: self.mem_slot_gaps.clone(),
            ipa_bits: self.ipa_bits,
            gic_done: AtomicBool::new(self.gic_done.load(Ordering::SeqCst)),
            ioevents: self.ioevents.clone(),
            power_control: self.power_control.clone(),
        })
    }

    fn check_capability(&self, _c: VmCap) -> bool {
        false
    }

    fn get_guest_phys_addr_bits(&self) -> u8 {
        self.ipa_bits
    }

    fn get_memory(&self) -> &GuestMemory {
        &self.guest_mem
    }

    fn add_memory_region(
        &mut self,
        guest_addr: GuestAddress,
        mem: Box<dyn MappedRegion>,
        read_only: bool,
        _log_dirty_pages: bool,
        _cache: MemCacheType,
    ) -> Result<MemSlot> {
        let pgsz = pagesize() as u64;
        let size = (mem.size() as u64 + pgsz - 1) / pgsz * pgsz;
        let end_addr = guest_addr
            .checked_add(size)
            .ok_or_else(|| Error::new(EOVERFLOW))?;
        if self.guest_mem.range_overlap(guest_addr, end_addr) {
            return Err(Error::new(ENOSPC));
        }
        let mut regions = lock(&self.mem_regions)?;
        let mut gaps = lock(&self.mem_slot_gaps)?;
        let slot = match gaps.pop() {
            Some(gap) => gap.0,
            None => (regions.len() + self.guest_mem.num_regions() as usize) as MemSlot,
        };

        let mut flags = HV_MEMORY_READ | HV_MEMORY_EXEC;
        if !read_only {
            flags |= HV_MEMORY_WRITE;
        }
        let mapped_bytes = size as usize;
        warn!(
            "HVF add_memory_region guest=0x{:x} size=0x{:x} host=0x{:x} read_only={}",
            guest_addr.offset(),
            mapped_bytes,
            mem.as_ptr() as usize,
            read_only
        );
        let r = unsafe {
            hv_vm_map(
                mem.as_ptr() as *const c_void,
                guest_addr.offset(),
                mapped_bytes,
                flags,
            )
        };
        if let Err(e) = check_hv(r) {
            gaps.push(Reverse(slot));
            return Err(e);
        }
        if read_only {
            let r = unsafe {
                hv_vm_protect(
                    guest_addr.offset(),
                    mapped_bytes,
                    HV_MEMORY_READ | HV_MEMORY_EXEC,
                )
            };
            if let Err(e) = check_hv(r) {
                unsafe {
                    let _ = hv_vm_unmap(guest_addr.offset(), mapped_bytes);
                }
                gaps.push(Reverse(slot));
                return Err(e);
            }
        }
        regions.insert(slot, (guest_addr, size, mem));
        Ok(slot)
    }

    fn msync_memory_region(&mut self, slot: MemSlot, offset: usize, size: usize) -> Result<()> {
        let mut regions = lock(&self.mem_regions)?;
        let mem = &mut regions.get_mut(&slot).ok_or_else(|| Error::new(ENOENT))?.2;
        mem.msync(offset, size).map_err(|err| match err {
            MmapError::InvalidAddress => Error::new(EFAULT),
            MmapError::NotPageAligned => Error::new(EINVAL),
            MmapError::SystemCallFailed(e) => e,
            _ => Error::new(EIO),
        })
    }

    fn remove_memory_region(&mut self, slot: MemSlot) -> Result<Box<dyn MappedRegion>> {
        let mut regions = lock(&self.mem_regions)?;
        let (guest_addr, mapped, mem) = regions.remove(&slot).ok_or_else(|| Error::new(ENOENT))?;
        lock(&self.mem_slot_gaps)?.push(Reverse(slot));
        check_hv(unsafe { hv_vm_unmap(guest_addr.offset(), mapped as usize) })?;
        Ok(mem)
    }

    fn create_device(&self, kind: DeviceKind) -> Result<SafeDescriptor> {
        let _ = kind;
        Err(Error::new(ENXIO))
    }

    fn get_dirty_log(&self, _slot: MemSlot, _dirty_log: &mut [u8]) -> Result<()> {
        Err(Error::new(ENXIO))
    }

    fn register_ioevent(
        &mut self,
        evt: &Event,
        addr: IoEventAddress,
        _datamatch: Datamatch,
    ) -> Result<()> {
        warn!("HVF register_ioevent addr={addr:?}");
        let mut map = lock(&self.ioevents)?;
        if map.insert(addr, evt.try_clone()?).is_some() {
            return Err(Error::new(EEXIST));
        }
        Ok(())
    }

    fn unregister_ioevent(
        &mut self,
        _evt: &Event,
        addr: IoEventAddress,
        _datamatch: Datamatch,
    ) -> Result<()> {
        lock(&self.ioevents)?.remove(&addr);
        Ok(())
    }

    fn handle_io_events(&self, addr: IoEventAddress, data: &[u8]) -> Result<()> {
        self.fire_ioevents(addr, data)
    }

    fn get_pvclock(&self) -> Result<ClockState> {
        Err(Error::new(ENXIO))
    }

    fn set_pvclock(&self, _state: &ClockState) -> Result<()> {
        Err(Error::new(ENXIO))
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
        let mut regions = lock(&self.mem_regions)?;
        let region = &mut regions.get_mut(&slot).ok_or_else(|| Error::new(EINVAL))?.2;
        region
            .add_fd_mapping(offset, size, fd, fd_offset, prot)
            .map_err(|e| match e {
                MmapError::SystemCallFailed(e) => e,
                _ => Error::new(EIO),
            })
    }

    fn remove_mapping(&mut self, slot: u32, offset: usize, size: usize) -> Result<()> {
        let mut regions = lock(&self.mem_regions)?;
        let region = &mut regions.get_mut(&slot).ok_or_else(|| Error::new(EINVAL))?.2;
        region.remove_mapping(offset, size).map_err(|e| match e {
            MmapError::SystemCallFailed(e) => e,
            _ => Error::new(EIO),
        })
    }

    fn handle_balloon_event(&mut self, event: BalloonEvent) -> Result<()> {
        match event {
            BalloonEvent::Inflate(m) => {
                match self.guest_mem.remove_range(m.guest_address, m.size) {
                    Ok(_) => Ok(()),
                    Err(vm_memory::Error::MemoryAccess(_, MmapError::SystemCallFailed(e))) => {
                        Err(e)
                    }
                    Err(_) => Err(Error::new(EIO)),
                }
            }
            BalloonEvent::Deflate(_) => Ok(()),
            BalloonEvent::BalloonTargetReached(_) => Ok(()),
        }
    }
}

impl VmAArch64 for HvfVm {
    fn get_hypervisor(&self) -> &dyn Hypervisor {
        &self._hypervisor
    }

    fn load_protected_vm_firmware(
        &mut self,
        _fw_addr: GuestAddress,
        _fw_max_size: u64,
    ) -> Result<()> {
        Err(Error::new(ENOSYS))
    }

    fn create_vcpu(&self, id: usize) -> Result<Box<dyn crate::VcpuAArch64>> {
        Ok(Box::new(HvfVcpu::new(
            id,
            self.ioevents.clone(),
            self.power_control.clone(),
        )?))
    }

    fn create_fdt(&self, _fdt: &mut Fdt, _phandles: &BTreeMap<&str, u32>) -> cros_fdt::Result<()> {
        Ok(())
    }

    fn init_arch(
        &self,
        _payload_entry_address: GuestAddress,
        _fdt_address: GuestAddress,
        _fdt_size: usize,
    ) -> Result<()> {
        Ok(())
    }
}
