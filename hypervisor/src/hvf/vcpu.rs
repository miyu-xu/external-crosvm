// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::Arc;

use applevisor_sys::hv_error_t;
use applevisor_sys::hv_exit_reason_t;
use applevisor_sys::hv_reg_t;
use applevisor_sys::hv_return_t;
use applevisor_sys::hv_sys_reg_t;
use applevisor_sys::hv_vcpu_create;
use applevisor_sys::hv_vcpu_destroy;
use applevisor_sys::hv_vcpu_exit_t;
use applevisor_sys::hv_vcpu_get_reg;
use applevisor_sys::hv_vcpu_get_simd_fp_reg;
use applevisor_sys::hv_vcpu_get_sys_reg;
use applevisor_sys::hv_vcpu_run;
use applevisor_sys::hv_vcpu_set_reg;
use applevisor_sys::hv_vcpu_set_simd_fp_reg;
use applevisor_sys::hv_vcpu_set_sys_reg;
use applevisor_sys::hv_vcpu_t;
use applevisor_sys::hv_vcpus_exit;
use applevisor_sys::hv_simd_fp_reg_t;
use base::Error;
use base::Event;
use base::Result;
use libc::EINVAL;
use libc::ENOSYS;
use libc::EIO;

use super::vm::check_hv;
use crate::AArch64SysRegId;
use crate::IoEventAddress;
use crate::IoOperation;
use crate::IoParams;
use crate::PsciVersion;
use crate::Vcpu;
use crate::VcpuAArch64;
use crate::VcpuExit;
use crate::VcpuFeature;
use crate::VcpuRegAArch64;
use crate::PSCI_0_2;

/// Pending guest data abort (MMIO) exit state for `handle_mmio`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MmioPending {
    pub gpa: u64,
    pub size: usize,
    pub write: bool,
    pub rt: u8,
}

pub struct HvfVcpu {
    id: usize,
    vcpu: hv_vcpu_t,
    exit: *const hv_vcpu_exit_t,
    pending_mmio: Mutex<Option<MmioPending>>,
    ioevents: Arc<Mutex<HashMap<IoEventAddress, Event>>>,
}

unsafe impl Send for HvfVcpu {}
unsafe impl Sync for HvfVcpu {}

impl HvfVcpu {
    pub fn new(id: usize, ioevents: Arc<Mutex<HashMap<IoEventAddress, Event>>>) -> Result<Self> {
        let mut vcpu: hv_vcpu_t = 0;
        let mut exit: *const hv_vcpu_exit_t = std::ptr::null();
        check_hv(unsafe {
            hv_vcpu_create(
                &mut vcpu,
                &mut exit as *mut *const hv_vcpu_exit_t,
                std::ptr::null_mut(),
            )
        })?;
        Ok(HvfVcpu {
            id,
            vcpu,
            exit,
            pending_mmio: Mutex::new(None),
            ioevents,
        })
    }

    fn q_reg(n: u8) -> Result<hv_simd_fp_reg_t> {
        if n > 31 {
            return Err(Error::new(EINVAL));
        }
        // SAFETY: `hv_simd_fp_reg_t` is a C enum with discriminants Q0..=Q31 == 0..=31.
        Ok(unsafe { std::mem::transmute::<u32, hv_simd_fp_reg_t>(n as u32) })
    }

    fn x_reg(n: u8) -> Result<hv_reg_t> {
        Ok(match n {
            0 => hv_reg_t::X0,
            1 => hv_reg_t::X1,
            2 => hv_reg_t::X2,
            3 => hv_reg_t::X3,
            4 => hv_reg_t::X4,
            5 => hv_reg_t::X5,
            6 => hv_reg_t::X6,
            7 => hv_reg_t::X7,
            8 => hv_reg_t::X8,
            9 => hv_reg_t::X9,
            10 => hv_reg_t::X10,
            11 => hv_reg_t::X11,
            12 => hv_reg_t::X12,
            13 => hv_reg_t::X13,
            14 => hv_reg_t::X14,
            15 => hv_reg_t::X15,
            16 => hv_reg_t::X16,
            17 => hv_reg_t::X17,
            18 => hv_reg_t::X18,
            19 => hv_reg_t::X19,
            20 => hv_reg_t::X20,
            21 => hv_reg_t::X21,
            22 => hv_reg_t::X22,
            23 => hv_reg_t::X23,
            24 => hv_reg_t::X24,
            25 => hv_reg_t::X25,
            26 => hv_reg_t::X26,
            27 => hv_reg_t::X27,
            28 => hv_reg_t::X28,
            29 => hv_reg_t::X29,
            30 => hv_reg_t::X30,
            _ => return Err(Error::new(EINVAL)),
        })
    }

