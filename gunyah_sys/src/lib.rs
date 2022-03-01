#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

pub mod bindings;
pub use bindings::*;

use base::{ioctl_io_nr, ioctl_ior_nr, ioctl_iow_nr};

const GH_IOCTL_TYPE: u32 = 0x67;

/* system ioctls */
ioctl_io_nr!(GH_GET_API_VERSION, GH_IOCTL_TYPE, 0x00);
ioctl_io_nr!(GH_CREATE_VM, GH_IOCTL_TYPE, 0x01);

/* vm ioctls */
ioctl_io_nr!(GH_CREATE_VCPU, GH_IOCTL_TYPE, 0x40);
ioctl_iow_nr!(GH_VM_SET_FW_NAME, GH_IOCTL_TYPE, 0x41, fw_name);
ioctl_ior_nr!(GH_VM_GET_FW_NAME, GH_IOCTL_TYPE, 0x42, fw_name);
ioctl_io_nr!(GH_VM_GET_VCPU_COUNT, GH_IOCTL_TYPE, 0x43);

/* vcpu ioctls */
ioctl_io_nr!(GH_VCPU_RUN, GH_IOCTL_TYPE, 0x80);
