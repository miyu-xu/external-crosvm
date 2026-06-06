// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::ffi::c_void;
use std::arch::x86_64::CpuidResult;
use std::collections::BTreeMap;
use std::convert::TryInto;
use std::mem::size_of;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use base::info;
use base::warn;
use base::Error;
use base::Result;
use libc::EINVAL;
use libc::EIO;
use libc::ENOENT;
use libc::ENXIO;
use vm_memory::GuestAddress;
use winapi::shared::winerror::E_UNEXPECTED;
use winapi::shared::winerror::S_OK;
use windows::Win32::Foundation::WHV_E_INSUFFICIENT_BUFFER;

use super::types::*;
use super::*;
use crate::CpuId;
use crate::CpuIdEntry;
use crate::DebugRegs;
use crate::Fpu;
use crate::IoOperation;
use crate::IoParams;
use crate::Regs;
use crate::Sregs;
use crate::Vcpu;
use crate::VcpuExit;
use crate::VcpuX86_64;
use crate::Xsave;

const WHPX_EXIT_DIRECTION_MMIO_READ: u8 = 0;
const WHPX_EXIT_DIRECTION_MMIO_WRITE: u8 = 1;
const WHPX_EXIT_DIRECTION_PIO_IN: u8 = 0;
const WHPX_EXIT_DIRECTION_PIO_OUT: u8 = 1;

/// Cap detailed WHPX IO/MMIO failure logs (set `CROSWVM_WHPX_IO_DEBUG=1` to enable).
const WHPX_IO_DEBUG_MAX_LOGS: u32 = 128;
static WHPX_IO_DEBUG_LOG_COUNT: AtomicU32 = AtomicU32::new(0);

fn whpx_io_debug_enabled() -> bool {
    std::env::var_os("CROSWVM_WHPX_IO_DEBUG").is_some_and(|v| !v.is_empty() && v != "0")
}

fn format_emulator_status(status: &WHV_EMULATOR_STATUS) -> String {
    let bits = unsafe { status.__bindgen_anon_1 };
    format!(
        "as_u32={} ok={} internal_fail={} io_cb_fail={} mmio_cb_fail={} translate_fail={}",
        unsafe { status.AsUINT32 },
        bits.EmulationSuccessful(),
        bits.InternalEmulationFailure(),
        bits.IoPortCallbackFailed(),
        bits.MemoryCallbackFailed(),
        bits.TranslateGvaPageCallbackFailed(),
    )
}

fn log_whpx_io_failure(kind: &str, detail: &str) {
    if !whpx_io_debug_enabled() {
        return;
    }
    let n = WHPX_IO_DEBUG_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
    if n >= WHPX_IO_DEBUG_MAX_LOGS {
        return;
    }
    warn!("whpx {} emulation failed: {}", kind, detail);
}

/// This is the whpx instruction emulator, useful for deconstructing
/// io & memory port instructions. Whpx does not do this automatically.
struct SafeInstructionEmulator {
    handle: WHV_EMULATOR_HANDLE,
}

impl SafeInstructionEmulator {
    fn new() -> Result<SafeInstructionEmulator> {
        const EMULATOR_CALLBACKS: WHV_EMULATOR_CALLBACKS = WHV_EMULATOR_CALLBACKS {
            Size: size_of::<WHV_EMULATOR_CALLBACKS>() as u32,
            Reserved: 0,
            WHvEmulatorIoPortCallback: Some(SafeInstructionEmulator::io_port_cb),
            WHvEmulatorMemoryCallback: Some(SafeInstructionEmulator::memory_cb),
            WHvEmulatorGetVirtualProcessorRegisters: Some(
                SafeInstructionEmulator::get_virtual_processor_registers_cb,
            ),
            WHvEmulatorSetVirtualProcessorRegisters: Some(
                SafeInstructionEmulator::set_virtual_processor_registers_cb,
            ),
            WHvEmulatorTranslateGvaPage: Some(SafeInstructionEmulator::translate_gva_page_cb),
        };
        let mut handle: WHV_EMULATOR_HANDLE = std::ptr::null_mut();
        // safe because pass in valid callbacks and a emulator handle for the kernel to place the
        // allocated handle into.
        check_whpx!(unsafe { WHvEmulatorCreateEmulator(&EMULATOR_CALLBACKS, &mut handle) })?;

        Ok(SafeInstructionEmulator { handle })
    }
}

trait InstructionEmulatorCallbacks {
    extern "stdcall" fn io_port_cb(
        context: *mut ::std::os::raw::c_void,
        io_access: *mut WHV_EMULATOR_IO_ACCESS_INFO,
    ) -> HRESULT;
    extern "stdcall" fn memory_cb(
        context: *mut ::std::os::raw::c_void,
        memory_access: *mut WHV_EMULATOR_MEMORY_ACCESS_INFO,
    ) -> HRESULT;
    extern "stdcall" fn get_virtual_processor_registers_cb(
        context: *mut ::std::os::raw::c_void,
        register_names: *const WHV_REGISTER_NAME,
        register_count: UINT32,
        register_values: *mut WHV_REGISTER_VALUE,
    ) -> HRESULT;
    extern "stdcall" fn set_virtual_processor_registers_cb(
        context: *mut ::std::os::raw::c_void,
        register_names: *const WHV_REGISTER_NAME,
        register_count: UINT32,
        register_values: *const WHV_REGISTER_VALUE,
    ) -> HRESULT;
    extern "stdcall" fn translate_gva_page_cb(
        context: *mut ::std::os::raw::c_void,
        gva: WHV_GUEST_VIRTUAL_ADDRESS,
        translate_flags: WHV_TRANSLATE_GVA_FLAGS,
        translation_result: *mut WHV_TRANSLATE_GVA_RESULT_CODE,
        gpa: *mut WHV_GUEST_PHYSICAL_ADDRESS,
    ) -> HRESULT;
}

/// Context passed into the instruction emulator when trying io or mmio emulation.
/// Since we need this for set/get registers and memory translation,
/// a single context is used that captures all necessary contextual information for the operation.
struct InstructionEmulatorContext<'a> {
    vm_partition: Arc<SafePartition>,
    index: u32,
    handle_mmio: Option<&'a mut dyn FnMut(IoParams) -> Result<Option<[u8; 8]>>>,
    handle_io: Option<&'a mut dyn FnMut(IoParams) -> Option<[u8; 8]>>,
}

impl InstructionEmulatorCallbacks for SafeInstructionEmulator {
    extern "stdcall" fn io_port_cb(
        context: *mut ::std::os::raw::c_void,
        io_access: *mut WHV_EMULATOR_IO_ACCESS_INFO,
    ) -> HRESULT {
        // unsafe because windows could decide to call this at any time.
        // However, we trust the kernel to call this while the vm/vcpu is valid.
        let ctx = unsafe { &mut *(context as *mut InstructionEmulatorContext) };
        // safe because we trust the kernel to fill in the io_access
        let io_access_info = unsafe { &mut *io_access };
        let address = io_access_info.Port.into();
        let size = io_access_info.AccessSize as usize;
        match io_access_info.Direction {
            WHPX_EXIT_DIRECTION_PIO_IN => {
                if let Some(handle_io) = &mut ctx.handle_io {
                    if let Some(data) = handle_io(IoParams {
                        address,
                        size,
                        operation: IoOperation::Read,
                    }) {
                        // Safe because we know this is an io_access_info field of u32,
                        //  so casting as a &mut [u8] of len 4 is safe.
                        let buffer = unsafe {
                            std::slice::from_raw_parts_mut(
                                &mut io_access_info.Data as *mut u32 as *mut u8,
                                4,
                            )
                        };
                        buffer[..size].copy_from_slice(&data[..size]);
                    }
                    S_OK
                } else {
                    E_UNEXPECTED
                }
            }
            WHPX_EXIT_DIRECTION_PIO_OUT => {
                if let Some(handle_io) = &mut ctx.handle_io {
                    handle_io(IoParams {
                        address,
                        size,
                        operation: IoOperation::Write {
                            data: (io_access_info.Data as u64).to_ne_bytes(),
                        },
                    });
                    S_OK
                } else {
                    E_UNEXPECTED
                }
            }
            _ => E_UNEXPECTED,
        }
    }
    extern "stdcall" fn memory_cb(
        context: *mut ::std::os::raw::c_void,
        memory_access: *mut WHV_EMULATOR_MEMORY_ACCESS_INFO,
    ) -> HRESULT {
        // unsafe because windows could decide to call this at any time.
        // However, we trust the kernel to call this while the vm/vcpu is valid.
        let ctx = unsafe { &mut *(context as *mut InstructionEmulatorContext) };
        // safe because we trust the kernel to fill in the memory_access
        let memory_access_info = unsafe { &mut *memory_access };
        let address = memory_access_info.GpaAddress;
        let size = memory_access_info.AccessSize as usize;
        match memory_access_info.Direction {
            WHPX_EXIT_DIRECTION_MMIO_READ => {
                ctx.handle_mmio
                    .as_mut()
                    .map_or(E_UNEXPECTED, |handle_mmio| {
                        handle_mmio(IoParams {
                            address,
                            size,
                            operation: IoOperation::Read,
                        })
                        .map_err(|e| {
                            error!("handle_mmio failed with {e}");
                            e
                        })
                        .ok()
                        .flatten()
                        .map_or(E_UNEXPECTED, |data| {
                            memory_access_info.Data = data;
                            S_OK
                        })
                    })
            }
            WHPX_EXIT_DIRECTION_MMIO_WRITE => {
                ctx.handle_mmio
                    .as_mut()
                    .map_or(E_UNEXPECTED, |handle_mmio| {
                        handle_mmio(IoParams {
                            address,
                            size,
                            operation: IoOperation::Write {
                                data: memory_access_info.Data,
                            },
                        })
                        .map_err(|e| {
                            error!("handle_mmio failed with {e}");
                            e
                        })
                        .map_or(E_UNEXPECTED, |_| S_OK)
                    })
            }
            _ => E_UNEXPECTED,
        }
    }
    extern "stdcall" fn get_virtual_processor_registers_cb(
        context: *mut ::std::os::raw::c_void,
        register_names: *const WHV_REGISTER_NAME,
        register_count: UINT32,
        register_values: *mut WHV_REGISTER_VALUE,
    ) -> HRESULT {
        // unsafe because windows could decide to call this at any time.
        // However, we trust the kernel to call this while the vm/vcpu is valid.
        let ctx = unsafe { &*(context as *const InstructionEmulatorContext) };
        // safe because the ctx has a weak reference to the vm partition, which should be
        // alive longer than the ctx
        unsafe {
            WHvGetVirtualProcessorRegisters(
                ctx.vm_partition.partition,
                ctx.index,
                register_names,
                register_count,
                register_values,
            )
        }
    }
    extern "stdcall" fn set_virtual_processor_registers_cb(
        context: *mut ::std::os::raw::c_void,
        register_names: *const WHV_REGISTER_NAME,
        register_count: UINT32,
        register_values: *const WHV_REGISTER_VALUE,
    ) -> HRESULT {
        // unsafe because windows could decide to call this at any time.
        // However, we trust the kernel to call this while the vm/vcpu is valid.
        let ctx = unsafe { &*(context as *const InstructionEmulatorContext) };
        // safe because the ctx has a weak reference to the vm partition, which should be
        // alive longer than the ctx
        unsafe {
            WHvSetVirtualProcessorRegisters(
                ctx.vm_partition.partition,
                ctx.index,
                register_names,
                register_count,
                register_values,
            )
        }
    }
    extern "stdcall" fn translate_gva_page_cb(
        context: *mut ::std::os::raw::c_void,
        gva: WHV_GUEST_VIRTUAL_ADDRESS,
        translate_flags: WHV_TRANSLATE_GVA_FLAGS,
        translation_result_code: *mut WHV_TRANSLATE_GVA_RESULT_CODE,
        gpa: *mut WHV_GUEST_PHYSICAL_ADDRESS,
    ) -> HRESULT {
        // unsafe because windows could decide to call this at any time.
        // However, we trust the kernel to call this while the vm/vcpu is valid.
        let ctx = unsafe { &*(context as *const InstructionEmulatorContext) };
        let mut translation_result: WHV_TRANSLATE_GVA_RESULT = Default::default();
        // safe because the ctx has a weak reference to the vm partition, which should be
        // alive longer than the ctx
        let ret = unsafe {
            WHvTranslateGva(
                ctx.vm_partition.partition,
                ctx.index,
                gva,
                translate_flags,
                &mut translation_result,
                gpa,
            )
        };
        if ret == S_OK {
            // safe assuming the kernel passed in a valid result_code ptr
            unsafe {
                *translation_result_code = translation_result.ResultCode;
            }
        }
        ret
    }
}