    fn parse_mmio(ex: &applevisor_sys::hv_vcpu_exit_exception_t) -> Option<MmioPending> {
        let ec = ex.syndrome >> 26;
        // Data Abort from lower EL / same EL (taken as MMIO candidates).
        if ec != 0x24 && ec != 0x25 {
            return None;
        }
        let iss = ex.syndrome & 0x01ff_ffff;
        let wnr = (iss >> 6) & 1 != 0;
        let sas = (iss >> 22) & 3;
        let size = 1usize << sas;
        let rt = ((iss >> 5) & 0x1f) as u8;
        Some(MmioPending {
            gpa: ex.physical_address,
            size,
            write: wnr,
            rt,
        })
    }
}

impl Drop for HvfVcpu {
    fn drop(&mut self) {
        unsafe {
            let _ = hv_vcpu_destroy(self.vcpu);
        }
    }
}

impl Vcpu for HvfVcpu {
    fn try_clone(&self) -> Result<Self> {
        Err(Error::new(ENOSYS))
    }

    fn as_vcpu(&self) -> &dyn Vcpu {
        self
    }

    fn run(&mut self) -> Result<VcpuExit> {
        let r = unsafe { hv_vcpu_run(self.vcpu) };
        if r != hv_error_t::HV_SUCCESS as hv_return_t {
            return Err(Error::new(EIO));
        }
        // SAFETY: `exit` is owned by the hypervisor and valid until destroy.
        let exit = unsafe { &*self.exit };
        match exit.reason {
            hv_exit_reason_t::EXCEPTION => {
                if let Some(mmio) = Self::parse_mmio(&exit.exception) {
                    *self.pending_mmio.lock().unwrap() = Some(mmio);
                    return Ok(VcpuExit::Mmio);
                }
                Ok(VcpuExit::Exception)
            }
            hv_exit_reason_t::VTIMER_ACTIVATED => Ok(VcpuExit::Intr),
            hv_exit_reason_t::CANCELED => Ok(VcpuExit::Canceled),
            hv_exit_reason_t::UNKNOWN => Ok(VcpuExit::InternalError),
        }
    }

    fn id(&self) -> usize {
        self.id
    }

    fn set_immediate_exit(&self, exit: bool) {
        if exit {
            let v = self.vcpu;
            unsafe {
                let _ = hv_vcpus_exit(&v, 1);
            }
        }
    }

    fn handle_mmio(
        &self,
        handle_fn: &mut dyn FnMut(IoParams) -> Result<Option<[u8; 8]>>,
    ) -> Result<()> {
        let mmio = self
            .pending_mmio
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| Error::new(EINVAL))?;

        let read_rt_u64 = |rt: u8| -> Result<u64> {
            if rt == 31 {
                let mut v = 0u64;
                check_hv(unsafe {
                    hv_vcpu_get_sys_reg(self.vcpu, hv_sys_reg_t::SP_EL1, &mut v)
                })?;
                Ok(v)
            } else {
                let mut v = 0u64;
                check_hv(unsafe { hv_vcpu_get_reg(self.vcpu, Self::x_reg(rt)?, &mut v) })?;
                Ok(v)
            }
        };

        let write_rt_u64 = |rt: u8, val: u64| -> Result<()> {
            if rt == 31 {
                check_hv(unsafe {
                    hv_vcpu_set_sys_reg(self.vcpu, hv_sys_reg_t::SP_EL1, val)
                })
            } else {
                check_hv(unsafe { hv_vcpu_set_reg(self.vcpu, Self::x_reg(rt)?, val) })
            }
        };

        let op = if mmio.write {
            let v = read_rt_u64(mmio.rt)?;
            let mut data = [0u8; 8];
            data[..mmio.size].copy_from_slice(&v.to_le_bytes()[..mmio.size]);
            IoOperation::Write { data }
        } else {
            IoOperation::Read
        };

        let params = IoParams {
            address: mmio.gpa,
            size: mmio.size,
            operation: op,
        };

        let out = handle_fn(params)?;
        if !mmio.write {
            let mut val = 0u64;
            if let Some(buf) = out {
                let n = mmio.size.min(8);
                val = u64::from_le_bytes([
                    buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
                ]);
                if n < 8 {
                    let cur = read_rt_u64(mmio.rt)?;
                    let mask = !0u64 >> (64 - n * 8);
                    val = (cur & !mask) | (val & mask);
                }
            }
            if mmio.rt != 31 {
                write_rt_u64(mmio.rt, val)?;
            }
        }

        // Advance PC past faulting instruction (AArch64: 4 bytes).
        let mut pc = 0u64;
        check_hv(unsafe { hv_vcpu_get_reg(self.vcpu, hv_reg_t::PC, &mut pc) })?;
        check_hv(unsafe { hv_vcpu_set_reg(self.vcpu, hv_reg_t::PC, pc + 4) })?;

        let addr = IoEventAddress::Mmio(mmio.gpa);
        let map = self.ioevents.lock().unwrap();
        if let Some(evt) = map.get(&addr) {
            let _ = evt.signal();
        }

        Ok(())
    }

    fn handle_io(
        &self,
        _handle_fn: &mut dyn FnMut(IoParams) -> Option<[u8; 8]>,
    ) -> Result<()> {
        Ok(())
    }

    fn on_suspend(&self) -> Result<()> {
        Ok(())
    }

    unsafe fn enable_raw_capability(&self, _cap: u32, _args: &[u64; 4]) -> Result<()> {
        Err(Error::new(ENOSYS))
    }
}

