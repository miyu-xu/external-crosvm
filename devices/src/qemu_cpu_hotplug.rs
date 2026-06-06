// Copyright 2026 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Minimal QEMU modern CPU hotplug register block for OVMF on Q35.
//!
//! OVMF probes this block at ICH9_CPU_HOTPLUG_BASE (0xCD8) to discover the
//! present/possible CPU counts. Without it, firmware falls back to fw_cfg
//! overrides and may still hang in MpInitLib when issuing LAPIC INIT/SIPI.

use crate::pci::CrosvmDeviceId;
use crate::BusAccessInfo;
use crate::BusDevice;
use crate::DeviceId;
use crate::Suspendable;

/// IO base for the Q35 CPU hotplug register block.
pub const QEMU_CPU_HOTPLUG_BASE: u64 = 0x0CD8;
/// Span covers CMD_DATA2/CPU_SEL through CMD_DATA.
pub const QEMU_CPU_HOTPLUG_LEN: u64 = 12;

const OFF_CMD_DATA2: u64 = 0x0;
const OFF_CPU_STAT: u64 = 0x4;
const OFF_CMD: u64 = 0x5;
const OFF_CMD_DATA: u64 = 0x8;

const STAT_ENABLED: u8 = 0x1;

pub struct QemuCpuHotplug {
    cpu_count: u32,
    selected_cpu: u32,
}

impl QemuCpuHotplug {
    pub fn new(cpu_count: usize) -> Self {
        QemuCpuHotplug {
            cpu_count: cpu_count.max(1) as u32,
            selected_cpu: 0,
        }
    }

    fn cpu_enabled(&self, cpu: u32) -> bool {
        cpu < self.cpu_count
    }
}

impl BusDevice for QemuCpuHotplug {
    fn device_id(&self) -> DeviceId {
        CrosvmDeviceId::QemuCpuHotplug.into()
    }

    fn debug_label(&self) -> String {
        "QemuCpuHotplug".to_owned()
    }

    fn read(&mut self, info: BusAccessInfo, data: &mut [u8]) {
        match (info.offset as u64, data.len()) {
            (OFF_CMD_DATA2, 4) => {
                // Modern mode is active when this reads as zero after GET_PENDING.
                data.copy_from_slice(&0u32.to_le_bytes());
            }
            (OFF_CPU_STAT, 1) => {
                data[0] = if self.cpu_enabled(self.selected_cpu) {
                    STAT_ENABLED
                } else {
                    0
                };
            }
            (OFF_CMD_DATA, 4) => {
                let val = if self.cpu_enabled(self.selected_cpu) {
                    self.selected_cpu
                } else {
                    0
                };
                data.copy_from_slice(&val.to_le_bytes());
            }
            _ => {
                for byte in data.iter_mut() {
                    *byte = 0xff;
                }
            }
        }
    }

    fn write(&mut self, info: BusAccessInfo, data: &[u8]) {
        match (info.offset as u64, data.len()) {
            (OFF_CMD_DATA2, 4) => {
                self.selected_cpu = u32::from_le_bytes(data.try_into().unwrap_or([0; 4]));
            }
            (OFF_CMD, 1) => {
                // QEMU_CPUHP_CMD_GET_PENDING and friends: no side effects needed.
            }
            (OFF_CMD_DATA, 4) => {}
            _ => {}
        }
    }
}

impl Suspendable for QemuCpuHotplug {
    fn sleep(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn wake(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
