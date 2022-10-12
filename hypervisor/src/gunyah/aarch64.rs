#![allow(unused_variable)]

use gunyah_sys::*;
use libc::ENOTSUP;
use vm_memory::GuestAddress;
use cros_fdt::FdtWriter;

use base::{Result, Error};

use crate::{VmAArch64, Hypervisor, VcpuAArch64, VcpuRegAArch64, PsciVersion, PSCI_0_2};

use crate::PayloadType;
use crate::ProtectionType;
use crate::VcpuInitAArch64;

use super::{GunyahVm, GunyahVcpu};

const GIC_FDT_IRQ_TYPE_SPI: u32 = 0;

const IRQ_TYPE_EDGE_RISING: u32 = 0x00000001;
const IRQ_TYPE_LEVEL_HIGH: u32 = 0x00000004;

impl VmAArch64 for GunyahVm {
    fn get_hypervisor(&self) -> &dyn Hypervisor {
        &self.gh
    }

    fn load_protected_vm_firmware(
        &mut self,
        _fw_addr: GuestAddress,
        _fw_max_size: u64,
    ) -> Result<()> {
        todo!()
    }

    fn create_vcpu(&self, id: usize) -> Result<Box<dyn VcpuAArch64>> {
        Ok(Box::new(GunyahVm::create_vcpu(self, id)?))
    }

    fn create_fdt(
        &self,
        fdt: &mut FdtWriter,
        fdt_address: GuestAddress,
        fdt_size: usize) -> cros_fdt::Result<()> {
        let top_node = fdt.begin_node("gunyah-vm-config")?;

        fdt.property_string("image-name", "crosvm-vm")?;
        fdt.property_string("os-type", "linux")?;

        let memory_node = fdt.begin_node("memory")?;
        fdt.property_u32("#address-cells", 2)?;
        fdt.property_u32("#size-cells", 2)?;

        let mut base_address: Option<GuestAddress> = None;
        self.guest_mem.with_regions(|index, guest_addr, size, host_addr, _, _, _| {
            if index == 0 {
                base_address = Some(guest_addr);
            }
            Ok(())
        })?;
        if let Some(guest_addr) = base_address {
            fdt.property_u64("base-address", guest_addr.offset())?;
        }
        fdt.end_node(memory_node)?;

        let interrupts_node = fdt.begin_node("interrupts")?;
        fdt.property_u32("config", 1)?; // Crosvm TODO: This is PHANDLE_GIC
        fdt.end_node(interrupts_node)?;

        let vcpus_node = fdt.begin_node("vcpus")?;
        fdt.property_string("affinity", "proxy")?;
        fdt.end_node(vcpus_node)?;

        let vdev_node = fdt.begin_node("vdevices")?;
        fdt.property_string("generate", "/hypervisor")?; // Gunyah TODO: don't require generation of /hypervisor node
        for irq in self.routes.lock().iter() {
            let bell_name = format!("bell-{:x}", irq.irq);
            let bell_node = fdt.begin_node(&bell_name)?;
            fdt.property_string("vdevice-type", "doorbell")?;
            let path_name = format!("/hypervisor/bell-{:x}", irq.irq);
            fdt.property_string("generate", &path_name)?;
            fdt.property_u32("label", irq.irq)?; // Gunyah TODO: remove "qcom,"? (maybe already done)
            fdt.property_null("peer-default")?;
            fdt.property_null("source-can-clear")?;

            let interrupt_type = if irq.level == true {
                IRQ_TYPE_LEVEL_HIGH
            } else {
                IRQ_TYPE_EDGE_RISING
            };
            let interrupts = [GIC_FDT_IRQ_TYPE_SPI, irq.irq, interrupt_type];
            fdt.property_array_u32("interrupts", &interrupts)?;
            fdt.end_node(bell_node)?;
        }

        self.guest_mem.with_regions(|label, guest_addr, _, _, _, _, _| {
            if label == 0 {
                return Ok(());
            }

            let shm_name = format!("shm-{:x}", label);
            let shm_node = fdt.begin_node(&shm_name)?;
            fdt.property_string("vdevice-type", "shm")?;
            fdt.property_null("peer-default")?;
            fdt.property_string("push-compatible", "restricted-dma-pool")?; // TODO: don't make this assumption
            fdt.property_u64("dma_base", 0)?;
            let mem_node = fdt.begin_node("memory")?;
            fdt.property_u32("label", label.try_into().unwrap())?;
            fdt.property_u32("#address-cells", 2)?;
            fdt.property_u64("base", guest_addr.offset() as u64)?;
            fdt.end_node(mem_node)?;
            fdt.end_node(shm_node)
        })?;

        // Crosvm TODO: other guest memory regions
        fdt.end_node(vdev_node)?;

        fdt.end_node(top_node)?;

        self.set_dtb_config(fdt_address, fdt_size).unwrap(); // TODO: not unwrap
        Ok(())
    }
}

impl VcpuAArch64 for GunyahVcpu {
    fn init(&self, features: &[crate::VcpuFeature]) -> Result<()> {
        Ok(())
    }

    fn init_pmu(&self, irq: u64) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    fn has_pvtime_support(&self) -> bool {
        false
    }

    fn init_pvtime(&self, pvtime_ipa: u64) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    fn set_one_reg(&self, reg_id: VcpuRegAArch64, data: u64) -> Result<()> {
        unimplemented!()
    }

    fn get_one_reg(&self, reg_id: VcpuRegAArch64) -> Result<u64> {
        Err(Error::new(ENOTSUP))
    }

    fn get_psci_version(&self) -> Result<PsciVersion> {
        Ok(PSCI_0_2)
    }

    fn vcpu_init(
        &self,
        payload: &PayloadType,
        fdt_address: GuestAddress,
        protection_type: ProtectionType,
        firmware_address: Option<u64>,
    ) -> Result<VcpuInitAArch64> {
        // TODO: check base memory address is also the entry point
        Ok(Default::default())
    }

    #[cfg(feature = "gdb")]
    fn set_guest_debug(&self, addrs: &[GuestAddress], enable_singlestep: bool) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    #[cfg(feature = "gdb")]
    fn set_gdb_registers(&self, regs: &<gdbstub_arch::aarch64::AArch64 as gdbstub::arch::Arch>::Registers) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    #[cfg(feature = "gdb")]
    fn get_gdb_registers(&self, regs: &mut <gdbstub_arch::aarch64::AArch64 as gdbstub::arch::Arch>::Registers) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    #[cfg(feature = "gdb")]
    fn get_max_hw_bps(&self) -> Result<usize> {
        Err(Error::new(ENOTSUP))
    }

    #[cfg(feature = "gdb")]
    fn set_gdb_register(&self, reg: <gdbstub_arch::aarch64::AArch64 as gdbstub::arch::Arch>::RegId, data: &[u8]) -> Result<()> {
        Err(Error::new(ENOTSUP))
    }

    #[cfg(feature = "gdb")]
    fn get_gdb_register(&self, reg: <gdbstub_arch::aarch64::AArch64 as gdbstub::arch::Arch>::RegId, data: &mut [u8]) -> Result<usize> {
        Err(Error::new(ENOTSUP))
    }
}