impl Drop for SafeInstructionEmulator {
    fn drop(&mut self) {
        // safe because we own the instruction emulator
        check_whpx!(unsafe { WHvEmulatorDestroyEmulator(self.handle) }).unwrap();
    }
}

// we can send and share the instruction emulator over threads safely even though it is void*.
unsafe impl Send for SafeInstructionEmulator {}
unsafe impl Sync for SafeInstructionEmulator {}

struct SafeVirtualProcessor {
    vm_partition: Arc<SafePartition>,
    index: u32,
}

impl SafeVirtualProcessor {
    fn new(vm_partition: Arc<SafePartition>, index: u32) -> Result<SafeVirtualProcessor> {
        // safe since the vm partition should be valid.
        check_whpx!(unsafe { WHvCreateVirtualProcessor(vm_partition.partition, index, 0) })?;
        Ok(SafeVirtualProcessor {
            vm_partition,
            index,
        })
    }
}

impl Drop for SafeVirtualProcessor {
    fn drop(&mut self) {
        // safe because we are the owner of this windows virtual processor.
        check_whpx!(unsafe { WHvDeleteVirtualProcessor(self.vm_partition.partition, self.index,) })
            .unwrap();
    }
}

pub struct WhpxVcpu {
    index: u32,
    safe_virtual_processor: Arc<SafeVirtualProcessor>,
    vm_partition: Arc<SafePartition>,
    last_exit_context: Arc<WHV_RUN_VP_EXIT_CONTEXT>,
    // must be arc, since we cannot "dupe" an instruction emulator similar to a handle.
    instruction_emulator: Arc<SafeInstructionEmulator>,
    tsc_frequency: Option<u64>,
    apic_frequency: Option<u32>,
}

impl WhpxVcpu {
    /// The SafePartition passed in is weak, so that there is no circular references.
    /// However, the SafePartition should be valid as long as this VCPU is alive. The index
    /// is the index for this vcpu.
    pub(super) fn new(vm_partition: Arc<SafePartition>, index: u32) -> Result<WhpxVcpu> {
        let safe_virtual_processor = SafeVirtualProcessor::new(vm_partition.clone(), index)?;
        let instruction_emulator = SafeInstructionEmulator::new()?;
        Ok(WhpxVcpu {
            index,
            safe_virtual_processor: Arc::new(safe_virtual_processor),
            vm_partition,
            last_exit_context: Arc::new(Default::default()),
            instruction_emulator: Arc::new(instruction_emulator),
            tsc_frequency: None,
            apic_frequency: None,
        })
    }

    pub fn set_frequencies(&mut self, tsc_frequency: Option<u64>, lapic_frequency: u32) {
        self.tsc_frequency = tsc_frequency;
        self.apic_frequency = Some(lapic_frequency);
    }

