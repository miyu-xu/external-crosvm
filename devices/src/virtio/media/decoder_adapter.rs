// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! An adapter from the virtio-media protocol to the video devices originally used with
//! virtio-video.
//!
//! This allows to reuse already-existing crosvm virtio-video devices with
//! virtio-media.

use std::borrow::Borrow;
use std::fs::File;
use std::os::fd::BorrowedFd;
use std::sync::Arc;

use base::AsRawDescriptor;
use base::MemoryMappingBuilder;
use base::SharedMemory;

use anyhow::Context;
use base::WaitContext;
use virtio_media::ioctl::virtio_media_dispatch_ioctl;
use virtio_media::ioctl::IoctlResult;
use virtio_media::ioctl::VirtioMediaIoctlHandler;
use virtio_media::mmap::MmapMappingManager;
use virtio_media::protocol::DequeueBufferEvent;
use virtio_media::protocol::MmapResp;
use virtio_media::protocol::MunmapResp;
use virtio_media::protocol::SessionEvent;
use virtio_media::protocol::V4l2Event;
use virtio_media::protocol::VIRTIO_MEDIA_MMAP_FLAG_RW;
use virtio_media::v4l2r;
use virtio_media::v4l2r::bindings;
use virtio_media::v4l2r::ioctl::BufferCapabilities;
use virtio_media::v4l2r::ioctl::BufferField;
use virtio_media::v4l2r::ioctl::BufferFlags;
use virtio_media::v4l2r::ioctl::DecoderCmd;
use virtio_media::v4l2r::ioctl::EventType;
use virtio_media::v4l2r::ioctl::SrcChanges;
use virtio_media::v4l2r::ioctl::V4l2Buffer;
use virtio_media::v4l2r::ioctl::V4l2PlaneAccessor;
use virtio_media::v4l2r::ioctl::V4l2PlanesWithBackingMut;
use virtio_media::v4l2r::memory::MemoryType;
use virtio_media::v4l2r::Colorspace;
use virtio_media::v4l2r::PixelFormat;
use virtio_media::v4l2r::Quantization;
use virtio_media::v4l2r::QueueDirection;
use virtio_media::v4l2r::QueueType;
use virtio_media::v4l2r::XferFunc;
use virtio_media::v4l2r::YCbCrEncoding;
use virtio_media::VirtioMediaDevice;
use virtio_media::VirtioMediaEventQueue;
use virtio_media::VirtioMediaHostMemoryMapper;

use crate::virtio::video::decoder::backend::DecoderEvent;
use crate::virtio::video::decoder::backend::DecoderSession;
use crate::virtio::video::decoder::capability::Capability;
use crate::virtio::video::decoder::Decoder;
use crate::virtio::video::decoder::DecoderBackend;
use crate::virtio::video::format::Format;
use crate::virtio::video::format::FramePlane;
use crate::virtio::video::format::PlaneFormat;
use crate::virtio::video::resource::GuestMemArea;
use crate::virtio::video::resource::GuestMemHandle;
use crate::virtio::video::resource::GuestResource;
use crate::virtio::video::resource::GuestResourceHandle;

use super::Token;

// Use MB input buffers by default.
// TODO: This should be resolution-dependent?
const INPUT_BUFFER_SIZE: u32 = 1024 * 1024;

/// Returns the V4L2 `(bytesperline, sizeimage)` for the given `format` and `coded_size`.
fn buffer_sizes_for_format(format: Format, coded_size: (u32, u32)) -> (u32, u32) {
    match PlaneFormat::get_plane_layout(format, coded_size.0, coded_size.1) {
        None => (0, INPUT_BUFFER_SIZE),
        Some(layout) => (
            layout.first().map(|p| p.stride).unwrap_or(0),
            layout.iter().map(|p| p.plane_size).sum(),
        ),
    }
}

struct VirtioMediaAdapterBuffer {
    v4l2_buffer: V4l2Buffer,
    shm: SharedMemory,
    // Cached here to avoid a match.
    mmap_offset: u32,
    // Switched to `true` once the buffer's backing memory has been registered with
    // `use_output_buffer`.
    registered: bool,
}

impl VirtioMediaAdapterBuffer {
    fn new(queue: QueueType, index: u32, size: usize, mmap_offset: u32) -> IoctlResult<Self> {
        let shm = SharedMemory::new(
            format!("virtio_media {:?} {}", queue.direction(), index),
            size as u64,
        )
        .map_err(|_| libc::ENOMEM)?;

        let mut v4l2_buffer = V4l2Buffer::new(queue, index, MemoryType::Mmap);
        if let V4l2PlanesWithBackingMut::Mmap(mut planes) =
            v4l2_buffer.planes_with_backing_iter_mut()
        {
            // SAFETY: every buffer has at least one plane.
            let mut plane = planes.next().unwrap();
            plane.set_mem_offset(mmap_offset);
            *plane.length = shm.size() as u32;
        } else {
            // SAFETY: we have just set the buffer type to MMAP. Reaching this point means a bug in
            // the code.
            panic!()
        }

        v4l2_buffer.set_flags(BufferFlags::TIMESTAMP_MONOTONIC);
        v4l2_buffer.set_field(BufferField::None);

        Ok(Self {
            v4l2_buffer,
            shm,
            mmap_offset,
            registered: false,
        })
    }
}

