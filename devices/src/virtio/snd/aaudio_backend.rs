// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE aaudio.

use audio_util::AaudioStreamSourceGenerator;

use crate::virtio::snd::common_backend::SndData;
use crate::virtio::snd::sys::SysAudioStreamSourceGenerator;

pub(crate) fn create_aaudio_stream_source_generators(
    snd_data: &SndData,
) -> Vec<SysAudioStreamSourceGenerator> {
    let mut generators: Vec<SysAudioStreamSourceGenerator> = Vec::new();
    generators.resize_with(snd_data.pcm_info_len(), || {
        Box::new(AaudioStreamSourceGenerator::new())
    });
    generators
}
