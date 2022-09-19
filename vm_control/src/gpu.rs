// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fmt;
use std::fmt::Display;
use std::path::Path;

pub use crate::sys::handle_request;
pub use crate::*;
use gpu_control::DisplayParameters;
use gpu_control::GpuControlCommand;
use gpu_control::GpuControlResult;

pub enum ModifyGpuError {
    SocketFailed,
    UnexpectedResponse(VmResponse),
    UnknownCommand(String),
    GpuControl(GpuControlResult),
}

impl fmt::Display for ModifyGpuError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use self::ModifyGpuError::*;

        match self {
            SocketFailed => write!(f, "socket failed"),
            UnexpectedResponse(r) => write!(f, "unexpected response: {}", r),
            UnknownCommand(c) => write!(f, "unknown display command: `{}`", c),
            GpuControl(e) => write!(f, "{}", e),
        }
    }
}

pub type ModifyGpuResult<T> = std::result::Result<T, ModifyGpuError>;

pub fn do_gpu_display_add<T: AsRef<Path> + std::fmt::Debug>(
    control_socket_path: T,
    displays: Vec<DisplayParameters>,
) -> ModifyGpuResult<GpuControlResult> {
    let request = VmRequest::GpuCommand(GpuControlCommand::AddDisplays{displays});
    let response = handle_request(&request, control_socket_path)
        .map_err(|_| ModifyGpuError::SocketFailed)?;
    match response {
        VmResponse::GpuResponse(display_resp) => Ok(display_resp),
        r => Err(ModifyGpuError::UnexpectedResponse(r)),
    }
}

pub fn do_gpu_display_list<T: AsRef<Path> + std::fmt::Debug>(
    control_socket_path: T,
) -> ModifyGpuResult<GpuControlResult> {
    let request = VmRequest::GpuCommand(GpuControlCommand::ListDisplays);
    let response = handle_request(&request, control_socket_path)
        .map_err(|_| ModifyGpuError::SocketFailed)?;
    match response {
        VmResponse::GpuResponse(display_resp) => Ok(display_resp),
        r => Err(ModifyGpuError::UnexpectedResponse(r)),
    }
}

pub fn do_gpu_display_remove<T: AsRef<Path> + std::fmt::Debug>(
    control_socket_path: T,
    display_ids: Vec<u32>,
) -> ModifyGpuResult<GpuControlResult> {
    let request = VmRequest::GpuCommand(GpuControlCommand::RemoveDisplays{display_ids});
    let response = handle_request(&request, control_socket_path)
        .map_err(|_| ModifyGpuError::SocketFailed)?;
    match response {
        VmResponse::GpuResponse(display_resp) => Ok(display_resp),
        r => Err(ModifyGpuError::UnexpectedResponse(r)),
    }
}
