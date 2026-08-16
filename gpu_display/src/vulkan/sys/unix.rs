// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::RefCell;
use std::ffi::c_void;
use std::fs::File;
use std::os::fd::FromRawFd;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use ash::vk;
use base::AsRawDescriptor;
use euclid::size2;
use euclid::Size2D;
use euclid::UnknownUnit;
use vulkano::device::Device;
use vulkano::instance::Instance;
use vulkano::memory::ExternalMemoryHandleType;
use vulkano::memory::ExternalMemoryHandleTypes;
use vulkano::memory::MemoryImportInfo;
use vulkano::VulkanObject;

use super::ApplicationState;
use super::ApplicationStateBuilder;
use super::Surface;
use super::Window;
use super::WindowEvent;
use super::WindowEventLoop;

#[derive(Clone, Copy)]
pub struct NativeWindowType {
    pub display: *mut c_void,
    pub window: u64,
}

pub struct UnixWindow {
    display: usize,
    window: u64,
    size: Size2D<u32, UnknownUnit>,
}

unsafe impl Send for UnixWindow {}
unsafe impl Sync for UnixWindow {}

impl Window for UnixWindow {
    fn create_vulkan_surface(self: Arc<Self>, instance: Arc<Instance>) -> Result<Arc<Surface>> {
        // SAFETY: The X Display and Window handles are owned by the DisplayX surface that creates
        // VulkanDisplay and outlive this Vulkan surface.
        unsafe {
            Surface::from_xlib(
                instance,
                self.display as *const c_void,
                self.window as vk::Window,
                Arc::clone(&self) as _,
            )
        }
        .map_err(|e| e.into())
    }

    fn get_inner_size(&self) -> Result<Size2D<u32, UnknownUnit>> {
        Ok(self.size)
    }
}

pub struct UnixWindowEventLoop<AppState: ApplicationState> {
    app_state: RefCell<AppState>,
    window: Arc<UnixWindow>,
}

// VulkanDisplay is driven by the gpu worker thread on Unix. The event loop is marked Send to
// satisfy the common trait, but send_event executes synchronously on the owning thread.
unsafe impl<AppState: ApplicationState> Send for UnixWindowEventLoop<AppState> {}

impl<AppState: ApplicationState> WindowEventLoop<AppState> for UnixWindowEventLoop<AppState> {
    type WindowType = UnixWindow;

    unsafe fn create<Builder>(
        parent: NativeWindowType,
        initial_window_size: &Size2D<i32, UnknownUnit>,
        application_state_builder: Builder,
    ) -> Result<Self>
    where
        Builder: ApplicationStateBuilder<Target = AppState>,
    {
        let window = Arc::new(UnixWindow {
            display: parent.display as usize,
            window: parent.window,
            size: size2(
                initial_window_size.width as u32,
                initial_window_size.height as u32,
            ),
        });
        let app_state = application_state_builder
            .build(Arc::clone(&window))
            .context("building Unix VulkanDisplay state")?;
        Ok(Self {
            app_state: RefCell::new(app_state),
            window,
        })
    }

    fn move_window(&self, _pos: &euclid::Box2D<i32, UnknownUnit>) -> Result<()> {
        // The Linux X backend owns placement for the parent surface. VulkanDisplay renders into
        // that X window directly, so there is no separate child to move here.
        Ok(())
    }

    fn send_event(&self, event: AppState::UserEvent) -> Result<()> {
        self.app_state
            .borrow()
            .process_event(WindowEvent::User(event));
        Ok(())
    }
}

pub(crate) fn create_post_image_external_memory_handle_types() -> ExternalMemoryHandleTypes {
    ExternalMemoryHandleTypes {
        opaque_fd: true,
        ..ExternalMemoryHandleTypes::empty()
    }
}

// The ownership of the descriptor is transferred to the returned MemoryImportInfo.
pub(crate) fn create_post_image_memory_import_info(
    memory_descriptor: &dyn AsRawDescriptor,
) -> MemoryImportInfo {
    // SAFETY: dup returns a new fd that is owned by the File stored in MemoryImportInfo.
    let fd = unsafe { libc::dup(memory_descriptor.as_raw_descriptor()) };
    assert!(fd >= 0, "dup failed while importing Vulkan display memory");
    MemoryImportInfo::Fd {
        handle_type: ExternalMemoryHandleType::OpaqueFd,
        // SAFETY: fd was returned by dup and is uniquely owned here.
        file: unsafe { File::from_raw_fd(fd) },
    }
}

pub(crate) fn import_semaphore_from_descriptor(
    device: &Arc<Device>,
    semaphore: vk::Semaphore,
    descriptor: &dyn AsRawDescriptor,
) -> vk::Result {
    // SAFETY: dup returns a new fd. vkImportSemaphoreFdKHR consumes fd on success.
    let fd = unsafe { libc::dup(descriptor.as_raw_descriptor()) };
    if fd < 0 {
        return vk::Result::ERROR_INVALID_EXTERNAL_HANDLE;
    }
    let import_handle_info = vk::ImportSemaphoreFdInfoKHR::builder()
        .semaphore(semaphore)
        .flags(vk::SemaphoreImportFlags::empty())
        .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
        .fd(fd)
        .build();
    // SAFETY: import_handle_info is local and outlives the call.
    unsafe {
        (device
            .fns()
            .khr_external_semaphore_fd
            .import_semaphore_fd_khr)(device.internal_object(), &import_handle_info)
    }
}