enum EosBuffer {
    /// No EOS buffer queued yet and no EOS pending.
    None,
    /// EOS buffer is available, and no EOS pending.
    Available(u32),
    /// EOS is pending but we have no buffer queued yet.
    Awaiting,
}

pub struct VirtioMediaAdapterSession<D: DecoderBackend> {
    id: u32,
    /// The session can only be created once we know the input format.
    session: Option<D::Session>,
    input_format: Format,
    output_format: Format,
    coded_size: (u32, u32),
    visible_rect: v4l2r::Rect,
    /// Minimum number of output buffers required to decode the stream, as reported by the backend.
    min_output_buffers: u32,

    colorspace: Colorspace,
    xfer_func: XferFunc,
    ycbcr_enc: YCbCrEncoding,
    quantization: Quantization,

    input_buffers: Vec<VirtioMediaAdapterBuffer>,
    input_streaming: bool,
    output_buffers: Vec<VirtioMediaAdapterBuffer>,
    output_streaming: bool,

    sequence_cpt: u32,

    /// Whether the input source change event has been subscribed to by the driver. If `true` then
    /// the device will emit resolution change events.
    src_change_subscribed: bool,
    /// Whether the EOS event has been subscribed to by the driver. If `true` then the device will
    /// emit EOS events.
    eos_subscribed: bool,

    /// Whether the `set_output_parameters` of the backend needs to be called before a CAPTURE
    /// buffer is sent.
    need_set_output_params: bool,

    /// Index of the capture buffer we kept in order to signal EOS.
    // TODO: rotate as buffers get queued? That way we don't hold on too much on a specific buffer,
    // which doesn't look good in traces.
    eos_capture_buffer: EosBuffer,
}

impl<D: DecoderBackend> VirtioMediaAdapterSession<D> {
    /// Returns a `v4l2_format` for `queue` with the current parameters that can be used as a basis
    /// for returning values to the driver.
    ///
    /// `type_` **must** be set by the caller.
    fn v4l2_format(&self, direction: QueueDirection) -> bindings::v4l2_format {
        let format = match direction {
            QueueDirection::Output => self.input_format,
            QueueDirection::Capture => self.output_format,
        };
        let (bytesperline, sizeimage) = buffer_sizes_for_format(format, self.coded_size);

        let mut plane_fmt: [bindings::v4l2_plane_pix_format; 8] = Default::default();
        plane_fmt[0] = bindings::v4l2_plane_pix_format {
            bytesperline,
            sizeimage,
            reserved: Default::default(),
        };

        bindings::v4l2_format {
            type_: match direction {
                QueueDirection::Output => QueueType::VideoOutputMplane,
                QueueDirection::Capture => QueueType::VideoCaptureMplane,
            } as u32,
            fmt: bindings::v4l2_format__bindgen_ty_1 {
                pix_mp: bindings::v4l2_pix_format_mplane {
                    width: self.coded_size.0,
                    height: self.coded_size.1,
                    pixelformat: PixelFormat::from_fourcc(virtio_video_format_to_fourcc(format))
                        .to_u32(),
                    field: bindings::v4l2_field_V4L2_FIELD_NONE,
                    colorspace: self.colorspace as u32,
                    plane_fmt,
                    num_planes: 1,
                    flags: 0,
                    __bindgen_anon_1: bindings::v4l2_pix_format_mplane__bindgen_ty_1 {
                        ycbcr_enc: self.ycbcr_enc as u8,
                    },
                    quantization: self.quantization as u8,
                    xfer_func: self.xfer_func as u8,
                    reserved: Default::default(),
                },
            },
        }
    }
}

pub struct VirtioMediaDecoderAdapter<
    D: DecoderBackend,
    Q: VirtioMediaEventQueue,
    HM: VirtioMediaHostMemoryMapper,
> {
    decoder: D,
    capability: Capability,
    event_queue: Q,
    wait_ctx: Arc<WaitContext<Token>>,
    host_mapper: MmapMappingManager<HM>,
}

