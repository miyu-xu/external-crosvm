// Copyright 2026 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! macOS CoreAudio backend for virtio-snd.
//!
//! AudioQueue owns the platform device callbacks. The virtio worker remains asynchronous: callbacks
//! notify the executor through a nonblocking Unix socket while bounded queues retain the data. No
//! CoreAudio call blocks the executor thread. Capture is only constructed when the VM exposes an
//! input PCM device; HD keeps that count at zero until the user explicitly enables Host microphone
//! routing for the instance.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::io;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::ptr;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use audio_streams::capture::AsyncCaptureBuffer;
use audio_streams::capture::AsyncCaptureBufferStream;
use audio_streams::AsyncBufferCommit;
use audio_streams::AsyncPlaybackBuffer;
use audio_streams::AsyncPlaybackBufferStream;
use audio_streams::AudioStreamsExecutor;
use audio_streams::BoxError;
use audio_streams::NoopStreamControl;
use audio_streams::SampleFormat;
use audio_streams::StreamControl;
use audio_streams::StreamEffect;
use audio_streams::StreamSource;
use audio_streams::StreamSourceGenerator;
use cros_async::Executor;
use futures::channel::mpsc::UnboundedSender;
use serde::Deserialize;
use serde::Serialize;

use crate::virtio::snd::common_backend::async_funcs::CaptureBufferReader;
use crate::virtio::snd::common_backend::async_funcs::PlaybackBufferWriter;
use crate::virtio::snd::common_backend::stream_info::StreamInfo;
use crate::virtio::snd::common_backend::DirectionalStream;
use crate::virtio::snd::common_backend::Error;
use crate::virtio::snd::common_backend::PcmResponse;
use crate::virtio::snd::common_backend::SndData;
use crate::virtio::snd::parameters::Error as ParametersError;
use crate::virtio::snd::parameters::Parameters;

const AUDIO_QUEUE_BUFFER_COUNT: usize = 3;
const K_AUDIO_FORMAT_LINEAR_PCM: u32 = u32::from_be_bytes(*b"lpcm");
const K_AUDIO_FORMAT_FLAG_IS_SIGNED_INTEGER: u32 = 1 << 2;
const K_AUDIO_FORMAT_FLAG_IS_PACKED: u32 = 1 << 3;

type OSStatus = i32;
type AudioQueueRef = *mut c_void;
type AudioQueueBufferRef = *mut AudioQueueBuffer;

#[repr(C)]
struct AudioStreamBasicDescription {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
struct AudioQueueBuffer {
    audio_data_bytes_capacity: u32,
    audio_data: *mut c_void,
    audio_data_byte_size: u32,
    user_data: *mut c_void,
    packet_description_capacity: u32,
    packet_descriptions: *const c_void,
    packet_description_count: u32,
}

type AudioQueueOutputCallback =
    unsafe extern "C" fn(*mut c_void, AudioQueueRef, AudioQueueBufferRef);
type AudioQueueInputCallback = unsafe extern "C" fn(
    *mut c_void,
    AudioQueueRef,
    AudioQueueBufferRef,
    *const c_void,
    u32,
    *const c_void,
);

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioQueueNewOutput(
        format: *const AudioStreamBasicDescription,
        callback: AudioQueueOutputCallback,
        user_data: *mut c_void,
        callback_run_loop: *const c_void,
        callback_run_loop_mode: *const c_void,
        flags: u32,
        queue: *mut AudioQueueRef,
    ) -> OSStatus;
    fn AudioQueueNewInput(
        format: *const AudioStreamBasicDescription,
        callback: AudioQueueInputCallback,
        user_data: *mut c_void,
        callback_run_loop: *const c_void,
        callback_run_loop_mode: *const c_void,
        flags: u32,
        queue: *mut AudioQueueRef,
    ) -> OSStatus;
    fn AudioQueueAllocateBuffer(
        queue: AudioQueueRef,
        capacity: u32,
        buffer: *mut AudioQueueBufferRef,
    ) -> OSStatus;
    fn AudioQueueEnqueueBuffer(
        queue: AudioQueueRef,
        buffer: AudioQueueBufferRef,
        packet_description_count: u32,
        packet_descriptions: *const c_void,
    ) -> OSStatus;
    fn AudioQueueStart(queue: AudioQueueRef, start_time: *const c_void) -> OSStatus;
    fn AudioQueueStop(queue: AudioQueueRef, immediate: u8) -> OSStatus;
    fn AudioQueueDispose(queue: AudioQueueRef, immediate: u8) -> OSStatus;
}

