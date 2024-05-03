// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::os::raw::c_int;
use std::os::raw::c_uint;
use std::ptr::addr_of_mut;
use std::rc::Rc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use audio_streams::AsyncBufferCommit;
use audio_streams::AsyncPlaybackBuffer;
use audio_streams::AsyncPlaybackBufferStream;
use audio_streams::AudioStreamsExecutor;
use audio_streams::BoxError;
use audio_streams::BufferCommit;
use audio_streams::NoopStreamControl;
use audio_streams::PlaybackBuffer;
use audio_streams::PlaybackBufferStream;
use audio_streams::SampleFormat;
use audio_streams::StreamControl;
use audio_streams::StreamSource;
use audio_streams::StreamSourceGenerator;

// Opaque blob
#[repr(C)]
struct AAudioStream {
    _data: [u8; 0],
    _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}

// Opaque blob
#[repr(C)]
struct AAudioStreamBuilder {
    _data: [u8; 0],
    _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}

extern "C" {
    fn AAudio_createStreamBuilder(builder: *mut *mut AAudioStreamBuilder) -> c_int;
    fn AAudioStreamBuilder_delete(builder: *mut AAudioStreamBuilder) -> c_int;
    fn AAudioStreamBuilder_setFormat(builder: *mut AAudioStreamBuilder, format: c_int) -> c_int;
    fn AAudioStreamBuilder_setSampleRate(
        builder: *mut AAudioStreamBuilder,
        sampleRate: c_uint,
    ) -> c_int;
    fn AAudioStreamBuilder_setChannelCount(
        builder: *mut AAudioStreamBuilder,
        channelCount: c_int,
    ) -> c_int;
    fn AAudioStreamBuilder_openStream(
        builder: *mut AAudioStreamBuilder,
        stream: *mut *mut AAudioStream,
    ) -> c_int;
    fn AAudioStream_requestStart(stream: *mut AAudioStream) -> c_int;
    fn AAudioStream_write(
        stream: *mut AAudioStream,
        buffer: *const u8,
        numFrames: c_int,
        timeoutNanos: c_int,
    ) -> c_int;
    fn AAudioStream_close(stream: *mut AAudioStream) -> c_int;
}

struct AndroidAudioStream {
    buffer: Box<[u8]>,
    frame_size: usize,
    interval: Duration,
    next_frame: Duration,
    start_time: Option<Instant>,
    // According to https://developer.android.com/ndk/guides/audio/aaudio/aaudio#thread-safety,
    // the AAudioStream is not thread-safe. A mutex is needed it is used with async functions.
    stream: Rc<Mutex<*mut AAudioStream>>,
    drop: AndroidAudioPlaybackBufferCommit,
}

// SAFETY:
// Mutex<*mut AAudioStream> is thread-safe
unsafe impl Send for AndroidAudioStream {}
// SAFETY:
// Mutex<*mut AAudioStream> is thread-safe
unsafe impl Sync for AndroidAudioStream {}

struct AndroidAudioPlaybackBufferCommit {
    buffer_ptr: *const u8,
    stream: Rc<Mutex<*mut AAudioStream>>,
}

impl BufferCommit for AndroidAudioPlaybackBufferCommit {
    fn commit(&mut self, nwritten: usize) {
        // TODO: Use callback function to avoid possible thread preemption and glitches cause by
        // using mutex with AAudio APIs.
        // SAFETY:
        // The AAudioStream_write reads buffer for nwritten * frame_size bytes
        // It is safe since nwritten < buffer_size and the buffer.len() == buffer_size * frame_size
        unsafe {
            AAudioStream_write(
                *self.stream.lock().unwrap(),
                self.buffer_ptr,
                nwritten as c_int,
                0, // this call will not wait.
            );
        }
    }
}

#[async_trait(?Send)]
impl AsyncBufferCommit for AndroidAudioPlaybackBufferCommit {
    async fn commit(&mut self, nwritten: usize) {
        // TODO: Use callback function to avoid possible thread preemption and glitches cause by
        // using mutex with AAudio APIs.
        // SAFETY:
        // The AAudioStream_write reads buffer for nwritten * frame_size bytes
        // It is safe since nwritten < buffer_size and the buffer.len() == buffer_size * frame_size
        unsafe {
            AAudioStream_write(
                *self.stream.lock().unwrap(),
                self.buffer_ptr,
                nwritten as c_int,
                0, // this call will not wait.
            );
        }
    }
}

