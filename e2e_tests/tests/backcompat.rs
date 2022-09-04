// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod fixture;
use fixture::vm::Config;
use fixture::vm::TestVm;

#[cfg(target_arch = "aarch64")]
use std::io::Read;

#[cfg(target_arch = "aarch64")]
use tempfile::NamedTempFile;

// Tests for possible backwards compatibility issues.
//
// There is no backwards compatibility policy yet, these are just "change detector" tests. If you
// break a test, make sure the change is intended and then ask in go/crosvm-chat to see if anyone
// objects to updating the golden file.

// Many changes to PCI devices can cause issues, e.g. some users depend on crosvm always choosing
// the same PCI slots for particular devices.
#[test]
fn backcompat_test_simple_lspci() {
    let mut vm = TestVm::new(Config::new()).unwrap();
    let expected = if cfg!(windows) {
        include_str!("goldens/backcompat_test_simple_lspci_win.txt").trim()
    } else {
        include_str!("goldens/backcompat_test_simple_lspci.txt").trim()
    };
    let result = vm
        .exec_in_guest("lspci -n")
        .unwrap()
        .trim()
        .replace("\r", "");
    assert_eq!(
        expected,
        result,
        "PCI Devices changed:\n<<< Expected <<<\n{}\n<<<<<<<<<<<<<<<<\n>>> Got      >>>\n{}\n>>>>>>>>>>>>>>>>\n",
        expected, result
    );
}

#[cfg(target_arch = "aarch64")]
#[test]
fn backcompat_test_dump_dtb() {
    let file = NamedTempFile::new().unwrap();
    let mut dtb_file = file.reopen().unwrap();
    let dtb_path = file.into_temp_path();
    TestVm::new(
            Config::new().extra_args(vec!["--dump-dtb".to_string(), dtb_path.to_str().unwrap().to_string()])
        ).unwrap();

    const EXPECTED: u32 = 0xd00dfeed;
    let mut result = [0u8; 4];
    dtb_file.read_exact(&mut result).unwrap();
    let result_u32 = u32::from_be_bytes(result);
    assert_eq!(
        EXPECTED,
        result_u32,
        "Dumped DTB file doesn't contain have expected magic. Expected: {:08x} Got: {:08x}\n",
        EXPECTED, result_u32
    );
}
