// Copyright 2020 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Wrapper around Android's logging library, liblog.

extern crate android_log_sys;

use android_log_sys::{
    __android_log_is_loggable, __android_log_message, __android_log_write_log_message,
};
pub use android_log_sys::{log_id_t, LogPriority};
use std::ffi::{CString, NulError};
use std::mem::size_of;

/// Send a log message to the Android logger (logd, by default) if it is currently configured to be
/// loggable based on the priority and tag.
///
/// # Arguments
/// * `priority` - The Android log priority. Used to determine whether the message is loggable.
/// * `tag` - A tag to indicate where the log comes from.
/// * `file` - The name of the file from where the message is being logged, if available.
/// * `line` - The line number from where the message is being logged, if available.
/// * `message` - The message to log.
pub fn android_log(
    buffer_id: log_id_t,
    priority: LogPriority,
    tag: &str,
    file: Option<&str>,
    line: Option<u32>,
    message: &str,
) -> Result<(), NulError> {
    let tag = CString::new(tag)?;
    let default_pri = LogPriority::VERBOSE;
    if (unsafe { __android_log_is_loggable(priority as i32, tag.as_ptr(), default_pri as i32) }
        != 0)
    {
        let c_file_name = match file {
            Some(file_name) => CString::new(file_name)?.as_ptr(),
            None => std::ptr::null(),
        };
        let line = line.unwrap_or(0);
        let message = CString::new(message)?;
        let mut log_message = __android_log_message {
            struct_size: size_of::<__android_log_message>(),
            buffer_id: buffer_id as i32,
            priority: priority as i32,
            tag: tag.as_ptr(),
            file: c_file_name,
            line,
            message: message.as_ptr(),
        };
        unsafe { __android_log_write_log_message(&mut log_message) };
    }
    Ok(())
}