impl AndroidAudioStream {
    pub fn new(
        num_channels: usize,
        format: SampleFormat,
        frame_rate: u32,
        buffer_size: usize,
    ) -> Self {
        let frame_size = format.sample_bytes() * num_channels;
        let interval = Duration::from_millis(buffer_size as u64 * 1000 / frame_rate as u64);

        let mut aaudio_stream: *mut AAudioStream = std::ptr::null_mut();
        let mut builder: *mut AAudioStreamBuilder = std::ptr::null_mut();

        // SAFETY:
        // Interfacing with the AAudio C API. Assumes correct linking
        // and `builder` and `stream` pointers are valid and properly initialized.
        unsafe {
            AAudio_createStreamBuilder(&mut builder);
            AAudioStreamBuilder_setFormat(builder, format as c_int);
            AAudioStreamBuilder_setSampleRate(builder, frame_rate as c_uint);
            AAudioStreamBuilder_setChannelCount(builder, num_channels as c_int);
            AAudioStreamBuilder_openStream(builder, addr_of_mut!(aaudio_stream));
            AAudioStreamBuilder_delete(builder);
            AAudioStream_requestStart(aaudio_stream);
        }
        let buffer = vec![0; buffer_size * frame_size].into_boxed_slice();
        let stream = Rc::new(Mutex::new(aaudio_stream));
        let drop = AndroidAudioPlaybackBufferCommit {
            buffer_ptr: buffer.as_ptr(),
            stream: stream.clone(),
        };
        AndroidAudioStream {
            buffer,
            frame_size,
            interval,
            next_frame: interval,
            start_time: None,
            stream,
            drop,
        }
    }
}

impl PlaybackBufferStream for AndroidAudioStream {
    fn next_playback_buffer<'b, 's: 'b>(&'s mut self) -> Result<PlaybackBuffer<'b>, BoxError> {
        if let Some(start_time) = self.start_time {
            let elapsed = start_time.elapsed();
            if elapsed < self.next_frame {
                thread::sleep(self.next_frame - elapsed);
            }
            self.next_frame += self.interval;
        } else {
            self.start_time = Some(Instant::now());
            self.next_frame = self.interval;
        }
        match PlaybackBuffer::new(self.frame_size, self.buffer.as_mut(), &mut self.drop) {
            Ok(playback_buffer) => Ok(playback_buffer),
            Err(err) => Err(Box::new(err)),
        }
    }
}

#[async_trait(?Send)]
impl AsyncPlaybackBufferStream for AndroidAudioStream {
    async fn next_playback_buffer<'a>(
        &'a mut self,
        ex: &dyn AudioStreamsExecutor,
    ) -> Result<AsyncPlaybackBuffer<'a>, BoxError> {
        if let Some(start_time) = self.start_time {
            let elapsed = start_time.elapsed();
            if elapsed < self.next_frame {
                ex.delay(self.next_frame - elapsed).await?;
            }
            self.next_frame += self.interval;
        } else {
            self.start_time = Some(Instant::now());
            self.next_frame = self.interval;
        }
        match AsyncPlaybackBuffer::new(self.frame_size, self.buffer.as_mut(), &mut self.drop) {
            Ok(playback_buffer) => Ok(playback_buffer),
            Err(err) => Err(Box::new(err)),
        }
    }
}

impl Drop for AndroidAudioStream {
    fn drop(&mut self) {
        // SAFETY:
        // Interfacing with the AAudio C API. Assumes correct linking
        // and `stream` are valid and properly initialized.
        unsafe {
            AAudioStream_close(*self.stream.lock().unwrap());
        }
    }
}

#[derive(Default)]
struct AndroidAudioStreamSource;

impl StreamSource for AndroidAudioStreamSource {
    #[allow(clippy::type_complexity)]
    fn new_playback_stream(
        &mut self,
        num_channels: usize,
        format: SampleFormat,
        frame_rate: u32,
        buffer_size: usize,
    ) -> Result<(Box<dyn StreamControl>, Box<dyn PlaybackBufferStream>), BoxError> {
        Ok((
            Box::new(NoopStreamControl::new()),
            Box::new(AndroidAudioStream::new(
                num_channels,
                format,
                frame_rate,
                buffer_size,
            )),
        ))
    }

    #[allow(clippy::type_complexity)]
    fn new_async_playback_stream(
        &mut self,
        num_channels: usize,
        format: SampleFormat,
        frame_rate: u32,
        buffer_size: usize,
        _ex: &dyn AudioStreamsExecutor,
    ) -> Result<(Box<dyn StreamControl>, Box<dyn AsyncPlaybackBufferStream>), BoxError> {
        Ok((
            Box::new(NoopStreamControl::new()),
            Box::new(AndroidAudioStream::new(
                num_channels,
                format,
                frame_rate,
                buffer_size,
            )),
        ))
    }
}

#[derive(Default)]
pub struct AndroidAudioStreamSourceGenerator;

impl AndroidAudioStreamSourceGenerator {
    pub fn new() -> Self {
        AndroidAudioStreamSourceGenerator {}
    }
}

/// `AndroidAudioStreamSourceGenerator` is a struct that implements [`StreamSourceGenerator`]
/// for `AndroidAudioStreamSource`.
impl StreamSourceGenerator for AndroidAudioStreamSourceGenerator {
    fn generate(&self) -> Result<Box<dyn StreamSource>, BoxError> {
        Ok(Box::new(AndroidAudioStreamSource))
    }
}
