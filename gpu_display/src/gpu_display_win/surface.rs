// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ffi::c_void;
use std::ops::ControlFlow;
use std::ops::Deref;
#[cfg(feature = "gfxstream")]
use std::os::raw::c_int;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Weak;
#[cfg(feature = "gfxstream")]
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use base::error;
use base::info;
use base::warn;
use base::Tube;
use euclid::point2;
use euclid::size2;
use euclid::Box2D;
use euclid::Size2D;
use metrics::sys::windows::Metrics;
use sync::Mutex;
use vm_control::gpu::DisplayMode;
use vm_control::gpu::DisplayParameters;
use win_util::keys_down;
use win_util::win32_wide_string;
use winapi::shared::minwindef::HIWORD;
use winapi::shared::minwindef::LOWORD;
use winapi::shared::minwindef::LPARAM;
use winapi::shared::minwindef::LRESULT;
use winapi::shared::minwindef::TRUE;
use winapi::shared::minwindef::WPARAM;
use winapi::um::winuser::GetPropW;
use winapi::um::winuser::RemovePropW;
use winapi::um::winuser::SetPropW;
use winapi::um::winuser::SWP_NOACTIVATE;
use winapi::um::winuser::SWP_NOZORDER;
use winapi::um::winuser::VK_F4;
use winapi::um::winuser::VK_MENU;
use winapi::um::winuser::WM_CLOSE;

use super::keyboard_input_manager::KeyboardInputManager;
use super::math_util::Rect;
use super::math_util::Size2DCheckedCast;
use super::mouse_input_manager::MouseInputManager;
use super::virtual_display_manager::NoopVirtualDisplayManager as VirtualDisplayManager;
use super::window::BasicWindow;
use super::window::GuiWindow;
use super::window_manager::NoopWindowManager as WindowManager;
use super::window_message_processor::GeneralMessage;
use super::window_message_processor::SurfaceResources;
use super::window_message_processor::WindowMessage;
use super::window_message_processor::WindowPosMessage;
use super::window_message_processor::APPLIED_ROTATION_PROPERTY;
use super::window_message_processor::APPLIED_VIEWPORT_PROPERTY;
use super::window_message_processor::FORCE_VIEWPORT_COMMIT_WPARAM;
use super::window_message_processor::FORCE_VIEWPORT_PENDING_PROPERTY;
use super::window_message_processor::HANDLE_WINDOW_MESSAGE_TIMEOUT;
use super::window_message_processor::LATEST_ROTATION_PROPERTY;
use super::window_message_processor::LATEST_VIEWPORT_PROPERTY;
use super::HostWindowSpace;
use super::MouseMode;
use super::VirtualDisplaySpace;
use super::VulkanDisplayWrapper;
use crate::EventDeviceKind;

#[cfg(feature = "gfxstream")]
#[link(name = "gfxstream_backend")]
extern "C" {
    fn gfxstream_backend_setup_window_for_display(
        scanout_id: u32,
        hwnd: *const c_void,
        window_x: c_int,
        window_y: c_int,
        window_width: c_int,
        window_height: c_int,
        fb_width: c_int,
        fb_height: c_int,
    );
    fn gfxstream_backend_commit_window_for_display(
        scanout_id: u32,
        hwnd: *const c_void,
        window_x: c_int,
        window_y: c_int,
        window_width: c_int,
        window_height: c_int,
        fb_width: c_int,
        fb_height: c_int,
    ) -> c_int;
    fn gfxstream_backend_select_display(scanout_id: u32) -> c_int;
}

