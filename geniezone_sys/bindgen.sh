#!/usr/bin/env bash
# Copyright 2023 Mediatek Inc.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
# Regenerate gzvm_sys bindgen bindings.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

source tools/impl/bindgen-common.sh

GZVM_EXTRAS="// Added by geniezone_sys/bindgen.sh
pub const GZVM_GET_API_VERSION: u32 = 0x00009200;
pub const GZVM_CREATE_VM: u32 = 0x00009201;
pub const GZVM_CHECK_EXTENSION: u32 = 0x00009203;
pub const GZVM_GET_VCPU_MMAP_SIZE: u32 = 0x00009204;
pub const GZVM_SET_MEMORY_REGION: u32 = 0x40189240;
pub const GZVM_CREATE_VCPU: u32 = 0x00009241;
pub const GZVM_GET_DIRTY_LOG: u32 = 0x40109242;
pub const GZVM_SET_NR_MMU_PAGES: u32 = 0x00009244;
pub const GZVM_GET_NR_MMU_PAGES: u32 = 0x00009245;
pub const GZVM_SET_USER_MEMORY_REGION: u32 = 0x40209246;
pub const GZVM_CREATE_IRQCHIP: u32 = 0x00009260;
pub const GZVM_IRQ_LINE: u32 = 0x40089261;
pub const GZVM_IRQ_LINE_STATUS: u32 = 0xc0089267;
pub const GZVM_REGISTER_COALESCED_MMIO: u32 = 0x40109267;
pub const GZVM_UNREGISTER_COALESCED_MMIO: u32 = 0x40109268;
pub const GZVM_ASSIGN_PCI_DEVICE: u32 = 0x80409269;
pub const GZVM_SET_GSI_ROUTING: u32 = 0x4008926a;
pub const GZVM_ASSIGN_DEV_IRQ: u32 = 0x40409270;
pub const GZVM_DEASSIGN_PCI_DEVICE: u32 = 0x40409272;
pub const GZVM_ASSIGN_SET_MSIX_NR: u32 = 0x40089273;
pub const GZVM_ASSIGN_SET_MSIX_ENTRY: u32 = 0x40109274;
pub const GZVM_DEASSIGN_DEV_IRQ: u32 = 0x40409275;
pub const GZVM_IRQFD: u32 = 0x40209276;
pub const GZVM_IOEVENTFD: u32 = 0x40409279;
pub const GZVM_SIGNAL_MSI: u32 = 0x402092a5;
pub const GZVM_ARM_SET_DEVICE_ADDR: u32 = 0x401092ab;
pub const GZVM_SET_PMU_EVENT_FILTER: u32 = 0x400892b2;
pub const GZVM_ARM_MTE_COPY_TAGS: u32 = 0x803092b4;
pub const GZVM_CREATE_DEVICE: u32 = 0xc03092e0;
pub const GZVM_SET_DEVICE_ATTR: u32 = 0x401892e1;
pub const GZVM_GET_DEVICE_ATTR: u32 = 0x401892e2;
pub const GZVM_HAS_DEVICE_ATTR: u32 = 0x401892e3;
pub const GZVM_RUN: u32 = 0x00009280;
pub const GZVM_GET_REGS: u32 = 0x83689281;
pub const GZVM_SET_REGS: u32 = 0x43689282;
pub const GZVM_SET_SIGNAL_MASK: u32 = 0x4004928b;
pub const GZVM_GET_MP_STATE: u32 = 0x80049298;
pub const GZVM_SET_MP_STATE: u32 = 0x40049299;
pub const GZVM_SET_GUEST_DEBUG: u32 = 0x4208929b;
pub const GZVM_GET_VCPU_EVENTS: u32 = 0x8040929f;
pub const GZVM_SET_VCPU_EVENTS: u32 = 0x404092a0;
pub const GZVM_ENABLE_CAP: u32 = 0x406892a3;
pub const GZVM_GET_ONE_REG: u32 = 0x401092ab;
pub const GZVM_SET_ONE_REG: u32 = 0x401092ac;
pub const GZVM_KVMCLOCK_CTRL: u32 = 0x000092ad;
pub const GZVM_ARM_VCPU_INIT: u32 = 0x402092ae;
pub const GZVM_ARM_PREFERRED_TARGET: u32 = 0x802092af;
pub const GZVM_GET_REG_LIST: u32 = 0xc00892b0;
pub const GZVM_MEMORY_ENCRYPT_OP: u32 = 0xc00892ba;
pub const GZVM_MEMORY_ENCRYPT_REG_REGION: u32 = 0x801092bb;
pub const GZVM_MEMORY_ENCRYPT_UNREG_REGION: u32 = 0x801092bc;
pub const GZVM_CLEAR_DIRTY_LOG: u32 = 0xc01892c0;
pub const GZVM_ARM_VCPU_FINALIZE: u32 = 0x400492c2;
"

bindgen_generate \
    --raw-line "${GZVM_EXTRAS}" \
    --blocklist-item='__kernel.*' \
    --blocklist-item='__BITS_PER_LONG' \
    --blocklist-item='__FD_SETSIZE' \
    --blocklist-item='_?IOC.*' \
    "${BINDGEN_LINUX_ARM64_HEADERS}/include/linux/gzvm_common.h" \
    -- \
    -isystem "${BINDGEN_LINUX_ARM64_HEADERS}/include" \
    | replace_linux_int_types \
    > hypervisor/src/geniezone/geniezone_sys/mod.rs