fn audio_error(operation: &str, status: OSStatus) -> BoxError {
    Box::new(io::Error::other(format!(
        "CoreAudio {operation} failed with OSStatus {status}"
    )))
}

fn checked_buffer_bytes(
    channels: usize,
    format: SampleFormat,
    frames: usize,
) -> Result<(usize, u32), BoxError> {
    let frame_size = channels
        .checked_mul(format.sample_bytes())
        .ok_or_else(|| io::Error::other("CoreAudio frame size overflow"))?;
    let bytes = frame_size
        .checked_mul(frames)
        .ok_or_else(|| io::Error::other("CoreAudio buffer size overflow"))?;
    Ok((frame_size, u32::try_from(bytes)?))
}

struct CallbackNotifier {
    stream: UnixStream,
}

impl CallbackNotifier {
    fn signal(&self) {
        let byte = 1u8;
        // SAFETY: the stream remains open for the lifetime of the callback state. A full socket is
        // intentionally ignored because the protected queue state remains the source of truth.
        unsafe {
            libc::write(
                self.stream.as_raw_fd(),
                ptr::from_ref(&byte).cast::<c_void>(),
                1,
            );
        }
    }
}

struct AsyncNotification {
    stream: UnixStream,
}

impl AsyncNotification {
    async fn wait(&self, ex: &dyn AudioStreamsExecutor) -> Result<(), BoxError> {
        ex.wait_fd_readable(self.stream.as_raw_fd()).await?;
        let mut bytes = [0u8; 64];
        let mut stream = &self.stream;
        loop {
            match stream.read(&mut bytes) {
                Ok(0) => {
                    return Err(Box::new(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "CoreAudio callback notification closed",
                    )));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(Box::new(error)),
            }
        }
    }
}

fn notification_pair() -> Result<(AsyncNotification, CallbackNotifier), BoxError> {
    let (reader, writer) = UnixStream::pair()?;
    reader.set_nonblocking(true)?;
    writer.set_nonblocking(true)?;
    Ok((
        AsyncNotification { stream: reader },
        CallbackNotifier { stream: writer },
    ))
}

fn audio_format(
    channels: usize,
    format: SampleFormat,
    frame_rate: u32,
) -> Result<AudioStreamBasicDescription, BoxError> {
    let channels = u32::try_from(channels)?;
    let sample_bytes = u32::try_from(format.sample_bytes())?;
    let (bits_per_channel, packed) = match format {
        SampleFormat::U8 => (8, true),
        SampleFormat::S16LE => (16, true),
        // audio_streams carries signed 24-bit samples in a 32-bit container.
        SampleFormat::S24LE => (24, false),
        SampleFormat::S32LE => (32, true),
    };
    let bytes_per_frame = channels
        .checked_mul(sample_bytes)
        .ok_or_else(|| io::Error::other("CoreAudio bytes-per-frame overflow"))?;
    let signed = !matches!(format, SampleFormat::U8);
    Ok(AudioStreamBasicDescription {
        sample_rate: f64::from(frame_rate),
        format_id: K_AUDIO_FORMAT_LINEAR_PCM,
        format_flags: if packed {
            K_AUDIO_FORMAT_FLAG_IS_PACKED
        } else {
            0
        } | if signed {
            K_AUDIO_FORMAT_FLAG_IS_SIGNED_INTEGER
        } else {
            0
        },
        bytes_per_packet: bytes_per_frame,
        frames_per_packet: 1,
        bytes_per_frame,
        channels_per_frame: channels,
        bits_per_channel,
        reserved: 0,
    })
}

