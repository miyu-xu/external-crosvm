// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(target_arch = "aarch64")]
use std::arch::asm;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use applevisor_sys::hv_error_t;
use applevisor_sys::hv_exit_reason_t;
use applevisor_sys::hv_reg_t;
use applevisor_sys::hv_return_t;
use applevisor_sys::hv_simd_fp_reg_t;
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
use base::warn;
use base::Error;
use base::Event;
use base::Result;
use libc::EINVAL;
use libc::EIO;
use libc::ENOSYS;

use super::vm::check_hv;
use super::vm::check_hv_quiet;
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
use crate::VcpuSignalHandle;
use crate::VcpuSignalHandleInner;
use crate::PSCI_0_2;

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
}

/// Pending guest data abort (MMIO) exit state for `handle_mmio`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MmioPending {
    pub gpa: u64,
    pub size: usize,
    pub write: bool,
    pub rt: u8,
    pub reg_64: bool,
    pub sign_extend: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VcpuPowerState {
    On,
    Off,
    OnPending { entry: u64, context: u64 },
}

#[derive(Default)]
pub(crate) struct VcpuPowerControl {
    states: Mutex<Vec<VcpuPowerState>>,
    changed: Condvar,
}

impl VcpuPowerControl {
    pub(crate) fn initialize(&self, count: usize) -> Result<()> {
        let mut states = self.states.lock().map_err(|_| Error::new(EIO))?;
        *states = (0..count)
            .map(|id| {
                if id == 0 {
                    VcpuPowerState::On
                } else {
                    VcpuPowerState::Off
                }
            })
            .collect();
        Ok(())
    }

    fn register(&self, id: usize) -> Result<()> {
        let mut states = self.states.lock().map_err(|_| Error::new(EIO))?;
        if states.len() <= id {
            let old_len = states.len();
            states.resize(id + 1, VcpuPowerState::Off);
            if old_len == 0 {
                states[0] = VcpuPowerState::On;
            }
        }
        Ok(())
    }

    fn set_off(&self, id: usize) -> Result<()> {
        let mut states = self.states.lock().map_err(|_| Error::new(EIO))?;
        let state = states.get_mut(id).ok_or_else(|| Error::new(EINVAL))?;
        *state = VcpuPowerState::Off;
        Ok(())
    }
}

impl VcpuPowerControl {
    fn request_on(&self, id: usize, entry: u64, context: u64) -> Result<i64> {
        let mut states = self.states.lock().map_err(|_| Error::new(EIO))?;
        let state = match states.get_mut(id) {
            Some(state) => state,
            None => return Ok(PSCI_RET_INVALID_PARAMS),
        };
        let response = match *state {
            VcpuPowerState::On => PSCI_RET_ALREADY_ON,
            VcpuPowerState::OnPending { .. } => PSCI_RET_ON_PENDING,
            VcpuPowerState::Off => {
                *state = VcpuPowerState::OnPending { entry, context };
                self.changed.notify_all();
                PSCI_RET_SUCCESS
            }
        };
        Ok(response)
    }
}

impl VcpuPowerControl {
    fn affinity_info(&self, id: usize) -> Result<i64> {
        let states = self.states.lock().map_err(|_| Error::new(EIO))?;
        Ok(match states.get(id) {
            Some(VcpuPowerState::On) => PSCI_AFFINITY_LEVEL_ON,
            Some(VcpuPowerState::Off) => PSCI_AFFINITY_LEVEL_OFF,
            Some(VcpuPowerState::OnPending { .. }) => PSCI_AFFINITY_LEVEL_ON_PENDING,
            None => PSCI_RET_INVALID_PARAMS,
        })
    }
}

struct HvfVcpuSignalHandle {
    vcpu: hv_vcpu_t,
    immediate_exit: Arc<AtomicBool>,
}

impl VcpuSignalHandleInner for HvfVcpuSignalHandle {
    fn signal_immediate_exit(&self) {
        self.immediate_exit.store(true, Ordering::Release);
        let vcpu = self.vcpu;
        // SAFETY: Hypervisor.framework supports canceling a running vCPU
        // from another thread with hv_vcpus_exit.
        unsafe {
            let _ = hv_vcpus_exit(&vcpu, 1);
        }
    }
}

