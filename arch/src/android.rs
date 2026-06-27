// Copyright 2019 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;

use cros_fdt::Error;
use cros_fdt::Fdt;
use cros_fdt::Result;

fn parse_fstab_line(line: &str) -> Result<Vec<String>> {
    let vec: Vec<&str> = line.split_whitespace().collect();
    if vec.len() != 5 {
        return Err(Error::FdtParseError("invalid fstab format".into()));
    }
    Ok(vec.iter().map(|s| s.to_string()).collect())
}

fn android_dt_string(value: &str) -> Vec<u8> {
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// Creates a flattened device tree containing all of the parameters used
/// by Android.
///
/// # Arguments
///
/// * `fdt` - The DTB to modify. The root node will be modified.
/// * `android-fstab` - A text file of Android fstab entries to add to the DTB
pub fn create_android_fdt(fdt: &mut Fdt, fstab: File) -> Result<()> {
    let vecs = BufReader::new(fstab)
        .lines()
        .map(|l| parse_fstab_line(l?.as_str()))
        .collect::<Result<Vec<Vec<String>>>>()?;
    let firmware_node = fdt.root_mut().subnode_mut("firmware")?;
    let android_node = firmware_node.subnode_mut("android")?;
    android_node.set_prop("compatible", android_dt_string("android,firmware"))?;

    let (dtprop, fstab): (_, Vec<_>) = vecs.into_iter().partition(|x| x[0] == "#dt-vendor");
    let vendor_node = android_node.subnode_mut("vendor")?;
    for vec in dtprop {
        let content = std::fs::read_to_string(&vec[2])?;
        vendor_node.set_prop(&vec[1], android_dt_string(&content))?;
    }
    let fstab_node = android_node.subnode_mut("fstab")?;
    fstab_node.set_prop("compatible", android_dt_string("android,fstab"))?;
    for vec in fstab {
        let partition = &vec[1][1..];
        let partition_node = fstab_node.subnode_mut(partition)?;
        partition_node.set_prop(
            "compatible",
            android_dt_string(&("android,".to_owned() + partition)),
        )?;
        partition_node.set_prop("dev", android_dt_string(vec[0].as_str()))?;
        partition_node.set_prop("type", android_dt_string(vec[2].as_str()))?;
        partition_node.set_prop("mnt_flags", android_dt_string(vec[3].as_str()))?;
        partition_node.set_prop("fsmgr_flags", android_dt_string(vec[4].as_str()))?;
    }
    Ok(())
}
