// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::geniezone::*;
use crate::geniezone::geniezone_sys::*;

use std::convert::TryFrom;

use base::errno_result;
use base::error;
use base::ioctl_with_mut_ref;
use base::ioctl_with_ref;
use base::ioctl_with_val;
use base::warn;
use base::Error;
use base::MemoryMappingBuilder;
use base::Result;
#[cfg(feature = "gdb")]
use gdbstub::arch::Arch;
#[cfg(feature = "gdb")]
use gdbstub_arch::aarch64::reg::id::AArch64RegId;
#[cfg(feature = "gdb")]
use gdbstub_arch::aarch64::AArch64 as GdbArch;
use libc::EINVAL;
#[cfg(feature = "gdb")]
use libc::ENOBUFS;
#[cfg(feature = "gdb")]
use libc::ENOENT;
use libc::ENOMEM;
use libc::ENOTSUP;
#[cfg(feature = "gdb")]
use libc::ENOTUNIQ;
use vm_memory::GuestAddress;

use super::Config;
use super::GeniezoneCap;
use super::Geniezone;
use super::GeniezoneVcpu;
use super::GeniezoneVm;
use crate::ClockState;
use crate::DeviceKind;
use crate::Hypervisor;
use crate::IrqSourceChip;
use crate::ProtectionType;
use crate::PsciVersion;
use crate::VcpuAArch64;
use crate::VcpuExit;
use crate::VcpuFeature;
use crate::VcpuRegAArch64;
use crate::Vm;
use crate::VmAArch64;
use crate::VmCap;
use crate::PSCI_0_2;

impl Geniezone {
    // Compute the machine type, which should be the IPA range for the VM
    // Ideally, this would take a description of the memory map and return
    // the closest machine type for this VM. Here, we just return the maximum
    // the kernel support.
    pub fn get_vm_type(&self, protection_type: ProtectionType) -> Result<u32> {
        // Safe because we know self is a real geniezone fd
        let ipa_size = match unsafe {
            ioctl_with_val(self, GZVM_CHECK_EXTENSION(), GZVM_CAP_ARM_VM_IPA_SIZE.into())
        } {
            // Not supported? Use 0 as the machine type, which implies 40bit IPA
            ret if ret < 0 => 0,
            ipa => ipa as u32,
        };
        let protection_flag = match protection_type {
            ProtectionType::Unprotected | ProtectionType::UnprotectedWithFirmware | ProtectionType::ProtectedWithCustomFirmware => 0,
            ProtectionType::Protected | ProtectionType::ProtectedWithoutFirmware => {
                GZVM_VM_TYPE_ARM_PROTECTED
            }
        };
        // Use the lower 8 bits representing the IPA space as the machine type
        Ok((ipa_size & GZVM_VM_TYPE_ARM_IPA_SIZE_MASK) | protection_flag)
    }

    /// Get the size of guest physical addresses (IPA) in bits.
    pub fn get_guest_phys_addr_bits(&self) -> u8 {
        // Safe because we know self is a real geniezone fd
        match unsafe { ioctl_with_val(self, GZVM_CHECK_EXTENSION(), GZVM_CAP_ARM_VM_IPA_SIZE.into()) }
        {
            // Default physical address size is 40 bits if the extension is not supported.
            ret if ret <= 0 => 40,
            ipa => ipa as u8,
        }
    }
}

impl GeniezoneVm {
    /// Does platform specific initialization for the GeniezoneVm.
    pub fn init_arch(&self, cfg: &Config) -> Result<()> {
        #[cfg(target_arch = "aarch64")]
        if cfg.mte {
            // Safe because it does not take pointer arguments.
            unsafe { self.enable_raw_capability(GeniezoneCap::ArmMte, 0, &[0, 0, 0, 0])? }
        }
        Ok(())
    }

    /// Checks if a particular `VmCap` is available, or returns None if arch-independent
    /// Vm.check_capability() should handle the check.
    pub fn check_capability_arch(&self, _c: VmCap) -> Option<bool> {
        None
    }

    /// Arch-specific implementation of `Vm::get_pvclock`.  Always returns an error on AArch64.
    pub fn get_pvclock_arch(&self) -> Result<ClockState> {
        // TODO: Geniezone not support pvclock currently
        error!("Geniezone: not support get_pvclock_arch");
        Err(Error::new(EINVAL))
    }

    /// Arch-specific implementation of `Vm::set_pvclock`.  Always returns an error on AArch64.
    pub fn set_pvclock_arch(&self, _state: &ClockState) -> Result<()> {
        // TODO: Geniezone not support pvclock currently
        error!("Geniezone: not support set_pvclock_arch");
        Err(Error::new(EINVAL))
    }

