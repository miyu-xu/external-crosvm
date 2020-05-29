// Copyright 2019 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(feature = "plugin")]
pub mod plugin {
  // needs to comment out #! and //! comments in included files.
  include!(concat!(env!("OUT_DIR"), "/plugin.rs"));
}

#[cfg(feature = "composite-disk")]
pub mod cdisk_spec {
  // needs to comment out #! and //! comments in included files.
  include!(concat!(env!("OUT_DIR"), "/cdisk_spec.rs"));
}

// This does not compile: unexpected token concat
// #[path = concat!(env!("OUT_DIR"), "/plugin.rs")]
// pub mod cdisk_spec;


// This works, just like symbolic link in ./cdisk_spec.rs to the generated file.
// #[path = "../../../../out/soong/.intermediates/external/crosvm/protos/crosvm_cdisk_spec_proto/gen/cdisk_spec.rs"]
// pub mod cdisk_spec;
