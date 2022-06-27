use crate::{Bus, IrqEdgeEvent, IrqLevelEvent, IrqEventSource};
use base::{error, Error, Event, Result};
use hypervisor::{IrqRoute, MPState, Vcpu};
use resources::SystemAllocator;

#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
mod aarch64;
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
pub use aarch64::*;

use crate::{IrqChip, IrqChipCap, IrqEventIndex, VcpuRunState};

// This IrqChip only works with Gunyah so we only implement it for GunyahVcpu.
impl IrqChip for GunyahKernelIrqChip {
    /// Add a vcpu to the irq chip.
    fn add_vcpu(&mut self, _vcpu_id: usize, _vcpu: &dyn Vcpu) -> Result<()> {
        Ok(())
    }

    /// Register an event with edge-trigger semantic that can trigger an interrupt
    /// for a particular GSI.
    fn register_edge_irq_event(
        &mut self,
        irq: u32,
        irq_event: &IrqEdgeEvent,
        source: IrqEventSource,
    ) -> Result<Option<IrqEventIndex>> {
        self.vm.register_irqfd(irq, irq_event.get_trigger(), None)?;
        Ok(None)
    }

    /// Unregister an event with edge-trigger semantic for a particular GSI.
    fn unregister_edge_irq_event(&mut self, irq: u32, irq_event: &IrqEdgeEvent) -> Result<()> {
        self.vm.unregister_irqfd(irq, irq_event.get_trigger())
    }

    /// Register an event with level-trigger semantic that can trigger an interrupt
    /// for a particular GSI.
    fn register_level_irq_event(
        &mut self,
        irq: u32,
        irq_event: &IrqLevelEvent,
        source: IrqEventSource,
    ) -> Result<Option<IrqEventIndex>> {
        self.vm
            .register_irqfd(irq, irq_event.get_trigger(), Some(irq_event.get_resample()))?;
        Ok(None)
    }

    /// Unregister an event with level-trigger semantic for a particular GSI.
    fn unregister_level_irq_event(&mut self, irq: u32, irq_event: &IrqLevelEvent) -> Result<()> {
        self.vm.unregister_irqfd(irq, irq_event.get_trigger())
    }

    /// Route an IRQ line to an interrupt controller, or to a particular MSI vector.
    fn route_irq(&mut self, _route: IrqRoute) -> Result<()> {
        Ok(())
    }

    /// Replace all irq routes with the supplied routes
    fn set_irq_routes(&mut self, _routes: &[IrqRoute]) -> Result<()> {
        Ok(())
    }

    /// Return a vector of all registered irq numbers and their associated events and event
    /// indices. These should be used by the main thread to wait for irq events.
    /// For the GunyahKernelIrqChip, the kernel handles listening to irq events being triggered by
    /// devices, so this function always returns an empty Vec.
    fn irq_event_tokens(&self) -> Result<Vec<(IrqEventIndex, IrqEventSource, Event)>> {
        Ok(Vec::new())
    }

    /// Either assert or deassert an IRQ line.  Sends to either an interrupt controller, or does
    /// a send_msi if the irq is associated with an MSI.
    /// For the GunyahKernelIrqChip this simply calls the GUNYAH_SET_IRQ_LINE ioctl.
    fn service_irq(&mut self, _irq: u32, _level: bool) -> Result<()> {
        todo!();
    }

    /// Service an IRQ event by asserting then deasserting an IRQ line. The associated Event
    /// that triggered the irq event will be read from. If the irq is associated with a resample
    /// Event, then the deassert will only happen after an EOI is broadcast for a vector
    /// associated with the irq line.
    /// This function should never be called on GunyahKernelIrqChip.
    fn service_irq_event(&mut self, _event_index: IrqEventIndex) -> Result<()> {
        error!("service_irq_event should never be called for GunyahKernelIrqChip");
        Ok(())
    }

    /// Broadcast an end of interrupt.
    /// This should never be called on a GunyahKernelIrqChip because a GUNYAH vcpu should never exit
    /// with the GUNYAH_EXIT_EOI_BROADCAST reason when an in-kernel irqchip exists.
    fn broadcast_eoi(&self, _vector: u8) -> Result<()> {
        error!("broadcast_eoi should never be called for GunyahKernelIrqChip");
        Ok(())
    }

    /// Injects any pending interrupts for `vcpu`.
    /// For GunyahKernelIrqChip this is a no-op because GUNYAH is responsible for injecting all
    /// interrupts.
    fn inject_interrupts(&self, _vcpu: &dyn Vcpu) -> Result<()> {
        Ok(())
    }

    /// Notifies the irq chip that the specified VCPU has executed a halt instruction.
    /// For GunyahKernelIrqChip this is a no-op because GUNYAH handles VCPU blocking.
    fn halted(&self, _vcpu_id: usize) {}

    /// Blocks until `vcpu` is in a runnable state or until interrupted by
    /// `IrqChip::kick_halted_vcpus`.  Returns `VcpuRunState::Runnable if vcpu is runnable, or
    /// `VcpuRunState::Interrupted` if the wait was interrupted.
    /// For GunyahKernelIrqChip this is a no-op and always returns Runnable because GUNYAH handles VCPU
    /// blocking.
    fn wait_until_runnable(&self, _vcpu: &dyn Vcpu) -> Result<VcpuRunState> {
        Ok(VcpuRunState::Runnable)
    }

    /// Makes unrunnable VCPUs return immediately from `wait_until_runnable`.
    /// For GunyahKernelIrqChip this is a no-op because GUNYAH handles VCPU blocking.
    fn kick_halted_vcpus(&self) {}

    /// Get the current MP state of the specified VCPU.
    fn get_mp_state(&self, _vcpu_id: usize) -> Result<MPState> {
        Err(Error::new(libc::ENOENT))
    }

    /// Set the current MP state of the specified VCPU.
    fn set_mp_state(&mut self, _vcpu_id: usize, _state: &MPState) -> Result<()> {
        Err(Error::new(libc::ENOENT))
    }

    /// Attempt to clone this IrqChip instance.
    fn try_clone(&self) -> Result<Self> {
        // Because the GunyahKernelIrqchip struct contains arch-specific fields we leave the
        // cloning to arch-specific implementations
        self.arch_try_clone()
    }

    /// Finalize irqchip setup. Should be called once all devices have registered irq events and
    /// been added to the io_bus and mmio_bus.
    /// GunyahKernelIrqChip does not need to do anything here.
    fn finalize_devices(
        &mut self,
        _resources: &mut SystemAllocator,
        _io_bus: &Bus,
        _mmio_bus: &Bus,
    ) -> Result<()> {
        Ok(())
    }

    /// The GunyahKernelIrqChip doesn't process irq events itself so this function does nothing.
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