    /// QEMU's whpx_cpu_synchronize_post_reset / whpx_set_registers(WHPX_LEVEL_RESET_STATE):
    /// pushes complete vCPU state (GPRs, segments, control regs, FPU, XMM, APIC_BASE MSR)
    /// in ONE WHvSetVirtualProcessorRegisters call BEFORE the first WHvRunVirtualProcessor.
    /// For AP vCPUs, also sets RIP to a HLT instruction so WHvRunVirtualProcessor exits
    /// immediately rather than entering the OVMF reset-vector PAUSE spin-loop.
    pub fn qemu_push_reset_state(&self) -> Result<()> {
        let gpr = WhpxRegs::get_register_names();   // 18 regs
        let sreg = WhpxSregs::get_register_names(); // 16 regs (includes CR8)
        let fpu = WhpxFpu::get_register_names();    // 26 regs

        let total = gpr.len() + sreg.len() + fpu.len() + 2; // +XCR0 +ApicBase
        let mut names = Vec::with_capacity(total);
        names.extend_from_slice(gpr);
        names.extend_from_slice(sreg);
        names.extend_from_slice(fpu);
        names.push(WHV_REGISTER_NAME_WHvX64RegisterXCr0);
        names.push(WHV_REGISTER_NAME_WHvX64RegisterApicBase);

        let mut values = vec![WHV_REGISTER_VALUE::default(); total];
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition, self.index,
                names.as_ptr(), total as u32, values.as_mut_ptr(),
            )
        })?;

        // For AP vCPUs: enable interrupt window notifications.  WHPX will exit
        // WHvRunVirtualProcessor when the AP is ready to receive interrupts,
        // creating a window where INIT/SIPI can be delivered.  Without these
        // periodic exits, the AP stays stuck in OVMF's PAUSE spin-loop and
        // WHPX cannot deliver INIT/SIPI to it.
        if self.index > 0 {
            // Request interrupt notification: WHPX exits when interrupts can be injected.
            const NOTIFY_REG: WHV_REGISTER_NAME = WHV_REGISTER_NAME_WHvX64RegisterDeliverabilityNotifications;
            let mut notify = WHV_X64_DELIVERABILITY_NOTIFICATIONS_REGISTER::default();
            unsafe {
                notify.__bindgen_anon_1.set_InterruptNotification(1);
            }
            let val = WHV_REGISTER_VALUE { DeliverabilityNotifications: notify };
            let _ = check_whpx!(unsafe {
                WHvSetVirtualProcessorRegisters(
                    self.vm_partition.partition, self.index,
                    &NOTIFY_REG, 1, &val,
                )
            });
        }

        let apic_base: u64 = 0xFEE0_0000u64
            | (1u64 << 11)  // EN: APIC enabled
            | (1u64 << 10)  // EXDE: x2APIC mode
            | if self.index == 0 { 1u64 << 8 } else { 0u64 }; // BSP flag
        let apic_idx = total - 1;
        values[apic_idx].Reg64 = apic_base;

        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition, self.index,
                names.as_ptr(), total as u32, values.as_ptr(),
            )
        })
    }

    /// Push reset state to a specific target vCPU (used from BSP INIT trap handler).
    fn qemu_push_reset_state_for_target(&self, target: u32) -> Result<()> {
        let gpr = WhpxRegs::get_register_names();
        let sreg = WhpxSregs::get_register_names();
        let fpu = WhpxFpu::get_register_names();
        let total = gpr.len() + sreg.len() + fpu.len() + 2;
        let mut names = Vec::with_capacity(total);
        names.extend_from_slice(gpr);
        names.extend_from_slice(sreg);
        names.extend_from_slice(fpu);
        names.push(WHV_REGISTER_NAME_WHvX64RegisterXCr0);
        names.push(WHV_REGISTER_NAME_WHvX64RegisterApicBase);
        let mut values = vec![WHV_REGISTER_VALUE::default(); total];
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition, target,
                names.as_ptr(), total as u32, values.as_mut_ptr(),
            )
        })?;
        unsafe {
            values[total - 1].Reg64 = 0xFEE0_0000u64 | (1u64 << 11) | (1u64 << 10);
            if values[total - 2].Reg64 == 0 { values[total - 2].Reg64 = 0x7; }
        }
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition, target,
                names.as_ptr(), total as u32, values.as_ptr(),
            )
        })
    }

    /// Set per-vCPU APIC ID via WHvSetVirtualProcessorInterruptControllerState2.
    /// QEMU's whpx_apic_put: whpx_lapic_state has fields[N].data at offset N*16.
    /// Crosvm WhpxLapicState maps this as regs[N*4] (= offset N*16 bytes).
    ///   APIC ID:   register 0x2 → regs[8]
    ///   Version:   register 0x3 → regs[12]
    ///   SVR:       register 0xF → regs[60]
    pub fn set_apic_id(&self, apic_id: u32) -> Result<()> {
        let mut state = [0u32; 1024];
        // Read current state first (preserves WHPX-initialized values)
        check_whpx!(unsafe {
            WHvGetVirtualProcessorInterruptControllerState2(
                self.vm_partition.partition, self.index,
                state.as_mut_ptr() as *mut c_void, (state.len() * 4) as u32,
                std::ptr::null_mut(),
            )
        })?;
        // Set APIC ID in QEMU format: regs[register_index * 4]
        state[0x2 * 4] = apic_id << 24;  // APIC ID register
        state[0x3 * 4] = 0x00050014;     // APIC version 0x14, max LVT=5
        state[0xF * 4] = 0x1FF;          // SVR: APIC enabled, spurious=0xFF
        let ret = check_whpx!(unsafe {
            WHvSetVirtualProcessorInterruptControllerState2(
                self.vm_partition.partition, self.index,
                state.as_ptr() as *const c_void, (state.len() * 4) as u32,
            )
        });
        match ret {
            Ok(()) => info!("whpx: vcpu={} APIC ID={}", self.index, apic_id),
            Err(ref e) => info!("whpx: vcpu={} APIC ID set: {}", self.index, e),
        }
        ret
    }

    /// Apply SIPI vector for x2APIC non-16-bit entry: flat protected mode.
    /// The non-16-bit startup vector at BFF35000 is 32-bit code, expects flat CS (base=0).
    /// Sets CS to flat 32-bit ring0, DS/ES/SS to flat data, RIP to the target address.
    pub fn apply_sipi_flat(&self, target_addr: u64) -> Result<()> {
        // Build flat 32-bit code segment: base=0, limit=4GB, D=1 (32-bit)
        let mut cs = WHV_X64_SEGMENT_REGISTER {
            Base: 0,
            Limit: 0xFFFFF,  // 4GB in 4K pages
            Selector: 0x10,   // typical ring0 code
            ..Default::default()
        };
        unsafe {
            let mut a = cs.__bindgen_anon_1.__bindgen_anon_1;
            a.set_SegmentType(0xB);       // code, exec/read, accessed
            a.set_NonSystemSegment(1);     // S=1
            a.set_DescriptorPrivilegeLevel(0);
            a.set_Present(1);
            a.set_Default(1);              // D=1: 32-bit default operand size
            a.set_Granularity(1);          // G=1: 4KB granularity
            cs.__bindgen_anon_1.__bindgen_anon_1 = a;
        }
        // Build flat data segment
        let mut ds = WHV_X64_SEGMENT_REGISTER {
            Base: 0, Limit: 0xFFFFF, Selector: 0x18, ..Default::default()
        };
        unsafe {
            let mut a = ds.__bindgen_anon_1.__bindgen_anon_1;
            a.set_SegmentType(0x3);        // data, read/write, accessed
            a.set_NonSystemSegment(1);
            a.set_DescriptorPrivilegeLevel(0);
            a.set_Present(1);
            a.set_Default(1);
            a.set_Granularity(1);
            ds.__bindgen_anon_1.__bindgen_anon_1 = a;
        }

        const REGS: [WHV_REGISTER_NAME; 7] = [
            WHV_REGISTER_NAME_WHvX64RegisterRip,
            WHV_REGISTER_NAME_WHvX64RegisterCs,
            WHV_REGISTER_NAME_WHvX64RegisterDs,
            WHV_REGISTER_NAME_WHvX64RegisterEs,
            WHV_REGISTER_NAME_WHvX64RegisterSs,
            WHV_REGISTER_NAME_WHvX64RegisterCr0,
            WHV_REGISTER_NAME_WHvX64RegisterCr4,
        ];
        // CR0: PE=1 (protected mode), PG=0 (paging off), NE=1, ET=1
        let cr0: u64 = (1 << 0) | (1 << 5) | (1 << 4);
        let cr4: u64 = 0;
        let vals = [
            WHV_REGISTER_VALUE { Reg64: target_addr },          // RIP
            WHV_REGISTER_VALUE { Segment: cs },                  // CS
            WHV_REGISTER_VALUE { Segment: ds },                  // DS
            WHV_REGISTER_VALUE { Segment: ds },                  // ES
            WHV_REGISTER_VALUE { Segment: ds },                  // SS
            WHV_REGISTER_VALUE { Reg64: cr0 },                   // CR0
            WHV_REGISTER_VALUE { Reg64: cr4 },                   // CR4
        ];
        info!("whpx: vcpu={} apply_sipi_flat RIP=0x{:x}", self.index, target_addr);
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition, self.index, REGS.as_ptr(), 7, vals.as_ptr(),
            )
        })
    }

    /// Apply SIPI vector: set CS:IP so the vCPU starts at `vector << 12` in real mode.
    pub fn apply_sipi_vector(&self, vector: u32) -> Result<()> {
        let sipi_base = (vector as u64) << 12;
        let sipi_sel = (vector as u16) << 8;
        const REGS: [WHV_REGISTER_NAME; 2] = [
            WHV_REGISTER_NAME_WHvX64RegisterRip,
            WHV_REGISTER_NAME_WHvX64RegisterCs,
        ];
        let mut cs = WHV_X64_SEGMENT_REGISTER {
            Base: sipi_base, Limit: 0xFFFF, Selector: sipi_sel,
            ..Default::default()
        };
        unsafe {
            let mut a = cs.__bindgen_anon_1.__bindgen_anon_1;
            a.set_SegmentType(0x0B); a.set_NonSystemSegment(1);
            a.set_DescriptorPrivilegeLevel(0); a.set_Present(1);
            cs.__bindgen_anon_1.__bindgen_anon_1 = a;
        }
        let vals = [WHV_REGISTER_VALUE { Reg64: 0 }, WHV_REGISTER_VALUE { Segment: cs }];
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition, self.index,
                REGS.as_ptr(), 2, vals.as_ptr(),
            )
        })
    }

    /// QEMU's whpx_vcpu_kick_out_of_hlt: clear all suspend bits in Activity State.
    /// WHPX puts AP vCPUs into StartupSuspend (bit 0, Wait-for-SIPI) or
    /// HaltSuspend (bit 1) in X2Apic mode. Without clearing these, the vCPU
    /// never executes instructions — appears as "stuck in PAUSE loop" (0 exits).
    pub fn kick_out_of_halt(&self) -> Result<()> {
        const REG: WHV_REGISTER_NAME = WHV_REGISTER_NAME_WHvRegisterInternalActivityState;
        let mut val = WHV_REGISTER_VALUE::default();
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition, self.index, &REG, 1, &mut val,
            )
        })?;
        let act = unsafe { val.InternalActivity.AsUINT64 };
        if act != 0 {
            // Clear ALL suspend bits: StartupSuspend(0), HaltSuspend(1), IdleSuspend(2)
            unsafe {
                val.InternalActivity.__bindgen_anon_1.set_StartupSuspend(0);
                val.InternalActivity.__bindgen_anon_1.set_HaltSuspend(0);
                val.InternalActivity.__bindgen_anon_1.set_IdleSuspend(0);
            }
            check_whpx!(unsafe {
                WHvSetVirtualProcessorRegisters(
                    self.vm_partition.partition, self.index, &REG, 1, &val,
                )
            })?;
            info!("whpx: vcpu={} suspend bits cleared (was 0x{:x})", self.index, act);
        }
        Ok(())
    }

    /// Partition accessor for cross-vCPU operations.
    pub fn partition_handle(&self) -> WHV_PARTITION_HANDLE {
        self.vm_partition.partition
    }

    /// Read RIP/DX/RFLAGS of another vCPU (for BSP diagnostic from AP thread).
    pub fn read_other_rip(&self, vp_index: u32) -> Result<u64> {
        const REG: WHV_REGISTER_NAME = WHV_REGISTER_NAME_WHvX64RegisterRip;
        let mut val = WHV_REGISTER_VALUE::default();
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition, vp_index, &REG, 1, &mut val,
            )
        })?;
        Ok(unsafe { val.Reg64 })
    }

    pub fn read_other_regs(&self, vp_index: u32) -> Result<(u64, u64, u64)> {
        const REGS: [WHV_REGISTER_NAME; 3] = [
            WHV_REGISTER_NAME_WHvX64RegisterRip,
            WHV_REGISTER_NAME_WHvX64RegisterRdx,
            WHV_REGISTER_NAME_WHvX64RegisterRflags,
        ];
        let mut vals = [WHV_REGISTER_VALUE::default(); 3];
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition, vp_index, REGS.as_ptr(), 3, vals.as_mut_ptr(),
            )
        })?;
        Ok(unsafe { (vals[0].Reg64, vals[1].Reg64, vals[2].Reg64) })
    }
    pub fn read_activity_state(&self) -> Result<u64> {
        const REG: WHV_REGISTER_NAME = WHV_REGISTER_NAME_WHvRegisterInternalActivityState;
        let mut val = WHV_REGISTER_VALUE::default();
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition, self.index, &REG, 1, &mut val,
            )
        })?;
        Ok(unsafe { val.InternalActivity.AsUINT64 })
    }

    /// Read RIP + CS of this vCPU for SMP INIT/SIPI diagnostics.
    pub fn read_initial_ip(&self) -> Result<(u64, u64, u16)> {
        const REG_NAMES: [WHV_REGISTER_NAME; 3] = [
            WHV_REGISTER_NAME_WHvX64RegisterRip,
            WHV_REGISTER_NAME_WHvX64RegisterCs,
            WHV_REGISTER_NAME_WHvX64RegisterRflags,
        ];
        let mut values = [WHV_REGISTER_VALUE::default(); 3];
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                REG_NAMES.as_ptr(),
                3,
                values.as_mut_ptr(),
            )
        })?;
        let rip = unsafe { values[0].Reg64 };
        let cs_base = unsafe { values[1].Segment.Base };
        let cs_sel = unsafe { values[1].Segment.Selector };
        Ok((rip, cs_base, cs_sel))
    }

    /// QEMU alignment: read all register groups, then write them back in ONE call.
    /// QEMU's whpx_init_vcpu sets cpu->vcpu_dirty=true, triggering whpx_set_registers()
    /// which reads the vCPU state from QEMU's software model and pushes it to WHPX.
    /// This initialize-then-push round-trip may be required for WHPX to finalize
    /// per-vCPU APIC identity state needed for INIT/SIPI delivery.
    pub fn init_vcpu_regs_roundtrip(&self) -> Result<()> {
        let gpr_names = WhpxRegs::get_register_names();
        let sreg_names = WhpxSregs::get_register_names();
        let fpu_names = WhpxFpu::get_register_names();

        let total = gpr_names.len() + sreg_names.len() + fpu_names.len();
        let mut names: Vec<WHV_REGISTER_NAME> = Vec::with_capacity(total);
        names.extend_from_slice(gpr_names);
        names.extend_from_slice(sreg_names);
        names.extend_from_slice(fpu_names);

        let mut values = vec![WHV_REGISTER_VALUE::default(); total];
        // Read current state from WHPX
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                names.as_ptr(),
                total as u32,
                values.as_mut_ptr(),
            )
        })?;
        // Write it back — this round-trip triggers WHPX per-vCPU state finalization
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                names.as_ptr(),
                total as u32,
                values.as_ptr(),
            )
        })
    }

    /// Reads WHvRegisterPendingInterruption from the live vCPU state (not last exit snapshot).
    pub fn interruption_pending_live(&self) -> Result<bool> {
        const REG_NAMES: [WHV_REGISTER_NAME; 1] =
            [WHV_REGISTER_NAME_WHvRegisterPendingInterruption];
        let mut value = WHV_REGISTER_VALUE::default();
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                REG_NAMES.as_ptr(),
                REG_NAMES.len() as u32,
                &mut value as *mut WHV_REGISTER_VALUE,
            )
        })?;
        let pending = unsafe {
            value
                .PendingInterruption
                .__bindgen_anon_1
                .InterruptionPending()
        };
        Ok(pending != 0)
    }

    /// Returns `(eflags_if, interrupt_shadow, interruption_pending)` from the last vCPU exit.
    pub fn interrupt_delivery_state(&self) -> (bool, bool, bool) {
        let pending = unsafe {
            self.last_exit_context
                .VpContext
                .ExecutionState
                .__bindgen_anon_1
                .InterruptionPending()
        };
        let shadow = unsafe {
            self.last_exit_context
                .VpContext
                .ExecutionState
                .__bindgen_anon_1
                .InterruptShadow()
        };
        const IF_MASK: u64 = 0x00000200;
        let eflags_if =
            (self.last_exit_context.VpContext.Rflags & IF_MASK) != 0;
        (eflags_if, shadow != 0, pending != 0)
    }

    /// Log guest RIP and exception details from the last WHPX vCPU exit (boot debugging).
    pub fn log_last_exit(&self, tag: &str) {
        let ctx = &self.last_exit_context;
        let rip = ctx.VpContext.Rip;
        match ctx.ExitReason {
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonException => {
                let exc = unsafe { ctx.__bindgen_anon_1.VpException };
                info!(
                    "whpx: {} Exception type={:#x} rip={:#x} err={:#x} param={:#x}",
                    tag, exc.ExceptionType, rip, exc.ErrorCode, exc.ExceptionParameter
                );
            }
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonUnrecoverableException => {
                info!("whpx: {} UnrecoverableException rip={:#x}", tag, rip);
            }
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonUnsupportedFeature => {
                let feat = unsafe { ctx.__bindgen_anon_1.UnsupportedFeature };
                info!(
                    "whpx: {} UnsupportedFeature code={:?} rip={:#x}",
                    tag, feat.FeatureCode, rip
                );
            }
            other => {
                info!("whpx: {} exit_reason={} rip={:#x}", tag, other, rip);
            }
        }
    }

    /// Reload CR3 so the guest TLB picks up host-side guest RAM updates (virtio rings).
    pub fn flush_tlb(&self) -> Result<()> {
        let sregs = self.get_sregs()?;
        self.set_sregs(&sregs)
    }

    fn advance_rip(&mut self) -> Result<()> {
        let rip = self.last_exit_context.VpContext.Rip
            + self.last_exit_context.VpContext.InstructionLength() as u64;
        let reg_name = WHV_REGISTER_NAME_WHvX64RegisterRip;
        let value = WHV_REGISTER_VALUE { Reg64: rip };
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                &reg_name,
                1,
                &value as *const WHV_REGISTER_VALUE,
            )
        })
    }

    /// Complete a trapped LAPIC INIT/SIPI ICR write. QEMU's approach: intercepts
    /// the MSR write, cancels target AP vCPUs from their PAUSE loop, and pushes the
    /// SIPI startup vector directly via WHvSetVirtualProcessorRegisters.
    /// This avoids WHPX deadlock when internal INIT delivery cannot wake a vCPU
    /// stuck in an exit-less spin loop.
    fn handle_apic_init_sipi_trap(&mut self) -> Result<()> {
        if self.last_exit_context.ExitReason
            != WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonX64ApicInitSipiTrap
        {
            return Err(Error::new(EINVAL));
        }

        const APIC_DM_INIT: u64 = 5;
        const APIC_DM_SIPI: u64 = 6;
        const APIC_DEST_ALLINC: u64 = 2;
        const APIC_DEST_ALLBUT: u64 = 3;

        let icr = unsafe { self.last_exit_context.__bindgen_anon_1.ApicInitSipi.ApicIcr };
        let delivery_mode = (icr >> 8) & 0x7;
        let dest_shorthand = (icr >> 18) & 0x3;
        let vector = (icr & 0xFF) as u32;

        let kind = if delivery_mode == APIC_DM_INIT { "INIT" } else { "SIPI" };
        info!("whpx: vcpu={} {} ICR=0x{:016x} sh={} vec={}", self.index, kind, icr, dest_shorthand, vector);

        // Build list of target vCPU indices based on destination shorthand.
        let processor_count = self.vm_partition.processor_count;
        let targets: Vec<u32> = if dest_shorthand == 0 {
            // Physical destination: use x2APIC APIC ID from ICR
            let dest_id = ((icr >> 56) & 0xFF) as u32;
            if dest_id < processor_count { vec![dest_id] } else { vec![] }
        } else if dest_shorthand == APIC_DEST_ALLINC {
            (0..processor_count).collect()
        } else if dest_shorthand == APIC_DEST_ALLBUT {
            (0..processor_count).filter(|&i| i != self.index).collect()
        } else {
            vec![]  // DEST_SELF = no-op for SMP bringup
        };

        for &target in &targets {
            if delivery_mode == APIC_DM_INIT {
                // QEMU's do_cpu_init: push reset state to dormant AP vCPU.
                // This resets the AP's registers so when it enters WHvRunVirtualProcessor
                // (after SIPI), it has a clean INIT state.
                info!("whpx: vcpu={} INIT -> pushing reset state to vcpu={}", self.index, target);
                let _ = self.qemu_push_reset_state_for_target(target);
            } else if delivery_mode == APIC_DM_SIPI && vector != 0 {
                info!("whpx: vcpu={} SIPI -> vcpu={} vector=0x{:x}", self.index, target, vector);
                WHV_SIPI_VECTOR.store(vector, Ordering::Release);
                WHV_SIPI_READY.store(true, Ordering::Release);
            }
        }

        // QEMU does NOT advance RIP for INIT/SIPI — WHPX re-processes the MSR
        // write internally after the VMM handler completes.  Advancing RIP skips
        // the MSR write that WHPX is still processing, causing deadlock.
        // Only advance RIP for non-INIT/SIPI delivery modes.
        if delivery_mode != APIC_DM_INIT && delivery_mode != APIC_DM_SIPI {
            self.advance_rip()?;
        }
        Ok(())
    }

    /// Handle reading the MSR with id `id`. For unsupported MSRs, return 0 (RAZ) instead
    /// of injecting #GP — a #GP before the kernel's exception handler is set up can cause
    /// a silent triple fault before earlycon output, making the guest appear hung.
    fn handle_msr_read(&mut self, id: u32) -> Result<()> {
        if self.last_exit_context.ExitReason
            != WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonX64MsrAccess
        {
            return Err(Error::new(EINVAL));
        }

        // QEMU kernel-irqchip=off: handle x2APIC MSR reads (0x800-0x8FF).
        // With LocalApicEmulationMode=None, WHPX does not emulate x2APIC and
        // all APIC MSR accesses cause #GP exits. We return 0 (RAZ) for most,
        // and the per-vCPU x2APIC ID for MSR 0x802.
        let value = match id {
            HV_X64_MSR_TSC_FREQUENCY => self.tsc_frequency.unwrap_or(0),
            HV_X64_MSR_APIC_FREQUENCY => self.apic_frequency.unwrap_or(0) as u64,
            // x2APIC MSRs: return RAZ/WI for reads, per-vCPU ID for 0x802
            0x802 => self.index as u64, // x2APIC ID
            0x803 => 0x0005_0014u64,    // APIC version (0x14, max LVT=5)
            0x808 | 0x80A | 0x80D | 0x80E | 0x80F
            | 0x828 | 0x82F..=0x837 | 0x838 | 0x839 | 0x83E => 0,
            _ => {
                warn!("whpx: RDMSR 0x{:x} unsupported, returning 0", id);
                0
            }
        };

        let rip = self.last_exit_context.VpContext.Rip
            + self.last_exit_context.VpContext.InstructionLength() as u64;

        const REG_NAMES: [WHV_REGISTER_NAME; 3] = [
            WHV_REGISTER_NAME_WHvX64RegisterRip,
            WHV_REGISTER_NAME_WHvX64RegisterRax,
            WHV_REGISTER_NAME_WHvX64RegisterRdx,
        ];

        let values = vec![
            WHV_REGISTER_VALUE { Reg64: rip },
            WHV_REGISTER_VALUE { Reg64: (value & 0xffffffff) },
            WHV_REGISTER_VALUE { Reg64: (value >> 32) },
        ];

        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                &REG_NAMES as *const WHV_REGISTER_NAME,
                REG_NAMES.len() as u32,
                values.as_ptr() as *const WHV_REGISTER_VALUE,
            )
        })
    }

    /// Handle writing the MSR with id `id`. For unsupported MSRs, silently ignore (WI)
    /// instead of injecting #GP — same rationale as `handle_msr_read`.
    fn handle_msr_write(&mut self, id: u32, value: u64) -> Result<()> {
        if self.last_exit_context.ExitReason
            != WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonX64MsrAccess
        {
            return Err(Error::new(EINVAL));
        }

        // QEMU kernel-irqchip=off: intercept x2APIC MSR writes (0x800-0x8FF).
        // Most APIC MSRs are silently ignored (WI). MSR 0x830 (ICR) is handled
        // with full INIT/SIPI delivery via WHvSetVirtualProcessorRegisters + cancel.
        let handled = match id {
            HV_X64_MSR_TSC_INVARIANT_CONTROL => true,
            // x2APIC MSRs: silently ignore writes (WI) for all except ICR
            0x802 | 0x803 | 0x808 | 0x80A | 0x80B | 0x80D | 0x80E | 0x80F
            | 0x828 | 0x82F..=0x837 | 0x838 | 0x839 | 0x83E => true,
            // x2APIC ICR (Interrupt Command Register) — full INIT/SIPI handling
            0x830 => {
                let delivery = (value >> 8) & 0x7; // 5=INIT, 6=SIPI
                let vector = (value & 0xFF) as u32;
                let shorthand = (value >> 18) & 0x3;
                let dest = ((value >> 56) & 0xFF) as u32;
                let proc_count = self.vm_partition.processor_count;

                let targets: Vec<u32> = if shorthand == 0 {
                    if dest < proc_count { vec![dest] } else { vec![] }
                } else if shorthand == 2 {
                    (0..proc_count).collect()
                } else if shorthand == 3 {
                    (0..proc_count).filter(|&i| i != self.index).collect()
                } else { vec![] };

                let kind = if delivery == 5 { "INIT" } else if delivery == 6 { "SIPI" } else { "FIXED" };
                info!("whpx: MSR-ICR vcpu={} {} value=0x{:016x} sh={} targets={:?}", self.index, kind, value, shorthand, targets);

                for &t in &targets {
                    if delivery == 6 && vector != 0 {
                        // SIPI: set target CS:IP to vector<<12
                        let seg = (vector as u16) << 8;
                        let base = (vector as u64) << 12;
                        info!("whpx: SIPI vcpu={} CS:IP={:04X}:0000", t, seg);
                        let mut cs = WHV_X64_SEGMENT_REGISTER { Base: base, Limit: 0xFFFF, Selector: seg, ..Default::default() };
                        unsafe {
                            let mut a = cs.__bindgen_anon_1.__bindgen_anon_1;
                            a.set_SegmentType(0x0B); a.set_NonSystemSegment(1); a.set_Present(1);
                            cs.__bindgen_anon_1.__bindgen_anon_1 = a;
                        }
                        const R: [WHV_REGISTER_NAME; 2] = [WHV_REGISTER_NAME_WHvX64RegisterRip, WHV_REGISTER_NAME_WHvX64RegisterCs];
                        let v = [WHV_REGISTER_VALUE { Reg64: 0 }, WHV_REGISTER_VALUE { Segment: cs }];
                        let _ = check_whpx!(unsafe { WHvSetVirtualProcessorRegisters(self.vm_partition.partition, t, R.as_ptr(), 2, v.as_ptr()) });
                    }
                    unsafe { WHvCancelRunVirtualProcessor(self.vm_partition.partition, t, 0); }
                }
                true
            }
            _ => {
                warn!("whpx: WRMSR 0x{:x} = 0x{:x} unsupported, ignoring", id, value);
                true
            }
        };

        if handled {
            let rip = self.last_exit_context.VpContext.Rip
                + self.last_exit_context.VpContext.InstructionLength() as u64;
            const REG_NAMES: [WHV_REGISTER_NAME; 1] = [WHV_REGISTER_NAME_WHvX64RegisterRip];
            let values = vec![WHV_REGISTER_VALUE { Reg64: rip }];
            check_whpx!(unsafe {
                WHvSetVirtualProcessorRegisters(
                    self.vm_partition.partition, self.index,
                    &REG_NAMES as *const WHV_REGISTER_NAME,
                    REG_NAMES.len() as u32,
                    values.as_ptr() as *const WHV_REGISTER_VALUE,
                )
            })
        } else {
            Err(Error::new(EINVAL))
        }
    }

    fn inject_gp_fault(&self) -> Result<()> {
        const REG_NAMES: [WHV_REGISTER_NAME; 1] = [WHV_REGISTER_NAME_WHvRegisterPendingEvent];

        let mut event = WHV_REGISTER_VALUE {
            ExceptionEvent: WHV_X64_PENDING_EXCEPTION_EVENT {
                __bindgen_anon_1: Default::default(),
            },
        };
        // safe because we have enough space for all the registers
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                &REG_NAMES as *const WHV_REGISTER_NAME,
                REG_NAMES.len() as u32,
                &mut event as *mut WHV_REGISTER_VALUE,
            )
        })?;

        if unsafe { event.ExceptionEvent.__bindgen_anon_1.EventPending() } != 0 {
            error!("Unable to inject gp fault because pending exception exists");
            return Err(Error::new(EINVAL));
        }

        let mut pending_exception = unsafe { event.ExceptionEvent.__bindgen_anon_1 };

        pending_exception.set_EventPending(1);
        // GP faults set error code
        pending_exception.set_DeliverErrorCode(1);
        // GP fault error code is 0 unless the fault is segment related
        pending_exception.ErrorCode = 0;
        // This must be set to WHvX64PendingEventException
        pending_exception
            .set_EventType(WHV_X64_PENDING_EVENT_TYPE_WHvX64PendingEventException as u32);
        // GP fault vector is 13
        const GP_VECTOR: u32 = 13;
        pending_exception.set_Vector(GP_VECTOR);

        let event = WHV_REGISTER_VALUE {
            ExceptionEvent: WHV_X64_PENDING_EXCEPTION_EVENT {
                __bindgen_anon_1: pending_exception,
            },
        };

        // safe because we have enough space for all the registers
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                &REG_NAMES as *const WHV_REGISTER_NAME,
                REG_NAMES.len() as u32,
                &event as *const WHV_REGISTER_VALUE,
            )
        })
    }

    /// Like [`Vcpu::handle_io`], but also supplies an MMIO handler for guest memory operands
    /// in the emulated PIO instruction (e.g. `ins`). Without this, WHPX sets
    /// `MemoryCallbackFailed` because `memory_cb` sees `handle_mmio: None`.
    pub fn handle_io_with_mmio(
        &self,
        handle_io_fn: &mut dyn FnMut(IoParams) -> Option<[u8; 8]>,
        handle_mmio_fn: &mut dyn FnMut(IoParams) -> Result<Option<[u8; 8]>>,
    ) -> Result<()> {
        let mut status: WHV_EMULATOR_STATUS = Default::default();
        // SAFETY: Only called after an X64IoPortAccess VP exit; union field matches ExitReason.
        let io_ctx = unsafe { &self.last_exit_context.__bindgen_anon_1.IoPortAccess };
        let port = io_ctx.PortNumber;
        let access = unsafe { io_ctx.AccessInfo.__bindgen_anon_1 };
        let is_write = access.IsWrite();
        let access_size = access.AccessSize();
        let mut ctx = InstructionEmulatorContext {
            vm_partition: self.vm_partition.clone(),
            index: self.index,
            handle_mmio: Some(handle_mmio_fn),
            handle_io: Some(handle_io_fn),
        };
        let hr = unsafe {
            WHvEmulatorTryIoEmulation(
                self.instruction_emulator.handle,
                &mut ctx as *mut _ as *mut c_void,
                &self.last_exit_context.VpContext,
                io_ctx,
                &mut status,
            )
        };
        if hr != S_OK {
            log_whpx_io_failure(
                "io",
                &format!(
                    "vcpu={} HRESULT=0x{:08x} port=0x{:x} dir={} size={} rip=0x{:x} {} insn_len={}",
                    self.index,
                    hr as u32,
                    port,
                    if is_write != 0 { "out" } else { "in" },
                    access_size,
                    self.last_exit_context.VpContext.Rip,
                    format_emulator_status(&status),
                    io_ctx.InstructionByteCount,
                ),
            );
            return Err(Error::new(hr));
        }
        let success = unsafe { status.__bindgen_anon_1.EmulationSuccessful() > 0 };
        if success {
            Ok(())
        } else {
            log_whpx_io_failure(
                "io",
                &format!(
                    "vcpu={} port=0x{:x} dir={} size={} rip=0x{:x} {} insn_len={}",
                    self.index,
                    port,
                    if is_write != 0 { "out" } else { "in" },
                    access_size,
                    self.last_exit_context.VpContext.Rip,
                    format_emulator_status(&status),
                    io_ctx.InstructionByteCount,
                ),
            );
            Err(Error::new(unsafe { status.AsUINT32 }))
        }
    }
}

