// Copyright 2026 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use base::error;
use cros_async::AsyncTube;
use cros_async::Executor;
use vm_control::VirtioIOMMURequest;

use crate::virtio::iommu::ipc_memory_mapper::IommuRequest;
use crate::virtio::iommu::Result;
use crate::virtio::iommu::State;
use crate::virtio::IommuError;

pub(in crate::virtio::iommu) async fn handle_command_tube(
    _state: &Rc<RefCell<State>>,
    command_tube: AsyncTube,
) -> Result<()> {
    loop {
        match command_tube.next::<VirtioIOMMURequest>().await {
            Ok(command) => {
                error!(
                    "virtio-iommu VFIO command is unsupported on macOS: {:?}",
                    command
                );
            }
            Err(e) => return Err(IommuError::VirtioIOMMUReqError(e)),
        }
    }
}

pub(in crate::virtio::iommu) async fn handle_translate_request(
    _ex: &Executor,
    _state: &Rc<RefCell<State>>,
    request_tube: Option<AsyncTube>,
    _response_tubes: Option<BTreeMap<u32, AsyncTube>>,
) -> Result<()> {
    let request_tube = match request_tube {
        Some(r) => r,
        None => {
            futures::future::pending::<()>().await;
            return Ok(());
        }
    };

    loop {
        match request_tube.next::<IommuRequest>().await {
            Ok(_req) => error!("virtio-iommu translation is unsupported on macOS"),
            Err(e) => return Err(IommuError::Tube(e)),
        }
    }
}
