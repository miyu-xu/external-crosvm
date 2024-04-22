// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use audio_streams::AsyncBufferCommit;
use audio_streams::AsyncPlaybackBuffer;
use audio_streams::AsyncPlaybackBufferStream;
use audio_streams::AudioStreamsExecutor;
use audio_streams::BoxError;
use audio_streams::BufferCommit;
use audio_streams::PlaybackBuffer;
use audio_streams::PlaybackBufferStream;
use audio_streams::SampleFormat;
use audio_streams::StreamControl;
use audio_streams::StreamSource;
use audio_streams::StreamSourceGenerator;

extern crate aaudio_backend;

pub struct AaudioStream {
    buffer: Vec<u8>,
    frame_size: usize,
    interval: Duration,
    next_frame: Duration,
    start_time: Option<Instant>,
}

impl BufferCommit for AaudioStream {
    fn commit(&mut self, _nwritten: usize) {
        unimplemented!();
    }
}

#[async_trait(?Send)]
impl AsyncBufferCommit for AaudioStream {
    async fn commit(&mut self, _nwritten: usize) {
        aaudio_backend::playback(self.buffer.as_mut_slice(), _nwritten);
    }
}

impl AaudioStream {
    pub fn new(
        num_channels: usize,
        format: SampleFormat,
        frame_rate: u32,
        buffer_size: usize,
    ) -> Self {
        let frame_size = format.sample_bytes() * num_channels;
        let interval = Duration::from_millis(buffer_size as u64 * 1000 / frame_rate as u64);
        aaudio_backend::init(num_channels, frame_rate);
        AaudioStream {
            buffer: vec![0; buffer_size * frame_size],
            frame_size,
            interval,
            next_frame: interval,
            start_time: None,
        }
    }
}

impl PlaybackBufferStream for AaudioStream {
    fn next_playback_buffer<'b, 's: 'b>(&'s mut self) -> Result<PlaybackBuffer<'b>, BoxError> {
        unimplemented!();
    }
}

#[async_trait(?Send)]
impl AsyncPlaybackBufferStream for AaudioStream {
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
        let slice =
            unsafe { std::slice::from_raw_parts_mut(self.buffer.as_mut_ptr(), self.buffer.len()) };
        Ok(AsyncPlaybackBuffer::new(self.frame_size, slice, self)?)
    }
}

impl Drop for AaudioStream {
    fn drop(&mut self) {
        aaudio_backend::release()
    }
}

#[derive(Default)]
pub struct AaudioStreamControl;

impl AaudioStreamControl {
    pub fn new() -> Self {
        AaudioStreamControl {}
    }
}

impl StreamControl for AaudioStreamControl {}

#[derive(Default)]
pub struct AaudioStreamSource;

impl StreamSource for AaudioStreamSource {
    #[allow(clippy::type_complexity)]
    fn new_playback_stream(
        &mut self,
        _num_channels: usize,
        _format: SampleFormat,
        _frame_rate: u32,
        _buffer_size: usize,
    ) -> Result<(Box<dyn StreamControl>, Box<dyn PlaybackBufferStream>), BoxError> {
        unimplemented!();
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
            Box::new(AaudioStreamControl::new()),
            Box::new(AaudioStream::new(
                num_channels,
                format,
                frame_rate,
                buffer_size,
            )),
        ))
    }
}

pub struct AaudioStreamSourceGenerator;

impl AaudioStreamSourceGenerator {
    pub fn new() -> Self {
        AaudioStreamSourceGenerator {}
    }
}

impl StreamSourceGenerator for AaudioStreamSourceGenerator {
    fn generate(&self) -> Result<Box<dyn StreamSource>, BoxError> {
        Ok(Box::new(AaudioStreamSource))
    }
}
