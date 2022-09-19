// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::marker::PhantomData;

use base::info;
use base::Tube;
use base::WaitContext;
use serde::Deserialize;
use serde::Serialize;
use winapi::um::winuser::GetSystemMetrics;
use winapi::um::winuser::SM_CXSCREEN;
use winapi::um::winuser::SM_CYSCREEN;

use crate::virtio::gpu::Frontend;
use crate::virtio::gpu::ResourceBridgesTrait;
use crate::virtio::gpu::WorkerToken;

// The resource bridge is not supported on Windows, so this struct simply takes the ownership of
// tubes without actual usage of them.
//
// A skin deep reason we want to get rid of resource bridge is that ResourceResponse is actually a
// wrapper of a dma buffer, and the Tube is not going to support that anyway. The fundamental reason
// is that the dma buffer wrapped inside the ResourceResponse is created by virgl_renderer_execute()
// and ultimately comes from drmPrimeHandleToFD(). There is no easy way to implement that in the
// short term. In addition, the other end of this resource bridge seems to be always a wayland
// device, which will not be used for Windows.
pub(crate) struct WinResourceBridges {
    _resource_bridges: Vec<Tube>,
}

impl WinResourceBridges {
    pub fn new(resource_bridges: Vec<Tube>) -> Self {
        Self {
            _resource_bridges: resource_bridges,
        }
    }
}

impl ResourceBridgesTrait for WinResourceBridges {
    fn add_to_wait_context(&self, _wait_ctx: &mut WaitContext<WorkerToken>) {}

    fn set_should_process(&mut self, _index: usize) {}

    fn process_resource_bridges(
        &mut self,
        _state: &mut Frontend,
        _wait_ctx: &mut WaitContext<WorkerToken>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borderless_full_screen_virtual_window_width_should_be_multiple_of_8() {
        struct MockDisplayDataProvider;

        impl ProvideDisplayData for MockDisplayDataProvider {
            fn get_host_display_size() -> (u32, u32) {
                (1366, 768)
            }
        }

        let mode = DisplayMode::<MockDisplayDataProvider>::BorderlessFullScreen(PhantomData);
        let (width, _) = mode.get_virtual_display_size();
        assert_eq!(width % 8, 0);
    }

    #[test]
    fn borderless_full_screen_virtual_window_size_should_be_smaller_than_soft_max() {
        struct MockDisplayDataProvider;

        impl ProvideDisplayData for MockDisplayDataProvider {
            fn get_host_display_size() -> (u32, u32) {
                (DISPLAY_WIDTH_SOFT_MAX + 1, DISPLAY_HEIGHT_SOFT_MAX + 1)
            }
        }

        let mode = DisplayMode::<MockDisplayDataProvider>::BorderlessFullScreen(PhantomData);
        let (width, height) = mode.get_virtual_display_size();
        assert!(width <= DISPLAY_WIDTH_SOFT_MAX);
        assert!(height <= DISPLAY_HEIGHT_SOFT_MAX);
    }
}
