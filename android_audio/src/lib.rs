// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod android_audio_streams;
#[cfg(feature = "libaaudio_stub")]
mod libaaudio_stub;

pub use android_audio_streams::AndroidAudioStreamSourceGenerator;