struct PlaybackCallbackState {
    free_buffers: Mutex<Vec<usize>>,
    notifier: CallbackNotifier,
}

unsafe extern "C" fn playback_callback(
    user_data: *mut c_void,
    _queue: AudioQueueRef,
    buffer: AudioQueueBufferRef,
) {
    if user_data.is_null() || buffer.is_null() {
        return;
    }
    // SAFETY: `user_data` points to the callback state owned by `PlaybackQueue`. AudioQueueDispose
    // completes active callbacks before the state is freed.
    let state = unsafe { &*(user_data.cast::<PlaybackCallbackState>()) };
    if let Ok(mut free) = state.free_buffers.lock() {
        free.push(buffer as usize);
    }
    state.notifier.signal();
}

struct PlaybackQueue {
    queue: AudioQueueRef,
    state: *mut PlaybackCallbackState,
    notification: AsyncNotification,
    last_error: AtomicI32,
}

// AudioQueue serializes its callbacks; access to Rust state is protected by a Mutex.
unsafe impl Send for PlaybackQueue {}
unsafe impl Sync for PlaybackQueue {}

impl PlaybackQueue {
    fn new(format: &AudioStreamBasicDescription, buffer_bytes: u32) -> Result<Arc<Self>, BoxError> {
        let (notification, notifier) = notification_pair()?;
        let state = Box::into_raw(Box::new(PlaybackCallbackState {
            free_buffers: Mutex::new(Vec::with_capacity(AUDIO_QUEUE_BUFFER_COUNT)),
            notifier,
        }));
        let mut queue = ptr::null_mut();
        // SAFETY: the ASBD and output pointer are valid for the duration of the call; callback
        // state remains allocated until the queue is synchronously disposed.
        let status = unsafe {
            AudioQueueNewOutput(
                format,
                playback_callback,
                state.cast(),
                ptr::null(),
                ptr::null(),
                0,
                &mut queue,
            )
        };
        if status != 0 {
            // SAFETY: AudioQueueNewOutput did not take ownership after failure.
            drop(unsafe { Box::from_raw(state) });
            return Err(audio_error("new output", status));
        }
        for _ in 0..AUDIO_QUEUE_BUFFER_COUNT {
            let mut buffer = ptr::null_mut();
            // SAFETY: queue is a valid AudioQueue and buffer is an out pointer.
            let status = unsafe { AudioQueueAllocateBuffer(queue, buffer_bytes, &mut buffer) };
            if status != 0 {
                // SAFETY: immediate disposal synchronizes callbacks and releases allocated buffers.
                unsafe { AudioQueueDispose(queue, 1) };
                drop(unsafe { Box::from_raw(state) });
                return Err(audio_error("allocate output buffer", status));
            }
            // SAFETY: state is live and exclusively initialized before queue start.
            unsafe { &*state }
                .free_buffers
                .lock()
                .map_err(|_| io::Error::other("CoreAudio playback queue lock poisoned"))?
                .push(buffer as usize);
        }
        // SAFETY: queue is valid. Starting without queued buffers is supported; playback begins on
        // the first enqueue.
        let status = unsafe { AudioQueueStart(queue, ptr::null()) };
        if status != 0 {
            unsafe { AudioQueueDispose(queue, 1) };
            drop(unsafe { Box::from_raw(state) });
            return Err(audio_error("start output", status));
        }
        Ok(Arc::new(Self {
            queue,
            state,
            notification,
            last_error: AtomicI32::new(0),
        }))
    }

    fn take_buffer(&self) -> Result<Option<AudioQueueBufferRef>, BoxError> {
        let status = self.last_error.load(Ordering::Acquire);
        if status != 0 {
            return Err(audio_error("enqueue output buffer", status));
        }
        let mut free = unsafe { &*self.state }
            .free_buffers
            .lock()
            .map_err(|_| io::Error::other("CoreAudio playback queue lock poisoned"))?;
        Ok(free.pop().map(|buffer| buffer as AudioQueueBufferRef))
    }

