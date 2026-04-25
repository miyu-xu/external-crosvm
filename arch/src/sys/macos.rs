// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::BTreeMap;
use std::sync::Arc;

use acpi_tables::aml::Aml;
use base::Tube;
use devices::Bus;
use devices::BusDevice;
use devices::IommuDevType;
use devices::IrqChip;
use devices::IrqEventSource;
use hypervisor::ProtectionType;
use hypervisor::Vm;
use jail::FakeMinijailStub as Minijail;
use resources::AllocOptions;
use resources::SystemAllocator;
use sync::Mutex;

use crate::DeviceRegistrationError;

pub struct PlatformBusResources {
    pub dt_symbol: String,
    pub regions: Vec<(u64, u64)>,
    pub irqs: Vec<(u32, u32)>,
    pub iommus: Vec<(IommuDevType, Option<u32>, Vec<u32>)>,
}

impl PlatformBusResources {
    pub const IRQ_TRIGGER_EDGE: u32 = 1;
    pub const IRQ_TRIGGER_LEVEL: u32 = 4;

    fn new(symbol: String) -> Self {
        Self {
            dt_symbol: symbol,
            regions: vec![],
            irqs: vec![],
            iommus: vec![],
        }
    }
}

pub fn add_goldfish_battery(
    amls: &mut Vec<u8>,
    battery_jail: Option<Minijail>,
    mmio_bus: &Bus,
    irq_chip: &mut dyn IrqChip,
    irq_num: u32,
    resources: &mut SystemAllocator,
    #[cfg(feature = "swap")] _swap_controller: &mut Option<swap::SwapController>,
) -> Result<(Tube, u64), DeviceRegistrationError> {
    if battery_jail.is_some() {
        return Err(DeviceRegistrationError::UnsupportedHostFeature(
            "sandboxed goldfish battery on macOS",
        ));
    }

    let alloc = resources.get_anon_alloc();
    let mmio_base = resources
        .allocate_mmio(
            devices::bat::GOLDFISHBAT_MMIO_LEN,
            alloc,
            "GoldfishBattery".to_string(),
            AllocOptions::new().align(devices::bat::GOLDFISHBAT_MMIO_LEN),
        )
        .map_err(DeviceRegistrationError::AllocateIoResource)?;

    let (control_tube, response_tube) =
        Tube::pair().map_err(DeviceRegistrationError::CreateTube)?;
    let irq_evt = devices::IrqLevelEvent::new().map_err(DeviceRegistrationError::EventCreate)?;
    let goldfish_bat = devices::GoldfishBattery::new(
        mmio_base,
        irq_num,
        irq_evt
            .try_clone()
            .map_err(DeviceRegistrationError::EventClone)?,
        response_tube,
        None,
    )
    .map_err(|_| DeviceRegistrationError::UnsupportedHostFeature("goldfish battery on macOS"))?;
    goldfish_bat.to_aml_bytes(amls);

    irq_chip
        .register_level_irq_event(
            irq_num,
            &irq_evt,
            IrqEventSource::from_device(&goldfish_bat),
        )
        .map_err(DeviceRegistrationError::RegisterIrqfd)?;

    mmio_bus
        .insert(
            Arc::new(Mutex::new(goldfish_bat)),
            mmio_base,
            devices::bat::GOLDFISHBAT_MMIO_LEN,
        )
        .map_err(DeviceRegistrationError::MmioInsert)?;

    Ok((control_tube, mmio_base))
}

pub fn generate_platform_bus<T: BusDevice>(
    devices: Vec<(T, Option<Minijail>)>,
    _irq_chip: &mut dyn IrqChip,
    _mmio_bus: &Bus,
    _resources: &mut SystemAllocator,
    _vm: &mut impl Vm,
    #[cfg(feature = "swap")] _swap_controller: &mut Option<swap::SwapController>,
    _protection_type: ProtectionType,
) -> Result<
    (
        Vec<Arc<Mutex<dyn BusDevice>>>,
        BTreeMap<u32, String>,
        Vec<PlatformBusResources>,
    ),
    DeviceRegistrationError,
> {
    if !devices.is_empty() {
        return Err(DeviceRegistrationError::UnsupportedHostFeature(
            "platform passthrough devices on macOS",
        ));
    }

    Ok((Vec::new(), BTreeMap::new(), Vec::new()))
}