impl<D, Q, HM> VirtioMediaDecoderAdapter<D, Q, HM>
where
    D: DecoderBackend,
    Q: VirtioMediaEventQueue,
    HM: VirtioMediaHostMemoryMapper,
{
    pub(super) fn new(
        decoder: D,
        event_queue: Q,
        host_mapper: HM,
        wait_ctx: Arc<WaitContext<Token>>,
    ) -> Self {
        let capability = decoder.get_capabilities();

        Self {
            decoder,
            capability,
            event_queue,
            host_mapper: MmapMappingManager::from(host_mapper),
            wait_ctx,
        }
    }

    /// Validate `format` for `queue` and return the adjusted format. If `commit` is `true`, also
    /// set the new parameters to our internal state.
    fn try_or_set_format(
        &mut self,
        session: &mut VirtioMediaAdapterSession<D>,
        queue: QueueType,
        format: bindings::v4l2_format,
        commit: bool,
    ) -> IoctlResult<bindings::v4l2_format> {
        let available_formats = match queue {
            QueueType::VideoOutputMplane => self.capability.input_formats(),
            QueueType::VideoCaptureMplane => self.capability.output_formats(),
            _ => return Err(libc::EINVAL),
        };
        // Safe because we have just confirmed that the queue is multiplanar OUTPUT or CAPTURE.
        let pix_format = unsafe { &format.fmt.pix_mp };
        let pixel_format = fourcc_to_virtio_video_format(
            &PixelFormat::from_u32(pix_format.pixelformat).to_fourcc(),
        );

        // If the received pixel format is valid, find the format in our capabilities that matches,
        // otherwise fall back to the first format.
        let matching_format = pixel_format
            .and_then(|format| available_formats.iter().find(|f| f.format == format))
            .or_else(|| available_formats.first())
            .ok_or(libc::ENODEV)?;

        // Now check that the requested resolution is within the supported range.
        // TODO: step-wise ranges only have one entry in V4L2, should we simplify in virtio-video
        // as well?
        let (mut width, mut height) = if matching_format
            .frame_formats
            .first()
            .map(|format| {
                let width = pix_format.width;
                let height = pix_format.height;
                (format.width.min..format.width.max).contains(&width)
                    && (format.height.min..format.height.max).contains(&height)
            })
            .unwrap_or(false)
        {
            // TODO: If we have a range, adjust the resolution using its step.
            (pix_format.width, pix_format.height)
        } else {
            // Otherwise fallback to the smallest supported resolution (TODO: might want to use the
            // closest matching resolution here?)
            matching_format
                .frame_formats
                .first()
                .map(|f| (f.width.min, f.height.min))
                .unwrap_or((0, 0))
        };

        // CAPTURE resolution cannot be lower than OUTPUT one.
        if queue.direction() == QueueDirection::Capture {
            width = std::cmp::max(width, session.coded_size.0);
            height = std::cmp::max(height, session.coded_size.1);
        }

        // Validate and clamp colorspace, xfer_func, ycbcr_enc and quantization, and use current
        // values if this is the CAPTURE queue.
        let (colorspace, xfer_func, ycbcr_enc, quantization) =
            if queue.direction() == QueueDirection::Output {
                (
                    Colorspace::n(pix_format.colorspace).unwrap_or(session.colorspace),
                    XferFunc::n(pix_format.xfer_func as u32).unwrap_or(session.xfer_func),
                    // TODO: safe because...
                    YCbCrEncoding::n(unsafe { pix_format.__bindgen_anon_1.ycbcr_enc as u32 })
                        .unwrap_or(session.ycbcr_enc),
                    Quantization::n(pix_format.quantization as u32).unwrap_or(session.quantization),
                )
            } else {
                (
                    session.colorspace,
                    session.xfer_func,
                    session.ycbcr_enc,
                    session.quantization,
                )
            };

        if commit {
            match queue.direction() {
                QueueDirection::Output => {
                    session.input_format = matching_format.format;
                    session.coded_size = (width, height);
                    session.colorspace = colorspace;
                    session.xfer_func = xfer_func;
                    session.ycbcr_enc = ycbcr_enc;
                    session.quantization = quantization;
                    // TODO: update output formats too - use a helper to validate the current
                    // settings and adjust them if needed?
                }
                QueueDirection::Capture => {
                    session.output_format = matching_format.format;
                    // TODO: check the resolution again.
                }
            }
        }

        // We only support one plane per buffer for now.
        let num_planes = 1;
        let (bytesperline, sizeimage) =
            buffer_sizes_for_format(matching_format.format, (width, height));
        let mut plane_fmt: [bindings::v4l2_plane_pix_format; 8] = Default::default();
        plane_fmt[0] = bindings::v4l2_plane_pix_format {
            bytesperline,
            sizeimage,
            reserved: Default::default(),
        };

        Ok(bindings::v4l2_format {
            type_: format.type_,
            fmt: bindings::v4l2_format__bindgen_ty_1 {
                pix_mp: bindings::v4l2_pix_format_mplane {
                    width,
                    height,
                    pixelformat: PixelFormat::from_fourcc(virtio_video_format_to_fourcc(
                        matching_format.format,
                    ))
                    .to_u32(),
                    field: bindings::v4l2_field_V4L2_FIELD_NONE,
                    colorspace: colorspace as u32,
                    plane_fmt,
                    num_planes,
                    flags: 0,
                    __bindgen_anon_1: bindings::v4l2_pix_format_mplane__bindgen_ty_1 {
                        ycbcr_enc: ycbcr_enc as u8,
                    },
                    quantization: quantization as u8,
                    xfer_func: xfer_func as u8,
                    reserved: Default::default(),
                },
            },
        })
    }

    pub fn process_one_event(&mut self, session: &mut VirtioMediaAdapterSession<D>) {
        if let Some(backend_session) = &mut session.session {
            let event = backend_session.read_event().unwrap();

            match event {
                DecoderEvent::NotifyEndOfBitstreamBuffer(id) => {
                    let Some(buffer) = session.input_buffers.get_mut(id as usize) else {
                        base::error!("no matching OUTPUT buffer with id {} to process event", id);
                        return;
                    };

                    buffer.v4l2_buffer.clear_flags(BufferFlags::QUEUED);

                    self.event_queue
                        .send_event(V4l2Event::DequeueBuffer(DequeueBufferEvent::new(
                            session.id,
                            buffer.v4l2_buffer.clone(),
                        )));
                }
                DecoderEvent::ProvidePictureBuffers {
                    min_num_buffers,
                    width,
                    height,
                    visible_rect,
                } => {
                    // Add one extra buffer to keep one on the side in order to signal EOS.
                    session.min_output_buffers = min_num_buffers + 1;
                    session.coded_size = (width as u32, height as u32);
                    session.visible_rect = v4l2r::Rect {
                        left: visible_rect.left,
                        top: visible_rect.top,
                        width: visible_rect.right.saturating_sub(visible_rect.left) as u32,
                        height: visible_rect.bottom.saturating_sub(visible_rect.top) as u32,
                    };
                    session.need_set_output_params = true;

                    // All buffers need to be registered again
                    for buffer in &mut session.output_buffers {
                        buffer.registered = false;
                    }

                    if session.src_change_subscribed {
                        self.event_queue
                            .send_event(V4l2Event::Event(SessionEvent::new(
                                session.id,
                                bindings::v4l2_event {
                                    type_: bindings::V4L2_EVENT_SOURCE_CHANGE,
                                    u: bindings::v4l2_event__bindgen_ty_1 {
                                        src_change: bindings::v4l2_event_src_change {
                                            changes: SrcChanges::RESOLUTION.bits(),
                                        },
                                    },
                                    // TODO: fill pending, sequence, and timestamp.
                                    ..Default::default()
                                },
                            )))
                    }
                }
                DecoderEvent::PictureReady {
                    picture_buffer_id,
                    timestamp,
                    visible_rect,
                } => {
                    let Some(buffer) = session.output_buffers.get_mut(picture_buffer_id as usize)
                    else {
                        base::error!(
                            "no matching CAPTURE buffer with id {} to process event",
                            picture_buffer_id
                        );
                        return;
                    };

                    buffer.v4l2_buffer.clear_flags(BufferFlags::QUEUED);
                    buffer.v4l2_buffer.set_flags(BufferFlags::TIMESTAMP_COPY);
                    buffer.v4l2_buffer.set_sequence(session.sequence_cpt);
                    session.sequence_cpt += 1;
                    buffer.v4l2_buffer.set_timestamp(bindings::timeval {
                        tv_sec: (timestamp / 1_000_000) as i64,
                        tv_usec: (timestamp % 1_000_000) as i64,
                    });
                    let first_plane = buffer.v4l2_buffer.get_first_plane_mut();
                    *first_plane.bytesused =
                        buffer_sizes_for_format(session.output_format, session.coded_size).1;

                    self.event_queue
                        .send_event(V4l2Event::DequeueBuffer(DequeueBufferEvent::new(
                            session.id,
                            buffer.v4l2_buffer.clone(),
                        )))
                }
                DecoderEvent::FlushCompleted(_) => {
                    // TODO: process argument
                    match session.eos_capture_buffer {
                        EosBuffer::Available(id) => {
                            self.send_eos(session, id);
                            session.eos_capture_buffer = EosBuffer::Awaiting;
                        }
                        _ => panic!(),
                    }
                }
                DecoderEvent::ResetCompleted(_) => todo!(),
                DecoderEvent::NotifyError(e) => todo!(),
            }
        }
    }

    /// Called when the conditions for sending EOS are all met.
    ///
    /// `eos_buf_id` is the ID of the empty buffer to send with the `LAST` flag set.
    fn send_eos(&mut self, session: &mut VirtioMediaAdapterSession<D>, eos_buf_id: u32) {
        let buffer = session.output_buffers.get_mut(eos_buf_id as usize).unwrap();

        buffer.v4l2_buffer.add_flags(BufferFlags::LAST);
        // TODO: set bytes_used to zero!
        self.event_queue
            .send_event(V4l2Event::DequeueBuffer(DequeueBufferEvent::new(
                session.id,
                buffer.v4l2_buffer.clone(),
            )));

        if session.eos_subscribed {
            self.event_queue
                .send_event(V4l2Event::Event(SessionEvent::new(
                    session.id,
                    bindings::v4l2_event {
                        type_: bindings::V4L2_EVENT_EOS,
                        ..Default::default()
                    },
                )))
        }
    }
}

