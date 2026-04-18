// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! ACPI / netlink helpers are Linux-only; macOS HVF builds skip host ACPI events.

use std::sync::Arc;

use base::NetlinkGenericSocket;
use sync::Mutex;

use crate::acpi::ACPIPMError;
use crate::acpi::GpeResource;
use crate::AcAdapter;
use crate::IrqLevelEvent;

pub(crate) fn get_acpi_event_sock() -> Result<Option<NetlinkGenericSocket>, ACPIPMError> {
    Ok(None)
}

pub(crate) fn acpi_event_run(
    _sci_evt: &IrqLevelEvent,
    _acpi_event_sock: &Option<NetlinkGenericSocket>,
    _gpe0: &Arc<Mutex<GpeResource>>,
    _ignored_gpe: &[u32],
    _ac_adapter: &Option<Arc<Mutex<AcAdapter>>>,
) {
}
