use base::{Result, Event, Error};
use hypervisor::{gunyah::GunyahVm, IrqRoute, Vcpu, MPState};

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod x86_64;
use libc::ENOTSUP;
use sync::Mutex;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub use x86_64::*;

#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
mod aarch64;
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
pub use aarch64::*;

use crate::{IrqChip, IrqEdgeEvent, IrqEventSource, IrqLevelEvent, VcpuRunState, IrqEventIndex, IrqChipCap};

pub struct GunyahIrqChip {
    vm: GunyahVm,
}

impl GunyahIrqChip {
    pub fn new(vm: GunyahVm) -> Result<GunyahIrqChip> {
        Ok(GunyahIrqChip {
            vm,
        })
    }
}

impl IrqChip for GunyahIrqChip {
    fn add_vcpu(&mut self, vcpu_id: usize, vcpu: &dyn Vcpu) -> Result<()> {
        Ok(())
    }

    fn register_edge_irq_event(
        &mut self,
        irq: u32,
        irq_event: &IrqEdgeEvent,
        source: IrqEventSource,
    ) -> Result<Option<IrqEventIndex>> {
        self.vm
            .register_irqfd(irq, irq_event.get_trigger(), false)?;
        Ok(None)
    }

    fn unregister_edge_irq_event(&mut self, irq: u32, irq_event: &IrqEdgeEvent) -> Result<()> {
        self.vm.unregister_irqfd(irq, irq_event.get_trigger())?;
        Ok(())
    }

    fn register_level_irq_event(
        &mut self,
        irq: u32,
        irq_event: &IrqLevelEvent,
        source: IrqEventSource,
    ) -> Result<Option<IrqEventIndex>> {
        self.vm
            .register_irqfd(irq, irq_event.get_trigger(), true)?;
        Ok(None)
    }

    fn unregister_level_irq_event(&mut self, irq: u32, irq_event: &IrqLevelEvent) -> Result<()> {
        self.vm.unregister_irqfd(irq, irq_event.get_trigger())?;
        Ok(())
    }

    fn route_irq(&mut self, route: IrqRoute) -> Result<()> {
        Ok(())
    }

    fn set_irq_routes(&mut self, routes: &[IrqRoute]) -> Result<()> {
        Ok(())
    }

    fn irq_event_tokens(&self) -> Result<Vec<(usize, IrqEventSource, Event)>> {
        Ok(Vec::new())
    }

    fn service_irq(&mut self, irq: u32, level: bool) -> Result<()> {
        Ok(())
    }

    fn service_irq_event(&mut self, event_index: usize) -> Result<()> {
        Ok(())
    }

    fn broadcast_eoi(&self, vector: u8) -> Result<()> {
        Ok(())
    }

    fn inject_interrupts(&self, vcpu: &dyn Vcpu) -> Result<()> {
        Ok(())
    }

    fn halted(&self, vcpu_id: usize) { }

    fn wait_until_runnable(&self, vcpu: &dyn Vcpu) -> Result<VcpuRunState> {
        // Gunyah handles vCPU blocking. From userspace perspective, vCPU is always runnable.
        Ok(VcpuRunState::Runnable)
    }

    fn kick_halted_vcpus(&self) { }

    fn get_mp_state(&self, vcpu_id: usize) -> Result<MPState> {
        Err(Error::new(ENOTSUP))
    }

    fn set_mp_state(&mut self, vcpu_id: usize, state: &MPState) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized {
        Ok(Self {
            vm: self.vm.try_clone()?,
        })
    }

    fn finalize_devices(
        &mut self,
        resources: &mut resources::SystemAllocator,
        io_bus: &crate::Bus,
        mmio_bus: &crate::Bus,
    ) -> Result<()> {
        Ok(())
    }

    fn process_delayed_irq_events(&mut self) -> Result<()> {
        Ok(())
    }

    fn irq_delayed_event_token(&self) -> Result<Option<Event>> {
        Ok(None)
    }

    fn check_capability(&self, c: IrqChipCap) -> bool {
        false
    }
}