impl<D, Q, HM, Reader, Writer> VirtioMediaDevice<Reader, Writer>
    for VirtioMediaDecoderAdapter<D, Q, HM>
where
    D: DecoderBackend,
    Q: VirtioMediaEventQueue,
    HM: VirtioMediaHostMemoryMapper,
    Reader: std::io::Read,
    Writer: std::io::Write,
{
    type Session = VirtioMediaAdapterSession<D>;

    fn new_session(&mut self, session_id: u32) -> Result<Self::Session, i32> {
        let first_input_format = self
            .capability
            .input_formats()
            .first()
            .ok_or(libc::ENODEV)?;
        let first_output_format = self
            .capability
            .output_formats()
            .first()
            .ok_or(libc::ENODEV)?;

        let coded_size = first_input_format
            .frame_formats
            .first()
            .map(|f| (f.width.min, f.height.min))
            .unwrap_or((0, 0));

        Ok(VirtioMediaAdapterSession {
            id: session_id,
            session: None,
            input_format: first_input_format.format,
            coded_size,
            visible_rect: v4l2r::Rect {
                left: 0,
                top: 0,
                width: coded_size.0,
                height: coded_size.1,
            },
            min_output_buffers: 0,
            output_format: first_output_format.format,
            colorspace: Colorspace::Rec709,
            xfer_func: XferFunc::None,
            ycbcr_enc: YCbCrEncoding::E709,
            quantization: Quantization::LimRange,
            input_buffers: Default::default(),
            input_streaming: false,
            output_buffers: Default::default(),
            output_streaming: false,
            sequence_cpt: 0,
            src_change_subscribed: false,
            eos_subscribed: false,
            need_set_output_params: false,
            eos_capture_buffer: EosBuffer::None,
        })
    }

    fn close_session(&mut self, session: Self::Session) {
        if let Some(backend_session) = &session.session {
            if let Err(e) = self.wait_ctx.delete(backend_session.event_pipe()) {
                base::error!(
                    "error while removing event pipe for session {}: {:#}",
                    session.id,
                    e
                );
            }
        }

        for buffer in &session.input_buffers {
            self.host_mapper
                .unregister_buffer(buffer.mmap_offset as u64);
        }
        for buffer in &session.output_buffers {
            self.host_mapper
                .unregister_buffer(buffer.mmap_offset as u64);
        }
    }

    fn do_ioctl(
        &mut self,
        session: &mut Self::Session,
        ioctl: virtio_media::protocol::V4l2Ioctl,
        reader: &mut Reader,
        writer: &mut Writer,
    ) -> std::io::Result<()> {
        virtio_media_dispatch_ioctl(self, session, ioctl, reader, writer)
    }

    fn do_mmap(
        &mut self,
        session: &mut Self::Session,
        flags: u32,
        offset: u64,
    ) -> Result<(u64, u64), i32> {
        let buffer = session
            .input_buffers
            .iter()
            .chain(session.output_buffers.iter())
            .find(|b| b.mmap_offset as u64 == offset)
            .ok_or(libc::EINVAL)?;
        let rw = (flags & VIRTIO_MEDIA_MMAP_FLAG_RW) != 0;

        // TODO: not great...
        let fd = unsafe { BorrowedFd::borrow_raw(buffer.shm.descriptor.as_raw_descriptor()) };

        self.host_mapper
            .create_mapping(offset, fd, rw)
            .map_err(|e| {
                base::error!(
                    "failed to map MMAP buffer at offset 0x{:x}: {:#}",
                    offset,
                    e
                );
                libc::EINVAL
            })
    }

    fn do_munmap(&mut self, guest_addr: u64) -> Result<(), i32> {
        self.host_mapper
            .remove_mapping(guest_addr)
            .map(|_| ())
            .map_err(|_| libc::EINVAL)
    }
}

