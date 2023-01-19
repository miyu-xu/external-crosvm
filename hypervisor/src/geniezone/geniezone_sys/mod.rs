// Copyright 2023 Mediatek Inc.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub const GZVM_EXIT_UNKNOWN: u32 = 0x92920000;
pub const GZVM_EXIT_MMIO: u32 = 0x92920001;
pub const GZVM_EXIT_HVC: u32 = 0x92920002;
pub const GZVM_EXIT_IRQ: u32 = 0x92920003;

pub const GZVM_DEV_TYPE_ARM_VGIC_V3_DIST: u32 = 0x0;
pub const GZVM_DEV_TYPE_ARM_VGIC_V3_REDIST: u32 = 0x1;

// Should be auto-generated later
#[repr(C)]
#[derive(Copy, Clone)]
pub struct gzvm_vcpu_run {
    pub exit_reason: u32,
    // pub immediate_exit: u8,
    pub anon_union: anon_union_t1,
    pub regs: cpu_user_regs,
}
impl Default for gzvm_vcpu_run {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct gzvm_create_device {
    pub dev_type: u32,
    pub id: u32,
    pub flags: u64,
    pub dev_addr: u64,
    pub dev_reg_size: u64,
    pub attr_addr: u64,
    pub attr_size: u64,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub union anon_union_t1 {
    pub mmio: anon_struct1,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct anon_struct1 {
    pub phys_addr: u64,
    pub data: [u8; 8usize],
    pub size: u64,
    pub reg_nr: i32,
    pub is_write: bool,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct cpu_user_regs {
}

#[repr(C)]
#[derive(Copy, Clone)]
pub union decl_reg {
    pub n64: u64,
    pub n32: u32,
}

