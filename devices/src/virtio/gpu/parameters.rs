use super::GpuMode;

use serde::{Deserialize, Serialize};

const DEFAULT_DISPLAY_WIDTH: u32 = 1280;
const DEFAULT_DISPLAY_HEIGHT: u32 = 1024;

const DEFAULT_VSYNC: u32 = 60;

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct DisplayParameters {
    pub width: u32,
    pub height: u32,
    pub hidden: bool,
    pub vsync: u32,
}

impl Default for DisplayParameters {
    fn default() -> Self {
        DisplayParameters {
            width: DEFAULT_DISPLAY_WIDTH,
            height: DEFAULT_DISPLAY_HEIGHT,
            hidden: false,
            vsync: DEFAULT_VSYNC,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuParameters {
    pub displays: Vec<DisplayParameters>,
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
