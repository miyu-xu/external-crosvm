// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod xhee_sys;

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::collections::BinaryHeap;
use std::convert::TryFrom;
use std::ffi::CString;
use std::os::raw::c_ulong;
use std::os::unix::prelude::OsStrExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use base::errno_result;
use base::error;
use base::ioctl;
use base::ioctl_with_mut_ref;
use base::ioctl_with_ref;
use base::ioctl_with_val;
use base::pagesize;
use base::AsRawDescriptor;
use base::Error;
use base::Event;
use base::FromRawDescriptor;
use base::MappedRegion;
use base::MemoryMapping;
use base::MemoryMappingBuilder;
use base::MmapError;
use base::Protection;
use base::RawDescriptor;
use base::Result;
use base::SafeDescriptor;
use cros_fdt::Fdt;
#[cfg(feature = "gdb")]
use gdbstub::arch::Arch;
#[cfg(feature = "gdb")]
use gdbstub_arch::aarch64::reg::id::AArch64RegId;
#[cfg(feature = "gdb")]
use gdbstub_arch::aarch64::AArch64 as GdbArch;
use libc::open;
use libc::EFAULT;
use libc::EINVAL;
use libc::EIO;
use libc::ENOENT;
use libc::ENOMEM;
use libc::ENOSPC;
use libc::ENOTSUP;
use libc::EOVERFLOW;
use libc::O_CLOEXEC;
use libc::O_RDWR;
use sync::Mutex;
use vm_memory::GuestAddress;
use vm_memory::GuestMemory;
use vm_memory::MemoryRegionPurpose;
pub use xhee_sys::*;

use crate::BalloonEvent;
use crate::ClockState;
use crate::Config;
use crate::Datamatch;
use crate::DeviceKind;
use crate::HypervHypercall;
use crate::Hypervisor;
use crate::HypervisorCap;
use crate::IoEventAddress;
use crate::IoOperation;
use crate::IoParams;
use crate::MemCacheType;
use crate::MemSlot;
use crate::PsciVersion;
use crate::Vcpu;
use crate::VcpuAArch64;
use crate::VcpuExit;
use crate::VcpuFeature;
use crate::VcpuRegAArch64;
use crate::VcpuSignalHandle;
use crate::VcpuSignalHandleInner;
use crate::Vm;
use crate::VmAArch64;
use crate::VmCap;
use crate::PSCI_0_2;

impl Xhee {
    /// Get the size of guest physical addresses (IPA) in bits.
    pub fn get_guest_phys_addr_bits(&self) -> u8 {
        let mut addr_bits: u64 = 0;
        let ret = unsafe { ioctl_with_mut_ref(self, XHEE_GET_VM_GPA_SIZE(), &mut addr_bits) };
        if ret < 0 {
            error!("xhee: get addr failed!, set default 40");
            return 40;
        }

        addr_bits as u8
    }
}

impl XheeVm {
    /// Arch-specific implementation of `Vm::get_pvclock`.  Always returns an error on AArch64.
    pub fn get_pvclock_arch(&self) -> Result<ClockState> {
        error!("xhee: not support get_pvclock_arch");
        Err(Error::new(EINVAL))
    }

    /// Arch-specific implementation of `Vm::set_pvclock`.  Always returns an error on AArch64.
    pub fn set_pvclock_arch(&self, _state: &ClockState) -> Result<()> {
        error!("xhee: not support set_pvclock_arch");
        Err(Error::new(EINVAL))
    }

    fn get_pvmfw_size(&self) -> Result<u64> {
        let mut vmfw_size: u64 = 0;
        let ret = unsafe { ioctl_with_mut_ref(self, XHEE_GET_PVMFW_SIZE(), &mut vmfw_size) };
        if ret < 0 {
            error!("xhee: get pvmfw size failed!");
            return Err(Error::new(EINVAL));
        }

        Ok(vmfw_size as u64)
    }

    fn set_pvmfw_gpa(&self, fw_addr: GuestAddress) -> Result<()> {
        let ret = unsafe { ioctl_with_ref(self, XHEE_SET_PVMFW_GPA(), &fw_addr.0) };
        if ret < 0 {
            error!("xhee: set pvmfw gpa failed!");
            return Err(Error::new(EINVAL));
        }

        Ok(())
    }
}

impl VmAArch64 for XheeVm {
    fn get_hypervisor(&self) -> &dyn Hypervisor {
        &self.xhee
    }

