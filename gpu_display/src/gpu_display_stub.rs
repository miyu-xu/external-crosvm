// Copyright 2020 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(all(target_os = "macos", feature = "gfxstream"))]
use std::ffi::c_void;
#[cfg(all(target_os = "macos", feature = "gfxstream"))]
use std::sync::atomic::AtomicBool;
#[cfg(all(target_os = "macos", feature = "gfxstream"))]
use std::sync::atomic::Ordering;
#[cfg(all(target_os = "macos", feature = "gfxstream"))]
use std::time::Duration;

use base::AsRawDescriptor;
#[cfg(not(target_os = "macos"))]
use base::Event;
#[cfg(target_os = "macos")]
use base::FromRawDescriptor;
use base::RawDescriptor;
#[cfg(target_os = "macos")]
use base::SafeDescriptor;
use base::VolatileSlice;
use vm_control::gpu::DisplayParameters;

#[cfg(target_os = "macos")]
use crate::gpu_display_cocoa::CocoaInputEvent;
#[cfg(target_os = "macos")]
use crate::keycode_converter::KeycodeTranslator;
#[cfg(target_os = "macos")]
use crate::keycode_converter::KeycodeTypes;
use crate::DisplayT;
#[cfg(target_os = "macos")]
use crate::EventDeviceKind;
use crate::GpuDisplayError;
use crate::GpuDisplayEvents;
use crate::GpuDisplayFramebuffer;
use crate::GpuDisplayResult;
use crate::GpuDisplaySurface;
use crate::SurfaceType;
use crate::SysDisplayT;

#[cfg(all(target_os = "macos", feature = "gfxstream"))]
#[link(name = "gfxstream_backend")]
extern "C" {
    fn gfxstream_backend_setup_window(
        native_window_handle: *const c_void,
        window_x: i32,
        window_y: i32,
        window_width: i32,
        window_height: i32,
        fb_width: i32,
        fb_height: i32,
    );
}

#[cfg(all(target_os = "macos", feature = "gfxstream"))]
#[repr(C)]
struct GfxstreamWindowSetup {
    window: *mut c_void,
    width: i32,
    height: i32,
}

#[cfg(all(target_os = "macos", feature = "gfxstream"))]
extern "C" fn setup_gfxstream_window_on_main(context: *mut c_void) {
    // SAFETY: run_on_main invokes this synchronously while the stack-owned setup remains live.
    let setup = unsafe { &*(context as *const GfxstreamWindowSetup) };
    // SAFETY: The Cocoa bridge owns the NSWindow and this callback runs on AppKit's main thread.
    unsafe {
        gfxstream_backend_setup_window(
            setup.window,
            0,
            0,
            setup.width,
            setup.height,
            setup.width,
            setup.height,
        );
    }
}

#[cfg(all(target_os = "macos", feature = "gfxstream"))]
static REMOTE_LAYER_PUBLISH_ACTIVE: AtomicBool = AtomicBool::new(false);

#[cfg(all(target_os = "macos", feature = "gfxstream"))]
fn publish_remote_layer_when_ready(endpoint: String, width: u32, height: u32) {
    if REMOTE_LAYER_PUBLISH_ACTIVE.swap(true, Ordering::AcqRel) {
        return;
    }
    if std::thread::Builder::new()
        .name("crosvm-ca-publish".to_owned())
        .spawn(move || loop {
            crate::gpu_display_cocoa::publish_remote_layer(&endpoint, width, height);
            std::thread::sleep(Duration::from_millis(250));
        })
        .is_err()
    {
        REMOTE_LAYER_PUBLISH_ACTIVE.store(false, Ordering::Release);
    }
}

#[allow(dead_code)]
struct Buffer {
    width: u32,
    _height: u32,
    bytes_per_pixel: u32,
    bytes: Vec<u8>,
}

impl Drop for Buffer {
    fn drop(&mut self) {}
}

impl Buffer {
    fn as_volatile_slice(&mut self) -> VolatileSlice {
        VolatileSlice::new(self.bytes.as_mut_slice())
    }

    fn stride(&self) -> usize {
        (self.bytes_per_pixel as usize) * (self.width as usize)
    }

    fn bytes_per_pixel(&self) -> usize {
        self.bytes_per_pixel as usize
    }
}

struct StubSurface {
    width: u32,
    height: u32,
    buffer: Option<Buffer>,
    uses_native_scanout: bool,
}

const fn should_allocate_framebuffer(uses_native_scanout: bool) -> bool {
    !uses_native_scanout
}

