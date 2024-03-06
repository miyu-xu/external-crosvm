#include "src/cxx_aaudio.hpp"
#include "src/aaudio_streams.rs.h"

AAudioStream *stream = nullptr;

int aaudio_init(size_t num_channel, uint32_t frame_rate) {
    if (stream != nullptr) {
        AAudioStream_release(stream);
    }
    AAudioStreamBuilder *builder;
    aaudio_result_t result;
    result = AAudio_createStreamBuilder(&builder);
    AAudioStreamBuilder_setFormat(builder, AAUDIO_FORMAT_PCM_I16);
    AAudioStreamBuilder_setSampleRate(builder, frame_rate);
    AAudioStreamBuilder_setChannelCount(builder, num_channel);
    result = AAudioStreamBuilder_openStream(builder, &stream);
    result = AAudioStream_requestStart(stream);
    return 0;
}

int aaudio_playback(uint8_t* buffer, size_t num_frame) {
    AAudioStream_write(stream, buffer, num_frame, 0);
    return 0;
}