pub struct HvfVcpu {
    id: usize,
    vcpu: hv_vcpu_t,
    exit: *const hv_vcpu_exit_t,
    cntfrq: u64,
    summary: Option<ExitSummary>,
    pending_mmio: Mutex<Option<MmioPending>>,
    ioevents: Arc<Mutex<HashMap<IoEventAddress, Event>>>,
    power_control: Arc<VcpuPowerControl>,
    immediate_exit: Arc<AtomicBool>,
}

unsafe impl Send for HvfVcpu {}
unsafe impl Sync for HvfVcpu {}

static HVF_EXIT_LOG_COUNT: AtomicUsize = AtomicUsize::new(0);
static HVF_MMIO_LOG_COUNT: AtomicUsize = AtomicUsize::new(0);

const EC_WFX_TRAP: u64 = 0x01;
const EC_AA64_HVC: u64 = 0x16;
const EC_AA64_SMC: u64 = 0x17;
const EC_SYSTEMREGISTERTRAP: u64 = 0x18;

const TMR_CTL_ENABLE: u64 = 1 << 0;
const TMR_CTL_IMASK: u64 = 1 << 1;

const HCR_TLOR: u64 = 1 << 35;
const HCR_RW: u64 = 1 << 31;
const HCR_TSW: u64 = 1 << 22;
const HCR_TACR: u64 = 1 << 21;
const HCR_TIDCP: u64 = 1 << 20;
const HCR_TSC: u64 = 1 << 19;
const HCR_TID3: u64 = 1 << 18;
const HCR_TWE: u64 = 1 << 14;
const HCR_TWI: u64 = 1 << 13;
const HCR_BSU_IS: u64 = 1 << 10;
const HCR_FB: u64 = 1 << 9;
const HCR_AMO: u64 = 1 << 5;
const HCR_IMO: u64 = 1 << 4;
const HCR_FMO: u64 = 1 << 3;
const HCR_PTW: u64 = 1 << 2;
const HCR_SWIO: u64 = 1 << 1;
const HCR_VM: u64 = 1 << 0;
const HCR_EL2_BITS: u64 = HCR_TSC
    | HCR_TSW
    | HCR_TWE
    | HCR_TWI
    | HCR_VM
    | HCR_BSU_IS
    | HCR_FB
    | HCR_TACR
    | HCR_AMO
    | HCR_SWIO
    | HCR_TIDCP
    | HCR_RW
    | HCR_TLOR
    | HCR_FMO
    | HCR_IMO
    | HCR_PTW
    | HCR_TID3;
const CNTHCTL_EL0VCTEN: u64 = 1 << 1;
const CNTHCTL_EL0PCTEN: u64 = 1 << 0;
const CNTHCTL_EL2_BITS: u64 = CNTHCTL_EL0VCTEN | CNTHCTL_EL0PCTEN;
const AA64PFR0_EL1_EL2EN: u64 = 1 << 8;
const AA64PFR0_EL1_GIC3EN: u64 = 1 << 24;
const AA64PFR1_EL1_SMEMASK: u64 = 3 << 24;
const CNTV_CTL_EL0: AArch64SysRegId =
    AArch64SysRegId::new_unchecked(0b11, 0b011, 0b1110, 0b0011, 0b001);

const PSCI_VERSION: u32 = 0x8400_0000;
const PSCI_CPU_SUSPEND_64: u32 = 0xc400_0001;
const PSCI_CPU_OFF: u32 = 0x8400_0002;
const PSCI_CPU_ON: u32 = 0x8400_0003;
const PSCI_CPU_ON_64: u32 = 0xc400_0003;
const PSCI_AFFINITY_INFO: u32 = 0x8400_0004;
const PSCI_AFFINITY_INFO_64: u32 = 0xc400_0004;
const PSCI_MIGRATE_INFO_TYPE: u32 = 0x8400_0006;
const PSCI_SYSTEM_OFF: u32 = 0x8400_0008;
const PSCI_SYSTEM_RESET: u32 = 0x8400_0009;
const PSCI_FEATURES: u32 = 0x8400_000a;

const PSCI_RET_SUCCESS: i64 = 0;
const PSCI_RET_NOT_SUPPORTED: i64 = -1;
const PSCI_RET_INVALID_PARAMS: i64 = -2;
const PSCI_RET_ALREADY_ON: i64 = -4;
const PSCI_RET_ON_PENDING: i64 = -5;

const PSCI_AFFINITY_LEVEL_ON: i64 = 0;
const PSCI_AFFINITY_LEVEL_OFF: i64 = 1;
const PSCI_AFFINITY_LEVEL_ON_PENDING: i64 = 2;

