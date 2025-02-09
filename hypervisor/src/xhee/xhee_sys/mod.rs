// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Bindings for the Xhee Hypervisor API.

#![cfg(any(target_os = "android", target_os = "linux"))]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

#[cfg(target_arch = "aarch64")]
pub mod aarch64 {
    pub mod bindings;
    use base::ioctl_io_nr;
    use base::ioctl_ior_nr;
    use base::ioctl_iow_nr;
    use base::ioctl_iowr_nr;
    pub use bindings::*;

    ioctl_io_nr!(XHEE_CREATE_VM, XHEE_IOC, 0x01);
    ioctl_ior_nr!(XHEE_GET_VM_GPA_SIZE, XHEE_IOC, 0x02, u64);

    ioctl_iow_nr!(
        XHEE_SET_USER_MEMORY_REGION,
        XHEE_IOC,
        0x11,
        xhee_userspace_memory_region
    );

    ioctl_io_nr!(XHEE_CREATE_VCPU, XHEE_IOC, 0x12);
    ioctl_iow_nr!(XHEE_SET_DTB_CONFIG, XHEE_IOC, 0x13, xhee_dtb_config);
    ioctl_iowr_nr!(XHEE_CREATE_DEVICE, XHEE_IOC, 0x14, xhee_create_device);
    ioctl_iow_nr!(XHEE_SET_PVMFW_GPA, XHEE_IOC, 0x15, u64);
    ioctl_ior_nr!(XHEE_GET_PVMFW_SIZE, XHEE_IOC, 0x16, u64);
    ioctl_iow_nr!(XHEE_IRQ_LINE, XHEE_IOC, 0x17, xhee_irq_level);
    ioctl_iow_nr!(XHEE_IRQFD, XHEE_IOC, 0x76, xhee_irqfd);
    ioctl_iow_nr!(XHEE_IOEVENTFD, XHEE_IOC, 0x79, xhee_ioeventfd);

    ioctl_io_nr!(XHEE_RUN, XHEE_IOC, 0x21);
    ioctl_iow_nr!(XHEE_GET_ONE_REG, XHEE_IOC, 0x22, xhee_one_reg);
    ioctl_iow_nr!(XHEE_SET_ONE_REG, XHEE_IOC, 0x23, xhee_one_reg);

    ioctl_io_nr!(XHEE_CREATE_IRQCHIP, XHEE_IOC, 0x60);
    ioctl_iow_nr!(XHEE_SET_MEMORY_REGION, XHEE_IOC, 0x40, xhee_memory_region);
}

#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
