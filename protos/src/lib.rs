// Copyright 2019 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Generated protobuf bindings.

#[cfg(feature = "plugin")]
pub use crosvm_plugin_proto::plugin;

#[cfg(feature = "composite-disk")]
<<<<<<< HEAD   (0344ff Merge "Revert "Revert "Update bitflags dependency to 2.2.1.")
pub use cdisk_spec_proto::cdisk_spec;
=======
pub use generated::cdisk_spec;

#[cfg(feature = "registered_events")]
pub use generated::registered_events;
>>>>>>> BRANCH (fefe0c rutabaga_gfx: move to updated stream_renderer_flush api)