impl StubSurface {
    /// Gets the buffer at buffer_index, allocating it if necessary.
    fn lazily_allocate_buffer(&mut self) -> Option<&mut Buffer> {
        if self.buffer.is_none() {
            // XRGB8888
            let bytes_per_pixel = 4;
            let bytes_total = (self.width as u64) * (self.height as u64) * (bytes_per_pixel as u64);

            self.buffer = Some(Buffer {
                width: self.width,
                _height: self.height,
                bytes_per_pixel,
                bytes: vec![0; bytes_total as usize],
            });
        }

        self.buffer.as_mut()
    }
}

impl GpuDisplaySurface for StubSurface {
    fn framebuffer(&mut self) -> Option<GpuDisplayFramebuffer> {
        if !should_allocate_framebuffer(self.uses_native_scanout) {
            return None;
        }
        let framebuffer = self.lazily_allocate_buffer()?;
        let framebuffer_stride = framebuffer.stride() as u32;
        let framebuffer_bytes_per_pixel = framebuffer.bytes_per_pixel() as u32;
        Some(GpuDisplayFramebuffer::new(
            framebuffer.as_volatile_slice(),
            framebuffer_stride,
            framebuffer_bytes_per_pixel,
        ))
    }
}

impl Drop for StubSurface {
    fn drop(&mut self) {}
}

pub struct DisplayStub {
    #[cfg(not(target_os = "macos"))]
    /// This event is never triggered and is used solely to fulfill AsRawDescriptor.
    event: Event,
    #[cfg(target_os = "macos")]
    event: SafeDescriptor,
    #[cfg(target_os = "macos")]
    current_event: Option<CocoaInputEvent>,
    #[cfg(target_os = "macos")]
    keycode_translator: KeycodeTranslator,
    #[cfg(target_os = "macos")]
    next_tracking_id: i32,
    #[cfg(target_os = "macos")]
    active_tracking_id: Option<i32>,
    #[cfg(target_os = "macos")]
    selected_display_id: u32,
}

impl DisplayStub {
    pub fn new() -> GpuDisplayResult<DisplayStub> {
        #[cfg(not(target_os = "macos"))]
        {
            let event = Event::new().map_err(|_| GpuDisplayError::CreateEvent)?;
            Ok(DisplayStub { event })
        }
        #[cfg(target_os = "macos")]
        {
            let descriptor = crate::gpu_display_cocoa::event_read_descriptor();
            if descriptor < 0 {
                return Err(GpuDisplayError::CreateEvent);
            }
            // SAFETY: the Cocoa bridge returns a new descriptor owned by the caller.
            let event = unsafe { SafeDescriptor::from_raw_descriptor(descriptor) };
            Ok(DisplayStub {
                event,
                current_event: None,
                keycode_translator: KeycodeTranslator::new(KeycodeTypes::MacScancode),
                next_tracking_id: 0,
                active_tracking_id: None,
                selected_display_id: 0,
            })
        }
    }
}

