// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use acpi_tables::aml;
use acpi_tables::aml::Aml;

use crate::pci::CrosvmDeviceId;
use crate::BusAccessInfo;
use crate::BusDevice;
use crate::DeviceId;
use crate::Suspendable;

/// ChromeOS ACPI device (GGL0001 / GOOG0016).
///
/// This is a stub that provides ChromeOS-specific ACPI methods
/// (HWID, FWID, FRID, CHSW, BINF, VDAT, etc.) with dummy values.
/// ChromeOS init checks for this device and triggers a security
/// shutdown if absent.
///
/// This is a pure ACPI device — it has no MMIO register interface.
pub struct ChromeOsAcpiDevice;

impl ChromeOsAcpiDevice {
    pub fn new() -> Self {
        ChromeOsAcpiDevice
    }
}

impl BusDevice for ChromeOsAcpiDevice {
    fn device_id(&self) -> DeviceId {
        CrosvmDeviceId::ChromeOsAcpi.into()
    }

    fn debug_label(&self) -> String {
        "ChromeOsAcpi".to_owned()
    }
}

impl Suspendable for ChromeOsAcpiDevice {}

impl Aml for ChromeOsAcpiDevice {
    fn to_aml_bytes(&self, bytes: &mut Vec<u8>) {
        // Build a ChromeOS ACPI device with minimal required methods.
        // HID: GGL0001 (Google's PNP ID for ChromeOS hardware)
        aml::Device::new(
            "CRHW".into(),
            vec![
                &aml::Name::new("_HID".into(), &"GGL0001"),
                &aml::Name::new("_UID".into(), &aml::ZERO),
                // _STA: 0xF = present, enabled, functional
                &aml::Name::new("_STA".into(), &0xFu32),
                // Method List — tells kernel driver which methods exist
                &aml::Name::new(
                    "MLST".into(),
                    &aml::Package::new(vec![
                        &"CHSW", &"HWID", &"FWID", &"FRID",
                        &"BINF", &"VBNV", &"FMAP", &"VDAT",
                    ]),
                ),
                // ChromeOS Switch: bit 5=dev_mode(0=off), bit 9=wp(1=enabled)
                &aml::Name::new("CHSW".into(), &(1u32 << 9)),
                // Hardware ID string
                &aml::Name::new("HWID".into(), &"CROSVM-TEST"),
                // Firmware Write ID
                &aml::Name::new("FWID".into(), &"crosvm_fwid.0.0"),
                // Firmware Read ID
                &aml::Name::new("FRID".into(), &"crosvm_frid.0.0"),
                // Boot Info: [EC_fw, main_fw] — 1=RW, 1=Normal
                &aml::Name::new(
                    "BINF".into(),
                    &aml::Package::new(vec![&1u32, &1u32]),
                ),
                // Vboot NVRAM: [offset, size]
                &aml::Name::new(
                    "VBNV".into(),
                    &aml::Package::new(vec![&0u32, &16u32]),
                ),
                // Flashmap base address
                &aml::Name::new("FMAP".into(), &aml::ZERO),
                // Verified boot data (empty)
                &aml::Name::new("VDAT".into(), &aml::Package::new(vec![&0u32])),
                // GPIO assignments for recovery button, dev switch, write protect
                // Format: [signal, active_low, ...]
                &aml::Name::new(
                    "GPIO".into(),
                    &aml::Package::new(vec![
                        &0u32, &0u32, &0u32, &0u32,  // GPIO.0
                        &0u32, &0u32, &0u32, &0u32,  // GPIO.1
                        &0u32, &0u32, &0u32, &0u32,  // GPIO.2
                        &0u32, &0u32, &0u32, &0u32,  // GPIO.3
                    ]),
                ),
                // MECK — Management Engine Checksum (placeholder)
                &aml::Name::new("MECK".into(), &aml::Package::new(vec![&0u32])),
            ],
        )
        .to_aml_bytes(bytes);
    }
}
