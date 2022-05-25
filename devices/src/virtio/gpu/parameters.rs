use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
#[cfg(windows)]
use winapi::um::winuser::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

use base::info;

use super::GpuMode;

const DEFAULT_DISPLAY_WIDTH: u32 = 1280;
const DEFAULT_DISPLAY_HEIGHT: u32 = 1024;

pub const DISPLAY_WIDTH_SOFT_MAX: u32 = 1920;
pub const DISPLAY_HEIGHT_SOFT_MAX: u32 = 1080;

const DEFAULT_VSYNC: u32 = 60;

pub trait DisplayDataProviderT {
    fn get_display_dimensions() -> (u32, u32);
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub struct DisplayDataProvider;

#[cfg(windows)]
impl DisplayDataProviderT for DisplayDataProvider {
    fn get_display_dimensions() -> (u32, u32) {
        // Safe because we're passing valid values and screen size won't exceed u32 range
        let (width, height) = unsafe {
            (
                GetSystemMetrics(SM_CXSCREEN) as u32,
                GetSystemMetrics(SM_CYSCREEN) as u32,
            )
        };

        // Note: This is the size of the host's display. The guest display size given by
        // (width, height) may be smaller if we are letterboxing
        info!("Host display size: {}x{}", width, height);

        (width, height)
    }
}

#[cfg(unix)]
impl DisplayDataProviderT for DisplayDataProvider {
    fn get_display_dimensions() -> (u32, u32) {
        unimplemented!();
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub enum DisplayMode<T: DisplayDataProviderT> {
    BorderlessFullScreen(PhantomData<T>),
    Windowed { width: u32, height: u32 },
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct DisplayParameters<T: DisplayDataProviderT> {
    pub display_mode: DisplayMode<T>,
    pub hidden: bool,
    pub vsync: u32,
}

impl<T: DisplayDataProviderT> DisplayParameters<T> {
    pub fn default_borderless_full_screen() -> Self {
        Self {
            display_mode: DisplayMode::BorderlessFullScreen(PhantomData),
            hidden: false,
            vsync: DEFAULT_VSYNC,
        }
    }

    pub fn default_windowed() -> Self {
        Self {
            display_mode: DisplayMode::Windowed {
                width: DEFAULT_DISPLAY_WIDTH,
                height: DEFAULT_DISPLAY_HEIGHT,
            },
            hidden: false,
            vsync: DEFAULT_VSYNC,
        }
    }

    fn get_window_size(&self) -> (u32, u32) {
        match &self.display_mode {
            DisplayMode::Windowed { width, height, .. } => (*width, *height),
            DisplayMode::BorderlessFullScreen(_) => T::get_display_dimensions(),
        }
    }

    pub fn get_virtual_display_size(&self) -> (u32, u32) {
        let (width, height) = match &self.display_mode {
            DisplayMode::Windowed { width, height, .. } => (*width, *height),
            DisplayMode::BorderlessFullScreen(_) => {
                let (width, height) = T::get_display_dimensions();
                let width = std::cmp::min(width, DISPLAY_WIDTH_SOFT_MAX);
                let height = std::cmp::min(height, DISPLAY_HEIGHT_SOFT_MAX);
                // Widths that aren't a multiple of 8 break gfxstream: b/156110663
                let width = width - (width % 8);
                (width, height)
            }
        };
        info!("Guest display size: {}x{}", width, height);
        (width, height)
    }
}

impl<T: DisplayDataProviderT> Default for DisplayParameters<T> {
    fn default() -> Self {
        Self::default_windowed()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuParameters {
    pub displays: Vec<DisplayParameters<DisplayDataProvider>>,
    pub renderer_use_egl: bool,
    pub renderer_use_gles: bool,
    pub renderer_use_glx: bool,
    pub renderer_use_surfaceless: bool,
    pub gfxstream_use_guest_angle: bool,
    pub gfxstream_use_syncfd: bool,
    pub use_vulkan: bool,
    pub gfxstream_ignore_host_gl_errors: bool,
    pub gfxstream_native_astc_etc2_texture_decompression: bool,
    pub gfxstream_bptc_texture_support: bool,
    pub gfxstream_s3tc_texture_support: bool,
    pub gfxstream_support_gles31: bool,
    pub gfxstream_use_vulkan_swapchain: bool,
    pub udmabuf: bool,
    pub mode: GpuMode,
    pub cache_path: Option<String>,
    pub cache_size: Option<String>,
}

impl Default for GpuParameters {
    fn default() -> Self {
        GpuParameters {
            displays: vec![],
            renderer_use_egl: true,
            renderer_use_gles: true,
            renderer_use_glx: false,
            renderer_use_surfaceless: true,
            gfxstream_use_guest_angle: false,
            gfxstream_use_syncfd: true,
            use_vulkan: false,
            mode: if cfg!(feature = "virgl_renderer") {
                GpuMode::ModeVirglRenderer
            } else {
                GpuMode::Mode2D
            },
            gfxstream_ignore_host_gl_errors: true,
            gfxstream_native_astc_etc2_texture_decompression: false,
            gfxstream_bptc_texture_support: false,
            gfxstream_s3tc_texture_support: false,
            gfxstream_support_gles31: true,
            gfxstream_use_vulkan_swapchain: false,
            cache_path: None,
            cache_size: None,
            udmabuf: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borderless_full_screen_virtual_window_width_should_be_multiple_of_8() {
        struct DisplayDataProvider;
        impl DisplayDataProviderT for DisplayDataProvider {
            fn get_display_dimensions() -> (u32, u32) {
                (1366, 768)
            }
        }
        let param = DisplayParameters::<DisplayDataProvider>::default_borderless_full_screen();
        let (width, _) = param.get_virtual_display_size();
        assert_eq!(width % 8, 0);
    }

    #[test]
    fn borderless_full_screen_virtual_window_size_should_be_smaller_than_soft_max() {
        struct DisplayDataProvider;
        impl DisplayDataProviderT for DisplayDataProvider {
            fn get_display_dimensions() -> (u32, u32) {
                (DISPLAY_WIDTH_SOFT_MAX + 1, DISPLAY_HEIGHT_SOFT_MAX + 1)
            }
        }
        let param = DisplayParameters::<DisplayDataProvider>::default_borderless_full_screen();
        let (width, height) = param.get_virtual_display_size();
        assert!(width <= DISPLAY_WIDTH_SOFT_MAX);
        assert!(height <= DISPLAY_HEIGHT_SOFT_MAX);
    }
}
