#!/usr/bin/env bash
# Copyright 2023 Mediatek Inc.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
# Regenerate gzvm_sys bindgen bindings.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

source tools/impl/bindgen-common.sh

GZVM_EXTRAS="// Added by geniezone_sys/bindgen.sh
pub const GZVM_GET_API_VERSION: u16 = 0x9292;
pub const GZVM_CREATE_VM: u16 = 0x9293;
pub const GZVM_CREATE_VCPU: u16 = 0x9295;
pub const GZVM_SET_REGS: u32 = 0x43689296;
pub const GZVM_GET_REGS: u32 = 0x83689297;
pub const GZVM_SET_MEMORY_REGION: u32 = 0x40209298;
pub const GZVM_RUN: u16 = 0x9299;
pub const GZVM_SET_ONE_REG: u32 = 0x4010929a;
pub const GZVM_GET_ONE_REG: u32 = 0x8010929b;
pub const GZVM_IRQ_LINE: u32 = 0x4008929c;
pub const GZVM_CREATE_DEVICE: u32 = 0xc030929d;
pub const GZVM_IOEVENTFD: u32 = 0x4040929e;
pub const GZVM_IRQFD: u32 = 0x4020929f;
pub const GZVM_ENABLE_CAP: u32 = 0x406892a0;
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
    > geniezone_sys/src/aarch64/bindings.rs
