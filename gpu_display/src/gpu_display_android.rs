// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ffi::c_char;
use std::ffi::CStr;
use std::panic::catch_unwind;
use std::process::abort;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::Mutex;
use std::slice;

use base::error;
use base::warn;
use base::AsRawDescriptor;
use base::Event;
use base::RawDescriptor;
use base::VolatileSlice;

use crate::DisplayT;
use crate::GpuDisplayError;
use crate::GpuDisplayFramebuffer;
use crate::GpuDisplayResult;
use crate::GpuDisplaySurface;
use crate::SurfaceType;
use crate::SysDisplayT;

#[repr(C)]
pub(crate) struct AndroidDisplayContext {
    _data: [u8; 0],
}

#[repr(C)]
pub(crate) struct ANativeWindow {
    _data: [u8; 0],

}

extern "C" {
    fn create_android_display_context(
        service_name: *const c_char,
    ) -> *mut AndroidDisplayContext;

    fn destroy_android_display_context(
        self_: *mut AndroidDisplayContext,
    );

    fn create_android_surface(
        ctx: *mut AndroidDisplayContext,
        width: u32,
        height: u32,
    ) -> *mut ANativeWindow;

    fn destroy_android_surface(
        ctx: *mut AndroidDisplayContext,
        surface: *mut ANativeWindow,
    );

    fn get_android_surface_buffer(
        surface: *mut ANativeWindow,
    ) -> *mut u8;

    fn post_android_surface_buffer(
        surface: *mut ANativeWindow,
    );
}

struct AndroidSurface {
    surface: NonNull<ANativeWindow>,
    width: u32,
    height: u32,
}

impl AndroidSurface {
    fn buffer_size(&self) -> usize {
        (self.width * self.height * self.bytes_per_pixel()) as usize
    }

    fn stride(&self) -> u32 {
        self.width * self.bytes_per_pixel()
    }

    fn bytes_per_pixel(&self) -> u32 {
        4
>>>>>>> 4d816cbd2 (gpu display)
    }
}

impl GpuDisplaySurface for AndroidSurface {
    fn framebuffer(&mut self) -> Option<GpuDisplayFramebuffer> {
        let buf = unsafe {
            get_android_surface_buffer(self.surface.as_ptr() as *mut ANativeWindow)
        };
        let buf = unsafe {
            slice::from_raw_parts_mut(buf, self.buffer_size())
        };
        Some(GpuDisplayFramebuffer::new(
            VolatileSlice::new(buf),
            self.stride(),
            self.bytes_per_pixel(),
        ))
    }

    fn flip(&mut self) {
        unsafe {
            post_android_surface_buffer(self.surface.as_ptr() as *mut ANativeWindow)
        }
    }
}

pub struct DisplayAndroid {
    context: NonNull<AndroidDisplayContext>,
    /// This event is never triggered and is used solely to fulfill AsRawDescriptor.
    event: Event,
}

impl DisplayAndroid {
    pub fn new(service_name: &str) -> GpuDisplayResult<DisplayAndroid> {
        let service_name = CString::new(service_name).or(Err(
            GpuDisplayError::InvalidAndroidDisplayServiceName(service_name.to_string()),
        ))?;
        let context = NonNull::new(
                // SAFETY: service_name is not leaked outside of this function
                unsafe {
                    create_android_display_context(service_name.as_ptr())
                }
            )
            .ok_or(GpuDisplayError::Unsupported)?;
        let event = Event::new().map_err(|_| GpuDisplayError::CreateEvent)?;
        Ok(DisplayAndroid { context, event })
    }
}

impl DisplayT for DisplayAndroid {
    fn create_surface(
        &mut self,
        parent_surface_id: Option<u32>,
        _surface_id: u32,
        _scanout_id: Option<u32>,
        requested_width: u32,
        requested_height: u32,
        _surf_type: SurfaceType,
    ) -> GpuDisplayResult<Box<dyn GpuDisplaySurface>> {
        if parent_surface_id.is_some() {
            return Err(GpuDisplayError::Unsupported);
        }

        let surface = NonNull::new(
            unsafe {
                create_android_surface(
                    self.context.as_ptr() as *mut AndroidDisplayContext,
                    requested_width,
                    requested_height,
                )
            }
        ).ok_or(GpuDisplayError::CreateSurface)?;


        Ok(Box::new(AndroidSurface {
            surface,
            width: requested_width,
            height: requested_height,
        }))
    }
}

impl SysDisplayT for DisplayAndroid {}

impl AsRawDescriptor for DisplayAndroid {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.event.as_raw_descriptor()
    }
}