    fn get_protected_vm_info(&self) -> Result<GzvmProtectedVmInfo> {
        let mut info = GzvmProtectedVmInfo {
            firmware_size: 0,
            reserved: [0; 7],
        };
        // Safe because we allocated the struct and we know the kernel won't write beyond the end of
        // the struct or keep a pointer to it.
        unsafe {
            self.enable_raw_capability(
                GeniezoneCap::ArmProtectedVm,
                GZVM_CAP_ARM_PROTECTED_VM_FLAGS_INFO,
                &[&mut info as *mut GzvmProtectedVmInfo as u64, 0, 0, 0],
            )
        }?;
        Ok(info)
    }

    fn set_protected_vm_firmware_ipa(&self, fw_addr: GuestAddress) -> Result<()> {
        // Safe because none of the args are pointers.
        unsafe {
            self.enable_raw_capability(
                GeniezoneCap::ArmProtectedVm,
                GZVM_CAP_ARM_PROTECTED_VM_FLAGS_SET_FW_IPA,
                &[fw_addr.0, 0, 0, 0],
            )
        }
    }

    /// Enable userspace msr. This is not available on ARM, just succeed.
    pub fn enable_userspace_msr(&self) -> Result<()> {
        Ok(())
    }
}

#[repr(C)]
struct GzvmProtectedVmInfo {
    firmware_size: u64,
    reserved: [u64; 7],
}

impl VmAArch64 for GeniezoneVm {
    fn get_hypervisor(&self) -> &dyn Hypervisor {
        &self.geniezone
    }

    fn load_protected_vm_firmware(
        &mut self,
        fw_addr: GuestAddress,
        fw_max_size: u64,
    ) -> Result<()> {
        let info = self.get_protected_vm_info()?;
        if info.firmware_size == 0 {
            Err(Error::new(EINVAL))
        } else {
            if info.firmware_size > fw_max_size {
                return Err(Error::new(ENOMEM));
            }
            self.set_protected_vm_firmware_ipa(fw_addr)
        }
    }

    fn create_vcpu(&self, id: usize) -> Result<Box<dyn VcpuAArch64>> {
        // create_vcpu is declared separately in VmAArch64 and VmX86, so it can return VcpuAArch64
        // or VcpuX86.  But both use the same implementation in GeniezoneVm::create_vcpu.
        Ok(Box::new(GeniezoneVm::create_vcpu(self, id)?))
    }
}

impl GeniezoneVcpu {
    /// Arch-specific implementation of `Vcpu::pvclock_ctrl`.  Always returns an error on AArch64.
    pub fn pvclock_ctrl_arch(&self) -> Result<()> {
        Err(Error::new(EINVAL))
    }

    /// Handles a `GZVM_EXIT_SYSTEM_EVENT` with event type `GZVM_SYSTEM_EVENT_RESET` with the given
    /// event flags and returns the appropriate `VcpuExit` value for the run loop to handle.
    ///
    /// `event_flags` should be one or more of the `GZVM_SYSTEM_EVENT_RESET_FLAG_*` values defined by
    /// Geniezone.
    pub fn system_event_reset(&self, event_flags: u64) -> Result<VcpuExit> {
        if event_flags & GZVM_SYSTEM_EVENT_RESET_FLAG_PSCI_RESET2 != 0 {
            // Read reset_type and cookie from x1 and x2.
            let reset_type = self.get_one_reg(VcpuRegAArch64::X(1))?;
            let cookie = self.get_one_reg(VcpuRegAArch64::X(2))?;
            warn!(
                "PSCI SYSTEM_RESET2 with reset_type={:#x}, cookie={:#x}",
                reset_type, cookie
            );
        }
        Ok(VcpuExit::SystemEventReset)
    }

    fn set_one_geniezone_reg_u64(&self, gzvm_reg_id: GeniezoneVcpuRegister, data: u64) -> Result<()> {
        self.set_one_geniezone_reg(gzvm_reg_id, data.to_ne_bytes().as_slice())
    }

