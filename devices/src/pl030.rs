// Copyright 2018 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::convert::TryFrom;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use base::warn;
use serde::Deserialize;
use serde::Serialize;

use crate::pci::CrosvmDeviceId;
use crate::BusAccessInfo;
use crate::BusDevice;
use crate::DeviceId;
use crate::IrqEdgeEvent;
use crate::Suspendable;

// Register offsets
// Data register
const RTCDR: u64 = 0x0;
// Match register
const RTCMR: u64 = 0x4;
// Interrupt status register
const RTCSTAT: u64 = 0x8;
// Interrupt clear register
const RTCEOI: u64 = 0x8;
// Counter load register
const RTCLR: u64 = 0xC;
// Control register
const RTCCR: u64 = 0x10;
const RTCCR_MIE: u32 = 1 << 0;

// A single 4K page is mapped for this device
pub const PL030_AMBA_IOMEM_SIZE: u64 = 0x1000;

// AMBA id registers are at the end of the allocated memory space
const AMBA_ID_OFFSET: u64 = PL030_AMBA_IOMEM_SIZE - 0x20;
const AMBA_MASK_OFFSET: u64 = PL030_AMBA_IOMEM_SIZE - 0x28;

// This is the AMBA id for this device
pub const PL030_AMBA_ID: u32 = 0x00041030;
pub const PL030_AMBA_MASK: u32 = 0x000FFFFF;

/// An emulated ARM pl030 RTC
pub struct Pl030 {
    // Event to be used to interrupt the guest for an alarm event
    alarm_evt: IrqEdgeEvent,

    // This is the delta we subtract from current time to get the
    // counter value
    counter_delta_time: u32,

    // This is the value that triggers an alarm interrupt when it
    // matches with the rtc time
    match_value: u32,

    // status flag to keep track of whether the interrupt is cleared
    // or not
    interrupt_active: bool,

    // interrupt control register
    control: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct Pl030Snapshot {
    counter_delta_time: u32,
    match_value: u32,
    interrupt_active: bool,
    control: u32,
}

fn get_epoch_time() -> u32 {
    let epoch_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("SystemTime::duration_since failed");
    epoch_time.as_secs() as u32
}

impl Pl030 {
    /// Constructs a Pl030 device
    pub fn new(evt: IrqEdgeEvent) -> Pl030 {
        Pl030 {
            alarm_evt: evt,
            counter_delta_time: 0,
            match_value: 0,
            interrupt_active: false,
            control: 0,
        }
    }

    fn counter_value(&self) -> u32 {
        get_epoch_time().wrapping_sub(self.counter_delta_time)
    }
}

impl BusDevice for Pl030 {
    fn device_id(&self) -> DeviceId {
        CrosvmDeviceId::Pl030.into()
    }

    fn debug_label(&self) -> String {
        "Pl030".to_owned()
    }

    fn write(&mut self, info: BusAccessInfo, data: &[u8]) {
        let data_array = match <&[u8; 4]>::try_from(data) {
            Ok(array) => array,
            _ => {
                warn!("bad write size: {} for pl030", data.len());
                return;
            }
        };

        let reg_val = u32::from_ne_bytes(*data_array);
        match info.offset {
            RTCDR => {
                warn!("invalid write to read-only RTCDR register");
            }
            RTCMR => {
                self.match_value = reg_val;
                // TODO(sonnyrao): here we need to set up a timer for
                // when host time equals the value written here and
                // fire the interrupt
                warn!("Not implemented: VM tried to set an RTC alarm");
            }
            RTCEOI => {
                let _ = reg_val;
                self.interrupt_active = false;
            }
            RTCLR => {
                self.counter_delta_time = get_epoch_time().wrapping_sub(reg_val);
            }
            RTCCR => {
                self.control = reg_val & RTCCR_MIE;
            }
            o => panic!("pl030: bad write {}", o),
        }
    }

    fn read(&mut self, info: BusAccessInfo, data: &mut [u8]) {
        let data_array = match <&mut [u8; 4]>::try_from(data) {
            Ok(array) => array,
            _ => {
                warn!("bad write size for pl030");
                return;
            }
        };

        let reg_content: u32 = match info.offset {
            RTCDR => self.counter_value(),
            RTCMR => self.match_value,
            RTCSTAT => self.interrupt_active as u32,
            RTCLR => {
                warn!("invalid read of RTCLR register");
                0
            }
            RTCCR => self.control,
            AMBA_ID_OFFSET => PL030_AMBA_ID,
            AMBA_MASK_OFFSET => PL030_AMBA_MASK,

            o => panic!("pl030: bad read {}", o),
        };
        *data_array = reg_content.to_ne_bytes();
    }
}

impl Suspendable for Pl030 {
    fn snapshot(&mut self) -> anyhow::Result<serde_json::Value> {
        serde_json::to_value(Pl030Snapshot {
            counter_delta_time: self.counter_delta_time,
            match_value: self.match_value,
            interrupt_active: self.interrupt_active,
            control: self.control,
        })
        .with_context(|| format!("error serializing {}", self.debug_label()))
    }

    fn restore(&mut self, data: serde_json::Value) -> anyhow::Result<()> {
        let deser: Pl030Snapshot = serde_json::from_value(data)
            .with_context(|| format!("failed to deserialize {}", self.debug_label()))?;
        self.counter_delta_time = deser.counter_delta_time;
        self.match_value = deser.match_value;
        self.interrupt_active = deser.interrupt_active;
        self.control = deser.control;
        Ok(())
    }

    fn sleep(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn wake(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The RTC device is placed at page 2 in the mmio bus
    const AARCH64_RTC_ADDR: u64 = 0x2000;

    fn pl030_bus_address(offset: u64) -> BusAccessInfo {
        BusAccessInfo {
            address: AARCH64_RTC_ADDR + offset,
            offset,
            id: 0,
        }
    }

    #[test]
    fn test_interrupt_status_register() {
        let mut device = Pl030::new(IrqEdgeEvent::new().unwrap());
        let mut register = [0, 0, 0, 0];

        // set interrupt
        device.interrupt_active = true;
        device.read(pl030_bus_address(RTCSTAT), &mut register);
        assert_eq!(register, [1, 0, 0, 0]);

        // clear interrupt
        device.write(pl030_bus_address(RTCEOI), &[1, 0, 0, 0]);
        device.read(pl030_bus_address(RTCSTAT), &mut register);
        assert_eq!(register, [0, 0, 0, 0]);
    }

    #[test]
    fn test_match_register() {
        let mut device = Pl030::new(IrqEdgeEvent::new().unwrap());
        let mut register = [0, 0, 0, 0];

        device.write(pl030_bus_address(RTCMR), &[1, 2, 3, 4]);
        device.read(pl030_bus_address(RTCMR), &mut register);
        assert_eq!(register, [1, 2, 3, 4]);
    }
}
