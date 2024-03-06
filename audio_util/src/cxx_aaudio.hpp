#pragma once

#include "rust/cxx.h"
#include "src/aaudio_streams.rs.h"
#include <aaudio/AAudio.h>
#include <android/log.h>

int aaudio_init(size_t num_channel, uint32_t frame_rate);
int aaudio_playback(uint8_t* buffer, size_t numFrame);
