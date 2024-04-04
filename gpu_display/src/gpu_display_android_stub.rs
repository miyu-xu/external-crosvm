// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Stub implementation of the native interface of libcrosvm_android_display_client
//!
//! This implementation is used to enable the gpu display backend for Android to be compiled
//! without libcrosvm_android_display_client available. It is only used for testing purposes and
//! not functional at runtime.

use std::ffi::c_char;

use crate::gpu_display_android::AndroidDisplayContext;
use crate::gpu_display_android::ANativeWindow;

#[no_mangle]
extern "C" fn create_android_display_context(
    service_name: *const c_char,
) -> *mut AndroidDisplayContext {
    unimplemented!();
}

#[no_mangle]
extern "C" fn destroy_android_display_context(
    self_: *mut AndroidDisplayContext,
) {
    unimplemented!();
}

#[no_mangle]
extern "C" fn create_android_surface(
    ctx: *mut AndroidDisplayContext,
    width: u32,
    height: u32,
) -> *mut ANativeWindow {
    unimplemented!();
}

#[no_mangle]
extern "C" fn destroy_android_surface(
    ctx: *mut AndroidDisplayContext,
    surface: *mut ANativeWindow,
) {
    unimplemented!();
}

#[no_mangle]
extern "C" fn get_android_surface_buffer(
    surface: *mut ANativeWindow,
) -> *mut u8 {
    unimplemented!();
}

#[no_mangle]
extern "C" fn post_android_surface_buffer(
    surface: *mut ANativeWindow,
) {
    unimplemented!();
}
