use base::Result;
use hypervisor::DeviceKind;

use crate::{GunyahIrqChip, IrqChipAArch64, IrqChip};

impl IrqChipAArch64 for GunyahIrqChip {
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