impl Vcpu for WhpxVcpu {
    /// Makes a shallow clone of this `Vcpu`.
    fn try_clone(&self) -> Result<Self> {
        Ok(WhpxVcpu {
            index: self.index,
            safe_virtual_processor: self.safe_virtual_processor.clone(),
            vm_partition: self.vm_partition.clone(),
            last_exit_context: self.last_exit_context.clone(),
            instruction_emulator: self.instruction_emulator.clone(),
            tsc_frequency: self.tsc_frequency,
            apic_frequency: self.apic_frequency,
        })
    }

    fn as_vcpu(&self) -> &dyn Vcpu {
        self
    }

    /// Returns the vcpu id.
    fn id(&self) -> usize {
        self.index.try_into().unwrap()
    }

    /// Exits the vcpu immediately if exit is true
    fn set_immediate_exit(&self, exit: bool) {
        if exit {
            // safe because we own this whpx virtual processor index, and assume the vm partition is
            // still valid
            unsafe {
                WHvCancelRunVirtualProcessor(self.vm_partition.partition, self.index, 0);
            }
        }
    }

    /// Signals to the hypervisor that this guest is being paused by userspace. On some hypervisors,
    /// this is used to control the pvclock. On WHPX, we handle it separately with virtio-pvclock.
    /// So the correct implementation here is to do nothing.
    fn on_suspend(&self) -> Result<()> {
        Ok(())
    }

