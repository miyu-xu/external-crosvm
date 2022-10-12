#![allow(unused_variables)]

use base::Result;
use vm_memory::GuestAddress;

use crate::{HypervisorX86_64, VmX86_64, VcpuX86_64, CpuIdEntry, CpuId, Register, Fpu, DebugRegs, Sregs, Regs};

use super::{GunyahVm, Gunyah, GunyahVcpu};

impl HypervisorX86_64 for Gunyah {
    fn get_supported_cpuid(&self) -> base::Result<crate::CpuId> {
        unimplemented!()
    }

    fn get_emulated_cpuid(&self) -> base::Result<crate::CpuId> {
        unimplemented!()
    }

    fn get_msr_index_list(&self) -> base::Result<Vec<u32>> {
        unimplemented!()
    }
}

impl VmX86_64 for GunyahVm {
    fn get_hypervisor(&self) -> &dyn HypervisorX86_64 {
        &self.gh
    }

    fn create_vcpu(&self, id: usize) -> base::Result<Box<dyn crate::VcpuX86_64>> {
        unimplemented!()
    }

    fn set_tss_addr(&self, addr: GuestAddress) -> base::Result<()> {
        unimplemented!()
    }

    fn set_identity_map_addr(&self, addr: GuestAddress) -> base::Result<()> {
        unimplemented!()
    }
}

impl VcpuX86_64 for GunyahVcpu {
    fn set_interrupt_window_requested(&self, requested: bool) {
        unimplemented!();
    }

    fn ready_for_interrupt(&self) -> bool {
        unimplemented!();
    }

    fn interrupt(&self, irq: u32) -> Result<()> {
        unimplemented!();
    }

    fn inject_nmi(&self) -> Result<()> {
        unimplemented!();
    }

    fn get_regs(&self) -> Result<Regs> {
        unimplemented!();
    }

    fn set_regs(&self, regs: &Regs) -> Result<()> {
        unimplemented!();
    }

    fn get_sregs(&self) -> Result<Sregs> {
        unimplemented!();
    }

    fn set_sregs(&self, sregs: &Sregs) -> Result<()> {
        unimplemented!();
    }

    fn get_fpu(&self) -> Result<Fpu> {
        unimplemented!();
    }

    fn set_fpu(&self, fpu: &Fpu) -> Result<()> {
        unimplemented!();
    }

    fn get_debugregs(&self) -> Result<DebugRegs> {
        unimplemented!();
    }

    fn set_debugregs(&self, debugregs: &DebugRegs) -> Result<()> {
        unimplemented!();
    }

    fn get_xcrs(&self) -> Result<Vec<Register>> {
        unimplemented!();
    }

    fn set_xcrs(&self, xcrs: &[Register]) -> Result<()> {
        unimplemented!();
    }

    fn get_msrs(&self, msrs: &mut Vec<Register>) -> Result<()> {
        unimplemented!();
    }

    fn set_msrs(&self, msrs: &[Register]) -> Result<()> {
        unimplemented!();
    }

    fn set_cpuid(&self, cpuid: &CpuId) -> Result<()> {
        unimplemented!();
    }

    fn get_hyperv_cpuid(&self) -> Result<CpuId> {
        unimplemented!();
    }

    fn set_guest_debug(&self, addrs: &[GuestAddress], enable_singlestep: bool) -> Result<()> {
        unimplemented!();
    }

    fn handle_cpuid(&mut self, entry: &CpuIdEntry) -> Result<()> {
        unimplemented!();
    }

    fn get_tsc_offset(&self) -> Result<u64> {
        unimplemented!();
    }

    fn set_tsc_offset(&self, offset: u64) -> Result<()> {
        unimplemented!();
    }
}
