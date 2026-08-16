// Copyright 2026 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ffi::c_void;

use base::RawDescriptor;

pub(crate) const COCOA_EVENT_KEY: i32 = 1;
pub(crate) const COCOA_EVENT_TOUCH_DOWN: i32 = 2;
pub(crate) const COCOA_EVENT_TOUCH_MOVE: i32 = 3;
pub(crate) const COCOA_EVENT_TOUCH_UP: i32 = 4;
pub(crate) const COCOA_EVENT_SELECT_DISPLAY: i32 = 6;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct CocoaInputEvent {
    pub kind: i32,
    pub code: i32,
    pub value: i32,
    pub repeat: i32,
    pub x: i32,
    pub y: i32,
}

extern "C" {
    fn crosvm_cocoa_run_main_loop();
    fn crosvm_cocoa_stop_main_loop();
    fn crosvm_cocoa_run_on_main(callback: extern "C" fn(*mut c_void), context: *mut c_void);
    fn crosvm_cocoa_create_window(width: u32, height: u32) -> *mut c_void;
    fn crosvm_cocoa_event_read_fd() -> RawDescriptor;
    fn crosvm_cocoa_pending_event() -> i32;
    fn crosvm_cocoa_next_event(event: *mut CocoaInputEvent) -> i32;
    fn crosvm_cocoa_publish_remote_layer(
        endpoint: *const std::ffi::c_char,
        width: u32,
        height: u32,
    ) -> i32;
    fn gfxstream_backend_configure_display(
        scanout_id: u32,
        width: u32,
        height: u32,
        dpi: u32,
    ) -> i32;
    fn gfxstream_backend_set_scanout_resource(scanout_id: u32, resource_id: u32);
}

pub(crate) fn event_read_descriptor() -> RawDescriptor {
    // SAFETY: Returns a duplicated, owned descriptor or -1 on failure.
    unsafe { crosvm_cocoa_event_read_fd() }
}

pub(crate) fn pending_event() -> bool {
    // SAFETY: Only reads the bridge's locked input queue state.
    unsafe { crosvm_cocoa_pending_event() != 0 }
}

pub(crate) fn next_event() -> Option<CocoaInputEvent> {
    let mut event = CocoaInputEvent::default();
    // SAFETY: `event` is valid for writes for the duration of the call.
    if unsafe { crosvm_cocoa_next_event(&mut event) } != 0 {
        Some(event)
    } else {
        None
    }
}

/// Runs the AppKit event loop on the process main thread.
pub fn run_main_loop() {
    // SAFETY: This function is only called by crosvm's macOS main thread.
    unsafe { crosvm_cocoa_run_main_loop() }
}

/// Requests that the AppKit event loop return.
pub fn stop_main_loop() {
    // SAFETY: The Objective-C bridge dispatches the request to the main queue.
    unsafe { crosvm_cocoa_stop_main_loop() }
}

pub(crate) fn publish_remote_layer(endpoint: &str, width: u32, height: u32) -> bool {
    let Ok(endpoint) = std::ffi::CString::new(endpoint) else {
        return false;
    };
    // SAFETY: endpoint is NUL-terminated for the call and the bridge copies the path into its
    // sockaddr before returning.
    unsafe { crosvm_cocoa_publish_remote_layer(endpoint.as_ptr(), width, height) != 0 }
}

pub(crate) fn configure_display(scanout_id: u32, width: u32, height: u32, dpi: u32) -> bool {
    // SAFETY: gfxstream is initialized before crosvm creates scanout surfaces. The call only
    // registers immutable geometry for this run and does not retain any Rust pointers.
    unsafe { gfxstream_backend_configure_display(scanout_id, width, height, dpi) != 0 }
}

pub(crate) fn set_scanout_resource(scanout_id: u32, resource_id: u32) {
    // SAFETY: The renderer copies both scalar identifiers into its atomic scanout registry.
    unsafe { gfxstream_backend_set_scanout_resource(scanout_id, resource_id) }
}

pub(crate) fn run_on_main(callback: extern "C" fn(*mut c_void), context: *mut c_void) {
    // SAFETY: The Cocoa bridge invokes the callback synchronously and does not retain context.
    unsafe { crosvm_cocoa_run_on_main(callback, context) }
}

pub(crate) fn create_window(width: u32, height: u32) -> *mut c_void {
    // SAFETY: The bridge creates and owns the NSWindow and returns a borrowed
    // pointer that remains valid until process exit.
    unsafe { crosvm_cocoa_create_window(width, height) }
}
