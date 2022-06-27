use base::{Result};
use hypervisor::gunyah::{GunyahVm};
use hypervisor::{DeviceKind, Vm};

use crate::{IrqChip, IrqChipAArch64};

/// IrqChip implementation where the entire IrqChip is emulated by GUNYAH.
///
/// This implementation will use the GUNYAH API to create and configure the in-kernel irqchip.
pub struct GunyahKernelIrqChip {
    pub(super) vm: GunyahVm,
    pub(super) vcpus: usize,
}

impl GunyahKernelIrqChip {
    /// Construct a new GunyahKernelIrqchip.
    pub fn new(vm: GunyahVm, vcpus: usize) -> Result<GunyahKernelIrqChip> {
        Ok(GunyahKernelIrqChip {
            vm,
            vcpus
        })
    }

    /// Attempt to create a shallow clone of this aarch64 GunyahKernelIrqChip instance.
    pub(super) fn arch_try_clone(&self) -> Result<Self> {
        Ok(GunyahKernelIrqChip {
            vm: self.vm.try_clone()?,
            vcpus: self.vcpus,
        })
    }
}

impl IrqChipAArch64 for GunyahKernelIrqChip {
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
