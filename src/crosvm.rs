// Copyright 2019 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The root level module that includes the config and aggregate of the submodules for running said
//! configs.

pub mod cmdline;
pub mod config;
#[cfg(all(target_arch = "x86_64", feature = "gdb"))]
pub mod gdb;
#[path = "crosvm/linux/mod.rs"]
pub mod platform;
#[cfg(feature = "plugin")]
pub mod plugin;
pub mod sys;

pub mod argument;

macro_rules! check_opt_path {
    ($struct:ident.$field:ident) => {
        if let Some(p) = &$struct.$field {
            if !p.exists() {
                return Err(format!(
                    "{} path {:?} does not exist",
                    stringify!($field),
                    p
                ));
            }
        }
    };
}

<<<<<<< HEAD   (eb9d1c ANDROID: Workaround to fix cuttlefish boot on intel 11th gen)
#[derive(Debug)]
pub struct FileBackedMappingParameters {
    pub address: u64,
    pub size: u64,
    pub path: PathBuf,
    pub offset: u64,
    pub writable: bool,
    pub sync: bool,
}

#[derive(Clone)]
pub struct HostPcieRootPortParameters {
    pub host_path: PathBuf,
    pub hp_gpe: Option<u32>,
}

#[derive(Debug)]
pub struct JailConfig {
    pub pivot_root: PathBuf,
    pub seccomp_policy_dir: PathBuf,
    pub seccomp_log_failures: bool,
}

impl Default for JailConfig {
    fn default() -> Self {
        JailConfig {
            pivot_root: PathBuf::from(option_env!("DEFAULT_PIVOT_ROOT").unwrap_or("/var/empty")),
            seccomp_policy_dir: PathBuf::from(SECCOMP_POLICY_DIR),
            seccomp_log_failures: false,
        }
    }
}

fn parse_bus_id_addr(v: &str) -> Result<(u8, u8, u16, u16), String> {
    debug!("parse_bus_id_addr: {}", v);
    let mut ids = v.split(':');
    let errorre = move |item| move |e| format!("{}: {}", item, e);
    match (ids.next(), ids.next(), ids.next(), ids.next()) {
        (Some(bus_id), Some(addr), Some(vid), Some(pid)) => {
            let bus_id = bus_id.parse::<u8>().map_err(errorre("bus_id"))?;
            let addr = addr.parse::<u8>().map_err(errorre("addr"))?;
            let vid = u16::from_str_radix(vid, 16).map_err(errorre("vid"))?;
            let pid = u16::from_str_radix(pid, 16).map_err(errorre("pid"))?;
            Ok((bus_id, addr, vid, pid))
        }
        _ => Err(String::from("BUS_ID:ADDR:BUS_NUM:DEV_NUM")),
    }
}

