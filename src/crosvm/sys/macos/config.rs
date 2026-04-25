// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::path::PathBuf;
use std::str::FromStr;

use anyhow::anyhow;
use anyhow::Result;
use devices::IommuDevType;
use devices::PciAddress;
use devices::SerialParameters;
use serde::Deserialize;
use serde::Serialize;
use serde_keyvalue::FromKeyValues;

use crate::crosvm::config::Config;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, FromKeyValues)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub enum HypervisorKind {
    Kvm {
        device: Option<PathBuf>,
    },
    #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
    #[cfg(feature = "geniezone")]
    Geniezone {
        device: Option<PathBuf>,
    },
    #[cfg(all(any(target_arch = "arm", target_arch = "aarch64"), feature = "gunyah"))]
    Gunyah {
        device: Option<PathBuf>,
    },
    Hvf,
}

pub fn check_serial_params(_serial_params: &SerialParameters) -> Result<(), String> {
    Ok(())
}

pub fn validate_config(_cfg: &mut Config) -> std::result::Result<(), String> {
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PmemExt2Option {
    pub path: PathBuf,
    pub blocks_per_group: u32,
    pub inodes_per_group: u32,
    pub size: u32,
    pub ugid: (Option<u32>, Option<u32>),
    pub uid_map: String,
    pub gid_map: String,
}

impl Default for PmemExt2Option {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            blocks_per_group: 4096,
            inodes_per_group: 1024,
            size: 4096 * 4096,
            ugid: (None, None),
            uid_map: String::new(),
            gid_map: String::new(),
        }
    }
}

pub fn parse_pmem_ext2_option(_s: &str) -> std::result::Result<PmemExt2Option, String> {
    Err("pmem-ext2 is unsupported on macOS".to_string())
}

#[derive(Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum SharedDirKind {
    #[default]
    FS,
    P9,
}

impl FromStr for SharedDirKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fs" | "FS" => Ok(Self::FS),
            "9p" | "9P" | "p9" | "P9" => Ok(Self::P9),
            _ => Err(anyhow!("invalid file system type")),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct FsConfig;

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct P9Config;

#[derive(Default, Serialize, Deserialize)]
pub struct SharedDir {
    pub src: PathBuf,
    pub tag: String,
    pub kind: SharedDirKind,
    pub ugid: (Option<u32>, Option<u32>),
    pub uid_map: String,
    pub gid_map: String,
    pub fs_cfg: FsConfig,
    pub p9_cfg: P9Config,
}

impl FromStr for SharedDir {
    type Err = anyhow::Error;

    fn from_str(_param: &str) -> Result<Self, Self::Err> {
        Err(anyhow!("shared-dir is unsupported on macOS"))
    }
}

#[derive(Default, Serialize, Deserialize, FromKeyValues)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct VfioOption {
    pub path: PathBuf,
    #[serde(default)]
    pub iommu: IommuDevType,
    pub guest_address: Option<PciAddress>,
    pub dt_symbol: Option<String>,
}
