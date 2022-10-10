// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Bindings for the Linux Gunyah API.

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use base::ioctl_io_nr;
use base::ioctl_iow_nr;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub mod x86 {
    // generated with gunyah_sys/bindgen.sh
    pub mod bindings;
    use base::ioctl_ior_nr;
    use base::ioctl_iow_nr;
    pub use bindings::*;
}

#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
pub mod aarch64 {
    // generated with gunyah_sys/bindgen.sh
    pub mod bindings;
    pub use bindings::*;
}

// These ioctls are commonly defined on all/multiple platforms.
ioctl_io_nr!(GH_CREATE_VM, GH_IOCTL_TYPE, 0x0);
ioctl_iow_nr!(
    GH_VM_SET_USER_MEM_REGION,
    GH_IOCTL_TYPE,
    0x1,
    gh_userspace_memory_region);
ioctl_iow_nr!(GH_VM_SET_DTB_CONFIG, GH_IOCTL_TYPE, 0x2, gh_vm_dtb_config);
ioctl_io_nr!(GH_VM_START, GH_IOCTL_TYPE, 0x3);
ioctl_iow_nr!(GH_VM_ADD_FUNCTION, GH_IOCTL_TYPE, 0x4, gh_vm_function);
ioctl_io_nr!(GH_VCPU_RUN, GH_IOCTL_TYPE, 0x5);
ioctl_io_nr!(GH_VCPU_MMAP_SIZE, GH_IOCTL_TYPE, 0x6);

// Along with the common ioctls, we reexport the ioctls of the current
// platform.

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub use crate::x86::*;

#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
pub use aarch64::*;