// Updates the rectangle in the window's client area to which gfxstream renders.
fn update_virtual_display_projection(
    #[allow(unused)] vulkan_display: impl Deref<Target = VulkanDisplayWrapper>,
    #[allow(unused)] window: &GuiWindow,
    #[allow(unused)] projection_box: &Box2D<i32, HostWindowSpace>,
    #[allow(unused)] host_viewport_size: &Size2D<i32, HostWindowSpace>,
    #[allow(unused)] virtual_display_size: &Size2D<i32, VirtualDisplaySpace>,
    #[allow(unused)] authoritative_host_viewport: bool,
) -> bool {
    #[cfg(feature = "vulkan_display")]
    let vulkan_projection_committed =
        if let VulkanDisplayWrapper::Initialized(ref vulkan_display) = *vulkan_display {
            match vulkan_display
                .move_window(&projection_box.cast_unit())
                .with_context(|| "move the subwindow")
            {
                Ok(()) => true,
                Err(err) => {
                    error!("{:?}", err);
                    false
                }
            }
        } else {
            true
        };
    #[cfg(not(feature = "vulkan_display"))]
    let vulkan_projection_committed = true;

    // gfxstream owns a separate native presentation surface. Moving VulkanDisplay alone only
    // changes crosvm's window; it does not resize gfxstream's swapchain or posting viewport. Keep
    // both projections in sync when the Host resizes an embedded display.
    //
    // HD already aspect-fits the native host to the selected Android display. Use that exact
    // authoritative viewport rather than recomputing a projection from crosvm's asynchronously
    // updated scanout size. During a 90-degree transition the latter can still describe the
    // previous orientation, leaving the Win32 HWND at the new size while DisplaySurface and its
    // Vulkan swapchain remain at the old extent. Non-HD gfxstream callers keep crosvm's ordinary
    // projection behavior.
    //
    // Safe because `Window` object won't outlive the HWND.
    #[cfg(feature = "gfxstream")]
    unsafe {
        let (window_x, window_y, window_width, window_height) = if authoritative_host_viewport {
            (0, 0, host_viewport_size.width, host_viewport_size.height)
        } else {
            (
                projection_box.min.x,
                projection_box.min.y,
                projection_box.width(),
                projection_box.height(),
            )
        };
        if authoritative_host_viewport {
            const REQUIRED_STATUS: c_int = 0b1110;
            const MAX_COMMIT_ATTEMPTS: usize = 4;
            let mut status = 0;
            for attempt in 0..MAX_COMMIT_ATTEMPTS {
                status = gfxstream_backend_commit_window_for_display(
                    window.scanout_id(),
                    window.handle() as *const c_void,
                    window_x,
                    window_y,
                    window_width,
                    window_height,
                    virtual_display_size.width,
                    virtual_display_size.height,
                );
                if status & REQUIRED_STATUS == REQUIRED_STATUS {
                    break;
                }
                if attempt + 1 < MAX_COMMIT_ATTEMPTS {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            info!(
                "HD gfxstream viewport commit scanout={} host={}x{} framebuffer={}x{} status={}",
                window.scanout_id(),
                window_width,
                window_height,
                virtual_display_size.width,
                virtual_display_size.height,
                status
            );
            let gfxstream_projection_committed = status & REQUIRED_STATUS == REQUIRED_STATUS;
            if !gfxstream_projection_committed {
                warn!("HD viewport commit incomplete: status={status}");
            }
            return vulkan_projection_committed && gfxstream_projection_committed;
        } else {
            gfxstream_backend_setup_window_for_display(
                window.scanout_id(),
                window.handle() as *const c_void,
                window_x,
                window_y,
                window_width,
                window_height,
                virtual_display_size.width,
                virtual_display_size.height,
            );
        }
    }
    vulkan_projection_committed
}

#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct DisplayProperties {
    pub start_hidden: bool,
    pub is_fullscreen: bool,
    pub window_width: u32,
    pub window_height: u32,
}

impl From<&DisplayParameters> for DisplayProperties {
    fn from(params: &DisplayParameters) -> Self {
        let is_fullscreen = matches!(params.mode, DisplayMode::BorderlessFullScreen(_));
        let (window_width, window_height) = params.get_window_size();

        Self {
            start_hidden: params.hidden,
            is_fullscreen,
            window_width,
            window_height,
        }
    }
}

pub struct Surface {
    surface_id: u32,
    mouse_input: MouseInputManager,
    window_manager: WindowManager,
    virtual_display_manager: VirtualDisplayManager,
    #[allow(dead_code)]
    gpu_main_display_tube: Option<Rc<Tube>>,
    vulkan_display: Arc<Mutex<VulkanDisplayWrapper>>,
}

impl Surface {
    pub fn new(
        surface_id: u32,
        window: &GuiWindow,
        _metrics: Option<Weak<Metrics>>,
        display_params: &DisplayParameters,
        resources: SurfaceResources,
        vulkan_display: Arc<Mutex<VulkanDisplayWrapper>>,
    ) -> Result<Self> {
        static CONTEXT_MESSAGE: &str = "When creating Surface";
        info!(
            "Creating surface {} to associate with scanout {}",
            surface_id,
            window.scanout_id()
        );

        let initial_host_viewport_size = window.get_client_rect().context(CONTEXT_MESSAGE)?.size;
        let virtual_display_size = {
            let (width, height) = display_params.get_virtual_display_size();
            size2(width, height).checked_cast()
        };
        let virtual_display_manager =
            VirtualDisplayManager::new(&initial_host_viewport_size, &virtual_display_size);
        // This will make gfxstream initialize the child window to which it will render.
        let _ = update_virtual_display_projection(
            vulkan_display.lock(),
            window,
            &virtual_display_manager.get_virtual_display_projection_box(),
            &initial_host_viewport_size,
            virtual_display_manager.get_virtual_display_size(),
            false,
        );

        let SurfaceResources {
            display_event_dispatcher,
            gpu_main_display_tube,
        } = resources;

        let mouse_input = MouseInputManager::new(
            window,
            *virtual_display_manager.get_host_to_guest_transform(),
            virtual_display_size.checked_cast(),
            display_params.refresh_rate,
            display_event_dispatcher,
        );

        Ok(Surface {
            surface_id,
            mouse_input,
            window_manager: WindowManager::new(
                window,
                &display_params.into(),
                initial_host_viewport_size,
                gpu_main_display_tube.clone(),
            )
            .context(CONTEXT_MESSAGE)?,
            virtual_display_manager,
            gpu_main_display_tube,
            vulkan_display,
        })
    }

    pub fn surface_id(&self) -> u32 {
        self.surface_id
    }

    fn handle_key_event(
        &mut self,
        window: &GuiWindow,
        _key_down: bool,
        w_param: WPARAM,
        _l_param: LPARAM,
    ) {
        // Since we handle WM_SYSKEYDOWN we have to handle Alt-F4 ourselves.
        if (w_param == VK_MENU as usize || w_param == VK_F4 as usize)
            && keys_down(&[VK_MENU, VK_F4])
        {
            info!("Got alt-F4 w_param={}, posting WM_CLOSE", w_param);
            if let Err(e) =
                window.post_message(WM_CLOSE, /* w_param */ 0, /* l_param */ 0)
            {
                error!("Failed to post WM_CLOSE: {:?}", e);
            }
        }
    }

    fn set_mouse_mode(&mut self, window: &GuiWindow, mouse_mode: MouseMode) {
        self.mouse_input
            .handle_change_mouse_mode_request(window, mouse_mode);
    }

    fn update_host_viewport_size(
        &mut self,
        window: &GuiWindow,
        host_viewport_size: &Size2D<i32, HostWindowSpace>,
        canonical_rotation_quarters: u8,
    ) -> bool {
        info!(
            "Updating host viewport size to {:?} at canonical rotation {}",
            host_viewport_size, canonical_rotation_quarters
        );
        let start = Instant::now();

        // HD embeds this input/projection HWND in a Player-owned display host. Applying the
        // authoritative viewport from the crosvm window thread avoids the cross-thread
        // SetWindowPos lag that otherwise leaves pointer coordinates and the gfxstream child at
        // the previous drag step for one or more DWM compositions.
        if host_viewport_size.width > 0 && host_viewport_size.height > 0 {
            match window.get_client_rect() {
                Ok(current) if current.size != *host_viewport_size => {
                    let target = Rect::new(point2(0, 0), *host_viewport_size);
                    if let Err(err) = window.set_pos(&target, SWP_NOACTIVATE | SWP_NOZORDER) {
                        error!("Failed to apply embedded host viewport: {err:#}");
                        return false;
                    }
                }
                Err(err) => {
                    error!("Failed to inspect embedded host viewport: {err:#}");
                    return false;
                }
                _ => {}
            }
        }

        self.virtual_display_manager
            .update_host_guest_transforms_with_rotation(
                host_viewport_size,
                canonical_rotation_quarters,
            );
        let virtual_display_projection_box = self
            .virtual_display_manager
            .get_virtual_display_projection_box();
        let projection_committed = update_virtual_display_projection(
            self.vulkan_display.lock(),
            window,
            &virtual_display_projection_box,
            host_viewport_size,
            self.virtual_display_manager.get_virtual_display_size(),
            true,
        );
        // Keep pointer routing on the last fully committed projection. If gfxstream rejects a
        // transient viewport/orientation update, advancing only the input transform would make
        // the still-visible old frame and Android hit-testing disagree until the host retries.
        // The unapplied request remains published by the host and this method recomputes the same
        // transform when the native projection can be committed.
        if projection_committed {
            self.mouse_input.update_host_to_guest_transform(
                *self.virtual_display_manager.get_host_to_guest_transform(),
            );
        }

        let elapsed = start.elapsed();
        let elapsed_millis = elapsed.as_millis();
        if elapsed < HANDLE_WINDOW_MESSAGE_TIMEOUT {
            info!(
                "Finished updating host viewport size in {}ms",
                elapsed_millis
            );
        } else {
            warn!(
                "Window might have been hung since updating host viewport size took \
                        too long ({}ms)!",
                elapsed_millis
            );
        }
        projection_committed
    }

    /// Called once when it is safe to assume all future messages targeting `window` will be
    /// dispatched to this `Surface`.
    fn on_message_dispatcher_attached(&mut self, window: &GuiWindow) {
        // `WindowManager` relies on window messages to properly set initial window pos.
        // We might see a suboptimal UI if any error occurs here, such as having black bars. Instead
        // of crashing the emulator, we would just log the error and still allow the user to
        // experience the app.
        if let Err(e) = self.window_manager.set_initial_window_pos(window) {
            error!("Failed to set initial window pos: {:#?}", e);
        }

        // HD records the requested viewport on the HWND before sending the private message. The
        // message can arrive during the short interval in which this window is parked in the
        // dispatcher's vacant map, where no Surface exists to consume it. Replay only an
        // unapplied value now that all future messages are guaranteed to reach this Surface.
        let latest = unsafe {
            GetPropW(
                window.handle(),
                win32_wide_string(LATEST_VIEWPORT_PROPERTY).as_ptr(),
            )
        };
        let applied = unsafe {
            GetPropW(
                window.handle(),
                win32_wide_string(APPLIED_VIEWPORT_PROPERTY).as_ptr(),
            )
        };
        let latest_rotation = unsafe {
            GetPropW(
                window.handle(),
                win32_wide_string(LATEST_ROTATION_PROPERTY).as_ptr(),
            )
        };
        let applied_rotation = unsafe {
            GetPropW(
                window.handle(),
                win32_wide_string(APPLIED_ROTATION_PROPERTY).as_ptr(),
            )
        };
        let force_pending_property = win32_wide_string(FORCE_VIEWPORT_PENDING_PROPERTY);
        let force_pending = unsafe { GetPropW(window.handle(), force_pending_property.as_ptr()) };
        if !latest.is_null()
            && (latest != applied
                || latest_rotation != applied_rotation
                || !force_pending.is_null())
        {
            self.on_host_viewport_change(
                window,
                latest_rotation as WPARAM
                    | if force_pending.is_null() {
                        0
                    } else {
                        FORCE_VIEWPORT_COMMIT_WPARAM
                    },
                latest as LPARAM,
            );
        }
    }

    /// Called whenever any window message is retrieved. Returns None if `DefWindowProcW()` should
    /// be called after our processing.
    #[inline]
    pub fn handle_window_message(
        &mut self,
        window: &GuiWindow,
        message: WindowMessage,
    ) -> Option<LRESULT> {
        if let ControlFlow::Break(ret) = self.mouse_input.handle_window_message(window, &message) {
            return ret;
        }

        // Just return 0 for most of the messages we processed.
        let mut ret: Option<LRESULT> = Some(0);
        match message {
            WindowMessage::Key {
                is_sys_key: _,
                is_down,
                w_param,
                l_param,
            } => self.handle_key_event(window, is_down, w_param, l_param),
            WindowMessage::WindowPos(window_pos_msg) => {
                ret = self.handle_window_pos_message(window, window_pos_msg)
            }
            WindowMessage::DisplayChange => self.window_manager.handle_display_change(window),
            WindowMessage::HostViewportChange { w_param, l_param } => {
                ret = Some(self.on_host_viewport_change(window, w_param, l_param))
            }
            WindowMessage::SelectDisplay => {
                #[cfg(feature = "gfxstream")]
                {
                    // The selected scanout is derived from the exact CROSVM_<id> HWND receiving
                    // the host message. This prevents an untrusted/stale payload from selecting a
                    // different render target than the input surface.
                    ret = Some(unsafe {
                        gfxstream_backend_select_display(window.scanout_id()) as LRESULT
                    });
                }
                #[cfg(not(feature = "gfxstream"))]
                {
                    ret = Some(0);
                }
            }
            // The following messages are handled by other modules.
            WindowMessage::WindowActivate { .. }
            | WindowMessage::Mouse(_)
            | WindowMessage::KeyboardFocus => (),
            WindowMessage::Other(..) => {
                // Request default processing for messages that we don't explicitly handle.
                ret = None;
            }
        }
        ret
    }

    #[inline]
    pub fn handle_general_message(
        &mut self,
        window: &GuiWindow,
        message: &GeneralMessage,
        keyboard_input_manager: &KeyboardInputManager,
    ) {
        match message {
            GeneralMessage::MessageDispatcherAttached => {
                self.on_message_dispatcher_attached(window)
            }
            GeneralMessage::RawInputEvent(raw_input) => {
                self.mouse_input.handle_raw_input_event(window, *raw_input)
            }
            GeneralMessage::GuestEvent {
                event_device_kind,
                event,
            } => {
                if let EventDeviceKind::Keyboard = event_device_kind {
                    keyboard_input_manager.handle_guest_event(window, *event);
                }
            }
            GeneralMessage::SetMouseMode(mode) => self.set_mouse_mode(window, *mode),
        }
    }

    /// Returns None if `DefWindowProcW()` should be called after our processing.
    #[inline]
    fn handle_window_pos_message(
        &mut self,
        window: &GuiWindow,
        message: WindowPosMessage,
    ) -> Option<LRESULT> {
        self.window_manager
            .handle_window_pos_message(window, &message);
        match message {
            WindowPosMessage::WindowPosChanged { .. } => {
                // Request default processing, otherwise `WM_SIZE` and `WM_MOVE` won't be sent.
                // https://learn.microsoft.com/en-us/windows/win32/winmsg/wm-windowposchanged#remarks
                return None;
            }
            // "An application should return TRUE if it processes this message."
            WindowPosMessage::WindowSizeChanging { .. } => return Some(TRUE as LRESULT),
            _ => (),
        }
        Some(0)
    }

    #[inline]
    fn on_host_viewport_change(
        &mut self,
        window: &GuiWindow,
        w_param: WPARAM,
        l_param: LPARAM,
    ) -> LRESULT {
        let property = win32_wide_string(APPLIED_VIEWPORT_PROPERTY);
        let applied = unsafe { GetPropW(window.handle(), property.as_ptr()) };
        let rotation_property = win32_wide_string(APPLIED_ROTATION_PROPERTY);
        let applied_rotation = unsafe { GetPropW(window.handle(), rotation_property.as_ptr()) };
        let force_commit = w_param & FORCE_VIEWPORT_COMMIT_WPARAM != 0;
        let rotation_wparam = w_param & !FORCE_VIEWPORT_COMMIT_WPARAM;
        if !force_commit
            && applied as LPARAM == l_param
            && applied_rotation as WPARAM == rotation_wparam
        {
            return 1;
        }
        let new_size = size2(LOWORD(l_param as u32) as i32, HIWORD(l_param as u32) as i32);
        let canonical_rotation_quarters = u8::try_from(rotation_wparam)
            .ok()
            .and_then(|value| value.checked_sub(1))
            .filter(|value| *value < 4)
            .unwrap_or_default();
        if !self.update_host_viewport_size(window, &new_size, canonical_rotation_quarters) {
            // Leave the latest request unapplied so a later host/layout repair can replay it.
            // Publishing APPLIED_* here would permanently deduplicate a transient gfxstream
            // surface failure and leave the HWND/input geometry ahead of the Vulkan swapchain.
            warn!("Deferring HD viewport acknowledgement until native projection commit succeeds");
            return 0;
        }

        // Publish the acknowledgement only after crosvm has updated its input transform and
        // gfxstream has committed the matching native surface. The Host uses this property for
        // deduplication, so ordinary WebView button clicks never recreate the Android swapchain.
        if unsafe { SetPropW(window.handle(), property.as_ptr(), l_param as *mut c_void) } == 0 {
            error!("Failed to acknowledge the applied HD viewport");
            return 0;
        }
        if unsafe {
            SetPropW(
                window.handle(),
                rotation_property.as_ptr(),
                rotation_wparam as *mut c_void,
            )
        } == 0
        {
            error!("Failed to acknowledge the applied HD rotation");
            return 0;
        }
        let force_pending_property = win32_wide_string(FORCE_VIEWPORT_PENDING_PROPERTY);
        unsafe { RemovePropW(window.handle(), force_pending_property.as_ptr()) };
        1
    }
}
