// Copyright 2020 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::RefCell;
use std::collections::btree_map::Entry;
use std::collections::BTreeMap as Map;
use std::collections::BTreeSet as Set;
use std::num::NonZeroU32;
use std::rc::Rc;

use super::protocol::{GpuResponse::*, VirtioGpuResult};
use base::error;
use data_model::*;
use gpu_display::*;
use vm_memory::GuestMemory;
use crate::virtio::gpu::GpuDisplayParameters;

pub trait VirtioResource {
    fn width(&self) -> u32;

    fn height(&self) -> u32;

    fn import_to_display(&mut self, display: &Rc<RefCell<GpuDisplay>>) -> Option<u32>;

    /// Performs a transfer to the given resource in the host from its backing in guest memory.
    fn write_from_guest_memory(
        &mut self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        src_offset: u64,
        _mem: &GuestMemory,
    );

    /// Reads from this resource in the host to a volatile slice of memory.
    fn read_to_volatile(
        &mut self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        dst: VolatileSlice,
        dst_stride: u32,
    );
}



/// Handles some of the common functionality across the virtio 2D and 3D backends.
pub struct VirtioBackend {
    pub display: Rc<RefCell<GpuDisplay>>,
    pub display_params: Vec<GpuDisplayParameters>,

    pub scanout_id_to_surface_id: Map<u32, u32>,
    pub scanout_id_to_resource_id: Map<u32, NonZeroU32>,
    pub resource_id_to_scanout_ids: Map<NonZeroU32, Set<u32>>,

    // The resource that is providing the cursor image data.
    pub cursor_resource_id: Option<NonZeroU32>,
    // The surface within the display for the cursor image.
    pub cursor_surface_id: Option<u32>,
    // The scanout that the cursor is currently positioned inside of.
    pub cursor_scanout_id: Option<u32>,

    // Maps event devices to scanout number.
    pub event_devices: Map<u32, u32>,
}

impl VirtioBackend {

    pub fn new(display: Rc<RefCell<GpuDisplay>>, display_params: Vec<GpuDisplayParameters>) -> VirtioBackend {
        VirtioBackend {
            display,
            display_params,
            scanout_id_to_surface_id: Default::default(),
            scanout_id_to_resource_id: Default::default(),
            resource_id_to_scanout_ids: Default::default(),
            cursor_resource_id: None,
            cursor_scanout_id: None,
            cursor_surface_id: None,
            event_devices: Default::default(),
        }
    }

    fn update_scanout_resource_id(&mut self, scanout_id: u32, resource_id: u32) {
        match self.scanout_id_to_resource_id.get(&scanout_id) {
            Some(previous_resource_id) => {
                match self.resource_id_to_scanout_ids.get_mut(previous_resource_id) {
                    Some(scanout_ids) => {
                        scanout_ids.remove(&scanout_id);
                    },
                    None => {},
                }
            },
            None => {},
        }

        if resource_id == 0 {
            self.scanout_id_to_resource_id.remove(&scanout_id);
        } else {
            let resource_id = NonZeroU32::new(resource_id).unwrap();
            self.resource_id_to_scanout_ids.entry(resource_id).or_default().insert(scanout_id);
            self.scanout_id_to_resource_id.insert(scanout_id, resource_id);
        }
    }

    pub fn get_scanouts_for_resource(&self, resource_id: u32) -> Vec<u32> {
        let mut dependent_scanout_ids = Vec::new();

        if let Some(resource_id) = NonZeroU32::new(resource_id) {
            if let Some(scanout_ids) = self.resource_id_to_scanout_ids.get(&resource_id) {
                dependent_scanout_ids.extend(scanout_ids);
            }
        }

        dependent_scanout_ids
    }

    pub fn get_or_create_scanout_surface_id(&mut self, scanout_id: u32) -> Option<u32> {
        let mut display = self.display.borrow_mut();

        match self.scanout_id_to_surface_id.entry(scanout_id) {
            Entry::Occupied(entry) => Some(*entry.get()),
            Entry::Vacant(entry) => {
                let display_width = self.display_params[scanout_id as usize].width;
                let display_height = self.display_params[scanout_id as usize].height;

                match display.create_surface(None, display_width, display_height) {
                    Ok(surface_id) => {
                        for (event_device_id, _) in &self.event_devices {
                            display.attach_event_device(surface_id, *event_device_id);
                        }
                        entry.insert(surface_id);
                        Some(surface_id)
                    }
                    Err(e) => {
                        error!("failed to create display surface: {}", e);
                        None
                    }
                }
            },
        }
    }