impl DisplayT for DisplayStub {
    fn pending_events(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            crate::gpu_display_cocoa::pending_event()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    fn next_event(&mut self) -> GpuDisplayResult<u64> {
        #[cfg(target_os = "macos")]
        {
            self.current_event = crate::gpu_display_cocoa::next_event();
        }
        Ok(0)
    }

    fn selected_touchscreen_display_id(&self) -> Option<u32> {
        #[cfg(target_os = "macos")]
        {
            return Some(self.selected_display_id);
        }
        #[cfg(not(target_os = "macos"))]
        None
    }

    fn set_scanout_resource(&mut self, scanout_id: u32, resource_id: u32) {
        #[cfg(all(target_os = "macos", feature = "gfxstream"))]
        crate::gpu_display_cocoa::set_scanout_resource(scanout_id, resource_id);
        #[cfg(not(all(target_os = "macos", feature = "gfxstream")))]
        let _ = (scanout_id, resource_id);
    }

    fn handle_next_event(
        &mut self,
        _surface: &mut Box<dyn GpuDisplaySurface>,
    ) -> Option<GpuDisplayEvents> {
        #[cfg(target_os = "macos")]
        {
            let event = self.current_event.take()?;
            match event.kind {
                crate::gpu_display_cocoa::COCOA_EVENT_SELECT_DISPLAY => {
                    self.selected_display_id = event.code.max(0) as u32;
                    None
                }
                crate::gpu_display_cocoa::COCOA_EVENT_KEY => {
                    let linux_keycode = self.keycode_translator.translate(event.code as u32)?;
                    Some(GpuDisplayEvents {
                        events: vec![linux_input_sys::virtio_input_event::key(
                            linux_keycode,
                            event.value != 0,
                            event.repeat != 0,
                        )],
                        device_type: EventDeviceKind::Keyboard,
                    })
                }
                crate::gpu_display_cocoa::COCOA_EVENT_TOUCH_DOWN => {
                    let tracking_id = self.next_tracking_id;
                    self.next_tracking_id = self.next_tracking_id.wrapping_add(1);
                    self.active_tracking_id = Some(tracking_id);
                    Some(GpuDisplayEvents {
                        events: vec![
                            linux_input_sys::virtio_input_event::multitouch_slot(0),
                            linux_input_sys::virtio_input_event::multitouch_tracking_id(
                                tracking_id,
                            ),
                            linux_input_sys::virtio_input_event::multitouch_absolute_x(event.x),
                            linux_input_sys::virtio_input_event::multitouch_absolute_y(event.y),
                            linux_input_sys::virtio_input_event::touch(true),
                        ],
                        device_type: EventDeviceKind::Touchscreen,
                    })
                }
                crate::gpu_display_cocoa::COCOA_EVENT_TOUCH_MOVE => {
                    let tracking_id = self.active_tracking_id?;
                    Some(GpuDisplayEvents {
                        events: vec![
                            linux_input_sys::virtio_input_event::multitouch_slot(0),
                            linux_input_sys::virtio_input_event::multitouch_tracking_id(
                                tracking_id,
                            ),
                            linux_input_sys::virtio_input_event::multitouch_absolute_x(event.x),
                            linux_input_sys::virtio_input_event::multitouch_absolute_y(event.y),
                            linux_input_sys::virtio_input_event::touch(true),
                        ],
                        device_type: EventDeviceKind::Touchscreen,
                    })
                }
                crate::gpu_display_cocoa::COCOA_EVENT_TOUCH_UP => {
                    self.active_tracking_id = None;
                    Some(GpuDisplayEvents {
                        events: vec![
                            linux_input_sys::virtio_input_event::multitouch_slot(0),
                            linux_input_sys::virtio_input_event::multitouch_tracking_id(-1),
                            linux_input_sys::virtio_input_event::touch(false),
                        ],
                        device_type: EventDeviceKind::Touchscreen,
                    })
                }
                _ => None,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    fn create_surface(
        &mut self,
        parent_surface_id: Option<u32>,
        _surface_id: u32,
        scanout_id: Option<u32>,
        display_params: &DisplayParameters,
        surf_type: SurfaceType,
    ) -> GpuDisplayResult<Box<dyn GpuDisplaySurface>> {
        if parent_surface_id.is_some() {
            return Err(GpuDisplayError::Unsupported);
        }

        let (width, height) = display_params.get_virtual_display_size();
        #[cfg(all(target_os = "macos", feature = "gfxstream"))]
        let uses_native_scanout = scanout_id.is_some()
            && surf_type == SurfaceType::Scanout
            && std::env::var_os("CROSVM_COCOA_DISPLAY").is_some();
        #[cfg(all(target_os = "macos", feature = "gfxstream"))]
        let publishes_native_surface = uses_native_scanout && scanout_id == Some(0);
        #[cfg(not(all(target_os = "macos", feature = "gfxstream")))]
        let uses_native_scanout = false;
        #[cfg(not(all(target_os = "macos", feature = "gfxstream")))]
        let _ = (scanout_id, surf_type);
        #[cfg(all(target_os = "macos", feature = "gfxstream"))]
        if let Some(scanout_id) = scanout_id.filter(|_| uses_native_scanout) {
            if !crate::gpu_display_cocoa::configure_display(
                scanout_id,
                width,
                height,
                display_params.horizontal_dpi(),
            ) {
                return Err(GpuDisplayError::Allocate);
            }
        }
        #[cfg(all(target_os = "macos", feature = "gfxstream"))]
        if publishes_native_surface {
            let window = crate::gpu_display_cocoa::create_window(width, height);
            if window.is_null() {
                return Err(GpuDisplayError::Allocate);
            }
            let mut setup = GfxstreamWindowSetup {
                window,
                width: width as i32,
                height: height as i32,
            };
            crate::gpu_display_cocoa::run_on_main(
                setup_gfxstream_window_on_main,
                (&mut setup as *mut GfxstreamWindowSetup).cast(),
            );
            if let Some(endpoint) = std::env::var_os("CROSVM_COCOA_CONTEXT_ENDPOINT") {
                publish_remote_layer_when_ready(
                    endpoint.to_string_lossy().into_owned(),
                    width,
                    height,
                );
            }
        }
        Ok(Box::new(StubSurface {
            width,
            height,
            buffer: None,
            uses_native_scanout,
        }))
    }
}

impl SysDisplayT for DisplayStub {}

impl AsRawDescriptor for DisplayStub {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.event.as_raw_descriptor()
    }
}

#[cfg(test)]
mod tests {
    use super::should_allocate_framebuffer;

    #[test]
    fn native_scanout_never_allocates_a_cpu_framebuffer() {
        assert!(!should_allocate_framebuffer(true));
        assert!(should_allocate_framebuffer(false));
    }
}