    /// Enables a hypervisor-specific extension on this Vcpu.  `cap` is a constant defined by the
    /// hypervisor API (e.g., kvm.h).  `args` are the arguments for enabling the feature, if any.
    unsafe fn enable_raw_capability(&self, _cap: u32, _args: &[u64; 4]) -> Result<()> {
        // Whpx does not support raw capability on the vcpu.
        Err(Error::new(ENXIO))
    }

    /// This function should be called after `Vcpu::run` returns `VcpuExit::Mmio`.
    ///
    /// Once called, it will determine whether a mmio read or mmio write was the reason for the mmio
    /// exit, call `handle_fn` with the respective IoOperation to perform the mmio read or
    /// write, and set the return data in the vcpu so that the vcpu can resume running.
    fn handle_mmio(
        &self,
        handle_fn: &mut dyn FnMut(IoParams) -> Result<Option<[u8; 8]>>,
    ) -> Result<()> {
        let mut status: WHV_EMULATOR_STATUS = Default::default();
        // SAFETY: Only called after a MemoryAccess VP exit; union field matches ExitReason.
        let mem_ctx = unsafe { &self.last_exit_context.__bindgen_anon_1.MemoryAccess };
        let mem_access_type = unsafe { mem_ctx.AccessInfo.__bindgen_anon_1.AccessType() };
        let mut ctx = InstructionEmulatorContext {
            vm_partition: self.vm_partition.clone(),
            index: self.index,
            handle_mmio: Some(handle_fn),
            handle_io: None,
        };
        // safe as long as all callbacks occur before this fn returns.
        let hr = unsafe {
            WHvEmulatorTryMmioEmulation(
                self.instruction_emulator.handle,
                &mut ctx as *mut _ as *mut c_void,
                &self.last_exit_context.VpContext,
                mem_ctx,
                &mut status,
            )
        };
        if hr != S_OK {
            log_whpx_io_failure(
                "mmio",
                &format!(
                    "vcpu={} HRESULT=0x{:08x} rip=0x{:x} gpa=0x{:x} {}",
                    self.index,
                    hr as u32,
                    self.last_exit_context.VpContext.Rip,
                    mem_ctx.Gpa,
                    format_emulator_status(&status),
                ),
            );
            return Err(Error::new(hr));
        }
        // safe because we trust the kernel to fill in the union field properly.
        let success = unsafe { status.__bindgen_anon_1.EmulationSuccessful() > 0 };
        if success {
            Ok(())
        } else {
            log_whpx_io_failure(
                "mmio",
                &format!(
                    "vcpu={} rip=0x{:x} gpa=0x{:x} dir={} {}",
                    self.index,
                    self.last_exit_context.VpContext.Rip,
                    mem_ctx.Gpa,
                    mem_access_type,
                    format_emulator_status(&status),
                ),
            );
            self.inject_gp_fault()?;
            // safe because we trust the kernel to fill in the union field properly.
            Err(Error::new(unsafe { status.AsUINT32 }))
        }
    }

