// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use audio_streams::BoxError;
use audio_streams::StreamSource;
use audio_streams::StreamSourceGenerator;

pub struct AaudioStreamSourceGenerator;

impl AaudioStreamSourceGenerator {
    pub fn new() -> Self {
        panic!("Cannot create aaudio audio device on non-android crosvm builds.")
    }
}

impl StreamSourceGenerator for AaudioStreamSourceGenerator {
    fn generate(&self) -> Result<Box<dyn StreamSource>, BoxError> {
        panic!("Cannot create aaudio audio device on non-android crosvm builds.")
    }
}

impl Default for AaudioStreamSourceGenerator {
    fn default() -> Self {
        Self::new()
    }
}