fn virtio_video_format_to_fourcc(format: Format) -> &'static [u8; 4] {
    match format {
        Format::NV12 => b"NV12",
        Format::YUV420 => b"YV12",
        Format::H264 => b"H264",
        Format::Hevc => b"HEVC",
        Format::VP8 => b"VP80",
        Format::VP9 => b"VP90",
    }
}

fn fourcc_to_virtio_video_format(fourcc: &[u8; 4]) -> Option<Format> {
    match fourcc {
        b"NV12" => Some(Format::NV12),
        b"YV12" => Some(Format::YUV420),
        b"H264" => Some(Format::H264),
        b"HEVC" => Some(Format::Hevc),
        b"VP80" => Some(Format::VP8),
        b"VP90" => Some(Format::VP9),
        _ => None,
    }
}

impl<D, Q, HM> VirtioMediaIoctlHandler for VirtioMediaDecoderAdapter<D, Q, HM>
where
    D: DecoderBackend,
    Q: VirtioMediaEventQueue,
    HM: VirtioMediaHostMemoryMapper,
{
    type Session = VirtioMediaAdapterSession<D>;

    fn enum_fmt(
        &mut self,
        session: &mut Self::Session,
        queue: QueueType,
        index: u32,
    ) -> IoctlResult<bindings::v4l2_fmtdesc> {
        let formats = match queue {
            QueueType::VideoCaptureMplane => self.capability.output_formats(),
            QueueType::VideoOutputMplane => self.capability.input_formats(),
            _ => return Err(libc::EINVAL),
        };
        let fmt = formats.get(index as usize).ok_or(libc::EINVAL)?;

        Ok(bindings::v4l2_fmtdesc {
            index,
            type_: queue as u32,
            pixelformat: PixelFormat::from_fourcc(virtio_video_format_to_fourcc(fmt.format))
                .to_u32(),
            ..Default::default()
        })
    }

    fn enum_framesizes(
        &mut self,
        session: &mut Self::Session,
        index: u32,
        pixel_format: u32,
    ) -> IoctlResult<bindings::v4l2_frmsizeenum> {
        let format =
            fourcc_to_virtio_video_format(&PixelFormat::from_u32(pixel_format).to_fourcc())
                .ok_or(libc::EINVAL)?;
        // We only support step-wise frame sizes.
        if index != 0 {
            return Err(libc::EINVAL);
        }
        let frame_sizes = self
            .capability
            .input_formats()
            .iter()
            .chain(self.capability.output_formats().iter())
            .find(|f| f.format == format)
            .and_then(|f| f.frame_formats.first())
            .ok_or(libc::EINVAL)?;

        Ok(bindings::v4l2_frmsizeenum {
            index: 0,
            pixel_format,
            type_: bindings::v4l2_frmsizetypes_V4L2_FRMSIZE_TYPE_STEPWISE,
            __bindgen_anon_1: bindings::v4l2_frmsizeenum__bindgen_ty_1 {
                stepwise: bindings::v4l2_frmsize_stepwise {
                    min_width: frame_sizes.width.min,
                    max_width: frame_sizes.width.max,
                    step_width: frame_sizes.width.step,
                    min_height: frame_sizes.height.min,
                    max_height: frame_sizes.height.max,
                    step_height: frame_sizes.height.step,
                },
            },
            ..Default::default()
        })
    }

    fn g_fmt(
        &mut self,
        session: &mut Self::Session,
        queue: QueueType,
    ) -> IoctlResult<bindings::v4l2_format> {
        let format = match queue {
            QueueType::VideoOutputMplane => session.input_format,
            QueueType::VideoCaptureMplane => session.output_format,
            _ => return Err(libc::EINVAL),
        };

        Ok(session.v4l2_format(queue.direction()))
    }

    fn try_fmt(
        &mut self,
        session: &mut Self::Session,
        queue: QueueType,
        format: bindings::v4l2_format,
    ) -> IoctlResult<bindings::v4l2_format> {
        self.try_or_set_format(session, queue, format, false)
    }

    fn s_fmt(
        &mut self,
        session: &mut Self::Session,
        queue: QueueType,
        format: bindings::v4l2_format,
    ) -> IoctlResult<bindings::v4l2_format> {
        self.try_or_set_format(session, queue, format, true)
    }

    fn reqbufs(
        &mut self,
        session: &mut Self::Session,
        queue: QueueType,
        memory: MemoryType,
        mut count: u32,
    ) -> IoctlResult<bindings::v4l2_requestbuffers> {
        if memory != MemoryType::Mmap {
            return Err(libc::EINVAL);
        }
        // TODO: fail if streaming?

        let (buffers, format) = match queue {
            QueueType::VideoOutputMplane => (&mut session.input_buffers, session.input_format),
            QueueType::VideoCaptureMplane => (&mut session.output_buffers, session.output_format),
            _ => return Err(libc::EINVAL),
        };

        count = count.max(session.min_output_buffers);

        if (count as usize) < buffers.len() {
            for buffer in &buffers[count as usize..] {
                self.host_mapper
                    .unregister_buffer(buffer.mmap_offset as u64);
            }
            buffers.truncate(count as usize);
        } else {
            let (_, sizeimage) = buffer_sizes_for_format(format, session.coded_size);
            let new_buffers = (buffers.len()..count as usize)
                .into_iter()
                .map(|i| {
                    let mmap_offset = self
                        .host_mapper
                        .register_buffer(None, sizeimage as u64)
                        .map_err(|_| libc::EINVAL)?;

                    VirtioMediaAdapterBuffer::new(
                        queue,
                        i as u32,
                        sizeimage as usize,
                        mmap_offset as u32,
                    )
                    .map_err(|e| {
                        // TODO: no, we need to unregister all the buffers and restore the
                        // previous state?
                        self.host_mapper.unregister_buffer(mmap_offset);
                        e
                    })
                })
                .collect::<IoctlResult<Vec<_>>>()?;
            buffers.extend(new_buffers);
        }

        Ok(bindings::v4l2_requestbuffers {
            count,
            type_: queue as u32,
            memory: memory as u32,
            capabilities: (BufferCapabilities::SUPPORTS_MMAP
                | BufferCapabilities::SUPPORTS_ORPHANED_BUFS)
                .bits(),
            flags: 0,
            reserved: Default::default(),
        })
    }

    fn querybuf(
        &mut self,
        session: &mut Self::Session,
        queue: QueueType,
        index: u32,
    ) -> IoctlResult<V4l2Buffer> {
        let buffers = match queue {
            QueueType::VideoOutputMplane => &session.input_buffers,
            QueueType::VideoCaptureMplane => &session.output_buffers,
            _ => return Err(libc::EINVAL),
        };
        let buffer = buffers.get(index as usize).ok_or(libc::EINVAL)?;

        Ok(buffer.v4l2_buffer.clone())
    }

    fn subscribe_event(
        &mut self,
        session: &mut Self::Session,
        event: v4l2r::ioctl::EventType,
        flags: v4l2r::ioctl::SubscribeEventFlags,
    ) -> IoctlResult<()> {
        match event {
            EventType::SourceChange(0) => {
                session.src_change_subscribed = true;
                Ok(())
            }
            EventType::Eos => {
                session.eos_subscribed = true;
                Ok(())
            }
            _ => Err(libc::EINVAL),
        }
    }

    // TODO: parse the event and use an enum value to signal ALL or single event?
    fn unsubscribe_event(
        &mut self,
        session: &mut Self::Session,
        event: bindings::v4l2_event_subscription,
    ) -> IoctlResult<()> {
        let mut valid = false;

        if event.type_ == 0 || matches!(EventType::try_from(&event), Ok(EventType::SourceChange(0)))
        {
            session.src_change_subscribed = false;
            valid = true;
        }
        if event.type_ == 0 || matches!(EventType::try_from(&event), Ok(EventType::Eos)) {
            session.eos_subscribed = false;
            valid = true;
        }

        if valid {
            Ok(())
        } else {
            Err(libc::EINVAL)
        }
    }

    fn streamon(&mut self, session: &mut Self::Session, queue: QueueType) -> IoctlResult<()> {
        let buffers = match queue {
            QueueType::VideoOutputMplane => &session.input_buffers,
            QueueType::VideoCaptureMplane => &session.output_buffers,
            _ => return Err(libc::EINVAL),
        };

        // Cannot stream if no buffers allocated.
        if buffers.is_empty() {
            return Err(libc::EINVAL);
        }

        match queue.direction() {
            QueueDirection::Output => session.input_streaming = true,
            QueueDirection::Capture => session.output_streaming = true,
        }

        // TODO: start queueing pending buffers?

        Ok(())
    }

    fn g_selection(
        &mut self,
        session: &mut Self::Session,
        sel_type: v4l2r::ioctl::SelectionType,
        sel_target: v4l2r::ioctl::SelectionTarget,
    ) -> IoctlResult<bindings::v4l2_rect> {
        Ok(session.visible_rect.into())
    }

    fn s_selection(
        &mut self,
        session: &mut Self::Session,
        sel_type: v4l2r::ioctl::SelectionType,
        sel_target: v4l2r::ioctl::SelectionTarget,
        sel_rect: bindings::v4l2_rect,
        sel_flags: v4l2r::ioctl::SelectionFlags,
    ) -> IoctlResult<bindings::v4l2_rect> {
        self.g_selection(session, sel_type, sel_target)
    }

    fn streamoff(&mut self, session: &mut Self::Session, queue: QueueType) -> IoctlResult<()> {
        let buffers = match queue {
            QueueType::VideoOutputMplane => &session.input_buffers,
            QueueType::VideoCaptureMplane => &session.output_buffers,
            _ => return Err(libc::EINVAL),
        };

        // TODO: unqueue all buffers.

        match queue.direction() {
            QueueDirection::Output => {
                // TODO: something to do on the backend?
                // TODO: remove queued flags from all input buffers?
                session.input_streaming = false
            }
            QueueDirection::Capture => {
                if let Some(session) = &mut session.session {
                    session.clear_output_buffers().unwrap();
                }
                // TODO: remove queued flags from all output buffers?
                session.eos_capture_buffer = EosBuffer::None;
                session.output_streaming = false;
            }
        }

        Ok(())
    }

    fn qbuf(
        &mut self,
        session: &mut Self::Session,
        buffer: V4l2Buffer,
        guest_regions: Vec<Vec<virtio_media::protocol::SgEntry>>,
    ) -> IoctlResult<V4l2Buffer> {
        let buffers = match buffer.queue() {
            QueueType::VideoOutputMplane => &mut session.input_buffers,
            QueueType::VideoCaptureMplane => &mut session.output_buffers,
            _ => return Err(libc::EINVAL),
        };
        let num_buffers = buffers.len();
        let host_buffer = buffers
            .get_mut(buffer.index() as usize)
            .ok_or(libc::EINVAL)?;
        let backend_session = match &mut session.session {
            Some(session) => session,
            None => {
                let backend_session =
                    self.decoder
                        .new_session(session.input_format)
                        .map_err(|e| {
                            base::error!(
                                "{:#}",
                                anyhow::anyhow!("while creating backend session: {:#}", e)
                            );
                            libc::EIO
                        })?;
                self.wait_ctx
                    .add(backend_session.event_pipe(), Token::V4l2Session(session.id))
                    .map_err(|e| {
                        base::error!(
                            "failed to listen to events of session {}: {:#}",
                            session.id,
                            e
                        );
                        libc::EIO
                    })?;
                session.session.get_or_insert(backend_session)
            }
        };

        let timestamp = buffer.timestamp();
        let timestamp = timestamp.tv_sec as u64 * 1_000_000 + timestamp.tv_usec as u64;
        let first_plane = buffer.get_first_plane();

        match buffer.queue().direction() {
            QueueDirection::Output => {
                let resource = GuestResourceHandle::GuestPages(GuestMemHandle {
                    desc: host_buffer
                        .shm
                        .descriptor
                        .try_clone()
                        .map_err(|_| libc::ENOMEM)?,
                    mem_areas: vec![GuestMemArea {
                        offset: 0,
                        length: host_buffer.shm.size as usize,
                    }],
                });

                // Send buffer to backend
                let bytes_used = if *first_plane.bytesused == 0 {
                    *first_plane.length
                } else {
                    *first_plane.bytesused
                };

                backend_session
                    .decode(buffer.index(), timestamp, resource, 0, bytes_used)
                    .map_err(|_| libc::EIO)?;

                // Update buffer state
                let host_buffer = &mut host_buffer.v4l2_buffer;
                host_buffer.set_field(buffer.field());
                host_buffer.set_timestamp(buffer.timestamp());
                *host_buffer.get_first_plane_mut().bytesused = *buffer.get_first_plane().bytesused;
                host_buffer.add_flags(BufferFlags::QUEUED);
                let host_first_plane = host_buffer.get_first_plane_mut();
                *host_first_plane.length = *first_plane.length;
                *host_first_plane.bytesused = *first_plane.bytesused;
                if let Some(data_offset) = host_first_plane.data_offset {
                    *data_offset = first_plane.data_offset.map(|o| *o).unwrap_or(0);
                }
            }
            QueueDirection::Capture => {
                // Set the output parameters if this is the first CAPTURE buffer we queue after a
                // resolution change event.
                if session.need_set_output_params {
                    backend_session
                        .set_output_parameters(num_buffers, session.output_format)
                        .map_err(|_| libc::EIO)
                        .unwrap();
                    session.need_set_output_params = false;
                }

                let plane_formats = PlaneFormat::get_plane_layout(
                    session.output_format,
                    session.visible_rect.width,
                    session.visible_rect.height,
                )
                .ok_or_else(|| {
                    base::error!("could not obtain plane layout for output buffer");
                    libc::EINVAL
                })?;

                if let EosBuffer::None = &session.eos_capture_buffer {
                    session.eos_capture_buffer = EosBuffer::Available(buffer.index());
                } else if !host_buffer.registered {
                    let resource = GuestResourceHandle::GuestPages(GuestMemHandle {
                        desc: host_buffer
                            .shm
                            .descriptor
                            .try_clone()
                            .map_err(|_| libc::ENOMEM)?,
                        mem_areas: vec![GuestMemArea {
                            offset: 0,
                            length: host_buffer.shm.size as usize,
                        }],
                    });

                    let mut buffer_offset = 0;
                    let resource_handle = GuestResource {
                        handle: resource,
                        planes: plane_formats
                            .into_iter()
                            .map(|p| {
                                let plane_offset = buffer_offset;
                                buffer_offset += p.plane_size;

                                FramePlane {
                                    offset: plane_offset as usize,
                                    stride: p.stride as usize,
                                    size: p.plane_size as usize,
                                }
                            })
                            .collect(),
                        width: session.visible_rect.width,
                        height: session.visible_rect.height,
                        format: session.output_format,
                        guest_cpu_mappable: false,
                    };

                    backend_session
                        .use_output_buffer(buffer.index() as i32, resource_handle)
                        // TODO: display error, map and bail?
                        //.map_err(|_| libc::EIO)
                        .unwrap();

                    // TODO: buffer should be re-registered if it has a new memory backing...
                    host_buffer.registered = true;
                } else {
                    backend_session
                        .reuse_output_buffer(buffer.index() as i32)
                        // TODO: display error, map and bail?
                        //.map_err(|_| libc::EIO)
                        .unwrap();
                }

                // Update buffer state
                let host_buffer = &mut host_buffer.v4l2_buffer;
                host_buffer.add_flags(BufferFlags::QUEUED);
                let host_first_plane = host_buffer.get_first_plane_mut();
                *host_first_plane.length = *first_plane.length;
                *host_first_plane.bytesused = *first_plane.bytesused;
                if let Some(data_offset) = host_first_plane.data_offset {
                    *data_offset = first_plane.data_offset.map(|o| *o).unwrap_or(0);
                }
            }
        }

        Ok(host_buffer.v4l2_buffer.clone())
    }

    fn try_decoder_cmd(
        &mut self,
        session: &mut Self::Session,
        cmd: bindings::v4l2_decoder_cmd,
    ) -> IoctlResult<bindings::v4l2_decoder_cmd> {
        Err(libc::ENOTTY)
    }

    fn decoder_cmd(
        &mut self,
        session: &mut Self::Session,
        cmd: bindings::v4l2_decoder_cmd,
    ) -> IoctlResult<bindings::v4l2_decoder_cmd> {
        let cmd = DecoderCmd::try_from(cmd).map_err(|_| libc::EINVAL)?;

        match cmd {
            DecoderCmd::Stop { .. } => {
                if let Some(backend_session) = &mut session.session {
                    backend_session.flush().map_err(|_| libc::EIO)?;
                }
                Ok(DecoderCmd::stop().into())
            }
            _ => Err(libc::EINVAL),
        }
    }
}
