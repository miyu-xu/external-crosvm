// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ffi::c_char;
use std::ffi::CStr;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::panic::catch_unwind;
use std::path::Path;
use std::process::abort;
use std::ptr::NonNull;
use std::slice;
use std::sync::Arc;
use std::sync::Mutex;

use base::error;
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

// Opaque blob
#[repr(C)]
pub(crate) struct AndroidDisplayContext {
    _data: [u8; 0],
}

// Opaque blob
#[repr(C)]
pub(crate) struct ANativeWindow {
    _data: [u8; 0],
}

// Should be the same as ANativeWindow_Buffer in android/native_window.h
#[repr(C)]
pub(crate) struct ANativeWindow_Buffer {
    width: i32,
    height: i32,
    stride: i32, // in number of pixels, NOT bytes
    format: i32,
    bits: *mut u8,
    reserved: [u32; 6],
}

pub(crate) type ErrorCallback = unsafe extern "C" fn(message: *const c_char);

extern "C" {
    fn create_android_display_context(
        name: *const c_char,
        error_callback: ErrorCallback,
    ) -> *mut AndroidDisplayContext;

    fn destroy_android_display_context(self_: *mut AndroidDisplayContext);

    fn create_android_surface(
        ctx: *mut AndroidDisplayContext,
        width: u32,
        height: u32,
    ) -> *mut ANativeWindow;

    fn destroy_android_surface(ctx: *mut AndroidDisplayContext, surface: *mut ANativeWindow);

    fn get_android_surface_buffer(
        ctx: *mut AndroidDisplayContext,
        surface: *mut ANativeWindow,
        out_buffer: *mut ANativeWindow_Buffer,
    ) -> bool;

    fn post_android_surface_buffer(ctx: *mut AndroidDisplayContext, surface: *mut ANativeWindow);
}

unsafe extern "C" fn error_callback(message: *const c_char) {
    catch_unwind(|| {
        error!(
            "{}",
            // SAFETY: message is null terminated
            unsafe { CStr::from_ptr(message) }.to_string_lossy()
        )
    })
    .unwrap_or_else(|_| abort())
}

impl Default for ANativeWindow_Buffer {
    fn default() -> Self {
        Self {
            bits: std::ptr::null_mut(),
            ..Default::default()
        }
    }
}

impl From<ANativeWindow_Buffer> for GpuDisplayFramebuffer<'_> {
    fn from(anb: ANativeWindow_Buffer) -> Self {
        // TODO: check anb.format to see if it's ARGB8888?
        // TODO: infer bpp from anb.format?
        const bytes_per_pixel: u32 = 4;
        let stride_bytes = bytes_per_pixel * u32::try_from(anb.stride).unwrap();
        let buffer_size = stride_bytes * u32::try_from(anb.height).unwrap();
        // SAFETY: ANativeWindow_lock guarantees that bits points to a valid buffer and the buffer
        // remains available until ANativeWindow_unlockAndPost is called.
        let buffer =
            unsafe { slice::from_raw_parts_mut(anb.bits, buffer_size.try_into().unwrap()) };
        Self::new(VolatileSlice::new(buffer), stride_bytes, bytes_per_pixel)
    }
}

struct AndroidSurface {
    context: NonNull<AndroidDisplayContext>,
    surface: NonNull<ANativeWindow>,
}

impl GpuDisplaySurface for AndroidSurface {
    fn framebuffer(&mut self) -> Option<GpuDisplayFramebuffer> {
        let mut anb = ANativeWindow_Buffer::default();
        // SAFETY: context and surface are opaque handles and buf is used as the out parameter to
        // hold the return values.
        let success = unsafe {
            get_android_surface_buffer(
                self.context.as_ptr(),
                self.surface.as_ptr() as *mut ANativeWindow,
                &mut anb as *mut ANativeWindow_Buffer,
            )
        };
        if success {
            Some(anb.into())
        } else {
            None
        }
    }

    fn flip(&mut self) {
        // SAFETY: context and surface are opaque handles.
        unsafe {
            post_android_surface_buffer(
                self.context.as_ptr(),
                self.surface.as_ptr() as *mut ANativeWindow,
            )
        }
    }
}

pub struct DisplayAndroid {
    context: NonNull<AndroidDisplayContext>,
    /// This event is never triggered and is used solely to fulfill AsRawDescriptor.
    event: Event,
}

impl DisplayAndroid {
    pub fn new(name: &str) -> GpuDisplayResult<DisplayAndroid> {
        let name = CString::new(name).unwrap();
        let context = NonNull::new(
            // SAFETY: service_name is not leaked outside of this function
            unsafe { create_android_display_context(name.as_ptr(), error_callback) },
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

        let surface = NonNull::new(unsafe {
            create_android_surface(
                self.context.as_ptr() as *mut AndroidDisplayContext,
                requested_width,
                requested_height,
            )
        })
        .ok_or(GpuDisplayError::CreateSurface)?;

        Ok(Box::new(AndroidSurface {
            context: self.context,
            surface,
        }))
    }
}

impl SysDisplayT for DisplayAndroid {}

impl AsRawDescriptor for DisplayAndroid {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.event.as_raw_descriptor()
    }
}