struct ExitSummary {
    last_log: Instant,
    total: u64,
    mmio: u64,
    psci: u64,
    sysreg: u64,
    wfx: u64,
    vtimer: u64,
    other_exception: u64,
    canceled: u64,
    unknown: u64,
    last_pc: u64,
    last_syndrome: u64,
    last_gpa: u64,
}

impl HvfVcpu {
    pub fn new(
        id: usize,
        ioevents: Arc<Mutex<HashMap<IoEventAddress, Event>>>,
        power_control: Arc<VcpuPowerControl>,
    ) -> Result<Self> {
        let mut vcpu: hv_vcpu_t = 0;
        let mut exit: *const hv_vcpu_exit_t = std::ptr::null();
        let cntfrq = {
            let cntfrq: u64;
            unsafe { asm!("mrs {}, cntfrq_el0", out(reg) cntfrq) };
            cntfrq
        };
        check_hv(unsafe {
            hv_vcpu_create(
                &mut vcpu,
                &mut exit as *mut *const hv_vcpu_exit_t,
                std::ptr::null_mut(),
            )
        })?;
        power_control.register(id)?;
        check_hv(unsafe { hv_vcpu_set_sys_reg(vcpu, hv_sys_reg_t::MPIDR_EL1, id as u64) })?;
        Ok(HvfVcpu {
            id,
            vcpu,
            exit,
            cntfrq,
            summary: std::env::var_os("CROSVM_HVF_EXIT_SUMMARY").map(|_| ExitSummary {
                last_log: Instant::now(),
                total: 0,
                mmio: 0,
                psci: 0,
                sysreg: 0,
                wfx: 0,
                vtimer: 0,
                other_exception: 0,
                canceled: 0,
                unknown: 0,
                last_pc: 0,
                last_syndrome: 0,
                last_gpa: 0,
            }),
            pending_mmio: Mutex::new(None),
            ioevents,
            power_control,
            immediate_exit: Arc::new(AtomicBool::new(false)),
        })
    }

    fn wait_until_powered_on(&self) -> Result<bool> {
        let mut states = self
            .power_control
            .states
            .lock()
            .map_err(|_| Error::new(EIO))?;
        loop {
            if self.immediate_exit.load(Ordering::Acquire) {
                return Ok(false);
            }
            match states.get(self.id).copied() {
                Some(VcpuPowerState::On) => return Ok(true),
                Some(VcpuPowerState::OnPending { entry, context }) => {
                    states[self.id] = VcpuPowerState::On;
                    drop(states);
                    self.write_reg(hv_reg_t::PC, entry)?;
                    self.write_reg(hv_reg_t::X0, context)?;
                    return Ok(true);
                }
                Some(VcpuPowerState::Off) => {
                    let (next_states, _) = self
                        .power_control
                        .changed
                        .wait_timeout(states, Duration::from_millis(50))
                        .map_err(|_| Error::new(EIO))?;
                    states = next_states;
                }
                None => return Err(Error::new(EINVAL)),
            }
        }
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
        let rt = ((iss >> 16) & 0x1f) as u8;
        let reg_64 = ((iss >> 15) & 1) != 0;
        let sign_extend = ((iss >> 21) & 1) != 0;
        Some(MmioPending {
            gpa: ex.physical_address,
            size,
            write: wnr,
            rt,
            reg_64,
            sign_extend,
        })
    }

    fn read_reg(&self, reg: hv_reg_t) -> Result<u64> {
        let mut v = 0u64;
        check_hv(unsafe { hv_vcpu_get_reg(self.vcpu, reg, &mut v) })?;
        Ok(v)
    }

    fn write_reg(&self, reg: hv_reg_t, value: u64) -> Result<()> {
        check_hv(unsafe { hv_vcpu_set_reg(self.vcpu, reg, value) })
    }

    fn read_sys_reg(&self, reg: AArch64SysRegId) -> Result<u64> {
        let mut value = 0u64;
        check_hv(unsafe { hv_vcpu_get_sys_reg(self.vcpu, Self::sys_reg(reg), &mut value) })?;
        Ok(value)
    }

    fn advance_pc(&self) -> Result<()> {
        let pc = self.read_reg(hv_reg_t::PC)?;
        self.write_reg(hv_reg_t::PC, pc.wrapping_add(4))
    }

