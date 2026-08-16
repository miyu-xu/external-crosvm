// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::marker::PhantomData;

use base::info;
use serde::Deserialize;
use serde::Serialize;
use serde_keyvalue::FromKeyValues;
use winapi::um::winuser::GetSystemMetrics;
use winapi::um::winuser::SM_CXSCREEN;
use winapi::um::winuser::SM_CYSCREEN;

use crate::gpu::DisplayModeTrait;

const DISPLAY_WIDTH_SOFT_MAX: u32 = 1920;
const DISPLAY_HEIGHT_SOFT_MAX: u32 = 1080;

const DISPLAY_WIDTH_SOFT_MAX_4K_UHD_ENABLED: u32 = 3840;
const DISPLAY_HEIGHT_SOFT_MAX_4K_UHD_ENABLED: u32 = 2160;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, FromKeyValues)]
#[serde(rename_all = "snake_case")]
pub enum WinMouseMode {
    /// Sends relative motion & mouse button events to the guest (captured only).
    Relative,
    /// Sends multi-touch events to the guest.
    Touchscreen,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WinDisplayMode<T> {
    Windowed(u32, u32),
    BorderlessFullScreen(#[serde(skip)] PhantomData<T>),
}

impl<T: ProvideDisplayData> DisplayModeTrait for WinDisplayMode<T> {
    fn get_window_size(&self) -> (u32, u32) {
        match self {
            Self::Windowed(width, height) => (*width, *height),
            Self::BorderlessFullScreen(_) => T::get_host_display_size(),
        }
    }

    fn get_virtual_display_size(&self) -> (u32, u32) {
        self.get_virtual_display_size_4k_uhd(vm_control_product::is_4k_uhd_enabled())
    }

    fn get_virtual_display_size_4k_uhd(&self, is_4k_uhd_enabled: bool) -> (u32, u32) {
        let (width, height) = match self {
            // An explicit windowed size is also the guest display contract. Clamping it to the
            // host-product soft limit makes the Windows backend silently disagree with the Linux
            // backend and with the resolution requested by HD. Host viewport sizing is independent
            // from the fixed guest scanout size and already fits that scanout to the work area.
            Self::Windowed(width, height) => (*width, *height),
            // Borderless full-screen follows the physical host display and keeps the historical
            // product limit/alignment rules.
            Self::BorderlessFullScreen(_) => {
                let (width, height) = self.get_window_size();
                adjust_virtual_display_size(width, height, is_4k_uhd_enabled)
            }
        };
        info!("Guest display size: {}x{}", width, height);
        (width, height)
    }
}

/// Trait for returning host display data such as resolution. Tests may overwrite this to specify
/// display data rather than rely on properties of the actual display device.
trait ProvideDisplayData {
    fn get_host_display_size() -> (u32, u32);
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayDataProvider;

impl ProvideDisplayData for DisplayDataProvider {
    fn get_host_display_size() -> (u32, u32) {
        // SAFETY:
        // Safe because we're passing valid values and screen size won't exceed u32 range.
        let (width, height) = unsafe {
            (
                GetSystemMetrics(SM_CXSCREEN) as u32,
                GetSystemMetrics(SM_CYSCREEN) as u32,
            )
        };
        // Note: This is the size of the host's display. The guest display size given by
        // (width, height) may be smaller if we are letterboxing.
        info!("Host display size: {}x{}", width, height);
        (width, height)
    }
}

fn adjust_virtual_display_size(width: u32, height: u32, is_4k_uhd_enabled: bool) -> (u32, u32) {
    let (landscape_max_width, landscape_max_height) = if is_4k_uhd_enabled {
        (
            DISPLAY_WIDTH_SOFT_MAX_4K_UHD_ENABLED,
            DISPLAY_HEIGHT_SOFT_MAX_4K_UHD_ENABLED,
        )
    } else {
        (DISPLAY_WIDTH_SOFT_MAX, DISPLAY_HEIGHT_SOFT_MAX)
    };
    // The soft limits describe a landscape display. Applying them to the coordinate axes
    // unconditionally truncates a valid portrait mode such as 1080x1920 to 1080x1080. Keep the
    // requested orientation when enforcing the same pixel limits.
    let (max_width, max_height) = if height > width {
        (landscape_max_height, landscape_max_width)
    } else {
        (landscape_max_width, landscape_max_height)
    };
    let width = std::cmp::min(width, max_width);
    let height = std::cmp::min(height, max_height);
    // Widths that aren't a multiple of 8 break gfxstream: b/156110663.
    let width = width - (width % 8);
    (width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borderless_full_screen_virtual_window_width_should_be_multiple_of_8() {
        struct MockDisplayDataProvider;

        impl ProvideDisplayData for MockDisplayDataProvider {
            fn get_host_display_size() -> (u32, u32) {
                (1366, 768)
            }
        }

        let mode = WinDisplayMode::<MockDisplayDataProvider>::BorderlessFullScreen(PhantomData);
        let (width, _) = mode.get_virtual_display_size_4k_uhd(/* is_4k_uhd_enabled */ false);
        assert_eq!(width % 8, 0);
    }

    #[test]
    fn borderless_full_screen_virtual_window_size_should_be_smaller_than_soft_max() {
        struct MockDisplayDataProvider;

        impl ProvideDisplayData for MockDisplayDataProvider {
            fn get_host_display_size() -> (u32, u32) {
                (DISPLAY_WIDTH_SOFT_MAX + 1, DISPLAY_HEIGHT_SOFT_MAX + 1)
            }
        }

        let mode = WinDisplayMode::<MockDisplayDataProvider>::BorderlessFullScreen(PhantomData);
        let (width, height) =
            mode.get_virtual_display_size_4k_uhd(/* is_4k_uhd_enabled */ false);
        assert!(width <= DISPLAY_WIDTH_SOFT_MAX);
        assert!(height <= DISPLAY_HEIGHT_SOFT_MAX);
    }

    #[test]
    fn portrait_virtual_window_preserves_portrait_soft_limits() {
        assert_eq!(adjust_virtual_display_size(1080, 1920, false), (1080, 1920));
        assert_eq!(adjust_virtual_display_size(1440, 2560, false), (1080, 1920));
    }

    #[test]
    fn explicit_windowed_virtual_display_size_matches_request() {
        let mode = WinDisplayMode::<DisplayDataProvider>::Windowed(1080, 1920);
        assert_eq!(
            mode.get_virtual_display_size_4k_uhd(/* is_4k_uhd_enabled */ false),
            (1080, 1920)
        );
        assert_eq!(
            mode.get_virtual_display_size_4k_uhd(/* is_4k_uhd_enabled */ true),
            (1080, 1920)
        );
    }
}