    fn set_one_geniezone_reg(&self, gzvm_reg_id: GeniezoneVcpuRegister, data: &[u8]) -> Result<()> {
        let onereg = gzvm_one_reg {
            id: gzvm_reg_id.into(),
            addr: (data.as_ptr() as usize)
                .try_into()
                .expect("can't represent usize as u64"),
        };
        // Safe because we allocated the struct and we know the kernel will read exactly the size of
        // the struct.
        let ret = unsafe { ioctl_with_ref(self, GZVM_SET_ONE_REG(), &onereg) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    fn get_one_geniezone_reg_u64(&self, gzvm_reg_id: GeniezoneVcpuRegister) -> Result<u64> {
        let mut bytes = 0u64.to_ne_bytes();
        self.get_one_geniezone_reg(gzvm_reg_id, bytes.as_mut_slice())?;
        Ok(u64::from_ne_bytes(bytes))
    }

    fn get_one_geniezone_reg(&self, gzvm_reg_id: GeniezoneVcpuRegister, data: &mut [u8]) -> Result<()> {
        let onereg = gzvm_one_reg {
            id: gzvm_reg_id.into(),
            addr: (data.as_mut_ptr() as usize)
                .try_into()
                .expect("can't represent usize as u64"),
        };

        // Safe because we allocated the struct and we know the kernel will read exactly the size of
        // the struct.
        let ret = unsafe { ioctl_with_ref(self, GZVM_GET_ONE_REG(), &onereg) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }
}

#[cfg(feature = "gdb")]
impl GeniezoneVcpu {

    fn set_one_geniezone_reg_u32(&self, gzvm_reg_id: GeniezoneVcpuRegister, data: u32) -> Result<()> {
        self.set_one_geniezone_reg(gzvm_reg_id, data.to_ne_bytes().as_slice())
    }

    fn set_one_geniezone_reg_u128(&self, gzvm_reg_id: GeniezoneVcpuRegister, data: u128) -> Result<()> {
        self.set_one_geniezone_reg(gzvm_reg_id, data.to_ne_bytes().as_slice())
    }

    fn get_one_geniezone_reg_u32(&self, gzvm_reg_id: GeniezoneVcpuRegister) -> Result<u32> {
        let mut bytes = 0u32.to_ne_bytes();
        self.get_one_geniezone_reg(gzvm_reg_id, bytes.as_mut_slice())?;
        Ok(u32::from_ne_bytes(bytes))
    }

    fn get_one_geniezone_reg_u128(&self, gzvm_reg_id: GeniezoneVcpuRegister) -> Result<u128> {
        let mut bytes = 0u128.to_ne_bytes();
        self.get_one_geniezone_reg(gzvm_reg_id, bytes.as_mut_slice())?;
        Ok(u128::from_ne_bytes(bytes))
    }

    /// Retrieves the value of the currently active "version" of a multiplexed registers.
    fn demux_register(&self, reg: &<GdbArch as Arch>::RegId) -> Result<Option<GeniezoneVcpuRegister>> {
        match *reg {
            AArch64RegId::CCSIDR_EL1 => {
                let csselr = GeniezoneVcpuRegister::try_from(AArch64RegId::CSSELR_EL1)
                    .expect("can't map AArch64RegId::CSSELR_EL1 to GeniezoneVcpuRegister");
                if let Ok(csselr) = self.get_one_geniezone_reg_u64(csselr) {
                    Ok(Some(GeniezoneVcpuRegister::Ccsidr(csselr as u8)))
                } else {
                    Ok(None)
                }
            }
            _ => {
                error!("Register {:?} is not multiplexed", reg);
                Err(Error::new(EINVAL))
            }
        }
    }
}

#[allow(dead_code)]
/// GZVM registers as used by the `GET_ONE_REG`/`SET_ONE_REG` ioctl API
pub enum GeniezoneVcpuRegister {
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
    /// Geniezone Firmware Pseudo-Registers
    Firmware(u16),
    /// Generic System Registers by (Op0, Op1, CRn, CRm, Op2)
    System(u16),
    /// CCSIDR_EL1 Demultiplexed by CSSELR_EL1
    Ccsidr(u8),
}

impl GeniezoneVcpuRegister {
    // Firmware pseudo-registers are part of the ARM KVM interface:
    //     https://docs.kernel.org/virt/kvm/arm/hypercalls.html
    pub const PSCI_VERSION: Self = Self::Firmware(0);
    pub const SMCCC_ARCH_WORKAROUND_1: Self = Self::Firmware(1);
    pub const SMCCC_ARCH_WORKAROUND_2: Self = Self::Firmware(2);
    pub const SMCCC_ARCH_WORKAROUND_3: Self = Self::Firmware(3);
}

/// Gives the `u64` register ID expected by the `GET_ONE_REG`/`SET_ONE_REG` ioctl API.
///
/// See the KVM documentation of those ioctls for details about the format of the register ID.
impl From<GeniezoneVcpuRegister> for u64 {
    fn from(register: GeniezoneVcpuRegister) -> Self {
        const fn reg(size: u64, kind: u64, fields: u64) -> u64 {
            GZVM_REG_ARM64 | size | kind | fields
        }

        const fn gzvm_regs_reg(size: u64, offset: usize) -> u64 {
            let offset = offset / std::mem::size_of::<u32>();

            reg(size, GZVM_REG_ARM_CORE as u64, offset as u64)
        }

        const fn gzvm_reg(offset: usize) -> u64 {
            gzvm_regs_reg(GZVM_REG_SIZE_U64, offset)
        }

        fn user_pt_reg(offset: usize) -> u64 {
            gzvm_regs_reg(
                GZVM_REG_SIZE_U64,
                memoffset::offset_of!(gzvm_regs, regs) + offset,
            )
        }

        fn user_fpsimd_state_reg(size: u64, offset: usize) -> u64 {
            gzvm_regs_reg(size, memoffset::offset_of!(gzvm_regs, fp_regs) + offset)
        }

        const fn reg_u64(kind: u64, fields: u64) -> u64 {
            reg(GZVM_REG_SIZE_U64, kind, fields)
        }

        const fn demux_reg(size: u64, index: u64, value: u64) -> u64 {
            let index = (index << GZVM_REG_ARM_DEMUX_ID_SHIFT) & (GZVM_REG_ARM_DEMUX_ID_MASK as u64);
            let value =
                (value << GZVM_REG_ARM_DEMUX_VAL_SHIFT) & (GZVM_REG_ARM_DEMUX_VAL_MASK as u64);

            reg(size, GZVM_REG_ARM_DEMUX as u64, index | value)
        }

        match register {
            GeniezoneVcpuRegister::X(n @ 0..=30) => {
                let n = std::mem::size_of::<u64>() * (n as usize);

                user_pt_reg(memoffset::offset_of!(user_pt_regs, regs) + n)
            }
            GeniezoneVcpuRegister::X(n) => unreachable!("invalid GeniezoneVcpuRegister Xn index: {n}"),
            GeniezoneVcpuRegister::Sp => user_pt_reg(memoffset::offset_of!(user_pt_regs, sp)),
            GeniezoneVcpuRegister::Pc => user_pt_reg(memoffset::offset_of!(user_pt_regs, pc)),
            GeniezoneVcpuRegister::Pstate => user_pt_reg(memoffset::offset_of!(user_pt_regs, pstate)),
            GeniezoneVcpuRegister::SpEl1 => gzvm_reg(memoffset::offset_of!(gzvm_regs, sp_el1)),
            GeniezoneVcpuRegister::ElrEl1 => gzvm_reg(memoffset::offset_of!(gzvm_regs, elr_el1)),
            GeniezoneVcpuRegister::Spsr(n @ 0..=4) => {
                let n = std::mem::size_of::<u64>() * (n as usize);

                gzvm_reg(memoffset::offset_of!(gzvm_regs, spsr) + n)
            }
            GeniezoneVcpuRegister::Spsr(n) => unreachable!("invalid GeniezoneVcpuRegister Spsr index: {n}"),
            GeniezoneVcpuRegister::V(n @ 0..=31) => {
                let n = std::mem::size_of::<u128>() * (n as usize);

                user_fpsimd_state_reg(
                    GZVM_REG_SIZE_U128,
                    memoffset::offset_of!(user_fpsimd_state, vregs) + n,
                )
            }
            GeniezoneVcpuRegister::V(n) => unreachable!("invalid GeniezoneVcpuRegister Vn index: {n}"),
            GeniezoneVcpuRegister::Fpsr => user_fpsimd_state_reg(
                GZVM_REG_SIZE_U32,
                memoffset::offset_of!(user_fpsimd_state, fpsr),
            ),
            GeniezoneVcpuRegister::Fpcr => user_fpsimd_state_reg(
                GZVM_REG_SIZE_U32,
                memoffset::offset_of!(user_fpsimd_state, fpcr),
            ),
            GeniezoneVcpuRegister::Firmware(n) => reg_u64(GZVM_REG_ARM_FW.into(), n.into()),
            GeniezoneVcpuRegister::System(n) => reg_u64(GZVM_REG_ARM64_SYSREG.into(), n.into()),
            GeniezoneVcpuRegister::Ccsidr(n) => demux_reg(GZVM_REG_SIZE_U32, 0, n.into()),
        }
    }
}

#[cfg(feature = "gdb")]
impl TryFrom<AArch64RegId> for GeniezoneVcpuRegister {
    type Error = Error;

    fn try_from(reg: <GdbArch as Arch>::RegId) -> std::result::Result<Self, Self::Error> {
        // TODO: Geniezone not support gdb currently
        error!("Geniezone: not support gdb");
        Err(Error::new(EINVAL))
    }
}

impl From<VcpuRegAArch64> for GeniezoneVcpuRegister {
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

impl VcpuAArch64 for GeniezoneVcpu {
    fn init(&self, features: &[VcpuFeature]) -> Result<()> {
        let mut gvi = gzvm_vcpu_init {
            target: GZVM_ARM_TARGET_GENERIC_V8,
            features: [0; 7],
        };

        for f in features {
            let shift = match f {
                VcpuFeature::PsciV0_2 => GZVM_ARM_VCPU_PSCI_0_2,
                VcpuFeature::PmuV3 => GZVM_ARM_VCPU_PMU_V3,
                VcpuFeature::PowerOff => GZVM_ARM_VCPU_POWER_OFF,
            };
            gvi.features[0] |= 1 << shift;
        }

        // Safe because we know self.vm is a real geniezone fd
        let check_extension = |ext: u32| -> bool {
            unsafe { ioctl_with_val(&self.vm, GZVM_CHECK_EXTENSION(), ext.into()) == 1 }
        };
        if check_extension(GZVM_CAP_ARM_PTRAUTH_ADDRESS)
            && check_extension(GZVM_CAP_ARM_PTRAUTH_GENERIC)
        {
            gvi.features[0] |= 1 << GZVM_ARM_VCPU_PTRAUTH_ADDRESS;
            gvi.features[0] |= 1 << GZVM_ARM_VCPU_PTRAUTH_GENERIC;
        }

        // Safe because we allocated the struct and we know the kernel will read exactly the size of
        // the struct.
        let ret = unsafe { ioctl_with_ref(self, GZVM_ARM_VCPU_INIT(), &gvi) };
        if ret == 0 {
            Ok(())
        } else {
            errno_result()
        }
    }

    fn init_pmu(&self, irq: u64) -> Result<()> {
        // TODO: Geniezone not support pmu currently
        // temporary return ok since aarch64/src/lib.rs will use this
        Ok(())
    }

    fn has_pvtime_support(&self) -> bool {
        // TODO: Geniezone not support pvtime currently
        return false;
    }

    fn init_pvtime(&self, pvtime_ipa: u64) -> Result<()> {
        // TODO: Geniezone not support pvtime currently
        error!("Geniezone: not support init_pvtime");
        Err(Error::new(EINVAL))
    }

    fn set_one_reg(&self, reg_id: VcpuRegAArch64, data: u64) -> Result<()> {
        self.set_one_geniezone_reg_u64(GeniezoneVcpuRegister::from(reg_id), data)
    }

    fn get_one_reg(&self, reg_id: VcpuRegAArch64) -> Result<u64> {
        self.get_one_geniezone_reg_u64(GeniezoneVcpuRegister::from(reg_id))
    }

    fn get_psci_version(&self) -> Result<PsciVersion> {
        Ok(PSCI_0_2)
    }

    #[cfg(feature = "gdb")]
    fn get_max_hw_bps(&self) -> Result<usize> {
        // TODO: Geniezone not support gdb currently
        error!("Geniezone: not support get_max_hw_bps");
        Err(Error::new(EINVAL))
    }

    #[cfg(feature = "gdb")]
    fn set_guest_debug(&self, addrs: &[GuestAddress], enable_singlestep: bool) -> Result<()> {
        // TODO: Geniezone not support gdb currently
        error!("Geniezone: not support set_gdb_registers");
        Err(Error::new(EINVAL))
    }

    #[cfg(feature = "gdb")]
    fn set_gdb_registers(&self, regs: &<GdbArch as Arch>::Registers) -> Result<()> {
        // TODO: Geniezone not support gdb currently
        error!("Geniezone: not support set_gdb_registers");
        Err(Error::new(EINVAL))
    }

    #[cfg(feature = "gdb")]
    fn get_gdb_registers(&self, regs: &mut <GdbArch as Arch>::Registers) -> Result<()> {
        // TODO: Geniezone not support gdb currently
        error!("Geniezone: not support get_gdb_registers");
        Err(Error::new(EINVAL))
    }

    #[cfg(feature = "gdb")]
    fn set_gdb_register(&self, reg: <GdbArch as Arch>::RegId, data: &[u8]) -> Result<()> {
        // TODO: Geniezone not support gdb currently
        error!("Geniezone: not support set_gdb_register");
        Err(Error::new(EINVAL))
    }

    #[cfg(feature = "gdb")]
    fn get_gdb_register(&self, reg: <GdbArch as Arch>::RegId, data: &mut [u8]) -> Result<usize> {
        // TODO: Geniezone not support gdb currently
        error!("Geniezone: not support get_gdb_register");
        Err(Error::new(EINVAL))
    }
}