    fn sys_reg(reg: AArch64SysRegId) -> hv_sys_reg_t {
        unsafe { std::mem::transmute(reg.encoded() as u32) }
    }

    fn trapped_sys_reg(syndrome: u64) -> AArch64SysRegId {
        let op0 = 2 + ((syndrome >> 20) & 0x1) as u8;
        let op1 = ((syndrome >> 14) & 0x7) as u8;
        let crn = ((syndrome >> 10) & 0xf) as u8;
        let crm = ((syndrome >> 1) & 0xf) as u8;
        let op2 = ((syndrome >> 17) & 0x7) as u8;
        AArch64SysRegId::new_unchecked(op0, op1, crn, crm, op2)
    }

    fn psci_ret(code: i64) -> u64 {
        code as u64
    }

    fn handle_psci_call(&self) -> Result<Option<VcpuExit>> {
        let function = self.read_reg(hv_reg_t::X0)? as u32;
        self.advance_pc()?;
        match function {
            PSCI_VERSION => {
                self.write_reg(hv_reg_t::X0, u64::from(PSCI_0_2.minor))?;
                Ok(None)
            }
            PSCI_FEATURES => {
                let requested = self.read_reg(hv_reg_t::X1)? as u32;
                let response = match requested {
                    PSCI_VERSION
                    | PSCI_CPU_SUSPEND_64
                    | PSCI_CPU_OFF
                    | PSCI_CPU_ON
                    | PSCI_CPU_ON_64
                    | PSCI_AFFINITY_INFO
                    | PSCI_AFFINITY_INFO_64
                    | PSCI_MIGRATE_INFO_TYPE
                    | PSCI_SYSTEM_OFF
                    | PSCI_SYSTEM_RESET
                    | PSCI_FEATURES => PSCI_RET_SUCCESS,
                    _ => PSCI_RET_NOT_SUPPORTED,
                };
                self.write_reg(hv_reg_t::X0, Self::psci_ret(response))?;
                Ok(None)
            }
            PSCI_MIGRATE_INFO_TYPE => {
                self.write_reg(hv_reg_t::X0, 2)?;
                Ok(None)
            }
            PSCI_CPU_OFF => {
                self.power_control.set_off(self.id)?;
                Ok(None)
            }
            PSCI_SYSTEM_OFF => {
                let pc = self.read_reg(hv_reg_t::PC)?;
                let lr = self.read_reg(hv_reg_t::X30)?;
                let x1 = self.read_reg(hv_reg_t::X1)?;
                let x2 = self.read_reg(hv_reg_t::X2)?;
                warn!(
                    "HVF PSCI shutdown function=0x{function:08x} pc=0x{pc:016x} lr=0x{lr:016x} x1=0x{x1:016x} x2=0x{x2:016x}"
                );
                Ok(Some(VcpuExit::SystemEventShutdown))
            }
            PSCI_SYSTEM_RESET => {
                let pc = self.read_reg(hv_reg_t::PC)?;
                let lr = self.read_reg(hv_reg_t::X30)?;
                let x1 = self.read_reg(hv_reg_t::X1)?;
                let x2 = self.read_reg(hv_reg_t::X2)?;
                warn!(
                    "HVF PSCI reset function=0x{function:08x} pc=0x{pc:016x} lr=0x{lr:016x} x1=0x{x1:016x} x2=0x{x2:016x}"
                );
                Ok(Some(VcpuExit::SystemEventReset))
            }
            PSCI_CPU_ON | PSCI_CPU_ON_64 => {
                let target = self.read_reg(hv_reg_t::X1)? as usize;
                let entry = self.read_reg(hv_reg_t::X2)?;
                let context = self.read_reg(hv_reg_t::X3)?;
                let response = self.power_control.request_on(target, entry, context)?;
                self.write_reg(hv_reg_t::X0, Self::psci_ret(response))?;
                Ok(None)
            }
            PSCI_AFFINITY_INFO | PSCI_AFFINITY_INFO_64 => {
                let target = self.read_reg(hv_reg_t::X1)? as usize;
                let response = self.power_control.affinity_info(target)?;
                self.write_reg(hv_reg_t::X0, Self::psci_ret(response))?;
                Ok(None)
            }
            PSCI_CPU_SUSPEND_64 => {
                self.write_reg(hv_reg_t::X0, Self::psci_ret(PSCI_RET_NOT_SUPPORTED))?;
                Ok(None)
            }
            _ => {
                warn!("HVF unhandled PSCI function 0x{function:08x}");
                self.write_reg(hv_reg_t::X0, Self::psci_ret(PSCI_RET_NOT_SUPPORTED))?;
                Ok(None)
            }
        }
    }