/// Aggregate of all configurable options for a running VM.
#[remain::sorted]
pub struct Config {
    #[cfg(feature = "audio")]
    pub ac97_parameters: Vec<Ac97Parameters>,
    pub acpi_tables: Vec<PathBuf>,
    pub android_fstab: Option<PathBuf>,
    pub balloon: bool,
    pub balloon_bias: i64,
    pub balloon_control: Option<PathBuf>,
    pub battery_type: Option<BatteryType>,
    pub cid: Option<u64>,
    pub coiommu_param: Option<devices::CoIommuParameters>,
    pub cpu_capacity: BTreeMap<usize, u32>, // CPU index -> capacity
    pub cpu_clusters: Vec<Vec<usize>>,
    #[cfg(feature = "audio_cras")]
    pub cras_snds: Vec<CrasSndParameters>,
    pub delay_rt: bool,
    #[cfg(feature = "direct")]
    pub direct_edge_irq: Vec<u32>,
    #[cfg(feature = "direct")]
    pub direct_gpe: Vec<u32>,
    #[cfg(feature = "direct")]
    pub direct_level_irq: Vec<u32>,
    #[cfg(feature = "direct")]
    pub direct_mmio: Option<DirectIoOption>,
    #[cfg(feature = "direct")]
    pub direct_pmio: Option<DirectIoOption>,
    pub disks: Vec<DiskOption>,
    pub display_window_keyboard: bool,
    pub display_window_mouse: bool,
    pub dmi_path: Option<PathBuf>,
    pub enable_pnp_data: bool,
    pub executable_path: Option<Executable>,
    pub file_backed_mappings: Vec<FileBackedMappingParameters>,
    pub force_s2idle: bool,
    #[cfg(all(target_arch = "x86_64", feature = "gdb"))]
    pub gdb: Option<u32>,
    #[cfg(feature = "gpu")]
    pub gpu_parameters: Option<GpuParameters>,
    #[cfg(feature = "gpu")]
    pub gpu_render_server_parameters: Option<GpuRenderServerParameters>,
    pub host_cpu_topology: bool,
    pub host_ip: Option<net::Ipv4Addr>,
    pub hugepages: bool,
    pub init_memory: Option<u64>,
    pub initrd_path: Option<PathBuf>,
    pub itmt: bool,
    pub jail_config: Option<JailConfig>,
    pub jail_enabled: bool,
    pub kvm_device_path: PathBuf,
    pub mac_address: Option<net_util::MacAddress>,
    pub memory: Option<u64>,
    pub memory_file: Option<PathBuf>,
    pub mmio_address_ranges: Vec<RangeInclusive<u64>>,
    pub net_vq_pairs: Option<u16>,
    pub netmask: Option<net::Ipv4Addr>,
    pub no_legacy: bool,
    pub no_smt: bool,
    pub params: Vec<String>,
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    pub pci_low_start: Option<u64>,
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    pub pcie_ecam: Option<MemRegion>,
    #[cfg(feature = "direct")]
    pub pcie_rp: Vec<HostPcieRootPortParameters>,
    pub per_vm_core_scheduling: bool,
    pub plugin_gid_maps: Vec<GidMap>,
    pub plugin_mounts: Vec<BindMount>,
    pub plugin_root: Option<PathBuf>,
    pub pmem_devices: Vec<DiskOption>,
    pub privileged_vm: bool,
    pub protected_vm: ProtectionType,
    pub pstore: Option<Pstore>,
    /// Must be `Some` iff `protected_vm == ProtectionType::UnprotectedWithFirmware`.
    pub pvm_fw: Option<PathBuf>,
    pub rng: bool,
    pub rt_cpus: Vec<usize>,
    pub serial_parameters: BTreeMap<(SerialHardware, u8), SerialParameters>,
    pub shared_dirs: Vec<SharedDir>,
    pub socket_path: Option<PathBuf>,
    pub software_tpm: bool,
    #[cfg(feature = "audio")]
    pub sound: Option<PathBuf>,
    pub split_irqchip: bool,
    pub strict_balloon: bool,
    pub stub_pci_devices: Vec<StubPciParameters>,
    pub swiotlb: Option<u64>,
    pub syslog_tag: Option<String>,
    pub tap_fd: Vec<RawFd>,
    pub tap_name: Vec<String>,
    #[cfg(target_os = "android")]
    pub task_profiles: Vec<String>,
    pub usb: bool,
    pub userspace_msr: BTreeMap<u32, MsrConfig>,
    pub vcpu_affinity: Option<VcpuAffinity>,
    pub vcpu_cgroup_path: Option<PathBuf>,
    pub vcpu_count: Option<usize>,
    pub vfio: Vec<VfioCommand>,
    pub vhost_net: bool,
    pub vhost_net_device_path: PathBuf,
    pub vhost_user_blk: Vec<VhostUserOption>,
    pub vhost_user_console: Vec<VhostUserOption>,
    pub vhost_user_fs: Vec<VhostUserFsOption>,
    pub vhost_user_gpu: Vec<VhostUserOption>,
    pub vhost_user_mac80211_hwsim: Option<VhostUserOption>,
    pub vhost_user_net: Vec<VhostUserOption>,
    #[cfg(feature = "audio")]
    pub vhost_user_snd: Vec<VhostUserOption>,
    pub vhost_user_vsock: Vec<VhostUserOption>,
    pub vhost_user_wl: Vec<VhostUserWlOption>,
    pub vhost_vsock_device: Option<PathBuf>,
    #[cfg(feature = "video-decoder")]
    pub video_dec: Option<VideoBackendType>,
    #[cfg(feature = "video-encoder")]
    pub video_enc: Option<VideoBackendType>,
    pub virtio_input_evdevs: Vec<PathBuf>,
    pub virtio_iommu: bool,
    pub virtio_keyboard: Vec<PathBuf>,
    pub virtio_mice: Vec<PathBuf>,
    pub virtio_multi_touch: Vec<TouchDeviceOption>,
    pub virtio_single_touch: Vec<TouchDeviceOption>,
    pub virtio_switches: Vec<PathBuf>,
    pub virtio_trackpad: Vec<TouchDeviceOption>,
    pub vvu_proxy: Vec<VvuOption>,
    pub wayland_socket_paths: BTreeMap<String, PathBuf>,
    pub x_display: Option<String>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            #[cfg(feature = "audio")]
            ac97_parameters: Vec::new(),
            acpi_tables: Vec::new(),
            android_fstab: None,
            balloon: true,
            balloon_bias: 0,
            balloon_control: None,
            battery_type: None,
            cid: None,
            coiommu_param: None,
            #[cfg(feature = "audio_cras")]
            cras_snds: Vec::new(),
            cpu_capacity: BTreeMap::new(),
            cpu_clusters: Vec::new(),
            delay_rt: false,
            #[cfg(feature = "direct")]
            direct_edge_irq: Vec::new(),
            #[cfg(feature = "direct")]
            direct_gpe: Vec::new(),
            #[cfg(feature = "direct")]
            direct_level_irq: Vec::new(),
            #[cfg(feature = "direct")]
            direct_mmio: None,
            #[cfg(feature = "direct")]
            direct_pmio: None,
            disks: Vec::new(),
            display_window_keyboard: false,
            display_window_mouse: false,
            dmi_path: None,
            enable_pnp_data: false,
            executable_path: None,
            file_backed_mappings: Vec::new(),
            force_s2idle: false,
            #[cfg(all(target_arch = "x86_64", feature = "gdb"))]
            gdb: None,
            #[cfg(feature = "gpu")]
            gpu_parameters: None,
            #[cfg(feature = "gpu")]
            gpu_render_server_parameters: None,
            host_cpu_topology: false,
            host_ip: None,
            hugepages: false,
            init_memory: None,
            initrd_path: None,
            itmt: false,
            // We initialize the jail configuration with a default value so jail-related options can
            // apply irrespective of whether jail is enabled or not. `jail_config` will then be
            // assigned `None` if it turns out that `jail_enabled` is `false` after we parse all the
            // arguments.
            jail_config: Some(Default::default()),
            jail_enabled: !cfg!(feature = "default-no-sandbox"),
            kvm_device_path: PathBuf::from(KVM_PATH),
            mac_address: None,
            memory: None,
            memory_file: None,
            mmio_address_ranges: Vec::new(),
            net_vq_pairs: None,
            netmask: None,
            no_legacy: false,
            no_smt: false,
            params: Vec::new(),
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            pci_low_start: None,
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            pcie_ecam: None,
            #[cfg(feature = "direct")]
            pcie_rp: Vec::new(),
            per_vm_core_scheduling: false,
            plugin_gid_maps: Vec::new(),
            plugin_mounts: Vec::new(),
            plugin_root: None,
            pmem_devices: Vec::new(),
            privileged_vm: false,
            protected_vm: ProtectionType::Unprotected,
            pstore: None,
            pvm_fw: None,
            rng: true,
            rt_cpus: Vec::new(),
            serial_parameters: BTreeMap::new(),
            shared_dirs: Vec::new(),
            socket_path: None,
            software_tpm: false,
            #[cfg(feature = "audio")]
            sound: None,
            split_irqchip: false,
            strict_balloon: false,
            stub_pci_devices: Vec::new(),
            swiotlb: None,
            syslog_tag: None,
            tap_fd: Vec::new(),
            tap_name: Vec::new(),
            #[cfg(target_os = "android")]
            task_profiles: Vec::new(),
            usb: true,
            userspace_msr: BTreeMap::new(),
            vcpu_affinity: None,
            vcpu_cgroup_path: None,
            vcpu_count: None,
            vfio: Vec::new(),
            vhost_net: false,
            vhost_net_device_path: PathBuf::from(VHOST_NET_PATH),
            vhost_user_blk: Vec::new(),
            vhost_user_console: Vec::new(),
            vhost_user_fs: Vec::new(),
            vhost_user_gpu: Vec::new(),
            vhost_user_mac80211_hwsim: None,
            vhost_user_net: Vec::new(),
            #[cfg(feature = "audio")]
            vhost_user_snd: Vec::new(),
            vhost_user_vsock: Vec::new(),
            vhost_user_wl: Vec::new(),
            vhost_vsock_device: None,
            #[cfg(feature = "video-decoder")]
            video_dec: None,
            #[cfg(feature = "video-encoder")]
            video_enc: None,
            virtio_input_evdevs: Vec::new(),
            virtio_iommu: false,
            virtio_keyboard: Vec::new(),
            virtio_mice: Vec::new(),
            virtio_multi_touch: Vec::new(),
            virtio_single_touch: Vec::new(),
            virtio_switches: Vec::new(),
            virtio_trackpad: Vec::new(),
            vvu_proxy: Vec::new(),
            wayland_socket_paths: BTreeMap::new(),
            x_display: None,
        }
    }
}
=======
pub(crate) use check_opt_path;
>>>>>>> BRANCH (ed071b crosvm: implement lock-guest-memory feature.)