    fn return_buffer(&self, buffer: AudioQueueBufferRef) {
        if let Ok(mut free) = unsafe { &*self.state }.free_buffers.lock() {
            free.push(buffer as usize);
        }
    }

    async fn wait_for_buffer(&self, ex: &dyn AudioStreamsExecutor) -> Result<(), BoxError> {
        self.notification.wait(ex).await
    }

    fn enqueue(&self, buffer: AudioQueueBufferRef, data: &[u8]) {
        if buffer.is_null() {
            self.last_error.store(-1, Ordering::Release);
            return;
        }
        // SAFETY: the buffer belongs to this queue and was reserved until this commit.
        let capacity = unsafe { (*buffer).audio_data_bytes_capacity as usize };
        if data.len() > capacity {
            self.last_error.store(-1, Ordering::Release);
            self.return_buffer(buffer);
            return;
        }
        unsafe {
            ptr::copy_nonoverlapping(data.as_ptr(), (*buffer).audio_data.cast::<u8>(), data.len());
            (*buffer).audio_data_byte_size = data.len() as u32;
        }
        // SAFETY: the buffer and queue are valid and linear PCM needs no packet descriptions.
        let status = unsafe { AudioQueueEnqueueBuffer(self.queue, buffer, 0, ptr::null()) };
        if status != 0 {
            self.last_error.store(status, Ordering::Release);
            self.return_buffer(buffer);
        }
    }
}

impl Drop for PlaybackQueue {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner after all stream Arcs are gone. Immediate stop/dispose
        // synchronizes callbacks before callback state is reclaimed.
        unsafe {
            AudioQueueStop(self.queue, 1);
            AudioQueueDispose(self.queue, 1);
            drop(Box::from_raw(self.state));
        }
    }
}

struct PlaybackCommit {
    queue: Arc<PlaybackQueue>,
    reserved: Option<AudioQueueBufferRef>,
    staging: *const Vec<u8>,
    frame_size: usize,
}

// The pointer always targets the staging Vec in the parent stream and is used only while the
// borrowed AsyncPlaybackBuffer keeps that stream exclusively borrowed.
unsafe impl Send for PlaybackCommit {}

#[async_trait(?Send)]
impl AsyncBufferCommit for PlaybackCommit {
    async fn commit(&mut self, frames: usize) {
        let Some(buffer) = self.reserved.take() else {
            return;
        };
        // SAFETY: see the Send invariant above; commit runs before the borrowed buffer is released.
        let staging = unsafe { &*self.staging };
        let bytes = staging.len().min(frames.saturating_mul(self.frame_size));
        self.queue.enqueue(buffer, &staging[..bytes]);
    }
}

struct CoreAudioPlaybackStream {
    queue: Arc<PlaybackQueue>,
    staging: Vec<u8>,
    frame_size: usize,
    commit: PlaybackCommit,
}

#[async_trait(?Send)]
impl AsyncPlaybackBufferStream for CoreAudioPlaybackStream {
    async fn next_playback_buffer<'a>(
        &'a mut self,
        ex: &dyn AudioStreamsExecutor,
    ) -> Result<AsyncPlaybackBuffer<'a>, BoxError> {
        loop {
            if let Some(buffer) = self.queue.take_buffer()? {
                self.staging.fill(0);
                self.commit.reserved = Some(buffer);
                self.commit.staging = &self.staging;
                return Ok(AsyncPlaybackBuffer::new(
                    self.frame_size,
                    &mut self.staging,
                    &mut self.commit,
                )?);
            }
            self.queue.wait_for_buffer(ex).await?;
        }
    }
}

struct CaptureCallbackState {
    data: Mutex<VecDeque<u8>>,
    max_bytes: usize,
    last_error: AtomicI32,
    notifier: CallbackNotifier,
}

