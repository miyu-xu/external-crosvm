use base::Error;
use base::Event;
use base::Result;
use hypervisor::gunyah::GunyahVm;
use hypervisor::IrqRoute;
use hypervisor::MPState;
use hypervisor::Vcpu;
use libc::ENOTSUP;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod x86_64;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub use x86_64::*;

#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
mod aarch64;
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
pub use aarch64::*;

use crate::IrqChip;
use crate::IrqChipCap;
use crate::IrqEdgeEvent;
use crate::IrqEventIndex;
use crate::IrqEventSource;
use crate::IrqLevelEvent;
use crate::VcpuRunState;

pub struct GunyahIrqChip {
    vm: GunyahVm,
}

impl GunyahIrqChip {
    pub fn new(vm: GunyahVm) -> Result<GunyahIrqChip> {
        Ok(GunyahIrqChip { vm })
    }
}

impl IrqChip for GunyahIrqChip {
    fn add_vcpu(&mut self, _vcpu_id: usize, _vcpu: &dyn Vcpu) -> Result<()> {
        Ok(())
    }

    fn register_edge_irq_event(
        &mut self,
        irq: u32,
        irq_event: &IrqEdgeEvent,
        _source: IrqEventSource,
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
        _source: IrqEventSource,
    ) -> Result<Option<IrqEventIndex>> {
        self.vm.register_irqfd(irq, irq_event.get_trigger(), true)?;
        Ok(None)
    }

    fn unregister_level_irq_event(&mut self, irq: u32, irq_event: &IrqLevelEvent) -> Result<()> {
        self.vm.unregister_irqfd(irq, irq_event.get_trigger())?;
        Ok(())
    }

    fn route_irq(&mut self, _route: IrqRoute) -> Result<()> {
        Ok(())
    }

    fn set_irq_routes(&mut self, _routes: &[IrqRoute]) -> Result<()> {
        Ok(())
    }

    fn irq_event_tokens(&self) -> Result<Vec<(usize, IrqEventSource, Event)>> {
        Ok(Vec::new())
    }

    fn service_irq(&mut self, _irq: u32, _level: bool) -> Result<()> {
        Ok(())
    }

    fn service_irq_event(&mut self, _event_index: usize) -> Result<()> {
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
        // Gunyah handles vCPU blocking. From userspace perspective, vCPU is always runnable.
        Ok(VcpuRunState::Runnable)
    }

    fn kick_halted_vcpus(&self) {}

    fn get_mp_state(&self, _vcpu_id: usize) -> Result<MPState> {
        Err(Error::new(ENOTSUP))
    }

    fn set_mp_state(&mut self, _vcpu_id: usize, _state: &MPState) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        Ok(Self {
            vm: self.vm.try_clone()?,
        })
    }

    fn finalize_devices(
        &mut self,
        _resources: &mut resources::SystemAllocator,
        _io_bus: &crate::Bus,
        _mmio_bus: &crate::Bus,
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
