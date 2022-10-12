use base::{Result, Error};
use hypervisor::{PicSelect, PicState, IoapicState, LapicState, PitState};
use libc::ENOTSUP;

use crate::{IrqChipX86_64, GunyahIrqChip, IrqChip};

impl IrqChipX86_64 for GunyahIrqChip {
    fn try_box_clone(&self) -> Result<Box<dyn IrqChipX86_64>> {
        Ok(Box::new(self.try_clone()?))
    }

    fn as_irq_chip(&self) -> &dyn IrqChip {
        self
    }

    fn as_irq_chip_mut(&mut self) -> &mut dyn IrqChip {
        self
    }

    fn get_pic_state(&self, _select: PicSelect) -> Result<PicState> {
        Err(Error::new(ENOTSUP))
    }

    fn set_pic_state(&mut self, _select: PicSelect,_statee: &PicState) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    fn get_ioapic_state(&self) -> Result<IoapicState> {
        Err(Error::new(ENOTSUP))
    }

    fn set_ioapic_state(&mut self, _state: &IoapicState) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    fn get_lapic_state(&self, _vcpu_id: usize) -> Result<LapicState> {
        Err(Error::new(ENOTSUP))
    }

    fn set_lapic_state(&mut self, _vcpu_id: usize, _state: &LapicState) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    fn lapic_frequency(&self) -> u32 {
        unimplemented!()
    }

    fn get_pit(&self) -> Result<PitState> {
        Err(Error::new(ENOTSUP))
    }

    fn set_pit(&mut self, _state: &PitState) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    fn pit_uses_speaker_port(&self) -> bool {
        false
    }
}