    fn load_protected_vm_firmware(
        &mut self,
        fw_addr: GuestAddress,
        fw_max_size: u64,
    ) -> Result<()> {
        let size: u64 = self.get_pvmfw_size()?;
        if size == 0 {
            error!("get pvmfw size == 0");
            return Err(Error::new(EINVAL));
        }

        if size > fw_max_size {
            error!("pvmfw size beyond max size: {:#x}", fw_max_size);
            return Err(Error::new(ENOMEM));
        }

        self.set_pvmfw_gpa(fw_addr)
    }

    fn create_vcpu(&self, id: usize) -> Result<Box<dyn VcpuAArch64>> {
        Ok(Box::new(XheeVm::create_vcpu(self, id)?))
    }

    fn create_fdt(&self, _fdt: &mut Fdt, _phandles: &BTreeMap<&str, u32>) -> cros_fdt::Result<()> {
        Ok(())
    }

    fn init_arch(
        &self,
        _payload_entry_address: GuestAddress,
        fdt_address: GuestAddress,
        fdt_size: usize,
    ) -> Result<()> {
        let dtb_config = xhee_dtb_config {
            dtb_addr: fdt_address.offset(),
            dtb_size: fdt_size.try_into().unwrap(),
        };
        // SAFETY:
        // Safe because we allocated the struct and we know the kernel will modify exactly the size
        // of the struct.
        let ret = unsafe { ioctl_with_ref(self, XHEE_SET_DTB_CONFIG(), &dtb_config) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }
}

impl XheeVcpu {
    fn set_one_xhee_reg_u64(&self, xhee_reg_id: XheeVcpuRegister, data: u64) -> Result<()> {
        self.set_one_xhee_reg(xhee_reg_id, data.to_ne_bytes().as_slice(), 8)
    }

