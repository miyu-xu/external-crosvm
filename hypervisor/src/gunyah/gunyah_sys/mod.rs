#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

pub mod bindings;
pub use bindings::*;

pub mod cap;
pub use cap::*;

use base::{ioctl_io_nr, ioctl_ior_nr, ioctl_iowr_nr, ioctl_iow_nr};

const GH_IOCTL_TYPE: u32 = 0xB2;

/* system ioctls */
ioctl_iow_nr!(GH_CREATE_VM, GH_IOCTL_TYPE, 0x01, u32);
ioctl_io_nr!(GH_GET_VCPU_MMAP_SIZE, GH_IOCTL_TYPE, 0x2);

/* vm ioctls */
ioctl_io_nr!(GH_CREATE_VCPU, GH_IOCTL_TYPE, 0x40);



ioctl_iow_nr!(GH_VM_SET_USER_MEMORY_REGION, GH_IOCTL_TYPE, 0x44, gh_userspace_memory_region);
ioctl_iowr_nr!(GH_VM_IOEVENTFD, GH_IOCTL_TYPE, 0x45, gh_ioeventfd);
ioctl_iow_nr!(GH_VM_IRQFD, GH_IOCTL_TYPE, 0x46, gh_irqfd);
ioctl_iowr_nr!(GH_VM_CREATE_DEVICE, GH_IOCTL_TYPE, 0x47, gh_create_device);
ioctl_io_nr!(GH_VM_CHECK_EXTENSION, GH_IOCTL_TYPE, 0x48);
ioctl_iow_nr!(GH_SET_VM_NAME, GH_IOCTL_TYPE, 0x49, fw_name);

/* vcpu ioctls */
ioctl_io_nr!(GH_VCPU_RUN, GH_IOCTL_TYPE, 0x80);
ioctl_iow_nr!(GH_SET_ONE_REG, GH_IOCTL_TYPE, 0x81, gh_one_reg);
ioctl_iow_nr!(GH_GET_ONE_REG, GH_IOCTL_TYPE, 0x82, gh_one_reg);
ioctl_iow_nr!(GH_ARM_PREFERRED_TARGET, GH_IOCTL_TYPE, 0x83, gh_vcpu_init);
ioctl_iow_nr!(GH_ARM_VCPU_INIT, GH_IOCTL_TYPE, 0x84, gh_vcpu_init);

