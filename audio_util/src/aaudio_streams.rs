use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use audio_streams::AsyncBufferCommit;
use audio_streams::AsyncPlaybackBuffer;
use audio_streams::AsyncPlaybackBufferStream;
use audio_streams::AudioStreamsExecutor;
use audio_streams::BoxError;
use audio_streams::BufferCommit;
use audio_streams::SampleFormat;
use audio_streams::StreamControl;
use audio_streams::StreamSource;
use audio_streams::StreamSourceGenerator;
use audio_streams::PlaybackBufferStream;
use audio_streams::PlaybackBuffer;

#[cxx::bridge]
mod ffi {
    unsafe extern "C++" {
        include!("src/cxx_aaudio.hpp");
        unsafe fn aaudio_init(num_channels: usize, framte_rate: u32) -> i32;
        unsafe fn aaudio_playback(buffer: *mut u8, num_frame: usize) ->i32;
    }
}

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
        unsafe{ffi::aaudio_playback(self.buffer.as_mut_ptr(), self.buffer.len() / self.frame_size)};
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
        unsafe{ffi::aaudio_init(num_channels, frame_rate);}
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
        let slice = unsafe {
            std::slice::from_raw_parts_mut(
                self.buffer.as_mut_ptr(),
                self.buffer.len(),
            )
        };
        Ok(AsyncPlaybackBuffer::new(
            self.frame_size,
            slice,
            self,
        )?)
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

impl AaudioStreamSource {
    pub fn new() -> Self {
        AaudioStreamSource {}
    }
}

impl StreamSource for AaudioStreamSource {
    #[allow(clippy::type_complexity)]
    fn new_playback_stream(
        &mut self,
        num_channels: usize,
        format: SampleFormat,
        frame_rate: u32,
        buffer_size: usize,
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