unsafe extern "C" fn capture_callback(
    user_data: *mut c_void,
    queue: AudioQueueRef,
    buffer: AudioQueueBufferRef,
    _start_time: *const c_void,
    _packets: u32,
    _packet_descriptions: *const c_void,
) {
    if user_data.is_null() || queue.is_null() || buffer.is_null() {
        return;
    }
    // SAFETY: callback state remains valid until synchronous AudioQueueDispose completes.
    let state = unsafe { &*(user_data.cast::<CaptureCallbackState>()) };
    let size = unsafe { (*buffer).audio_data_byte_size as usize };
    let bytes = unsafe { std::slice::from_raw_parts((*buffer).audio_data.cast::<u8>(), size) };
    if let Ok(mut data) = state.data.lock() {
        let excess = data
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(state.max_bytes);
        if excess > 0 {
            let drain_len = excess.min(data.len());
            data.drain(..drain_len);
        }
        data.extend(bytes);
    }
    state.notifier.signal();
    unsafe { (*buffer).audio_data_byte_size = 0 };
    // SAFETY: the callback owns the input buffer until it is re-enqueued.
    let status = unsafe { AudioQueueEnqueueBuffer(queue, buffer, 0, ptr::null()) };
    if status != 0 {
        state.last_error.store(status, Ordering::Release);
    }
}

struct CaptureQueue {
    queue: AudioQueueRef,
    state: *mut CaptureCallbackState,
    notification: AsyncNotification,
}

unsafe impl Send for CaptureQueue {}
unsafe impl Sync for CaptureQueue {}

impl CaptureQueue {
    fn new(format: &AudioStreamBasicDescription, buffer_bytes: u32) -> Result<Arc<Self>, BoxError> {
        let (notification, notifier) = notification_pair()?;
        let max_bytes = usize::try_from(buffer_bytes)?.saturating_mul(8);
        let state = Box::into_raw(Box::new(CaptureCallbackState {
            data: Mutex::new(VecDeque::with_capacity(max_bytes)),
            max_bytes,
            last_error: AtomicI32::new(0),
            notifier,
        }));
        let mut queue = ptr::null_mut();
        let status = unsafe {
            AudioQueueNewInput(
                format,
                capture_callback,
                state.cast(),
                ptr::null(),
                ptr::null(),
                0,
                &mut queue,
            )
        };
        if status != 0 {
            drop(unsafe { Box::from_raw(state) });
            return Err(audio_error("new input", status));
        }
        for _ in 0..AUDIO_QUEUE_BUFFER_COUNT {
            let mut buffer = ptr::null_mut();
            let status = unsafe { AudioQueueAllocateBuffer(queue, buffer_bytes, &mut buffer) };
            if status == 0 {
                unsafe { (*buffer).audio_data_byte_size = 0 };
                let enqueue_status =
                    unsafe { AudioQueueEnqueueBuffer(queue, buffer, 0, ptr::null()) };
                if enqueue_status != 0 {
                    unsafe { AudioQueueDispose(queue, 1) };
                    drop(unsafe { Box::from_raw(state) });
                    return Err(audio_error("enqueue input buffer", enqueue_status));
                }
            } else {
                unsafe { AudioQueueDispose(queue, 1) };
                drop(unsafe { Box::from_raw(state) });
                return Err(audio_error("allocate input buffer", status));
            }
        }
        let status = unsafe { AudioQueueStart(queue, ptr::null()) };
        if status != 0 {
            unsafe { AudioQueueDispose(queue, 1) };
            drop(unsafe { Box::from_raw(state) });
            return Err(audio_error("start input", status));
        }
        Ok(Arc::new(Self {
            queue,
            state,
            notification,
        }))
    }

    fn read_exact(&self, target: &mut [u8]) -> Result<bool, BoxError> {
        let state = unsafe { &*self.state };
        let status = state.last_error.load(Ordering::Acquire);
        if status != 0 {
            return Err(audio_error("capture callback", status));
        }
        let mut data = state
            .data
            .lock()
            .map_err(|_| io::Error::other("CoreAudio capture queue lock poisoned"))?;
        if data.len() < target.len() {
            return Ok(false);
        }
        for byte in target {
            *byte = data.pop_front().unwrap_or_default();
        }
        Ok(true)
    }

    async fn wait_for_data(&self, ex: &dyn AudioStreamsExecutor) -> Result<(), BoxError> {
        self.notification.wait(ex).await
    }
}

