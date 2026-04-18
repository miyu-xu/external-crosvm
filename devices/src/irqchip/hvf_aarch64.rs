// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! In-hypervisor GICv3 irqchip for Apple Hypervisor.framework (HVF).
//!
//! SPI delivery uses [`hypervisor::hvf::HvfVm::set_gic_spi`]. Virtio and other devices signal
//! interrupts via [`IrqEdgeEvent`] / [`IrqLevelEvent`]; this chip registers those [`Event`]s and
//! exposes them through [`IrqChip::irq_event_tokens`] so the IRQ handler thread can call
//! [`IrqChip::service_irq_event`], mirroring the WHPX split irqchip pattern.

use std::sync::Arc;

use base::error;
use base::Error;
use base::Event;
use base::Result;
use hypervisor::hvf::HvfVm;
use hypervisor::DeviceKind;
use hypervisor::IrqRoute;
use hypervisor::IrqSource;
use hypervisor::IrqSourceChip;
use hypervisor::MPState;
use hypervisor::Vcpu;
use sync::Mutex;

use super::AARCH64_GIC_NR_SPIS;
use crate::Bus;
use crate::IrqChip;
use crate::IrqChipAArch64;
use crate::IrqChipCap;
use crate::IrqEdgeEvent;
use crate::IrqEventIndex;
use crate::IrqEventSource;
use crate::IrqLevelEvent;
use crate::VcpuRunState;

struct RegisteredIrq {
    gsi: u32,
    event: Event,
    #[allow(dead_code)]
    resample_event: Option<Event>,
    #[allow(dead_code)]
    level: bool,
    source: IrqEventSource,
}

/// AArch64 irqchip backed by the HVF in-framework GICv3.
pub struct HvfKernelIrqChip {
    vm: HvfVm,
    routes: Arc<Mutex<Vec<IrqRoute>>>,
    irq_events: Arc<Mutex<Vec<Option<RegisteredIrq>>>>,
}

