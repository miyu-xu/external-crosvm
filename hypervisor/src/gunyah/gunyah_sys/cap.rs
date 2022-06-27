
#![allow(dead_code)]

use crate::gunyah::*;

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u32)]
pub enum GhCap {
        UserMemory = GH_CAP_USER_MEMORY,
        ArmProtectedVm = GH_CAP_ARM_PROTECTED_VM,
}