impl Drop for CaptureQueue {
    fn drop(&mut self) {
        unsafe {
            AudioQueueStop(self.queue, 1);
            AudioQueueDispose(self.queue, 1);
            drop(Box::from_raw(self.state));
        }
    }
}

#[derive(Default)]
struct CaptureCommit;

#[async_trait(?Send)]
impl AsyncBufferCommit for CaptureCommit {
    async fn commit(&mut self, _frames: usize) {}
}

struct CoreAudioCaptureStream {
    queue: Arc<CaptureQueue>,
    staging: Vec<u8>,
    frame_size: usize,
    commit: CaptureCommit,
}

#[async_trait(?Send)]
impl AsyncCaptureBufferStream for CoreAudioCaptureStream {
    async fn next_capture_buffer<'a>(
        &'a mut self,
        ex: &dyn AudioStreamsExecutor,
    ) -> Result<AsyncCaptureBuffer<'a>, BoxError> {
        loop {
            if self.queue.read_exact(&mut self.staging)? {
                return Ok(AsyncCaptureBuffer::new(
                    self.frame_size,
                    &mut self.staging,
                    &mut self.commit,
                )?);
            }
            self.queue.wait_for_data(ex).await?;
        }
    }
}

#[derive(Default)]
struct CoreAudioStreamSource;

#[async_trait(?Send)]
impl StreamSource for CoreAudioStreamSource {
    fn new_playback_stream(
        &mut self,
        _num_channels: usize,
        _format: SampleFormat,
        _frame_rate: u32,
        _buffer_size: usize,
    ) -> Result<
        (
            Box<dyn StreamControl>,
            Box<dyn audio_streams::PlaybackBufferStream>,
        ),
        BoxError,
    > {
        Err(Box::new(io::Error::other(
            "CoreAudio backend requires the asynchronous playback API",
        )))
    }

    fn new_async_playback_stream(
        &mut self,
        num_channels: usize,
        format: SampleFormat,
        frame_rate: u32,
        buffer_size: usize,
        _ex: &dyn AudioStreamsExecutor,
    ) -> Result<(Box<dyn StreamControl>, Box<dyn AsyncPlaybackBufferStream>), BoxError> {
        let (frame_size, buffer_bytes) = checked_buffer_bytes(num_channels, format, buffer_size)?;
        let queue = PlaybackQueue::new(
            &audio_format(num_channels, format, frame_rate)?,
            buffer_bytes,
        )?;
        let staging = vec![0; usize::try_from(buffer_bytes)?];
        let commit = PlaybackCommit {
            queue: Arc::clone(&queue),
            reserved: None,
            staging: ptr::null(),
            frame_size,
        };
        Ok((
            Box::new(NoopStreamControl::new()),
            Box::new(CoreAudioPlaybackStream {
                queue,
                staging,
                frame_size,
                commit,
            }),
        ))
    }

    fn new_async_capture_stream(
        &mut self,
        num_channels: usize,
        format: SampleFormat,
        frame_rate: u32,
        buffer_size: usize,
        _effects: &[StreamEffect],
        _ex: &dyn AudioStreamsExecutor,
    ) -> Result<(Box<dyn StreamControl>, Box<dyn AsyncCaptureBufferStream>), BoxError> {
        let (frame_size, buffer_bytes) = checked_buffer_bytes(num_channels, format, buffer_size)?;
        let queue = CaptureQueue::new(
            &audio_format(num_channels, format, frame_rate)?,
            buffer_bytes,
        )?;
        Ok((
            Box::new(NoopStreamControl::new()),
            Box::new(CoreAudioCaptureStream {
                queue,
                staging: vec![0; usize::try_from(buffer_bytes)?],
                frame_size,
                commit: CaptureCommit,
            }),
        ))
    }
}

struct CoreAudioStreamSourceGenerator;

impl StreamSourceGenerator for CoreAudioStreamSourceGenerator {
    fn generate(&self) -> Result<Box<dyn StreamSource>, BoxError> {
        Ok(Box::<CoreAudioStreamSource>::default())
    }
}

