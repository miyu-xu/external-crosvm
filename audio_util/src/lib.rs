// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(feature = "audio_aaudio")]
mod aaudio_streams;
mod file_streams;

#[cfg(feature = "audio_aaudio")]
pub use aaudio_streams::AaudioStreamSourceGenerator;
pub use file_streams::Error;
pub use file_streams::FileStreamSourceGenerator;
