# r8brain-free-src

- Upstream: https://github.com/avaneev/r8brain-free-src
- Revision: `901cfd827dc6881b45232676670efb54dee9378f`
- License: MIT (see `LICENSE`)

Only the headers selected by the compiler dependency graph, C ABI wrapper,
license, and upstream README needed for the default resampler are vendored
here. The wrapper is compiled as a static C++ object by `win_audio/build.rs`.
