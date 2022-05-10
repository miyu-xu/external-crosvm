// Copyright 2019 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

<<<<<<< HEAD   (77faf7 Revert "ANDROID: Cargo.toml used named-profiles feature.")
=======
//! Generated protobuf bindings.

mod generated {
    include!(concat!(env!("OUT_DIR"), "/generated.rs"));
}

>>>>>>> BRANCH (4c2017 metrics: Add metrics crate.)
#[cfg(feature = "plugin")]
pub use crosvm_plugin_proto::plugin;

#[cfg(feature = "composite-disk")]
pub use cdisk_spec_proto::cdisk_spec;
