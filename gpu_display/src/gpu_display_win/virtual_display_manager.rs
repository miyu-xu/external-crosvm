// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use euclid::Box2D;
use euclid::Point2D;
use euclid::Size2D;
use euclid::Transform2D;

use super::HostWindowSpace;
use super::VirtualDisplaySpace;

type HostWindowSize = Size2D<i32, HostWindowSpace>;
type VirtualDisplaySize = Size2D<i32, VirtualDisplaySpace>;
type HostToGuestTransform = Transform2D<f64, HostWindowSpace, VirtualDisplaySpace>;

/// This struct is managing the host window to guest display coordinates transform.
pub struct NoopVirtualDisplayManager {
    host_to_guest_transform: HostToGuestTransform,
    host_viewport_size: HostWindowSize,
}

impl NoopVirtualDisplayManager {
    pub fn new(
        host_viewport_size: &HostWindowSize,
        _virtual_display_size: &VirtualDisplaySize,
    ) -> Self {
        Self {
            host_to_guest_transform: Default::default(),
            host_viewport_size: *host_viewport_size,
        }
    }

    /// Returns the rectangle to show the virtual display in the host window coordinate.
    /// Uses the full window client rect so gfxstream has proper rendering bounds.
    pub fn get_virtual_display_projection_box(&self) -> Box2D<i32, HostWindowSpace> {
        Box2D::from_origin_and_size(
            Point2D::new(0, 0),
            self.host_viewport_size,
        )
    }

    pub fn update_host_guest_transforms(&mut self, host_viewport_size: &HostWindowSize) {
        self.host_viewport_size = *host_viewport_size;
    }

    pub fn get_host_to_guest_transform(&self) -> &HostToGuestTransform {
        &self.host_to_guest_transform
    }
}
