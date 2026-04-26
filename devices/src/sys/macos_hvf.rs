// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! ACPI / netlink helpers for macOS HVF builds.
//!
//! macOS does not provide a Linux-equivalent netlink ACPI event interface.
//!
//! # Guest-initiated shutdown/reset (✅ working)
//!
//! Guest-initiated power-off and reboot are handled via the PSCI handler in the
//! HVF VCPU code (`hypervisor/src/hvf/vcpu.rs`):
//! - `PSCI_SYSTEM_OFF` → `VcpuExit::SystemEventShutdown`
//! - `PSCI_SYSTEM_RESET` → `VcpuExit::SystemEventReset`
//!
//! These are the standard crosvm VM exit events for shutdown/reset and work
//! identically to the KVM path. No ACPI tables are needed for this flow.
//!
//! # Host-initiated ACPI events (🔧 future work)
//!
//! Host-initiated events (macOS UI → VM sleep button, battery status) would
//! require an IOKit power-state notification bridge, which is out of scope.
//!
//! To support host-initiated shutdown from the macOS UI:
//! 1. Replace the `Event`-based approach below with an IOKit power-state callback
//! 2. Register the event fd in `acpi.rs` wait context (line 264)
//! 3. On power event, set the `_S5` GPE and fire the SCI

use std::sync::Arc;

use base::warn;
use base::NetlinkGenericSocket;
use sync::Mutex;

use crate::ac_adapter::AcAdapter;
use crate::acpi::ACPIPMError;
use crate::acpi::GpeResource;
use crate::IrqLevelEvent;

pub(crate) fn get_acpi_event_sock() -> Result<Option<NetlinkGenericSocket>, ACPIPMError> {
    // macOS has no netlink ACPI event family.  Return `Ok(None)` — the caller
    // (`acpi.rs:261-265`) skips adding the socket to the wait context, so
    // `acpi_event_run` below is never dispatched macOS.
    warn!("macOS ACPI: get_acpi_event_sock called (no netlink available, returning None)");
    Ok(None)
}

pub(crate) fn acpi_event_run(
    _sci_evt: &IrqLevelEvent,
    _acpi_event_sock: &Option<NetlinkGenericSocket>,
    _gpe0: &Arc<Mutex<GpeResource>>,
    _ignored_gpe: &[u32],
    _ac_adapter: &Option<Arc<Mutex<AcAdapter>>>,
) {
    // No-op on macOS: called only when `acpi_event_sock.is_some()`, which never
    // happens because `get_acpi_event_sock()` always returns `None`.
    //
    // Guest-initiated shutdown/reset is handled at the VCPU level via PSCI.
    // Host-initiated ACPI events require IOKit integration (future work).
}