    fn handle_system_register_trap(&self, syndrome: u64) -> Result<()> {
        let is_read = (syndrome & 1) != 0;
        let rt = ((syndrome >> 5) & 0x1f) as u8;
        let reg = Self::trapped_sys_reg(syndrome);
        let sys = Self::sys_reg(reg);

        self.advance_pc()?;

        if is_read {
            let mut value = 0u64;
            if check_hv_quiet(unsafe { hv_vcpu_get_sys_reg(self.vcpu, sys, &mut value) }).is_err() {
                warn!(
                    "HVF defaulting trapped sysreg read reg={:?} encoded=0x{:x} to zero",
                    reg,
                    reg.encoded()
                );
            }
            if rt < 31 {
                self.write_reg(Self::x_reg(rt)?, value)?;
            }
        } else {
            let value = if rt < 31 {
                self.read_reg(Self::x_reg(rt)?)?
            } else {
                0
            };
            if check_hv_quiet(unsafe { hv_vcpu_set_sys_reg(self.vcpu, sys, value) }).is_err() {
                warn!(
                    "HVF ignoring trapped sysreg write reg={:?} encoded=0x{:x} value=0x{:x}",
                    reg,
                    reg.encoded(),
                    value
                );
            }
        }

        Ok(())
    }

    fn handle_wait_for_event_trap(&self) -> Result<()> {
        self.advance_pc()?;

        let ctl = self.read_sys_reg(CNTV_CTL_EL0)?;
        if (ctl & TMR_CTL_ENABLE) == 0 || (ctl & TMR_CTL_IMASK) != 0 {
            thread::sleep(Duration::from_millis(1));
            return Ok(());
        }

        let cval = self.read_sys_reg(AArch64SysRegId::CNTV_CVAL_EL0)?;
        let now = unsafe { mach_absolute_time() };
        if now >= cval {
            thread::yield_now();
            return Ok(());
        }

        let ticks = u128::from(cval - now);
        let nanos = ticks.saturating_mul(1_000_000_000u128) / u128::from(self.cntfrq.max(1));
        let timeout = Duration::from_nanos(nanos.min(u64::MAX as u128) as u64);
        thread::sleep(timeout.min(Duration::from_millis(1)));
        Ok(())
    }