    fn set_one_xhee_reg(
        &self,
        xhee_reg_id: XheeVcpuRegister,
        data: &[u8],
        size: u64,
    ) -> Result<()> {
        let onereg = xhee_one_reg {
            id: xhee_reg_id.into(),
            addr: (data.as_ptr() as usize)
                .try_into()
                .expect("can't represent usize as u64"),
            size,
        };

        // SAFETY:
        // Safe because we allocated the struct and we know the kernel will read exactly the size of
        // the struct.
        let ret = unsafe { ioctl_with_ref(self, XHEE_SET_ONE_REG(), &onereg) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    fn get_one_xhee_reg_u64(&self, xhee_reg_id: XheeVcpuRegister) -> Result<u64> {
        let mut bytes = 0u64.to_ne_bytes();
        self.get_one_xhee_reg(xhee_reg_id, bytes.as_mut_slice())?;
        Ok(u64::from_ne_bytes(bytes))
    }

    fn get_one_xhee_reg(&self, xhee_reg_id: XheeVcpuRegister, data: &mut [u8]) -> Result<()> {
        let onereg = xhee_one_reg {
            id: xhee_reg_id.into(),
            addr: (data.as_mut_ptr() as usize)
                .try_into()
                .expect("can't represent usize as u64"),
            size: 8,
        };

        // SAFETY:
        // Safe because we allocated the struct and we know the kernel will read exactly the size of
        // the struct.
        let ret = unsafe { ioctl_with_ref(self, XHEE_GET_ONE_REG(), &onereg) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }
}

#[allow(dead_code)]
/// xhee registers as used by the `GET_ONE_REG`/`SET_ONE_REG` ioctl API
pub enum XheeVcpuRegister {
    /// General Purpose Registers X0-X30
    X(u8),
    /// Stack Pointer
    Sp,
    /// Program Counter
    Pc,
    /// Processor State
    Pstate,
    /// Stack Pointer (EL1)
    SpEl1,
    /// Exception Link Register (EL1)
    ElrEl1,
    /// Saved Program Status Register (EL1, abt, und, irq, fiq)
    Spsr(u8),
    /// FP & SIMD Registers V0-V31
    V(u8),
    /// Floating-point Status Register
    Fpsr,
    /// Floating-point Control Register
    Fpcr,
    /// xhee Firmware Pseudo-Registers
    Firmware(u16),
    /// Generic System Registers by (Op0, Op1, CRn, CRm, Op2)
    System(u16),
    /// CCSIDR_EL1 Demultiplexed by CSSELR_EL1
    Ccsidr(u8),
}

/// Gives the `u64` register ID expected by the `GET_ONE_REG`/`SET_ONE_REG` ioctl API.
impl From<XheeVcpuRegister> for u64 {
    fn from(register: XheeVcpuRegister) -> Self {
        const fn reg(size: u64, kind: u64, fields: u64) -> u64 {
            XHEE_REG_ARM64 | size | kind | fields
        }

        const fn xhee_regs_reg(size: u64, offset: usize) -> u64 {
            let offset = offset / std::mem::size_of::<u32>();

            reg(size, XHEE_REG_ARM_CORE as u64, offset as u64)
        }

        const fn xhee_reg(offset: usize) -> u64 {
            xhee_regs_reg(XHEE_REG_SIZE_U64, offset)
        }

        fn user_pt_reg(offset: usize) -> u64 {
            xhee_regs_reg(
                XHEE_REG_SIZE_U64,
                memoffset::offset_of!(xhee_regs, regs) + offset,
            )
        }

        fn user_fpsimd_state_reg(size: u64, offset: usize) -> u64 {
            xhee_regs_reg(size, memoffset::offset_of!(xhee_regs, fp_regs) + offset)
        }

        const fn reg_u64(kind: u64, fields: u64) -> u64 {
            reg(XHEE_REG_SIZE_U64, kind, fields)
        }

        const fn demux_reg(size: u64, index: u64, value: u64) -> u64 {
            let index =
                (index << XHEE_REG_ARM_DEMUX_ID_SHIFT) & (XHEE_REG_ARM_DEMUX_ID_MASK as u64);
            let value =
                (value << XHEE_REG_ARM_DEMUX_VAL_SHIFT) & (XHEE_REG_ARM_DEMUX_VAL_MASK as u64);

            reg(size, XHEE_REG_ARM_DEMUX as u64, index | value)
        }

        match register {
            XheeVcpuRegister::X(n @ 0..=30) => {
                let n = std::mem::size_of::<u64>() * (n as usize);

                user_pt_reg(memoffset::offset_of!(user_pt_regs, regs) + n)
            }
            XheeVcpuRegister::X(n) => {
                unreachable!("invalid XheeVcpuRegister Xn index: {n}")
            }
            XheeVcpuRegister::Sp => user_pt_reg(memoffset::offset_of!(user_pt_regs, sp)),
            XheeVcpuRegister::Pc => user_pt_reg(memoffset::offset_of!(user_pt_regs, pc)),
            XheeVcpuRegister::Pstate => user_pt_reg(memoffset::offset_of!(user_pt_regs, pstate)),
            XheeVcpuRegister::SpEl1 => xhee_reg(memoffset::offset_of!(xhee_regs, sp_el1)),
            XheeVcpuRegister::ElrEl1 => xhee_reg(memoffset::offset_of!(xhee_regs, elr_el1)),
            XheeVcpuRegister::Spsr(n @ 0..=4) => {
                let n = std::mem::size_of::<u64>() * (n as usize);
                xhee_reg(memoffset::offset_of!(xhee_regs, spsr) + n)
            }
            XheeVcpuRegister::Spsr(n) => {
                unreachable!("invalid XheeVcpuRegister Spsr index: {n}")
            }
            XheeVcpuRegister::V(n @ 0..=31) => {
                let n = std::mem::size_of::<u128>() * (n as usize);
                user_fpsimd_state_reg(
                    XHEE_REG_SIZE_U128,
                    memoffset::offset_of!(user_fpsimd_state, vregs) + n,
                )
            }
            XheeVcpuRegister::V(n) => {
                unreachable!("invalid XheeVcpuRegister Vn index: {n}")
            }
            XheeVcpuRegister::Fpsr => user_fpsimd_state_reg(
                XHEE_REG_SIZE_U32,
                memoffset::offset_of!(user_fpsimd_state, fpsr),
            ),
            XheeVcpuRegister::Fpcr => user_fpsimd_state_reg(
                XHEE_REG_SIZE_U32,
                memoffset::offset_of!(user_fpsimd_state, fpcr),
            ),
            XheeVcpuRegister::Firmware(n) => reg_u64(XHEE_REG_ARM, n.into()),
            XheeVcpuRegister::System(n) => reg_u64(XHEE_REG_ARM64_SYSREG.into(), n.into()),
            XheeVcpuRegister::Ccsidr(n) => demux_reg(XHEE_REG_SIZE_U32, 0, n.into()),
        }
    }
}

#[cfg(feature = "gdb")]
impl TryFrom<AArch64RegId> for XheeVcpuRegister {
    type Error = Error;

    fn try_from(_reg: <GdbArch as Arch>::RegId) -> std::result::Result<Self, Self::Error> {
        error!("xhee: not support gdb");
        Err(Error::new(EINVAL))
    }
}

impl From<VcpuRegAArch64> for XheeVcpuRegister {
    fn from(reg: VcpuRegAArch64) -> Self {
        match reg {
            VcpuRegAArch64::X(n @ 0..=30) => Self::X(n),
            VcpuRegAArch64::X(n) => unreachable!("invalid VcpuRegAArch64 index: {n}"),
            VcpuRegAArch64::Sp => Self::Sp,
            VcpuRegAArch64::Pc => Self::Pc,
            VcpuRegAArch64::Pstate => Self::Pstate,
        }
    }
}

impl VcpuAArch64 for XheeVcpu {
    fn init(&self, _features: &[VcpuFeature]) -> Result<()> {
        Ok(())
    }

    fn init_pmu(&self, _irq: u64) -> Result<()> {
        Ok(())
    }

    fn has_pvtime_support(&self) -> bool {
        false
    }

    fn init_pvtime(&self, _pvtime_ipa: u64) -> Result<()> {
        error!("xhee: not support init_pvtime");
        Err(Error::new(EINVAL))
    }

    fn set_one_reg(&self, reg_id: VcpuRegAArch64, data: u64) -> Result<()> {
        self.set_one_xhee_reg_u64(XheeVcpuRegister::from(reg_id), data)
    }

    fn get_one_reg(&self, reg_id: VcpuRegAArch64) -> Result<u64> {
        self.get_one_xhee_reg_u64(XheeVcpuRegister::from(reg_id))
    }

    fn set_vector_reg(&self, _reg_num: u8, _data: u128) -> Result<()> {
        unimplemented!()
    }

    fn get_vector_reg(&self, _reg_num: u8) -> Result<u128> {
        unimplemented!()
    }

    fn get_psci_version(&self) -> Result<PsciVersion> {
        Ok(PSCI_0_2)
    }

    #[cfg(feature = "gdb")]
    fn get_max_hw_bps(&self) -> Result<usize> {
        error!("xhee: not support get_max_hw_bps");
        Err(Error::new(EINVAL))
    }

    #[cfg(feature = "gdb")]
    fn set_guest_debug(&self, _addrs: &[GuestAddress], _enable_singlestep: bool) -> Result<()> {
        error!("xhee: not support set_gdb_registers");
        Err(Error::new(EINVAL))
    }

    #[cfg(feature = "gdb")]
    fn set_gdb_registers(&self, _regs: &<GdbArch as Arch>::Registers) -> Result<()> {
        error!("xhee: not support set_gdb_registers");
        Err(Error::new(EINVAL))
    }

    #[cfg(feature = "gdb")]
    fn get_gdb_registers(&self, _regs: &mut <GdbArch as Arch>::Registers) -> Result<()> {
        error!("xhee: not support get_gdb_registers");
        Err(Error::new(EINVAL))
    }

    #[cfg(feature = "gdb")]
    fn set_gdb_register(&self, _reg: <GdbArch as Arch>::RegId, _data: &[u8]) -> Result<()> {
        error!("xhee: not support set_gdb_register");
        Err(Error::new(EINVAL))
    }

    #[cfg(feature = "gdb")]
    fn get_gdb_register(&self, _reg: <GdbArch as Arch>::RegId, _data: &mut [u8]) -> Result<usize> {
        error!("xhee: not support get_gdb_register");
        Err(Error::new(EINVAL))
    }
}

// Wrapper around XHEE_SET_USER_MEMORY_REGION ioctl, which creates, modifies, or deletes a mapping
// from guest physical to host user pages.
//
// SAFETY:
// Safe when the guest regions are guaranteed not to overlap.
unsafe fn set_user_memory_region(
    descriptor: &SafeDescriptor,
    slot: MemSlot,
    _read_only: bool,
    _log_dirty_pages: bool,
    guest_addr: u64,
    memory_size: u64,
    userspace_addr: *mut u8,
    flags: u32,
) -> Result<()> {
    let region = xhee_userspace_memory_region {
        slot,
        flags,
        guest_phys_addr: guest_addr,
        memory_size,
        userspace_addr: userspace_addr as u64,
    };

    let ret = ioctl_with_ref(descriptor, XHEE_SET_USER_MEMORY_REGION(), &region);
    if ret == 0 {
        Ok(())
    } else {
        errno_result()
    }
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

pub struct Xhee {
    xhee: SafeDescriptor,
}

impl Xhee {
    pub fn new_with_path(device_path: &Path) -> Result<Xhee> {
        let c_path = CString::new(device_path.as_os_str().as_bytes()).unwrap();
        // SAFETY:
        // Open calls are safe because we give a nul-terminated string and verify the result.
        let ret = unsafe { open(c_path.as_ptr(), O_RDWR | O_CLOEXEC) };
        if ret < 0 {
            return errno_result();
        }
        Ok(Xhee {
            // SAFETY:
            // Safe because we verify that ret is valid and we own the fd.
            xhee: unsafe { SafeDescriptor::from_raw_descriptor(ret) },
        })
    }

    /// Opens `/dev/xrvm/` and returns a xhee object on success.
    pub fn new() -> Result<Xhee> {
        Xhee::new_with_path(&PathBuf::from("/dev/xrvm"))
    }

    /// Gets the size of the mmap required to use vcpu's `xhee_vcpu_run` structure.
    pub fn get_vcpu_mmap_size(&self) -> Result<usize> {
        // We don't use mmap, return sizeof(xhee_vcpu_run) directly
        let res = std::mem::size_of::<xhee_vcpu_run>();
        Ok(res)
    }
}

impl AsRawDescriptor for Xhee {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.xhee.as_raw_descriptor()
    }
}

impl Hypervisor for Xhee {
    fn try_clone(&self) -> Result<Self> {
        Ok(Xhee {
            xhee: self.xhee.try_clone()?,
        })
    }

    fn check_capability(&self, cap: HypervisorCap) -> bool {
        match cap {
            HypervisorCap::UserMemory => true,
            HypervisorCap::ArmPmuV3 => false,
            HypervisorCap::ImmediateExit => true,
            HypervisorCap::StaticSwiotlbAllocationRequired => true,
            HypervisorCap::HypervisorInitializedBootContext => false,
            HypervisorCap::S390UserSigp | HypervisorCap::TscDeadlineTimer => false,
        }
    }
}

/// A wrapper around creating and using a xhee VM.
pub struct XheeVm {
    xhee: Xhee,
    vm: SafeDescriptor,
    guest_mem: GuestMemory,
    mem_regions: Arc<Mutex<BTreeMap<MemSlot, Box<dyn MappedRegion>>>>,
    /// A min heap of MemSlot numbers that were used and then removed and can now be re-used
    mem_slot_gaps: Arc<Mutex<BinaryHeap<Reverse<MemSlot>>>>,
}

impl XheeVm {
    /// Constructs a new `XheeVm` using the given `xhee` instance.
    pub fn new(xhee: &Xhee, guest_mem: GuestMemory, cfg: Config) -> Result<XheeVm> {
        // SAFETY:
        // Safe because we know xhee is a real fd as this module is the only one that can make
        // xhee vm objects.
        let ret = unsafe { ioctl_with_val(xhee, XHEE_CREATE_VM(), 0) };
        if ret < 0 {
            return errno_result();
        }

        // SAFETY:
        // Safe because we verify that ret is valid and we own the fd.
        let vm_descriptor = unsafe { SafeDescriptor::from_raw_descriptor(ret) };
        for region in guest_mem.regions() {
            let flags = match region.options.purpose {
                MemoryRegionPurpose::GuestMemoryRegion => XHEE_USER_MEM_REGION_GUEST_MEM,
                MemoryRegionPurpose::ProtectedFirmwareRegion => XHEE_USER_MEM_REGION_PROTECT_FW,
                MemoryRegionPurpose::StaticSwiotlbRegion => XHEE_USER_MEM_REGION_STATIC_SWIOTLB,
            };

            // SAFETY:
            // Safe because the guest regions are guaranteed not to overlap.
            unsafe {
                set_user_memory_region(
                    &vm_descriptor,
                    region.index as MemSlot,
                    false,
                    false,
                    region.guest_addr.offset(),
                    region.size as u64,
                    region.host_addr as *mut u8,
                    flags,
                )
            }?;
        }

        let vm = XheeVm {
            xhee: xhee.try_clone()?,
            vm: vm_descriptor,
            guest_mem,
            mem_regions: Arc::new(Mutex::new(BTreeMap::new())),
            mem_slot_gaps: Arc::new(Mutex::new(BinaryHeap::new())),
        };
        Ok(vm)
    }

    fn create_vcpu(&self, id: usize) -> Result<XheeVcpu> {
        // run is a data stucture shared with ko and xhee
        let run_mmap_size = self.xhee.get_vcpu_mmap_size()?;

        let fd =
            unsafe { ioctl_with_val(self, XHEE_CREATE_VCPU(), c_ulong::try_from(id).unwrap()) };
        if fd < 0 {
            return errno_result();
        }

        let vcpu = unsafe { SafeDescriptor::from_raw_descriptor(fd) };
        // Memory mapping --> Memory allocation
        let run_mmap = MemoryMappingBuilder::new(run_mmap_size)
            .build()
            .map_err(|_| Error::new(ENOSPC))?;

        Ok(XheeVcpu {
            vm: self.vm.try_clone()?,
            vcpu,
            id,
            run_mmap: Arc::new(run_mmap),
        })
    }

    /// Creates an in kernel interrupt controller.
    ///
    /// See the documentation on the XHEE_CREATE_IRQCHIP ioctl.
    pub fn create_irq_chip(&self) -> Result<()> {
        // SAFETY:
        // Safe because we know that our file is a VM fd and we verify the return result.
        // Todo: the current stage is stubbing, and adaptation will be carried out later.
        let ret = unsafe { ioctl(self, XHEE_CREATE_IRQCHIP()) };
        if ret == 0 {
            Ok(())
        } else {
            error!("xhee: create irq chip failed!");
            errno_result()
        }
    }

    /// Sets the level on the given irq to 1 if `active` is true, and 0 otherwise.
    pub fn set_irq_line(&self, irq: u32, active: bool) -> Result<()> {
        let mut irq_level = xhee_irq_level::default();
        irq_level.__bindgen_anon_1.irq = irq;
        irq_level.level = active as u32;

        // SAFETY:
        // Safe because we know that our file is a VM fd, we know the kernel will only read the
        // correct amount of memory from our pointer, and we verify the return result.
        // Todo: the current stage is stubbing, and adaptation will be carried out later.
        let ret = unsafe { ioctl_with_ref(self, XHEE_IRQ_LINE(), &irq_level) };
        if ret == 0 {
            Ok(())
        } else {
            error!("xhee: set irq line failed!");
            errno_result()
        }
    }

    /// Registers an event that will, when signalled, trigger the `gsi` irq, and `resample_evt`
    /// ( when not None ) will be triggered when the irqchip is resampled.
    pub fn register_irqfd(
        &self,
        gsi: u32,
        evt: &Event,
        resample_evt: Option<&Event>,
    ) -> Result<()> {
        let mut irqfd = xhee_irqfd {
            fd: evt.as_raw_descriptor() as u32,
            gsi,
            ..Default::default()
        };

        if let Some(r_evt) = resample_evt {
            irqfd.flags = XHEE_IRQFD_FLAG_RESAMPLE;
            irqfd.resamplefd = r_evt.as_raw_descriptor() as u32;
        }

        // SAFETY:
        // Safe because we know that our file is a VM fd, we know the kernel will only read the
        // correct amount of memory from our pointer, and we verify the return result.
        // Todo: the current stage is stubbing, and adaptation will be carried out later.
        let ret = unsafe { ioctl_with_ref(self, XHEE_IRQFD(), &irqfd) };
        if ret == 0 {
            Ok(())
        } else {
            error!("xhee: register irq fd failed!");
            errno_result()
        }
    }

    /// Unregisters an event that was previously registered with
    /// `register_irqfd`.
    ///
    /// The `evt` and `gsi` pair must be the same as the ones passed into
    /// `register_irqfd`.
    pub fn unregister_irqfd(&self, gsi: u32, evt: &Event) -> Result<()> {
        let irqfd = xhee_irqfd {
            fd: evt.as_raw_descriptor() as u32,
            gsi,
            flags: XHEE_IRQFD_FLAG_DEASSIGN,
            ..Default::default()
        };
        // SAFETY:
        // Safe because we know that our file is a VM fd, we know the kernel will only read the
        // correct amount of memory from our pointer, and we verify the return result.
        // Todo: the current stage is stubbing, and adaptation will be carried out later.
        let ret = unsafe { ioctl_with_ref(self, XHEE_IRQFD(), &irqfd) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
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
                Some(u) => (true, u, 8),
                None => (false, 0, 8),
            },
        };
        let mut flags = 0;
        if deassign {
            flags |= 1 << xhee_ioeventfd_flag_nr_deassign;
        }
        if do_datamatch {
            flags |= 1 << xhee_ioeventfd_flag_nr_datamatch
        }
        if let IoEventAddress::Pio(_) = addr {
            flags |= 1 << xhee_ioeventfd_flag_nr_pio;
        }
        let ioeventfd = xhee_ioeventfd {
            datamatch: datamatch_value,
            len: datamatch_len,
            addr: match addr {
                IoEventAddress::Pio(p) => p,
                IoEventAddress::Mmio(m) => m,
            },
            fd: evt.as_raw_descriptor(),
            flags,
            ..Default::default()
        };
        // SAFETY:
        // Safe because we know that our file is a VM fd, we know the kernel will only read the
        // correct amount of memory from our pointer, and we verify the return result.
        // Todo: the current stage is stubbing, and adaptation will be carried out later.
        let ret = unsafe { ioctl_with_ref(self, XHEE_IOEVENTFD(), &ioeventfd) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    pub fn create_xhee_device(&self, dev: xhee_create_device) -> Result<()> {
        // SAFETY:
        // Safe because we allocated the struct and we know the kernel will modify exactly the size
        // of the struct and the return value is checked.
        let ret = unsafe { base::ioctl_with_ref(self, XHEE_CREATE_DEVICE(), &dev) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
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
        // No-op, when the guest attempts to access the pages again, Linux/XHEE will provide them.
        Ok(())
    }
}

impl Vm for XheeVm {
    fn try_clone(&self) -> Result<Self> {
        Ok(XheeVm {
            xhee: self.xhee.try_clone()?,
            vm: self.vm.try_clone()?,
            guest_mem: self.guest_mem.clone(),
            mem_regions: self.mem_regions.clone(),
            mem_slot_gaps: self.mem_slot_gaps.clone(),
        })
    }

    fn check_capability(&self, c: VmCap) -> bool {
        match c {
            VmCap::DirtyLog => true,
            VmCap::PvClock => false,
            VmCap::Protected => true,
            VmCap::EarlyInitCpuid => false,
            VmCap::ReadOnlyMemoryRegion => false,
            VmCap::MemNoncoherentDma => false,
        }
    }

    fn get_guest_phys_addr_bits(&self) -> u8 {
        self.xhee.get_guest_phys_addr_bits()
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
        let pgsz = pagesize() as u64;
        // XHEE require to set the user memory region with page size aligned size. Safe to extend
        // the mem.size() to be page size aligned because the mmap will round up the size to be
        // page size aligned if it is not.
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
        let flags = 0;

        // SAFETY:
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
                guest_addr.offset(),
                size,
                mem.as_ptr(),
                flags,
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
        // SAFETY:
        // Safe because the slot is checked against the list of memory slots.
        unsafe {
            set_user_memory_region(&self.vm, slot, false, false, 0, 0, std::ptr::null_mut(), 0)?;
        }
        self.mem_slot_gaps.lock().push(Reverse(slot));
        // This remove will always succeed because of the contains_key check above.
        Ok(regions.remove(&slot).unwrap())
    }

    fn create_device(&self, _kind: DeviceKind) -> Result<SafeDescriptor> {
        // This function should not be invoked because the vgic device is created in irqchip.
        errno_result()
    }

    fn get_dirty_log(&self, _slot: MemSlot, _dirty_log: &mut [u8]) -> Result<()> {
        Err(Error::new(ENOTSUP))
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
        // XHEE delivers IO events in-kernel with ioeventfds, so this is a no-op
        Ok(())
    }

    fn get_pvclock(&self) -> Result<ClockState> {
        self.get_pvclock_arch()
    }

    fn set_pvclock(&self, state: &ClockState) -> Result<()> {
        self.set_pvclock_arch(state)
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

    fn handle_balloon_event(&mut self, event: BalloonEvent) -> Result<()> {
        match event {
            BalloonEvent::Inflate(m) => self.handle_inflate(m.guest_address, m.size),
            BalloonEvent::Deflate(m) => self.handle_deflate(m.guest_address, m.size),
            BalloonEvent::BalloonTargetReached(_) => Ok(()),
        }
    }
}

impl AsRawDescriptor for XheeVm {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.vm.as_raw_descriptor()
    }
}

struct XheeVcpuSignalHandle {
    run_mmap: Arc<MemoryMapping>,
}

impl VcpuSignalHandleInner for XheeVcpuSignalHandle {
    fn signal_immediate_exit(&self) {
        // SAFETY: we ensure `run_mmap` is a valid mapping of `kvm_run` at creation time, and the
        // `Arc` ensures the mapping still exists while we hold a reference to it.
        unsafe {
            let run = self.run_mmap.as_ptr() as *mut xhee_vcpu_run;
            (*run).immediate_exit = 1;
        }
    }
}

/// A wrapper around using a Xhee Vcpu.
pub struct XheeVcpu {
    vm: SafeDescriptor,
    vcpu: SafeDescriptor,
    id: usize,
    run_mmap: Arc<MemoryMapping>,
}

impl Vcpu for XheeVcpu {
    fn try_clone(&self) -> Result<Self> {
        let vm = self.vm.try_clone()?;
        let vcpu = self.vcpu.try_clone()?;

        Ok(XheeVcpu {
            vm,
            vcpu,
            id: self.id,
            run_mmap: self.run_mmap.clone(),
        })
    }

    fn as_vcpu(&self) -> &dyn Vcpu {
        self
    }

    fn id(&self) -> usize {
        self.id
    }

    #[allow(clippy::cast_ptr_alignment)]
    fn set_immediate_exit(&self, exit: bool) {
        // TODO(b/315998194): Add safety comment
        #[allow(clippy::undocumented_unsafe_blocks)]
        let run = unsafe { &mut *(self.run_mmap.as_ptr() as *mut xhee_vcpu_run) };
        run.immediate_exit = exit as u8;
    }

    fn signal_handle(&self) -> VcpuSignalHandle {
        VcpuSignalHandle {
            inner: Box::new(XheeVcpuSignalHandle {
                run_mmap: self.run_mmap.clone(),
            }),
        }
    }

    fn on_suspend(&self) -> Result<()> {
        Ok(())
    }

    unsafe fn enable_raw_capability(&self, _cap: u32, _args: &[u64; 4]) -> Result<()> {
        Err(Error::new(libc::ENXIO))
    }

    #[allow(clippy::cast_ptr_alignment)]
    // The pointer is page aligned so casting to a different type is well defined, hence the clippy
    // allow attribute.
    fn run(&mut self) -> Result<VcpuExit> {
        // SAFETY:
        // Safe because we know that our file is a VCPU fd and we verify the return result.
        let ret = unsafe { ioctl_with_val(self, XHEE_RUN(), self.run_mmap.as_ptr() as u64) };
        if ret != 0 {
            return errno_result();
        }

        // SAFETY:
        // Safe because we know we mapped enough memory to hold the xhee_vcpu_run struct because the
        // kernel told us how large it was.
        let run = unsafe { &mut *(self.run_mmap.as_ptr() as *mut xhee_vcpu_run) };

        match run.exit_reason {
            XHEE_EXIT_MMIO => Ok(VcpuExit::Mmio),
            XHEE_EXIT_IRQ => Ok(VcpuExit::IrqWindowOpen),
            XHEE_EXIT_HVC => Ok(VcpuExit::Hypercall),
            XHEE_EXIT_EXCEPTION => Err(Error::new(EINVAL)),
            XHEE_EXIT_DEBUG => Ok(VcpuExit::Debug),
            XHEE_EXIT_FAIL_ENTRY => {
                // SAFETY:
                // Safe because the exit_reason (which comes from the kernel) told us which
                // union field to use.
                let hardware_entry_failure_reason = unsafe {
                    run.__bindgen_anon_1
                        .fail_entry
                        .hardware_entry_failure_reason
                };
                Ok(VcpuExit::FailEntry {
                    hardware_entry_failure_reason,
                })
            }
            XHEE_EXIT_SYSTEM_EVENT => {
                // SAFETY:
                // Safe because the exit_reason (which comes from the kernel) told us which
                // union field to use.
                let event_type = unsafe { run.__bindgen_anon_1.system_event.type_ };
                match event_type {
                    XHEE_SYSTEM_EVENT_SHUTDOWN => Ok(VcpuExit::SystemEventShutdown),
                    XHEE_SYSTEM_EVENT_RESET => Ok(VcpuExit::SystemEventReset),
                    XHEE_SYSTEM_EVENT_CRASH => Ok(VcpuExit::SystemEventCrash),
                    _ => {
                        error!("unknown xhee system event {}", event_type);
                        Err(Error::new(EINVAL))
                    }
                }
            }
            XHEE_EXIT_INTERNAL_ERROR => Ok(VcpuExit::InternalError),
            XHEE_EXIT_SHUTDOWN => Ok(VcpuExit::Shutdown),
            XHEE_EXIT_UNKNOWN => panic!("unknown xhee exit reason\n"),
            r => panic!("unknown xhee exit reason: {}", r),
        }
    }

    fn handle_mmio(&self, handle_fn: &mut dyn FnMut(IoParams) -> Option<[u8; 8]>) -> Result<()> {
        // SAFETY:
        // Safe because we know we mapped enough memory to hold the xhee_vcpu_run struct because the
        // kernel told us how large it was. The pointer is page aligned so casting to a different
        // type is well defined, hence the clippy allow attribute.
        let run = unsafe { &mut *(self.run_mmap.as_ptr() as *mut xhee_vcpu_run) };

        // Verify that the handler is called in the right context.
        assert!(run.exit_reason == XHEE_EXIT_MMIO);
        // SAFETY:
        // Safe because the exit_reason (which comes from the kernel) told us which
        // union field to use.
        let mmio = unsafe { &mut run.__bindgen_anon_1.mmio };
        let address = mmio.phys_addr;

        let size = mmio.size as usize;

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

    fn handle_wrmsr(&self) {}
}

impl AsRawDescriptor for XheeVcpu {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.vcpu.as_raw_descriptor()
    }
}