    /// Once called, it will determine whether an io in or io out was the reason for the io exit,
    /// call `handle_fn` with the respective IoOperation to perform the io in or io out,
    /// and set the return data in the vcpu so that the vcpu can resume running.
    fn handle_io(&self, handle_fn: &mut dyn FnMut(IoParams) -> Option<[u8; 8]>) -> Result<()> {
        let mut status: WHV_EMULATOR_STATUS = Default::default();
        // SAFETY: Only called after an X64IoPortAccess VP exit; union field matches ExitReason.
        let io_ctx = unsafe { &self.last_exit_context.__bindgen_anon_1.IoPortAccess };
        let port = io_ctx.PortNumber;
        // SAFETY: AccessInfo is a union; firmware IO exits use the bitfield layout.
        let access = unsafe { io_ctx.AccessInfo.__bindgen_anon_1 };
        let is_write = access.IsWrite();
        let access_size = access.AccessSize();
        let mut ctx = InstructionEmulatorContext {
            vm_partition: self.vm_partition.clone(),
            index: self.index,
            handle_mmio: None,
            handle_io: Some(handle_fn),
        };
        // safe as long as all callbacks occur before this fn returns.
        let hr = unsafe {
            WHvEmulatorTryIoEmulation(
                self.instruction_emulator.handle,
                &mut ctx as *mut _ as *mut c_void,
                &self.last_exit_context.VpContext,
                io_ctx,
                &mut status,
            )
        };
        if hr != S_OK {
            log_whpx_io_failure(
                "io",
                &format!(
                    "vcpu={} HRESULT=0x{:08x} port=0x{:x} dir={} size={} rip=0x{:x} {} insn_len={}",
                    self.index,
                    hr as u32,
                    port,
                    if is_write != 0 { "out" } else { "in" },
                    access_size,
                    self.last_exit_context.VpContext.Rip,
                    format_emulator_status(&status),
                    io_ctx.InstructionByteCount,
                ),
            );
            return Err(Error::new(hr));
        }
        // safe because we trust the kernel to fill in the union field properly.
        let success = unsafe { status.__bindgen_anon_1.EmulationSuccessful() > 0 };
        if success {
            Ok(())
        } else {
            log_whpx_io_failure(
                "io",
                &format!(
                    "vcpu={} port=0x{:x} dir={} size={} rip=0x{:x} {} insn_len={}",
                    self.index,
                    port,
                    if is_write != 0 { "out" } else { "in" },
                    access_size,
                    self.last_exit_context.VpContext.Rip,
                    format_emulator_status(&status),
                    io_ctx.InstructionByteCount,
                ),
            );
            // safe because we trust the kernel to fill in the union field properly.
            Err(Error::new(unsafe { status.AsUINT32 }))
        }
    }

    #[allow(non_upper_case_globals)]
    fn run(&mut self) -> Result<VcpuExit> {
        // safe because we own this whpx virtual processor index, and assume the vm partition is
        // still valid
        let exit_context_ptr = Arc::as_ptr(&self.last_exit_context);
        check_whpx!(unsafe {
            WHvRunVirtualProcessor(
                self.vm_partition.partition,
                self.index,
                exit_context_ptr as *mut WHV_RUN_VP_EXIT_CONTEXT as *mut c_void,
                size_of::<WHV_RUN_VP_EXIT_CONTEXT>() as u32,
            )
        })?;

        match self.last_exit_context.ExitReason {
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonMemoryAccess => Ok(VcpuExit::Mmio),
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonX64IoPortAccess => Ok(VcpuExit::Io),
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonUnrecoverableException => {
                Ok(VcpuExit::UnrecoverableException)
            }
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonInvalidVpRegisterValue => {
                Ok(VcpuExit::InvalidVpRegister)
            }
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonUnsupportedFeature => {
                Ok(VcpuExit::UnsupportedFeature)
            }
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonX64InterruptWindow => {
                Ok(VcpuExit::IrqWindowOpen)
            }
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonX64Halt => Ok(VcpuExit::Hlt),
            // additional exits that are configurable
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonX64ApicEoi => {
                // safe because we trust the kernel to fill in the union field properly.
                let vector = unsafe {
                    self.last_exit_context
                        .__bindgen_anon_1
                        .ApicEoi
                        .InterruptVector as u8
                };
                Ok(VcpuExit::IoapicEoi { vector })
            }
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonX64MsrAccess => {
                // Safe because we know this was an MSR access exit.
                let id = unsafe { self.last_exit_context.__bindgen_anon_1.MsrAccess.MsrNumber };

                // Safe because we know this was an MSR access exit
                let is_write = unsafe {
                    self.last_exit_context
                        .__bindgen_anon_1
                        .MsrAccess
                        .AccessInfo
                        .__bindgen_anon_1
                        .IsWrite()
                        == 1
                };
                if is_write {
                    // Safe because we know this was an MSR access exit
                    let value = unsafe {
                        // WRMSR writes the contents of registers EDX:EAX into the 64-bit model
                        // specific register
                        (self.last_exit_context.__bindgen_anon_1.MsrAccess.Rdx << 32)
                            | (self.last_exit_context.__bindgen_anon_1.MsrAccess.Rax & 0xffffffff)
                    };
                    self.handle_msr_write(id, value)?;
                } else {
                    self.handle_msr_read(id)?;
                }
                Ok(VcpuExit::MsrAccess)
            }
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonX64Cpuid => {
                // Safe because we know this was a CPUID exit.
                let entry = unsafe {
                    CpuIdEntry {
                        function: self.last_exit_context.__bindgen_anon_1.CpuidAccess.Rax as u32,
                        index: self.last_exit_context.__bindgen_anon_1.CpuidAccess.Rcx as u32,
                        flags: 0,
                        cpuid: CpuidResult {
                            eax: self
                                .last_exit_context
                                .__bindgen_anon_1
                                .CpuidAccess
                                .DefaultResultRax as u32,
                            ebx: self
                                .last_exit_context
                                .__bindgen_anon_1
                                .CpuidAccess
                                .DefaultResultRbx as u32,
                            ecx: self
                                .last_exit_context
                                .__bindgen_anon_1
                                .CpuidAccess
                                .DefaultResultRcx as u32,
                            edx: self
                                .last_exit_context
                                .__bindgen_anon_1
                                .CpuidAccess
                                .DefaultResultRdx as u32,
                        },
                    }
                };
                Ok(VcpuExit::Cpuid { entry })
            }
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonException => Ok(VcpuExit::Exception),
            // undocumented exit calls from the header file, WinHvPlatformDefs.h.
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonX64Rdtsc => Ok(VcpuExit::RdTsc),
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonX64ApicSmiTrap => Ok(VcpuExit::ApicSmiTrap),
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonHypercall => Ok(VcpuExit::Hypercall),
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonX64ApicInitSipiTrap => {
                self.handle_apic_init_sipi_trap()?;
                Ok(VcpuExit::ApicInitSipiTrap)
            }
            // exit caused by host cancellation thorugh WHvCancelRunVirtualProcessor,
            WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonCanceled => Ok(VcpuExit::Canceled),
            r => panic!("unknown exit reason: {}", r),
        }
    }
}