impl HvfKernelIrqChip {
    /// Creates the chip and installs the GIC. Must run before any `hv_vcpu_create` (i.e. before
    /// VCPUs are created on this VM).
    pub fn new(vm: HvfVm, num_vcpus: usize) -> Result<HvfKernelIrqChip> {
        vm.init_gic(num_vcpus)?;
        let mut routes: Vec<IrqRoute> = Vec::new();
        for i in 0..AARCH64_GIC_NR_SPIS {
            routes.push(IrqRoute::gic_irq_route(i));
        }
        Ok(HvfKernelIrqChip {
            vm,
            routes: Arc::new(Mutex::new(routes)),
            irq_events: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn arch_try_clone(&self) -> Result<Self> {
        Ok(HvfKernelIrqChip {
            vm: self.vm.try_clone()?,
            routes: self.routes.clone(),
            irq_events: self.irq_events.clone(),
        })
    }

    /// Maps a crosvm GSI from the default GIC routing table to a full GIC interrupt ID (SPIs
    /// start at 32).
    fn gsi_to_spi_intid(gsi: u32) -> u32 {
        gsi.saturating_add(32)
    }

    fn register_irq_event(
        &mut self,
        irq: u32,
        irq_event: &Event,
        resample_event: Option<&Event>,
        level: bool,
        source: IrqEventSource,
    ) -> Result<Option<IrqEventIndex>> {
        let mut reg = RegisteredIrq {
            gsi: irq,
            event: irq_event.try_clone()?,
            resample_event: None,
            level,
            source,
        };
        if let Some(r) = resample_event {
            reg.resample_event = Some(r.try_clone()?);
        }
        let mut irq_events = self.irq_events.lock();
        let index = irq_events.len();
        irq_events.push(Some(reg));
        Ok(Some(index))
    }

    fn unregister_irq_event(&mut self, irq: u32, irq_event: &Event) -> Result<()> {
        let mut irq_events = self.irq_events.lock();
        for slot in irq_events.iter_mut() {
            if let Some(evt) = slot {
                if evt.gsi == irq && irq_event.eq(&evt.event) {
                    *slot = None;
                    break;
                }
            }
        }
        Ok(())
    }
}

impl IrqChip for HvfKernelIrqChip {
    fn add_vcpu(&mut self, _vcpu_id: usize, _vcpu: &dyn Vcpu) -> Result<()> {
        Ok(())
    }

    fn register_edge_irq_event(
        &mut self,
        irq: u32,
        irq_event: &IrqEdgeEvent,
        source: IrqEventSource,
    ) -> Result<Option<IrqEventIndex>> {
        self.register_irq_event(irq, irq_event.get_trigger(), None, false, source)
    }

    fn unregister_edge_irq_event(&mut self, irq: u32, irq_event: &IrqEdgeEvent) -> Result<()> {
        self.unregister_irq_event(irq, irq_event.get_trigger())
    }

    fn register_level_irq_event(
        &mut self,
        irq: u32,
        irq_event: &IrqLevelEvent,
        source: IrqEventSource,
    ) -> Result<Option<IrqEventIndex>> {
        self.register_irq_event(
            irq,
            irq_event.get_trigger(),
            Some(irq_event.get_resample()),
            true,
            source,
        )
    }

    fn unregister_level_irq_event(&mut self, irq: u32, irq_event: &IrqLevelEvent) -> Result<()> {
        self.unregister_irq_event(irq, irq_event.get_trigger())
    }

    fn route_irq(&mut self, route: IrqRoute) -> Result<()> {
        let mut routes = self.routes.lock();
        routes.retain(|r| r.gsi != route.gsi);
        routes.push(route);
        Ok(())
    }

    fn set_irq_routes(&mut self, routes: &[IrqRoute]) -> Result<()> {
        *self.routes.lock() = routes.to_vec();
        Ok(())
    }

    fn irq_event_tokens(&self) -> Result<Vec<(IrqEventIndex, IrqEventSource, Event)>> {
        let mut out = Vec::new();
        for (index, slot) in self.irq_events.lock().iter().enumerate() {
            if let Some(evt) = slot {
                out.push((index, evt.source.clone(), evt.event.try_clone()?));
            }
        }
        Ok(out)
    }

    fn service_irq(&mut self, irq: u32, level: bool) -> Result<()> {
        let routes: Vec<IrqRoute> = self.routes.lock().clone();
        let matches: Vec<IrqRoute> = routes.into_iter().filter(|r| r.gsi == irq).collect();
        if matches.is_empty() {
            return self
                .vm
                .set_gic_spi(Self::gsi_to_spi_intid(irq), level);
        }
        for route in matches {
            match route.source {
                IrqSource::Msi { address, data } => {
                    if level {
                        self.vm.send_gic_msi(address, data)?;
                    }
                }
                IrqSource::Irqchip {
                    chip: IrqSourceChip::Gic,
                    pin,
                } => {
                    self.vm
                        .set_gic_spi(Self::gsi_to_spi_intid(pin), level)?;
                }
                _ => {
                    error!("HvfKernelIrqChip: unexpected route source {:?}", route.source);
                    return Err(Error::new(libc::EINVAL));
                }
            }
        }
        Ok(())
    }

    fn service_irq_event(&mut self, event_index: IrqEventIndex) -> Result<()> {
        let gsi = {
            let irq_events = self.irq_events.lock();
            let evt = irq_events
                .get(event_index)
                .and_then(|s| s.as_ref())
                .ok_or_else(|| Error::new(libc::EINVAL))?;
            evt.event.wait()?;
            evt.gsi
        };

        let routes: Vec<IrqRoute> = self
            .routes
            .lock()
            .iter()
            .filter(|r| r.gsi == gsi)
            .cloned()
            .collect();

        if routes.is_empty() {
            let intid = Self::gsi_to_spi_intid(gsi);
            return self.vm.set_gic_spi(intid, true);
        }

        for route in routes {
            match route.source {
                IrqSource::Msi { address, data } => {
                    self.vm.send_gic_msi(address, data)?;
                }
                IrqSource::Irqchip {
                    chip: IrqSourceChip::Gic,
                    pin,
                } => {
                    let intid = Self::gsi_to_spi_intid(pin);
                    self.vm.set_gic_spi(intid, true)?;
                }
                _ => {
                    error!(
                        "HvfKernelIrqChip::service_irq_event: unexpected route {:?}",
                        route.source
                    );
                    return Err(Error::new(libc::EINVAL));
                }
            }
        }
        Ok(())
    }

    fn broadcast_eoi(&self, _vector: u8) -> Result<()> {
        Ok(())
    }

    fn inject_interrupts(&self, _vcpu: &dyn Vcpu) -> Result<()> {
        Ok(())
    }

    fn halted(&self, _vcpu_id: usize) {}

    fn wait_until_runnable(&self, _vcpu: &dyn Vcpu) -> Result<VcpuRunState> {
        Ok(VcpuRunState::Runnable)
    }

    fn kick_halted_vcpus(&self) {}

    fn get_mp_state(&self, _vcpu_id: usize) -> Result<MPState> {
        Err(Error::new(libc::ENOENT))
    }

    fn set_mp_state(&mut self, _vcpu_id: usize, _state: &MPState) -> Result<()> {
        Err(Error::new(libc::ENOENT))
    }

    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        self.arch_try_clone()
    }

    fn finalize_devices(
        &mut self,
        _resources: &mut resources::SystemAllocator,
        _io_bus: &Bus,
        _mmio_bus: &Bus,
    ) -> Result<()> {
        Ok(())
    }

    fn process_delayed_irq_events(&mut self) -> Result<()> {
        Ok(())
    }

    fn irq_delayed_event_token(&self) -> Result<Option<Event>> {
        Ok(None)
    }

    fn check_capability(&self, _c: IrqChipCap) -> bool {
        false
    }
}

impl IrqChipAArch64 for HvfKernelIrqChip {
    fn try_box_clone(&self) -> Result<Box<dyn IrqChipAArch64>> {
        Ok(Box::new(self.try_clone()?))
    }

    fn as_irq_chip(&self) -> &dyn IrqChip {
        self
    }

    fn as_irq_chip_mut(&mut self) -> &mut dyn IrqChip {
        self
    }

    fn get_vgic_version(&self) -> DeviceKind {
        DeviceKind::ArmVgicV3
    }

    fn finalize(&self) -> Result<()> {
        Ok(())
    }
}
