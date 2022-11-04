use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::{File, OpenOptions},
    path::Path,
};

use base::{ioctl_with_ref, AsRawDescriptor, RawDescriptor};
use virtio_sys::VHOST_SCMI_SET_RUNNING;

use super::{ioctl_result, Error, Result, Vhost};

/// Handle for running VHOST_SCMI ioctls.
pub struct Scmi {
    descriptor: File,
}

impl Scmi {
    /// Open a handle to a new VHOST_SCMI instance.
    pub fn new(vhost_scmi_device_path: &Path) -> Result<Scmi> {
        Ok(Scmi {
            descriptor: OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open(vhost_scmi_device_path)
                .map_err(Error::VhostOpen)?,
        })
    }
    /// Tell the VHOST driver to start performing data transfer.
    pub fn start(&self) -> Result<()> {
        self.set_running(true)
    }

    /// Tell the VHOST driver to stop performing data transfer.
    pub fn stop(&self) -> Result<()> {
        self.set_running(false)
    }

    fn set_running(&self, running: bool) -> Result<()> {
        let on: ::std::os::raw::c_int = if running { 1 } else { 0 };
        // Safe because we own the descriptor and is valid. We also check the return result.
        let ret = unsafe { ioctl_with_ref(&self.descriptor, VHOST_SCMI_SET_RUNNING(), &on) };

        if ret < 0 {
            return ioctl_result();
        }
        Ok(())
    }
}

impl Vhost for Scmi {}

impl AsRawDescriptor for Scmi {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.descriptor.as_raw_descriptor()
    }
}
