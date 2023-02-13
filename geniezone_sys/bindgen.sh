#!/usr/bin/env bash
# Copyright 2023 The ChromiumOS Authors
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Regenerate gzvm_sys bindgen bindings.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

source tools/impl/bindgen-common.sh

GZVM_BINDINGS="hypervisor/src/geniezone/geniezone_sys/aarch64/bindings.rs"

GZVM_EXTRAS="// Added by geniezone_sys/bindgen.sh
pub const GZVM_SYSTEM_EVENT_RESET_FLAG_PSCI_RESET2: u64 = 0x1;
pub const GZVM_VGIC_V3_ADDR_TYPE_REDIST: u32 = 3;
pub const GZVM_DEV_ARM_VGIC_GRP_ADDR: u32 = 0;
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
    > ${GZVM_BINDINGS}

# TODO: temp solution for making sure duplicated macros actually being removed
sed -i -E '/^pub const GZVM_SYSTEM_EVENT_RESET_FLAG_PSCI_RESET2: u32/d' ${GZVM_BINDINGS}