    fn record_exit(&mut self, exit: &hv_vcpu_exit_t, pc: u64) {
        let Some(summary) = self.summary.as_mut() else {
            return;
        };
        summary.total += 1;
        summary.last_pc = pc;
        match exit.reason {
            hv_exit_reason_t::EXCEPTION => {
                let ec = exit.exception.syndrome >> 26;
                summary.last_syndrome = exit.exception.syndrome;
                summary.last_gpa = exit.exception.physical_address;
                if ec == EC_AA64_HVC || ec == EC_AA64_SMC {
                    summary.psci += 1;
                } else if ec == EC_SYSTEMREGISTERTRAP {
                    summary.sysreg += 1;
                } else if ec == EC_WFX_TRAP {
                    summary.wfx += 1;
                } else if Self::parse_mmio(&exit.exception).is_some() {
                    summary.mmio += 1;
                } else {
                    summary.other_exception += 1;
                }
            }
            hv_exit_reason_t::VTIMER_ACTIVATED => {
                summary.vtimer += 1;
            }
            hv_exit_reason_t::CANCELED => {
                summary.canceled += 1;
            }
            hv_exit_reason_t::UNKNOWN => {
                summary.unknown += 1;
            }
        }

        if summary.last_log.elapsed() >= Duration::from_secs(1) {
            warn!(
                "HVF summary vcpu={} total={} mmio={} psci={} sysreg={} wfx={} vtimer={} other_exc={} canceled={} unknown={} last_pc=0x{:x} last_syndrome=0x{:x} last_gpa=0x{:x}",
                self.id,
                summary.total,
                summary.mmio,
                summary.psci,
                summary.sysreg,
                summary.wfx,
                summary.vtimer,
                summary.other_exception,
                summary.canceled,
                summary.unknown,
                summary.last_pc,
                summary.last_syndrome,
                summary.last_gpa,
            );
            summary.last_log = Instant::now();
        }
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
        loop {
            if !self.wait_until_powered_on()? {
                return Ok(VcpuExit::Canceled);
            }
            let r = unsafe { hv_vcpu_run(self.vcpu) };
            if r != hv_error_t::HV_SUCCESS as hv_return_t {
                let mut pc = 0u64;
                let _ = check_hv(unsafe { hv_vcpu_get_reg(self.vcpu, hv_reg_t::PC, &mut pc) });
                let mut cpsr = 0u64;
                let _ = check_hv(unsafe { hv_vcpu_get_reg(self.vcpu, hv_reg_t::CPSR, &mut cpsr) });
                let mut esr = 0u64;
                let _ = check_hv(unsafe {
                    hv_vcpu_get_sys_reg(self.vcpu, hv_sys_reg_t::ESR_EL1, &mut esr)
                });
                let mut far = 0u64;
                let _ = check_hv(unsafe {
                    hv_vcpu_get_sys_reg(self.vcpu, hv_sys_reg_t::FAR_EL1, &mut far)
                });
                let mut x0 = 0u64;
                let _ = check_hv(unsafe { hv_vcpu_get_reg(self.vcpu, hv_reg_t::X0, &mut x0) });
                warn!(
                    "HVF hv_vcpu_run failed for vcpu {} with code {} pc=0x{:x} cpsr=0x{:x} x0=0x{:x} esr=0x{:x} far=0x{:x}",
                    self.id, r, pc, cpsr, x0, esr, far
                );
                return Err(Error::new(EIO));
            }
            // SAFETY: `exit` is owned by the hypervisor and valid until destroy.
            let exit = unsafe { &*self.exit };
            let mut pc = 0u64;
            let _ = check_hv(unsafe { hv_vcpu_get_reg(self.vcpu, hv_reg_t::PC, &mut pc) });
            self.record_exit(exit, pc);
            if self.summary.is_some() && HVF_EXIT_LOG_COUNT.fetch_add(1, Ordering::Relaxed) < 2048 {
                match exit.reason {
                    hv_exit_reason_t::EXCEPTION => warn!(
                        "HVF exit vcpu={} reason=EXCEPTION pc=0x{:x} syndrome=0x{:x} gpa=0x{:x} gva=0x{:x}",
                        self.id,
                        pc,
                        exit.exception.syndrome,
                        exit.exception.physical_address,
                        exit.exception.virtual_address
                    ),
                    hv_exit_reason_t::VTIMER_ACTIVATED => {
                        warn!("HVF exit vcpu={} reason=VTIMER_ACTIVATED pc=0x{:x}", self.id, pc)
                    }
                    hv_exit_reason_t::CANCELED => {
                        warn!("HVF exit vcpu={} reason=CANCELED pc=0x{:x}", self.id, pc)
                    }
                    hv_exit_reason_t::UNKNOWN => {
                        warn!("HVF exit vcpu={} reason=UNKNOWN pc=0x{:x}", self.id, pc)
                    }
                }
            }
            match exit.reason {
                hv_exit_reason_t::EXCEPTION => {
                    let ec = exit.exception.syndrome >> 26;
                    if ec == EC_WFX_TRAP {
                        self.handle_wait_for_event_trap()?;
                        continue;
                    }
                    if ec == EC_AA64_HVC || ec == EC_AA64_SMC {
                        if let Some(vcpu_exit) = self.handle_psci_call()? {
                            return Ok(vcpu_exit);
                        }
                        continue;
                    }
                    if ec == EC_SYSTEMREGISTERTRAP {
                        self.handle_system_register_trap(exit.exception.syndrome)?;
                        continue;
                    }
                    if let Some(mmio) = Self::parse_mmio(&exit.exception) {
                        *self.pending_mmio.lock().unwrap() = Some(mmio);
                        return Ok(VcpuExit::Mmio);
                    }
                    return Ok(VcpuExit::Exception);
                }
                hv_exit_reason_t::VTIMER_ACTIVATED => return Ok(VcpuExit::Intr),
                hv_exit_reason_t::CANCELED => return Ok(VcpuExit::Canceled),
                hv_exit_reason_t::UNKNOWN => return Ok(VcpuExit::InternalError),
            }
        }
    }

    fn id(&self) -> usize {
        self.id
    }

    fn set_immediate_exit(&self, exit: bool) {
        self.immediate_exit.store(exit, Ordering::Release);
        if exit {
            self.power_control.changed.notify_all();
            let v = self.vcpu;
            unsafe {
                let _ = hv_vcpus_exit(&v, 1);
            }
        }
    }