    fn release_scanout_surface(&mut self, scanout_id: u32) {
        if let Some(surface_id) = self.scanout_id_to_surface_id.get(&scanout_id) {
            self.display.borrow_mut().release_surface(*surface_id);
        }
    }

    pub fn get_scanout_dimensions(&self, scanout_id: u32) -> Option<(u32, u32)> {
        self.display_params.get(scanout_id as usize).map(|params| (params.width, params.height))
    }

    pub fn import_event_device(&mut self, event_device: EventDevice, scanout_id: u32) -> VirtioGpuResult {
        // TODO(zachr): support more than one scanout.
        if scanout_id != 0 {
            error!("got nonzero scanout_id: {:}, but only support zero.", scanout_id);
            return Err(ErrUnspec);
        }

        let mut display = self.display.borrow_mut();
        let event_device_id = match display.import_event_device(event_device) {
            Ok(id) => id,
            Err(e) => {
                error!("error importing event device: {}", e);
                return Err(ErrUnspec);
            }
        };

        if let Some(surface_id) = self.scanout_id_to_surface_id.get(&scanout_id) {
            display.attach_event_device(*surface_id, event_device_id)
        }

        self.event_devices.insert(event_device_id, scanout_id);
        Ok(OkNoData)
    }

    /// Gets the list of supported display resolutions as a slice of `(width, height)` tuples.
    pub fn display_info(&self) -> Vec<(u32, u32)> {
        self.display_params.iter().map(|params| (params.width, params.height)).collect::<Vec<_>>()
    }

    /// Processes the internal `display` events and returns `true` if the main display was closed.
    pub fn process_display(&mut self) -> bool {
        let mut display = self.display.borrow_mut();
        display.dispatch_events();
        self.scanout_id_to_surface_id.values().any(|surface_id| !display.close_requested(*surface_id))
    }

    /// Sets the given resource id as the source of scanout to the display.
    pub fn set_scanout(&mut self, scanout_id: u32, resource_id: u32) -> VirtioGpuResult {
        self.update_scanout_resource_id(scanout_id, resource_id);

        if resource_id == 0 {
            self.release_scanout_surface(scanout_id);
        } else {
            self.get_or_create_scanout_surface_id(scanout_id);
        }

        Ok(OkNoData)
    }

    pub fn flush_resource(
        &mut self,
        resource: &mut dyn VirtioResource,
        resource_id: u32,
    ) -> VirtioGpuResult {
        let mut response = Ok(OkNoData);

        if resource_id == 0 {
            return response;
        }

        let scanout_ids = self.get_scanouts_for_resource(resource_id);
        for scanout_id in scanout_ids {
            let scanout_dimensions = match self.get_scanout_dimensions(scanout_id) {
                Some(d) => d,
                None => {
                    error!("unknown scanout dimensions");
                    return Err(ErrUnspec);
                },
            };

            let surface_width = scanout_dimensions.0;
            let surface_height = scanout_dimensions.1;

            if let Some(surface_id) = self.scanout_id_to_surface_id.get(&scanout_id) {
                response = self.flush_resource_to_surface(resource,
                                                          *surface_id,
                                                          surface_width,
                                                          surface_height);
                if !response.is_ok() {
                    return response;
                }
            }
        }

        if let Some(cursor_resource_id) = self.cursor_resource_id {
            if cursor_resource_id.get() == resource_id {
                if let Some(surface_id) = self.cursor_surface_id {
                    let resource_width = resource.width();
                    let resource_height = resource.height();
                    response = self.flush_resource_to_surface(resource,
                                                              surface_id,
                                                              resource_width,
                                                              resource_height);
                }
            }
        }

        Ok(OkNoData)
    }

