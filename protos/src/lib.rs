// Copyright 2019 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Generated protobuf bindings.

#[cfg(feature = "plugin")]
pub use crosvm_plugin_proto::plugin;

#[cfg(feature = "composite-disk")]
<<<<<<< HEAD   (d5f3f5 Merge remote-tracking branch 'aosp/upstream-main' into merge)
pub use cdisk_spec_proto::cdisk_spec;

=======
pub use generated::cdisk_spec;
>>>>>>> BRANCH (8bbcbe devices: virtio: block: print full errors)
#[cfg(feature = "registered_events")]
pub use registered_events_proto::registered_events;