    fn signal_handle(&self) -> VcpuSignalHandle {
        VcpuSignalHandle {
            inner: Box::new(HvfVcpuSignalHandle {
                vcpu: self.vcpu,
                immediate_exit: self.immediate_exit.clone(),
            }),
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
            let mut v = 0u64;
            check_hv(unsafe { hv_vcpu_get_reg(self.vcpu, Self::x_reg(rt)?, &mut v) })?;
            Ok(v)
        };

        let write_rt_u64 = |rt: u8, val: u64| -> Result<()> {
            check_hv(unsafe { hv_vcpu_set_reg(self.vcpu, Self::x_reg(rt)?, val) })
        };

        let op = if mmio.write {
            let v = if mmio.rt < 31 {
                read_rt_u64(mmio.rt)?
            } else {
                0
            };
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
        if self.summary.is_some() && HVF_MMIO_LOG_COUNT.fetch_add(1, Ordering::Relaxed) < 2048 {
            match (&params.operation, &out) {
                (IoOperation::Write { data }, _) => warn!(
                    "HVF mmio vcpu={} addr=0x{:x} size={} write=true rt={} data={:02x?}",
                    self.id,
                    mmio.gpa,
                    mmio.size,
                    mmio.rt,
                    &data[..mmio.size.min(8)]
                ),
                (_, Some(buf)) => warn!(
                    "HVF mmio vcpu={} addr=0x{:x} size={} write=false rt={} out={:02x?}",
                    self.id,
                    mmio.gpa,
                    mmio.size,
                    mmio.rt,
                    &buf[..mmio.size.min(8)]
                ),
                (_, None) => warn!(
                    "HVF mmio vcpu={} addr=0x{:x} size={} write=false rt={} out=<none>",
                    self.id, mmio.gpa, mmio.size, mmio.rt
                ),
            }
        }
        if !mmio.write {
            let mut val = 0u64;
            if let Some(buf) = out {
                let n = mmio.size.min(8);
                val = u64::from_le_bytes([
                    buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
                ]);
                if n < 8 {
                    let bits = (n * 8) as u32;
                    let mask = (1u64 << bits) - 1;
                    val &= mask;
                    if mmio.sign_extend && bits > 0 {
                        let sign_bit = 1u64 << (bits - 1);
                        if (val & sign_bit) != 0 {
                            val |= !mask;
                        }
                    }
                }
                if !mmio.reg_64 {
                    val &= u32::MAX as u64;
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

    fn handle_io(&self, _handle_fn: &mut dyn FnMut(IoParams) -> Option<[u8; 8]>) -> Result<()> {
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
                VcpuFeature::PowerOff => self.power_control.set_off(self.id)?,
            }
        }

        check_hv(unsafe { hv_vcpu_set_sys_reg(self.vcpu, hv_sys_reg_t::HCR_EL2, HCR_EL2_BITS) })?;
        check_hv(unsafe {
            hv_vcpu_set_sys_reg(self.vcpu, hv_sys_reg_t::CNTHCTL_EL2, CNTHCTL_EL2_BITS)
        })?;

        let mut pfr0 = 0u64;
        check_hv(unsafe {
            hv_vcpu_get_sys_reg(self.vcpu, hv_sys_reg_t::ID_AA64PFR0_EL1, &mut pfr0)
        })?;
        check_hv(unsafe {
            hv_vcpu_set_sys_reg(
                self.vcpu,
                hv_sys_reg_t::ID_AA64PFR0_EL1,
                pfr0 | AA64PFR0_EL1_EL2EN | AA64PFR0_EL1_GIC3EN,
            )
        })?;

        let mut pfr1 = 0u64;
        check_hv(unsafe {
            hv_vcpu_get_sys_reg(self.vcpu, hv_sys_reg_t::ID_AA64PFR1_EL1, &mut pfr1)
        })?;
        check_hv(unsafe {
            hv_vcpu_set_sys_reg(
                self.vcpu,
                hv_sys_reg_t::ID_AA64PFR1_EL1,
                pfr1 & !AA64PFR1_EL1_SMEMASK,
            )
        })?;

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
            VcpuRegAArch64::Sp => {
                check_hv(unsafe { hv_vcpu_set_sys_reg(self.vcpu, hv_sys_reg_t::SP_EL1, data) })
            }
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