    pub fn flush_resource_to_surface(
        &mut self,
        resource: &mut dyn VirtioResource,
        surface_id: u32,
        surface_width: u32,
        surface_height: u32,
    ) -> VirtioGpuResult {
        if let Some(import_id) = resource.import_to_display(&self.display) {
            self.display.borrow_mut().flip_to(surface_id, import_id);
            return Ok(OkNoData);
        }

        // Import failed, fall back to a copy.
        let mut display = self.display.borrow_mut();
        // Prevent overwriting a buffer that is currently being used by the compositor.
        if display.next_buffer_in_use(surface_id) {
            return Ok(OkNoData);
        }

        let fb = match display.framebuffer_region(
            surface_id,
            0,
            0,
            surface_width,
            surface_height,
        ) {
            Some(fb) => fb,
            None => {
                error!("failed to access framebuffer for surface {}", surface_id);
                return Err(ErrUnspec);
            }
        };

        resource.read_to_volatile(
            0,
            0,
            surface_width,
            surface_height,
            fb.as_volatile_slice(),
            fb.stride(),
        );

        display.flip(surface_id);

        Ok(OkNoData)
    }

    /// Updates the cursor's memory to the given id, and sets its position to the given coordinates.
    pub fn update_cursor(
        &mut self,
        resource_id: u32,
        scanout_id: u32,
        x: u32,
        y: u32,
        resource: Option<&mut dyn VirtioResource>,
    ) -> VirtioGpuResult {
        if resource_id == 0 {
            if let Some(cursor_surface_id) = self.cursor_surface_id.take() {
                self.display.borrow_mut().release_surface(cursor_surface_id);
            }
            self.cursor_resource_id = None;
            return Err(OkNoData);
        }

        if resource.is_none() {
            return Err(ErrInvalidResourceId);
        }
        let resource = resource.unwrap();

        self.cursor_resource_id = NonZeroU32::new(resource_id);

        let cursor_scanout_id = match self.cursor_scanout_id {
            Some(id) => id,
            None => {
                error!("scanout not yet specified for cursor");
                return Err(ErrUnspec);
            }
        };

        let cursor_scanout_surface_id = match self.get_or_create_scanout_surface_id(cursor_scanout_id) {
            Some(id) => id,
            None => {
                error!("scanout not available for cursor");
                return Err(ErrUnspec);
            }
        };

        if self.cursor_surface_id.is_none() {
            match self.display.borrow_mut().create_surface(
                Some(cursor_scanout_surface_id),
                resource.width(),
                resource.height(),
            ) {
                Ok(surface_id) => self.cursor_surface_id = Some(surface_id),
                Err(e) => {
                    error!("failed to create cursor surface: {}", e);
                    return Err(ErrUnspec);
                }
            }
        }

        let cursor_surface_id = self.cursor_surface_id.unwrap();
        self.display
            .borrow_mut()
            .set_position(cursor_surface_id, x, y);

        // Gets the resource's pixels into the display by importing the buffer.
        if let Some(import_id) = resource.import_to_display(&self.display) {
            self.display
                .borrow_mut()
                .flip_to(cursor_surface_id, import_id);
            return Ok(OkNoData);
        }

        // Importing failed, so try copying the pixels into the surface's slower shared memory
        // framebuffer.
        if let Some(fb) = self.display.borrow_mut().framebuffer(cursor_surface_id) {
            resource.read_to_volatile(
                0,
                0,
                resource.width(),
                resource.height(),
                fb.as_volatile_slice(),
                fb.stride(),
            )
        }
        self.display.borrow_mut().flip(cursor_surface_id);
        Ok(OkNoData)
    }

    /// Moves the cursor's position to the given coordinates.
    pub fn move_cursor(&mut self, scanout_id: u32, x: u32, y: u32) -> VirtioGpuResult {
        self.cursor_scanout_id = Some(scanout_id);

        if let Some(cursor_surface_id) = self.cursor_surface_id {
            let cursor_scanout_surface_id = match self.get_or_create_scanout_surface_id(scanout_id) {
                Some(id) => id,
                None => return Err(OkNoData),
            };

            let mut display = self.display.borrow_mut();
            display.set_position(cursor_surface_id, x, y);
            display.commit(cursor_scanout_surface_id);
        }
        Ok(OkNoData)
    }
}
