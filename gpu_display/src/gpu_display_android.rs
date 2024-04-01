// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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

#[repr(C)]
pub(crate) struct ANativeWindow_buffer {
    _data: [u8; 0],
}

extern "C" {
    fn create_android_display_context(
        service_name: *const ::std::os::raw::c_char,
        service_name_len: usize,
    ) -> *mut AndroidDisplayContext;

    fn destroy_android_display_context(
        error_callback: android_display_error_callback_type,
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
        ctx: *mut AndroidDisplayContext,
        surface: *mut ANativeWindow,
    ) -> *mut u8;

    fn post_android_surface((
        ctx: *mut AndroidDisplayContext,
        surface: *mut AndroidSurface,
    );
}

unsafe extern "C" fn error_callback(message: *const ::std::os::raw::c_char) {
    catch_unwind(|| {
        error!(
            "{}",
            // SAFETY:  message is null terminated
            unsafe { CStr::from_ptr(message) }.to_string_lossy()
        )
    })
    .unwrap_or_else(|_| abort())
}

impl Drop for AndroidDisplayContext {
    fn drop(&mut self) {
        // SAFETY: the context pointer is non-null and always valid.
        unsafe {
            destroy_android_display_context(Some(error_callback), self as *mut AndroidDisplayContext);
        }
    }
}

impl GpuDisplaySurface for AndroidDisplayContext {
    fn framebuffer(&mut self) -> Option<GpuDisplayFramebuffer> {
        let ctx = self as *mut AndroidDisplayContext;
        let buf = unsafe { get_android_display_context(ctx) };
        let width = unsafe { get_android_display_width(ctx) };
        let height = unsafe { get_android_display_height(ctx) };
        let stride = (width * 4) as usize;
        let total_size = (width * height * 4) as usize;
        let buf = unsafe { slice::from_raw_parts_mut(buf,  total_size)};
        Some(GpuDisplayFramebuffer::new(
            VolatileSlice::new(buf),
            stride,
            4,
        ))
    }

    fn flip(&mut self) {
        let ctx = self.context.lock().unwrap();
        // SAFETY: self.buffer is not leaked outside of this function
        unsafe {
            blit_android_display(
                Some(error_callback),
                &mut *ctx as *mut AndroidDisplayContext,
            )
        };
    }
}

pub struct DisplayAndroid {
    context: NonNull<AndroidDisplayContext>,
    /// This event is never triggered and is used solely to fulfill AsRawDescriptor.
    event: Event,
}

impl DisplayAndroid {
    pub fn new(service_name: &str) -> GpuDisplayResult<DisplayAndroid> {
        let event = Event::new().map_err(|_| GpuDisplayError::CreateEvent)?;

        let service_name = CString::new(service_name).or(Err(
            GpuDisplayError::InvalidAndroidDisplayServiceName(service_name.to_string()),
        ))?;

        let context = 
            NonNull::new(
                // SAFETY: service_name is not leaked outside of this function
                unsafe {
                    create_android_display_context(
                        service_name.as_ptr() as *const ::std::os::raw::c_char,
                        service_name.len(),
                        Some(error_callback),
                    )
                },
            )
            .ok_or(GpuDisplayError::Unsupported)?;

        Ok(DisplayAndroid {
            context: Arc::new(Mutex::new(context)),
            event,
        })
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

        let bytes_per_pixel = 4;
        let bytes_total =
            (requested_width as u64) * (requested_height as u64) * (bytes_per_pixel as u64);
        Ok(Box::new(AndroidSurface {
            context: self.context.clone(),
            buffer: Buffer {
                width: requested_width,
                height: requested_height,
                bytes_per_pixel,
            },
        }))
    }
}

impl SysDisplayT for DisplayAndroid {}

impl AsRawDescriptor for DisplayAndroid {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.event.as_raw_descriptor()
    }
}
