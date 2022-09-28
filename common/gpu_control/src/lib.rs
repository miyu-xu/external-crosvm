// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Definitions and utilities for GPU related parameters.

mod sys;

use std::fmt;
use std::fmt::Display;
#[cfg(windows)]
use std::marker::PhantomData;

use serde::Deserialize;
use serde::Serialize;
use serde_keyvalue::FromKeyValues;

pub use crate::sys::DisplayMode;
use crate::sys::DisplayModeArg;

pub const DEFAULT_DISPLAY_WIDTH: u32 = 1280;
pub const DEFAULT_DISPLAY_HEIGHT: u32 = 1024;
pub const DEFAULT_REFRESH_RATE: u32 = 60;

fn default_windowed_mode() -> DisplayMode {
    DisplayMode::Windowed {
        width: DEFAULT_DISPLAY_WIDTH,
        height: DEFAULT_DISPLAY_HEIGHT,
    }
}

fn default_refresh_rate() -> u32 {
    DEFAULT_REFRESH_RATE
}

/// Trait that the platform-specific type `DisplayMode` needs to implement.
pub trait DisplayModeTrait {
    fn get_virtual_display_size(&self) -> (u32, u32);
}

// This struct is only used for argument parsing. It will be converted to platform-specific
// `DisplayParameters` implementation.
#[derive(Deserialize, Serialize, FromKeyValues)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DisplayParametersArgs {
    pub mode: Option<DisplayModeArg>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default = "default_refresh_rate")]
    pub refresh_rate: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, FromKeyValues)]
#[serde(try_from = "DisplayParametersArgs", into = "DisplayParametersArgs")]
pub struct DisplayParameters {
    pub mode: DisplayMode,
    pub hidden: bool,
    pub refresh_rate: u32,
}

impl DisplayParameters {
    pub fn new(mode: DisplayMode, hidden: bool, refresh_rate: u32) -> Self {
        Self {
            mode,
            hidden,
            refresh_rate,
        }
    }

    pub fn default_with_mode(mode: DisplayMode) -> Self {
        Self::new(mode, false, DEFAULT_REFRESH_RATE)
    }

    pub fn get_virtual_display_size(&self) -> (u32, u32) {
        self.mode.get_virtual_display_size()
    }
}

impl Default for DisplayParameters {
    fn default() -> Self {
        Self::default_with_mode(default_windowed_mode())
    }
}

impl From<DisplayParameters> for DisplayParametersArgs {
    fn from(params: DisplayParameters) -> DisplayParametersArgs {
        let (width, height) = params.get_virtual_display_size();

        DisplayParametersArgs {
            mode: Some(DisplayModeArg::from(params.mode)),
            width: Some(width),
            height: Some(height),
            hidden: params.hidden,
            refresh_rate: params.refresh_rate,
        }
    }
}

impl TryFrom<DisplayParametersArgs> for DisplayParameters {
    type Error = String;

    fn try_from(args: DisplayParametersArgs) -> Result<Self, Self::Error> {
        let mode = match args.mode.unwrap_or(DisplayModeArg::Windowed) {
            DisplayModeArg::Windowed => match (args.width, args.height) {
                (Some(width), Some(height)) => DisplayMode::Windowed { width, height },
                (None, None) => default_windowed_mode(),
                _ => {
                    return Err(
                        "must include both 'width' and 'height' if either is supplied".to_string(),
                    )
                }
            },

            #[cfg(windows)]
            DisplayModeArg::BorderlessFullScreen => match (args.width, args.height) {
                (None, None) => DisplayMode::BorderlessFullScreen(PhantomData),
                _ => return Err("'width' and 'height' are invalid with borderless_full_screen"),
            },
        };

        Ok(DisplayParameters::new(mode, args.hidden, args.refresh_rate))
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub enum GpuControlCommand {
    AddDisplays { displays: Vec<DisplayParameters> },
    ListDisplays,
    RemoveDisplays { display_ids: Vec<u32> },
}

#[derive(Serialize, Deserialize, Debug)]
pub enum GpuControlResult {
    DisplaysUpdated,
    DisplayList { displays: Vec<DisplayParameters> },
    TooManyDisplays(usize),
    NoSuchDisplay { display_id: u32 },
}

impl Display for GpuControlResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use self::GpuControlResult::*;

        match self {
            DisplaysUpdated => write!(f, "displays updated"),
            DisplayList { displays } => {
                let json: serde_json::Value = serde_json::json!({
                    "displays": displays,
                });
                let json_pretty =
                    serde_json::to_string_pretty(&json).map_err(|_| std::fmt::Error)?;
                write!(f, "{}", json_pretty)
            }
            TooManyDisplays(n) => write!(f, "too_many_displays {}", n),
            NoSuchDisplay { display_id } => write!(f, "no_such_display {}", display_id),
        }
    }
}