impl VcpuX86_64 for WhpxVcpu {
    /// Sets or clears the flag that requests the VCPU to exit when it becomes possible to inject
    /// interrupts into the guest.
    fn set_interrupt_window_requested(&self, requested: bool) {
        const REG_NAMES: [WHV_REGISTER_NAME; 1] =
            [WHV_REGISTER_NAME_WHvX64RegisterDeliverabilityNotifications];
        let mut notifications: WHV_X64_DELIVERABILITY_NOTIFICATIONS_REGISTER__bindgen_ty_1 =
            Default::default();
        notifications.set_InterruptNotification(if requested { 1 } else { 0 });
        let notify_register = WHV_REGISTER_VALUE {
            DeliverabilityNotifications: WHV_X64_DELIVERABILITY_NOTIFICATIONS_REGISTER {
                __bindgen_anon_1: notifications,
            },
        };
        // safe because we have enough space for all the registers
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                &REG_NAMES as *const WHV_REGISTER_NAME,
                REG_NAMES.len() as u32,
                &notify_register as *const WHV_REGISTER_VALUE,
            )
        })
        .unwrap();
    }

    /// Checks if we can inject an interrupt into the VCPU.
    fn ready_for_interrupt(&self) -> bool {
        // safe because InterruptShadow bit is always valid in ExecutionState struct
        let shadow = unsafe {
            self.last_exit_context
                .VpContext
                .ExecutionState
                .__bindgen_anon_1
                .InterruptShadow()
        };

        let eflags = self.last_exit_context.VpContext.Rflags;
        const IF_MASK: u64 = 0x00000200;

        // WHvRegisterPendingInterruption reflects injections made via WHvSetVirtualProcessorRegisters
        // since the last exit; last_exit_context.ExecutionState can be stale until the next run.
        let pending = match self.interruption_pending_live() {
            Ok(pending) => pending,
            Err(_) => unsafe {
                self.last_exit_context
                    .VpContext
                    .ExecutionState
                    .__bindgen_anon_1
                    .InterruptionPending()
                    != 0
            },
        };

        // can't inject an interrupt if InterruptShadow or InterruptPending bits are set, or if
        // the IF flag is clear
        shadow == 0 && !pending && (eflags & IF_MASK) != 0
    }

    /// Injects interrupt vector `irq` into the VCPU.
    fn interrupt(&self, irq: u8) -> Result<()> {
        const REG_NAMES: [WHV_REGISTER_NAME; 1] =
            [WHV_REGISTER_NAME_WHvRegisterPendingInterruption];
        let mut pending_interrupt: WHV_X64_PENDING_INTERRUPTION_REGISTER__bindgen_ty_1 =
            Default::default();
        pending_interrupt.set_InterruptionPending(1);
        pending_interrupt
            .set_InterruptionType(WHV_X64_PENDING_INTERRUPTION_TYPE_WHvX64PendingInterrupt as u32);
        pending_interrupt.set_InterruptionVector(irq.into());
        let interrupt = WHV_REGISTER_VALUE {
            PendingInterruption: WHV_X64_PENDING_INTERRUPTION_REGISTER {
                __bindgen_anon_1: pending_interrupt,
            },
        };
        // safe because we have enough space for all the registers
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                &REG_NAMES as *const WHV_REGISTER_NAME,
                REG_NAMES.len() as u32,
                &interrupt as *const WHV_REGISTER_VALUE,
            )
        })
    }

    /// Injects a non-maskable interrupt into the VCPU.
    fn inject_nmi(&self) -> Result<()> {
        const REG_NAMES: [WHV_REGISTER_NAME; 1] =
            [WHV_REGISTER_NAME_WHvRegisterPendingInterruption];
        let mut pending_interrupt: WHV_X64_PENDING_INTERRUPTION_REGISTER__bindgen_ty_1 =
            Default::default();
        pending_interrupt.set_InterruptionPending(1);
        pending_interrupt
            .set_InterruptionType(WHV_X64_PENDING_INTERRUPTION_TYPE_WHvX64PendingNmi as u32);
        const NMI_VECTOR: u32 = 2; // 2 is the NMI vector.
        pending_interrupt.set_InterruptionVector(NMI_VECTOR);
        let interrupt = WHV_REGISTER_VALUE {
            PendingInterruption: WHV_X64_PENDING_INTERRUPTION_REGISTER {
                __bindgen_anon_1: pending_interrupt,
            },
        };
        // safe because we have enough space for all the registers
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                &REG_NAMES as *const WHV_REGISTER_NAME,
                REG_NAMES.len() as u32,
                &interrupt as *const WHV_REGISTER_VALUE,
            )
        })
    }

    /// Gets the VCPU general purpose registers.
    fn get_regs(&self) -> Result<Regs> {
        let mut whpx_regs: WhpxRegs = Default::default();
        let reg_names = WhpxRegs::get_register_names();
        // safe because we have enough space for all the registers
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                reg_names as *const WHV_REGISTER_NAME,
                reg_names.len() as u32,
                whpx_regs.as_mut_ptr(),
            )
        })?;
        Ok(Regs::from(&whpx_regs))
    }

    /// Sets the VCPU general purpose registers.
    fn set_regs(&self, regs: &Regs) -> Result<()> {
        let whpx_regs = WhpxRegs::from(regs);
        let reg_names = WhpxRegs::get_register_names();
        // safe because we have enough space for all the registers
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                reg_names as *const WHV_REGISTER_NAME,
                reg_names.len() as u32,
                whpx_regs.as_ptr(),
            )
        })
    }

    /// Gets the VCPU special registers.
    fn get_sregs(&self) -> Result<Sregs> {
        let mut whpx_sregs: WhpxSregs = Default::default();
        let reg_names = WhpxSregs::get_register_names();
        // safe because we have enough space for all the registers
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                reg_names as *const WHV_REGISTER_NAME,
                reg_names.len() as u32,
                whpx_sregs.as_mut_ptr(),
            )
        })?;
        Ok(Sregs::from(&whpx_sregs))
    }

    /// Sets the VCPU special registers.
    fn set_sregs(&self, sregs: &Sregs) -> Result<()> {
        let whpx_sregs = WhpxSregs::from(sregs);
        let reg_names = WhpxSregs::get_register_names();
        // safe because we have enough space for all the registers
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                reg_names as *const WHV_REGISTER_NAME,
                reg_names.len() as u32,
                whpx_sregs.as_ptr(),
            )
        })
    }

    /// Gets the VCPU FPU registers.
    fn get_fpu(&self) -> Result<Fpu> {
        let mut whpx_fpu: WhpxFpu = Default::default();
        let reg_names = WhpxFpu::get_register_names();
        // safe because we have enough space for all the registers
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                reg_names as *const WHV_REGISTER_NAME,
                reg_names.len() as u32,
                whpx_fpu.as_mut_ptr(),
            )
        })?;
        Ok(Fpu::from(&whpx_fpu))
    }

    /// Sets the VCPU FPU registers.
    fn set_fpu(&self, fpu: &Fpu) -> Result<()> {
        let whpx_fpu = WhpxFpu::from(fpu);
        let reg_names = WhpxFpu::get_register_names();
        // safe because we have enough space for all the registers
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                reg_names as *const WHV_REGISTER_NAME,
                reg_names.len() as u32,
                whpx_fpu.as_ptr(),
            )
        })
    }

    /// Gets the VCPU XSAVE.
    fn get_xsave(&self) -> Result<Xsave> {
        let mut empty_buffer = [0u8; 1];
        let mut needed_buf_size: u32 = 0;

        // Find out how much space is needed for XSAVEs.
        let res = unsafe {
            WHvGetVirtualProcessorXsaveState(
                self.vm_partition.partition,
                self.index,
                empty_buffer.as_mut_ptr() as *mut _,
                0,
                &mut needed_buf_size,
            )
        };
        if res != WHV_E_INSUFFICIENT_BUFFER.0 {
            // This should always work, so if it doesn't, we'll return unsupported.
            error!("failed to get size of vcpu xsave");
            return Err(Error::new(EIO));
        }

        let mut xsave = Xsave::new(needed_buf_size as usize);
        // SAFETY: xsave_data is valid for the duration of the FFI call, and we pass its length in
        // bytes so writes are bounded within the buffer.
        check_whpx!(unsafe {
            WHvGetVirtualProcessorXsaveState(
                self.vm_partition.partition,
                self.index,
                xsave.as_mut_ptr(),
                xsave.len() as u32,
                &mut needed_buf_size,
            )
        })?;
        Ok(xsave)
    }

    /// Sets the VCPU XSAVE.
    fn set_xsave(&self, xsave: &Xsave) -> Result<()> {
        // SAFETY: the xsave buffer is valid for the duration of the FFI call, and we pass its
        // length in bytes so reads are bounded within the buffer.
        check_whpx!(unsafe {
            WHvSetVirtualProcessorXsaveState(
                self.vm_partition.partition,
                self.index,
                xsave.as_ptr(),
                xsave.len() as u32,
            )
        })
    }

    fn get_interrupt_state(&self) -> Result<serde_json::Value> {
        let mut whpx_interrupt_regs: WhpxInterruptRegs = Default::default();
        let reg_names = WhpxInterruptRegs::get_register_names();
        // SAFETY: we have enough space for all the registers & the memory lives for the duration
        // of the FFI call.
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                reg_names as *const WHV_REGISTER_NAME,
                reg_names.len() as u32,
                whpx_interrupt_regs.as_mut_ptr(),
            )
        })?;

        serde_json::to_value(whpx_interrupt_regs.into_serializable()).map_err(|e| {
            error!("failed to serialize interrupt state: {:?}", e);
            Error::new(EIO)
        })
    }

    fn set_interrupt_state(&self, data: serde_json::Value) -> Result<()> {
        let whpx_interrupt_regs =
            WhpxInterruptRegs::from_serializable(serde_json::from_value(data).map_err(|e| {
                error!("failed to serialize interrupt state: {:?}", e);
                Error::new(EIO)
            })?);
        let reg_names = WhpxInterruptRegs::get_register_names();
        // SAFETY: we have enough space for all the registers & the memory lives for the duration
        // of the FFI call.
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                reg_names as *const WHV_REGISTER_NAME,
                reg_names.len() as u32,
                whpx_interrupt_regs.as_ptr(),
            )
        })
    }

    /// Gets the VCPU debug registers.
    fn get_debugregs(&self) -> Result<DebugRegs> {
        let mut whpx_debugregs: WhpxDebugRegs = Default::default();
        let reg_names = WhpxDebugRegs::get_register_names();
        // safe because we have enough space for all the registers
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                reg_names as *const WHV_REGISTER_NAME,
                reg_names.len() as u32,
                whpx_debugregs.as_mut_ptr(),
            )
        })?;
        Ok(DebugRegs::from(&whpx_debugregs))
    }

    /// Sets the VCPU debug registers.
    fn set_debugregs(&self, debugregs: &DebugRegs) -> Result<()> {
        let whpx_debugregs = WhpxDebugRegs::from(debugregs);
        let reg_names = WhpxDebugRegs::get_register_names();
        // safe because we have enough space for all the registers
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                reg_names as *const WHV_REGISTER_NAME,
                reg_names.len() as u32,
                whpx_debugregs.as_ptr(),
            )
        })
    }

    /// Gets the VCPU extended control registers.
    fn get_xcrs(&self) -> Result<BTreeMap<u32, u64>> {
        const REG_NAME: WHV_REGISTER_NAME = WHV_REGISTER_NAME_WHvX64RegisterXCr0;
        let mut reg_value = WHV_REGISTER_VALUE::default();
        // safe because we have enough space for all the registers in whpx_regs
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                &REG_NAME,
                /* RegisterCount */ 1,
                &mut reg_value,
            )
        })?;

        // safe because the union value, reg64, is safe to pull out assuming
        // kernel filled in the xcrs properly.
        let xcr0 = unsafe { reg_value.Reg64 };

        // whpx only supports xcr0
        let xcrs = BTreeMap::from([(0, xcr0)]);
        Ok(xcrs)
    }

    /// Sets a VCPU extended control register.
    fn set_xcr(&self, xcr_index: u32, value: u64) -> Result<()> {
        if xcr_index != 0 {
            // invalid xcr register provided
            return Err(Error::new(EINVAL));
        }

        const REG_NAME: WHV_REGISTER_NAME = WHV_REGISTER_NAME_WHvX64RegisterXCr0;
        let reg_value = WHV_REGISTER_VALUE { Reg64: value };
        // safe because we have enough space for all the registers in whpx_xcrs
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                &REG_NAME,
                /* RegisterCount */ 1,
                &reg_value,
            )
        })
    }

    /// Gets the value of a single model-specific register.
    fn get_msr(&self, msr_index: u32) -> Result<u64> {
        let msr_name = get_msr_name(msr_index).ok_or(Error::new(libc::ENOENT))?;
        let mut msr_value = WHV_REGISTER_VALUE::default();
        // safe because we have enough space for all the registers in whpx_regs
        check_whpx!(unsafe {
            WHvGetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                &msr_name,
                /* RegisterCount */ 1,
                &mut msr_value,
            )
        })?;

        // safe because Reg64 will be a valid union value
        let value = unsafe { msr_value.Reg64 };
        Ok(value)
    }

    fn get_all_msrs(&self) -> Result<BTreeMap<u32, u64>> {
        // Note that some members of VALID_MSRS cannot be fetched from WHPX with
        // WHvGetVirtualProcessorRegisters per the HTLFS, so we enumerate all of
        // permitted MSRs here.
        //
        // We intentionally exclude WHvRegisterPendingInterruption and
        // WHvRegisterInterruptState because they are included in
        // get_interrupt_state.
        //
        // We intentionally exclude MSR_TSC because in snapshotting it is
        // handled by the generic x86_64 VCPU snapshot/restore. Non snapshot
        // consumers should use get/set_tsc_adjust to access the adjust register
        // if needed.
        const MSRS_TO_SAVE: &[u32] = &[
            MSR_EFER,
            MSR_KERNEL_GS_BASE,
            MSR_APIC_BASE,
            MSR_SYSENTER_CS,
            MSR_SYSENTER_EIP,
            MSR_SYSENTER_ESP,
            MSR_STAR,
            MSR_LSTAR,
            MSR_CSTAR,
            MSR_SFMASK,
        ];

        let registers = MSRS_TO_SAVE
            .iter()
            .map(|msr_index| {
                let value = self.get_msr(*msr_index)?;
                Ok((*msr_index, value))
            })
            .collect::<Result<BTreeMap<u32, u64>>>()?;

        Ok(registers)
    }

    /// Sets the value of a single model-specific register.
    fn set_msr(&self, msr_index: u32, value: u64) -> Result<()> {
        match get_msr_name(msr_index) {
            Some(msr_name) => {
                let msr_value = WHV_REGISTER_VALUE { Reg64: value };
                check_whpx!(unsafe {
                    WHvSetVirtualProcessorRegisters(
                        self.vm_partition.partition,
                        self.index,
                        &msr_name,
                        /* RegisterCount */ 1,
                        &msr_value,
                    )
                })
            }
            None => {
                warn!("msr 0x{msr_index:X} write unsupported by WHPX, dropping");
                Ok(())
            }
        }
    }

    /// Sets up the data returned by the CPUID instruction.
    /// For WHPX, this is not valid on the vcpu, and needs to be setup on the vm.
    fn set_cpuid(&self, _cpuid: &CpuId) -> Result<()> {
        Err(Error::new(ENXIO))
    }

    /// This function should be called after `Vcpu::run` returns `VcpuExit::Cpuid`, and `entry`
    /// should represent the result of emulating the CPUID instruction. The `handle_cpuid` function
    /// will then set the appropriate registers on the vcpu.
    fn handle_cpuid(&mut self, entry: &CpuIdEntry) -> Result<()> {
        // Verify that we're only being called in a situation where the last exit reason was
        // ExitReasonX64Cpuid
        if self.last_exit_context.ExitReason != WHV_RUN_VP_EXIT_REASON_WHvRunVpExitReasonX64Cpuid {
            return Err(Error::new(EINVAL));
        }

        // Get the next rip from the exit context
        let rip = self.last_exit_context.VpContext.Rip
            + self.last_exit_context.VpContext.InstructionLength() as u64;

        const REG_NAMES: [WHV_REGISTER_NAME; 5] = [
            WHV_REGISTER_NAME_WHvX64RegisterRip,
            WHV_REGISTER_NAME_WHvX64RegisterRax,
            WHV_REGISTER_NAME_WHvX64RegisterRbx,
            WHV_REGISTER_NAME_WHvX64RegisterRcx,
            WHV_REGISTER_NAME_WHvX64RegisterRdx,
        ];

        let values = vec![
            WHV_REGISTER_VALUE { Reg64: rip },
            WHV_REGISTER_VALUE {
                Reg64: entry.cpuid.eax as u64,
            },
            WHV_REGISTER_VALUE {
                Reg64: entry.cpuid.ebx as u64,
            },
            WHV_REGISTER_VALUE {
                Reg64: entry.cpuid.ecx as u64,
            },
            WHV_REGISTER_VALUE {
                Reg64: entry.cpuid.edx as u64,
            },
        ];

        // safe because we have enough space for all the registers
        check_whpx!(unsafe {
            WHvSetVirtualProcessorRegisters(
                self.vm_partition.partition,
                self.index,
                &REG_NAMES as *const WHV_REGISTER_NAME,
                REG_NAMES.len() as u32,
                values.as_ptr() as *const WHV_REGISTER_VALUE,
            )
        })
    }

    /// Sets up debug registers and configure vcpu for handling guest debug events.
    fn set_guest_debug(&self, _addrs: &[GuestAddress], _enable_singlestep: bool) -> Result<()> {
        // TODO(b/173807302): Implement this
        Err(Error::new(ENOENT))
    }

    fn restore_timekeeping(&self, host_tsc_reference_moment: u64, tsc_offset: u64) -> Result<()> {
        // Set the guest TSC such that it has the same TSC_OFFSET as it did at
        // the moment it was snapshotted. This is required for virtio-pvclock
        // to function correctly. (virtio-pvclock assumes the offset is fixed,
        // and adjusts CLOCK_BOOTTIME accordingly. It also hides the TSC jump
        // from CLOCK_MONOTONIC by setting the timebase.)
        self.set_tsc_value(host_tsc_reference_moment.wrapping_add(tsc_offset))
    }
}

