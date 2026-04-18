// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Linux-only ACPI / netlink types that are not used on macOS HVF builds.

/// Placeholder; generic netlink is unavailable on macOS hosts.
#[derive(Debug)]
pub struct NetlinkGenericSocket;

/// Placeholder ACPI notification (no kernel generic netlink on macOS).
#[derive(Debug)]
pub struct AcpiNotifyEvent {
    pub device_class: String,
    pub bus_id: String,
    pub _type: u32,
    pub data: u32,
}