pub(crate) type SysAudioStreamSourceGenerator = Box<dyn StreamSourceGenerator>;
pub(crate) type SysAudioStreamSource = Box<dyn StreamSource>;
pub(crate) type SysBufferReader = MacOsBufferReader;

pub struct SysDirectionOutput {
    pub async_playback_buffer_stream: Box<dyn AsyncPlaybackBufferStream>,
    pub buffer_writer: Box<dyn PlaybackBufferWriter>,
}

pub(crate) struct SysAsyncStreamObjects {
    pub(crate) stream: DirectionalStream,
    pub(crate) pcm_sender: UnboundedSender<PcmResponse>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum StreamSourceBackend {
    COREAUDIO,
}

impl From<StreamSourceBackend> for String {
    fn from(_backend: StreamSourceBackend) -> Self {
        "coreaudio".to_owned()
    }
}

impl TryFrom<&str> for StreamSourceBackend {
    type Error = ParametersError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "coreaudio" => Ok(Self::COREAUDIO),
            _ => Err(ParametersError::InvalidBackend),
        }
    }
}

pub(crate) fn create_stream_source_generators(
    _backend: StreamSourceBackend,
    _params: &Parameters,
    snd_data: &SndData,
) -> Vec<SysAudioStreamSourceGenerator> {
    snd_data
        .pcm_info_iter()
        .map(|_| Box::new(CoreAudioStreamSourceGenerator) as SysAudioStreamSourceGenerator)
        .collect()
}

pub(crate) fn set_audio_thread_priority() -> Result<(), base::Error> {
    Ok(())
}

impl StreamInfo {
    async fn set_up_async_playback_stream(
        &mut self,
        frame_size: usize,
        ex: &Executor,
    ) -> Result<Box<dyn AsyncPlaybackBufferStream>, Error> {
        Ok(self
            .stream_source
            .as_mut()
            .ok_or(Error::EmptyStreamSource)?
            .async_new_async_playback_stream(
                self.channels as usize,
                self.format,
                self.frame_rate,
                self.period_bytes / frame_size,
                ex,
            )
            .await
            .map_err(Error::CreateStream)?
            .1)
    }

    pub(crate) async fn set_up_async_capture_stream(
        &mut self,
        frame_size: usize,
        ex: &Executor,
    ) -> Result<SysBufferReader, Error> {
        let stream = self
            .stream_source
            .as_mut()
            .ok_or(Error::EmptyStreamSource)?
            .async_new_async_capture_stream(
                self.channels as usize,
                self.format,
                self.frame_rate,
                self.period_bytes / frame_size,
                &self.effects,
                ex,
            )
            .await
            .map_err(Error::CreateStream)?
            .1;
        Ok(MacOsBufferReader { stream })
    }

    pub(crate) async fn create_directionstream_output(
        &mut self,
        frame_size: usize,
        ex: &Executor,
    ) -> Result<DirectionalStream, Error> {
        Ok(DirectionalStream::Output(SysDirectionOutput {
            async_playback_buffer_stream: self.set_up_async_playback_stream(frame_size, ex).await?,
            buffer_writer: Box::new(MacOsBufferWriter::new(self.period_bytes)),
        }))
    }
}

pub(crate) struct MacOsBufferReader {
    stream: Box<dyn AsyncCaptureBufferStream>,
}

#[async_trait(?Send)]
impl CaptureBufferReader for MacOsBufferReader {
    async fn get_next_capture_period(
        &mut self,
        ex: &Executor,
    ) -> Result<AsyncCaptureBuffer, BoxError> {
        self.stream.next_capture_buffer(ex).await
    }
}

pub(crate) struct MacOsBufferWriter {
    guest_period_bytes: usize,
}

#[async_trait(?Send)]
impl PlaybackBufferWriter for MacOsBufferWriter {
    fn new(guest_period_bytes: usize) -> Self {
        Self { guest_period_bytes }
    }

    fn endpoint_period_bytes(&self) -> usize {
        self.guest_period_bytes
    }
}
