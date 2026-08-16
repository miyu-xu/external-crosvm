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
    virtual_display_size: VirtualDisplaySize,
    canonical_rotation_quarters: u8,
}

impl NoopVirtualDisplayManager {
    pub fn new(
        host_viewport_size: &HostWindowSize,
        virtual_display_size: &VirtualDisplaySize,
    ) -> Self {
        let mut manager = Self {
            host_to_guest_transform: Transform2D::identity(),
            host_viewport_size: *host_viewport_size,
            virtual_display_size: *virtual_display_size,
            canonical_rotation_quarters: if virtual_display_size.width > virtual_display_size.height
            {
                1
            } else {
                0
            },
        };
        manager.update_host_guest_transforms(host_viewport_size);
        manager
    }

    /// Returns the rectangle to show the virtual display in the host window coordinate.
    /// Uses the full window client rect so gfxstream has proper rendering bounds.
    pub fn get_virtual_display_projection_box(&self) -> Box2D<i32, HostWindowSpace> {
        Box2D::from_origin_and_size(Point2D::new(0, 0), self.host_viewport_size)
    }

    pub fn update_host_guest_transforms(&mut self, host_viewport_size: &HostWindowSize) {
        self.update_host_guest_transforms_with_rotation(
            host_viewport_size,
            self.canonical_rotation_quarters,
        );
    }

    /// Updates the host projection and the absolute touchscreen transform as one transaction.
    ///
    /// `canonical_rotation_quarters` is Android's clockwise orientation from the instance's
    /// natural portrait display. The crosvm virtual display remains fixed at its launch mode, so
    /// absolute input must be projected into the matching orientation-aware physical axes before
    /// Android InputReader maps it back into the app's logical display.
    pub fn update_host_guest_transforms_with_rotation(
        &mut self,
        host_viewport_size: &HostWindowSize,
        canonical_rotation_quarters: u8,
    ) {
        self.host_viewport_size = *host_viewport_size;
        self.canonical_rotation_quarters = canonical_rotation_quarters & 3;
        let host_width = f64::from(host_viewport_size.width.max(1));
        let host_height = f64::from(host_viewport_size.height.max(1));
        let virtual_width = f64::from(self.virtual_display_size.width.max(1));
        let virtual_height = f64::from(self.virtual_display_size.height.max(1));
        let physical_base_rotation =
            u8::from(self.virtual_display_size.width > self.virtual_display_size.height);
        let relative_rotation = self
            .canonical_rotation_quarters
            .wrapping_sub(physical_base_rotation)
            & 3;
        self.host_to_guest_transform = match relative_rotation {
            // Android InputReader rotates an orientation-aware physical touchscreen back into
            // the app's logical display. Feed the corresponding clockwise physical projection:
            // host logical (x, y) -> physical (1 - y, x).
            1 => Transform2D::new(
                0.0,
                virtual_height / host_width,
                -virtual_width / host_height,
                0.0,
                virtual_width,
                0.0,
            ),
            2 => Transform2D::new(
                -virtual_width / host_width,
                0.0,
                0.0,
                -virtual_height / host_height,
                virtual_width,
                virtual_height,
            ),
            // ROTATION_270: host logical (x, y) -> physical (y, 1 - x).
            3 => Transform2D::new(
                0.0,
                -virtual_height / host_width,
                virtual_width / host_height,
                0.0,
                0.0,
                virtual_height,
            ),
            _ => Transform2D::scale(virtual_width / host_width, virtual_height / host_height),
        };
    }

    pub fn get_host_to_guest_transform(&self) -> &HostToGuestTransform {
        &self.host_to_guest_transform
    }

    pub fn get_virtual_display_size(&self) -> &VirtualDisplaySize {
        &self.virtual_display_size
    }
}
