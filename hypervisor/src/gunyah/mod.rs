use crate::*;

mod gunyah_sys;
use gunyah_sys::*;

mod vm;
pub use vm::*;

use libc::open;
use libc::O_CLOEXEC;
use libc::O_RDWR;

use base::ioctl;
use base::errno_result;

use base::{FromRawDescriptor, RawDescriptor};

use std::path::{Path, PathBuf};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

pub struct Gunyah {
    gunyah: SafeDescriptor,
}

impl AsRawDescriptor for Gunyah {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.gunyah.as_raw_descriptor()
    }
}

impl Gunyah {
    pub fn new_with_path(device_path: &Path) -> Result<Gunyah> {
        // Open calls are safe because we give a nul-terminated string and verify the result.
        let c_path = CString::new(device_path.as_os_str().as_bytes()).unwrap();
        let ret = unsafe { open(c_path.as_ptr(), O_RDWR | O_CLOEXEC) };
        if ret < 0 {
            return errno_result();
        }
        // Safe because we verify that ret is valid and we own the fd.
        Ok(Gunyah {
            gunyah: unsafe { SafeDescriptor::from_raw_descriptor(ret) },
        })
    }

    /// Opens GUNYAH device and returns a Gunyah object on success.
    pub fn new() -> Result<Gunyah> {
        Gunyah::new_with_path(&PathBuf::from("/dev/gunyah"))
    }

    pub fn get_vm_type(&self, protection_type: ProtectionType) -> Result<u32> {
        let protection_flag = match protection_type {
            ProtectionType::Unprotected | ProtectionType::UnprotectedWithFirmware => 0,
            ProtectionType::Protected | ProtectionType::ProtectedWithoutFirmware => {
                GH_VM_TYPE_ARM_PROTECTED
            }
        };
        // Use the lower 8 bits representing the IPA space as the machine type
        Ok(protection_flag)
    }
 
    /// Gets the size of the mmap required to use vcpu's `gh_run` structure.
    pub fn get_vcpu_mmap_size(&self) -> Result<usize> {
        // Safe because we know that our file is a KVM fd and we verify the return result.
        let res = unsafe { ioctl(self, GH_GET_VCPU_MMAP_SIZE()) };
        if res > 0 {
            Ok(res as usize)
        } else {
            errno_result()
        }
    }
 
}

impl Hypervisor for Gunyah {
    fn check_capability(&self, cap: HypervisorCap) -> bool {
        match cap {
            HypervisorCap::UserMemory => true,
            HypervisorCap::ImmediateExit => true,
            _ => false,
        }
    }

    /// Makes a shallow clone of this `Hypervisor`.
    fn try_clone(&self) -> Result<Self> {
        Ok(Gunyah {
            gunyah: self.gunyah.try_clone()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_gunyah() {
        Gunyah::new().expect("failed to instantiate GUNYAH");
    }

    #[test]
    fn check_capability() {
        let gunyah = Gunyah::new().expect("failed to instantiate GUNYAH");
        assert!(gunyah.check_capability(HypervisorCap::UserMemory));
        assert!(!gunyah.check_capability(HypervisorCap::ImmediateExit));
    }
}