fn get_msr_name(msr_index: u32) -> Option<WHV_REGISTER_NAME> {
    VALID_MSRS.get(&msr_index).copied()
}

// run calls are tested with the integration tests since the full vcpu needs to be setup for it.
#[cfg(test)]
mod tests {
    use vm_memory::GuestAddress;
    use vm_memory::GuestMemory;

    use super::*;
    use crate::VmX86_64;

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
    fn try_clone() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let vcpu = vm.create_vcpu(0).expect("failed to create vcpu");
        let vcpu: &WhpxVcpu = vcpu.downcast_ref().expect("Expected a WhpxVcpu");
        let _vcpu_clone = vcpu.try_clone().expect("failed to clone whpx vcpu");
    }

    #[test]
    fn index() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 2;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let mut vcpu = vm.create_vcpu(0).expect("failed to create vcpu");
        let vcpu0: &WhpxVcpu = vcpu.downcast_ref().expect("Expected a WhpxVcpu");
        assert_eq!(vcpu0.index, 0);
        vcpu = vm.create_vcpu(1).expect("failed to create vcpu");
        let vcpu1: &WhpxVcpu = vcpu.downcast_ref().expect("Expected a WhpxVcpu");
        assert_eq!(vcpu1.index, 1);
    }

    #[test]
    fn get_regs() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let vcpu = vm.create_vcpu(0).expect("failed to create vcpu");

        vcpu.get_regs().expect("failed to get regs");
    }

    #[test]
    fn set_regs() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let vcpu = vm.create_vcpu(0).expect("failed to create vcpu");

        let mut regs = vcpu.get_regs().expect("failed to get regs");
        let new_val = regs.rax + 2;
        regs.rax = new_val;

        vcpu.set_regs(&regs).expect("failed to set regs");
        let new_regs = vcpu.get_regs().expect("failed to get regs");
        assert_eq!(new_regs.rax, new_val);
    }

    #[test]
    fn debugregs() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let vcpu = vm.create_vcpu(0).expect("failed to create vcpu");

        let mut dregs = vcpu.get_debugregs().unwrap();
        dregs.dr7 += 13;
        vcpu.set_debugregs(&dregs).unwrap();
        let dregs2 = vcpu.get_debugregs().unwrap();
        assert_eq!(dregs.dr7, dregs2.dr7);
    }

    #[test]
    fn sregs() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let vcpu = vm.create_vcpu(0).expect("failed to create vcpu");

        let mut sregs = vcpu.get_sregs().unwrap();
        sregs.cs.base += 7;
        vcpu.set_sregs(&sregs).unwrap();
        let sregs2 = vcpu.get_sregs().unwrap();
        assert_eq!(sregs.cs.base, sregs2.cs.base);
    }

    #[test]
    fn fpu() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let vcpu = vm.create_vcpu(0).expect("failed to create vcpu");

        let mut fpu = vcpu.get_fpu().unwrap();
        fpu.fpr[0].significand += 3;
        vcpu.set_fpu(&fpu).unwrap();
        let fpu2 = vcpu.get_fpu().unwrap();
        assert_eq!(fpu.fpr, fpu2.fpr);
    }

    #[test]
    fn xcrs() {
        if !Whpx::is_enabled() {
            return;
        }
        let whpx = Whpx::new().expect("failed to instantiate whpx");
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let vcpu = vm.create_vcpu(0).expect("failed to create vcpu");
        // check xsave support
        if !whpx.check_capability(HypervisorCap::Xcrs) {
            return;
        }

        vcpu.set_xcr(0, 1).unwrap();
        let xcrs = vcpu.get_xcrs().unwrap();
        let xcr0 = xcrs.get(&0).unwrap();
        assert_eq!(*xcr0, 1);
    }

    #[test]
    fn set_msr() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let vcpu = vm.create_vcpu(0).expect("failed to create vcpu");

        vcpu.set_msr(MSR_KERNEL_GS_BASE, 42).unwrap();

        let gs_base = vcpu.get_msr(MSR_KERNEL_GS_BASE).unwrap();
        assert_eq!(gs_base, 42);
    }

    #[test]
    fn get_msr() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let vcpu = vm.create_vcpu(0).expect("failed to create vcpu");

        // This one should succeed
        let _value = vcpu.get_msr(MSR_TSC).unwrap();

        // This one will fail to fetch
        vcpu.get_msr(MSR_TSC + 1)
            .expect_err("invalid MSR index should fail");
    }

    #[test]
    fn set_efer() {
        if !Whpx::is_enabled() {
            return;
        }
        // EFER Bits
        const EFER_SCE: u64 = 0x00000001;
        const EFER_LME: u64 = 0x00000100;
        const EFER_LMA: u64 = 0x00000400;
        const X86_CR0_PE: u64 = 0x1;
        const X86_CR0_PG: u64 = 0x80000000;
        const X86_CR4_PAE: u64 = 0x20;

        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let vcpu = vm.create_vcpu(0).expect("failed to create vcpu");

        let mut sregs = vcpu.get_sregs().expect("failed to get sregs");
        // Initial value should be 0
        assert_eq!(sregs.efer, 0);

        // Enable and activate long mode
        sregs.cr0 |= X86_CR0_PE; // enable protected mode
        sregs.cr0 |= X86_CR0_PG; // enable paging
        sregs.cr4 |= X86_CR4_PAE; // enable physical address extension
        sregs.efer = EFER_LMA | EFER_LME;
        vcpu.set_sregs(&sregs).expect("failed to set sregs");

        // Verify that setting stuck
        let sregs = vcpu.get_sregs().expect("failed to get sregs");
        assert_eq!(sregs.efer, EFER_LMA | EFER_LME);
        assert_eq!(sregs.cr0 & X86_CR0_PE, X86_CR0_PE);
        assert_eq!(sregs.cr0 & X86_CR0_PG, X86_CR0_PG);
        assert_eq!(sregs.cr4 & X86_CR4_PAE, X86_CR4_PAE);

        let efer = vcpu.get_msr(MSR_EFER).expect("failed to get msr");
        assert_eq!(efer, EFER_LMA | EFER_LME);

        // Enable SCE via set_msrs
        vcpu.set_msr(MSR_EFER, efer | EFER_SCE)
            .expect("failed to set msr");

        // Verify that setting stuck
        let sregs = vcpu.get_sregs().expect("failed to get sregs");
        assert_eq!(sregs.efer, EFER_SCE | EFER_LME | EFER_LMA);
        let new_efer = vcpu.get_msr(MSR_EFER).expect("failed to get msr");
        assert_eq!(new_efer, EFER_SCE | EFER_LME | EFER_LMA);
    }

    #[test]
    fn get_and_set_xsave_smoke() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let vcpu = vm.create_vcpu(0).expect("failed to create vcpu");

        // XSAVE is essentially opaque for our purposes. We just want to make sure our syscalls
        // succeed.
        let xsave = vcpu.get_xsave().unwrap();
        vcpu.set_xsave(&xsave).unwrap();
    }

    #[test]
    fn get_and_set_interrupt_state_smoke() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let vcpu = vm.create_vcpu(0).expect("failed to create vcpu");

        // For the sake of snapshotting, interrupt state is essentially opaque. We just want to make
        // sure our syscalls succeed.
        let interrupt_state = vcpu.get_interrupt_state().unwrap();
        vcpu.set_interrupt_state(interrupt_state).unwrap();
    }

    #[test]
    fn get_all_msrs() {
        if !Whpx::is_enabled() {
            return;
        }
        let cpu_count = 1;
        let mem =
            GuestMemory::new(&[(GuestAddress(0), 0x1000)]).expect("failed to create guest memory");
        let vm = new_vm(cpu_count, mem);
        let vcpu = vm.create_vcpu(0).expect("failed to create vcpu");

        let all_msrs = vcpu.get_all_msrs().unwrap();

        // Our MSR buffer is init'ed to zeros in the registers. The APIC base will be non-zero, so
        // by asserting that we know the MSR fetch actually did get us data.
        let apic_base = all_msrs.get(&MSR_APIC_BASE).unwrap();
        assert_ne!(*apic_base, 0);
    }
}
