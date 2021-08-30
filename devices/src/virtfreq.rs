use crate::{BusAccessInfo, BusDevice};
use base::{AsRawDescriptor, Descriptor, Error, Event, PollToken, RawDescriptor, Tube, WaitContext, error, warn};
use sys_util::syscall;
use libc::pid_t;
use std::sync::{Arc,RwLock};

const SCHED_FLAG_KEEP_POLICY: u64 =	0x08;
const SCHED_FLAG_KEEP_PARAMS: u64 =	0x10;
const SCHED_FLAG_UTIL_CLAMP_MIN: u64 = 0x20;
const SCHED_FLAG_UTIL_CLAMP_MAX: u64 = 0x40;

const SCHED_FLAG_KEEP_ALL:u64 = (SCHED_FLAG_KEEP_POLICY | SCHED_FLAG_KEEP_PARAMS);

#[repr(C)]
struct sched_attr_t {
    pub size: u32,

    pub sched_policy: u32,
    pub sched_flags: u64,
    pub sched_nice: i32,

    pub sched_priority: u32,

    pub sched_runtime: u64,
    pub sched_deadline: u64,
    pub sched_period: u64,

    pub sched_util_min: u32,
    pub sched_util_max: u32,
}

impl sched_attr_t {
    fn default() -> Self {
        Self {
            size: std::mem::size_of::<sched_attr_t>() as u32,
            sched_policy: 0,
            sched_flags: 0,
            sched_nice: 0,
            sched_priority: 0,
            sched_runtime: 0,
            sched_deadline: 0,
            sched_period: 0,
            sched_util_min: 0,
            sched_util_max: 0,
        }
    }
}

fn sched_setattr(pid: libc::pid_t, attr: &mut sched_attr_t, flags: u32) -> Result<(), Error> {
    syscall!(unsafe {
        libc::syscall(
            libc::SYS_sched_setattr,
            pid as usize,
            attr as *mut sched_attr_t as usize,
            flags as usize)
    })?;
    Ok(())
}

fn sched_getattr(pid: libc::pid_t,  attr: &mut sched_attr_t, size: u32, flags: u32) -> Result<(), Error> {
    syscall!(unsafe {
        libc::syscall(
            libc::SYS_sched_getattr,
            pid as usize,
            attr as *mut sched_attr_t as usize,
            size as usize,
            flags as usize)
    })?;
    Ok(())
}

pub struct VCPUHandle {
    pub tid: Option<libc::pid_t>,
    pub cluster_id: usize,
}

pub struct VirtFreq {
    cpu_clusters: Vec<Vec<usize>>,
    vcpu_threads: Arc<RwLock<Vec<VCPUHandle>>>,
}


impl VirtFreq {
    pub fn new(cpu_clusters: Vec<Vec<usize>>, vcpu_threads: Arc<RwLock<Vec<VCPUHandle>>>) -> Self {
        VirtFreq{cpu_clusters, vcpu_threads}
    }
}

fn extract_value(data: &[u8]) -> u32 {
        let mut val = u32::to_ne_bytes(0);
        val.copy_from_slice(&data[..4]);
        u32::from_ne_bytes(val)
}

impl BusDevice for VirtFreq {
    fn debug_label(&self) -> String {
        "VirtFreq Device".to_owned()
    }

    fn write(&mut self, offset: BusAccessInfo, data: &[u8]) {

        if data.len() != std::mem::size_of::<u32>() {
            warn!(
                "{}: unsupported write length {}, only support 4bytes write",
                self.debug_label(),
                data.len()
            );
            return;
        }

        let freq = extract_value(data);

        // If no clusters were specified, treat as independent freq domains.
        if self.cpu_clusters.is_empty() {
            let mut sched_attr =  sched_attr_t::default();
            sched_attr.sched_flags = SCHED_FLAG_KEEP_ALL | SCHED_FLAG_UTIL_CLAMP_MIN;
            sched_attr.sched_util_min = freq;

            if let Err(e)  = sched_setattr(0, &mut sched_attr, 0) {
                panic!("{}: Error setting uclamp value: {}", self.debug_label(), e);
            }
        } else {
            let id = offset.id;
            let vcpu_threads = self.vcpu_threads.read().unwrap();
            let cluster_id = vcpu_threads[id].cluster_id;

            for vcpu in self.cpu_clusters[cluster_id].iter() {
                if let Some(tid) = vcpu_threads[*vcpu].tid {
                    let mut sched_attr =  sched_attr_t::default();
                    sched_attr.sched_flags = SCHED_FLAG_KEEP_ALL | SCHED_FLAG_UTIL_CLAMP_MIN;
                    sched_attr.sched_util_min = freq;

                    if let Err(e)  = sched_setattr(tid, &mut sched_attr, 0) {
                        panic!("{}: Error setting uclamp value: {}", self.debug_label(), e);
                    }
                } else {
                    panic!("{}: tid should never be uninitialized at this point", self.debug_label());
                }
            }
        }
    }
}