impl VcpuAArch64 for HvfVcpu {
    fn init(&self, features: &[VcpuFeature]) -> Result<()> {
        for f in features {
            match f {
                VcpuFeature::PsciV0_2 => {}
                VcpuFeature::PmuV3 => return Err(Error::new(ENOSYS)),
                VcpuFeature::PowerOff => {}
            }
        }
        Ok(())
    }

    fn init_pmu(&self, _irq: u64) -> Result<()> {
        Err(Error::new(ENOSYS))
    }

    fn has_pvtime_support(&self) -> bool {
        false
    }

    fn init_pvtime(&self, _pvtime_ipa: u64) -> Result<()> {
        Err(Error::new(ENOSYS))
    }

    fn set_one_reg(&self, reg_id: VcpuRegAArch64, data: u64) -> Result<()> {
        match reg_id {
            VcpuRegAArch64::X(31) => Ok(()),
            VcpuRegAArch64::X(n) => {
                let r = Self::x_reg(n)?;
                check_hv(unsafe { hv_vcpu_set_reg(self.vcpu, r, data) })
            }
            VcpuRegAArch64::Sp => check_hv(unsafe {
                hv_vcpu_set_sys_reg(self.vcpu, hv_sys_reg_t::SP_EL1, data)
            }),
            VcpuRegAArch64::Pc => {
                check_hv(unsafe { hv_vcpu_set_reg(self.vcpu, hv_reg_t::PC, data) })
            }
            VcpuRegAArch64::Pstate => {
                check_hv(unsafe { hv_vcpu_set_reg(self.vcpu, hv_reg_t::CPSR, data) })
            }
            VcpuRegAArch64::System(id) => {
                let code = id.encoded() as u32;
                let sys: hv_sys_reg_t = unsafe { std::mem::transmute(code) };
                check_hv(unsafe { hv_vcpu_set_sys_reg(self.vcpu, sys, data) })
            }
        }
    }

    fn get_one_reg(&self, reg_id: VcpuRegAArch64) -> Result<u64> {
        let mut v = 0u64;
        match reg_id {
            VcpuRegAArch64::X(31) => return Ok(0),
            VcpuRegAArch64::X(n) => {
                let r = Self::x_reg(n)?;
                check_hv(unsafe { hv_vcpu_get_reg(self.vcpu, r, &mut v) })?;
            }
            VcpuRegAArch64::Sp => {
                check_hv(unsafe { hv_vcpu_get_sys_reg(self.vcpu, hv_sys_reg_t::SP_EL1, &mut v) })?;
            }
            VcpuRegAArch64::Pc => {
                check_hv(unsafe { hv_vcpu_get_reg(self.vcpu, hv_reg_t::PC, &mut v) })?;
            }
            VcpuRegAArch64::Pstate => {
                check_hv(unsafe { hv_vcpu_get_reg(self.vcpu, hv_reg_t::CPSR, &mut v) })?;
            }
            VcpuRegAArch64::System(id) => {
                let code = id.encoded() as u32;
                let sys: hv_sys_reg_t = unsafe { std::mem::transmute(code) };
                check_hv(unsafe { hv_vcpu_get_sys_reg(self.vcpu, sys, &mut v) })?;
            }
        }
        Ok(v)
    }

    fn set_vector_reg(&self, reg_num: u8, data: u128) -> Result<()> {
        let reg = Self::q_reg(reg_num)?;
        check_hv(unsafe { hv_vcpu_set_simd_fp_reg(self.vcpu, reg, data) })
    }

    fn get_vector_reg(&self, reg_num: u8) -> Result<u128> {
        let reg = Self::q_reg(reg_num)?;
        let mut v: applevisor_sys::hv_simd_fp_uchar16_t = 0;
        check_hv(unsafe { hv_vcpu_get_simd_fp_reg(self.vcpu, reg, &mut v) })?;
        Ok(v)
    }

    fn get_system_regs(&self) -> Result<BTreeMap<AArch64SysRegId, u64>> {
        Ok(BTreeMap::new())
    }

    fn hypervisor_specific_snapshot(&self) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }

    fn hypervisor_specific_restore(&self, _data: serde_json::Value) -> anyhow::Result<()> {
        Ok(())
    }

    fn get_psci_version(&self) -> Result<PsciVersion> {
        Ok(PSCI_0_2)
    }

    fn set_guest_debug(
        &self,
        _addrs: &[vm_memory::GuestAddress],
        _enable_singlestep: bool,
    ) -> Result<()> {
        Err(Error::new(ENOSYS))
    }

    fn get_max_hw_bps(&self) -> Result<usize> {
        Ok(0)
    }

    fn get_cache_info(&self) -> Result<BTreeMap<u8, u64>> {
        Ok(BTreeMap::new())
    }

    fn set_cache_info(&self, _cache_info: BTreeMap<u8, u64>) -> Result<()> {
        Ok(())
    }
}
