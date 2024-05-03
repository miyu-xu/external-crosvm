// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Stub implementation of Android AAudio NDK
//!
//! This implementation is used to enable the virtio-snd for Android to be compiled without
//! Andoird AAudio NDK available. It is only used for testing purposes and no functional at
//! runtime.

use std::os::raw::c_int;
use std::os::raw::c_uint;
use std::os::raw::c_void;

#[no_mangle]
extern "C" fn AAudio_createStreamBuilder(_builder: *mut *mut c_void) -> c_int {
    unimplemented!();
}

#[no_mangle]
extern "C" fn AAudioStreamBuilder_delete(_builder: *mut c_void) -> c_int {
    unimplemented!();
}

#[no_mangle]
extern "C" fn AAudioStreamBuilder_setFormat(_builder: *mut c_void, _format: c_int) -> c_int {
    unimplemented!();
}

#[no_mangle]
extern "C" fn AAudioStreamBuilder_setSampleRate(
    _builder: *mut c_void,
    _sample_rate: c_uint,
) -> c_int {
    unimplemented!();
}

#[no_mangle]
extern "C" fn AAudioStreamBuilder_setChannelCount(
    _builder: *mut c_void,
    _channel_count: c_int,
) -> c_int {
    unimplemented!();
}

#[no_mangle]
extern "C" fn AAudioStreamBuilder_openStream(
    _builder: *mut c_void,
    _stream: *mut *mut c_void,
) -> c_int {
    unimplemented!();
}

#[no_mangle]
extern "C" fn AAudioStream_requestStart(_stream: *mut c_void) -> c_int {
    unimplemented!();
}

#[no_mangle]
extern "C" fn AAudioStream_write(
    _stream: *mut c_void,
    _buffer: *const u8,
    _num_frames: c_int,
    _timeout_nanos: c_int,
) -> c_int {
    unimplemented!();
}

#[no_mangle]
extern "C" fn AAudioStream_close(_stream: *mut c_void) -> c_int {
    unimplemented!();
}
