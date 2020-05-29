// Copyright 2019 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(feature = "plugin")]
include!(concat!(env!("OUT_DIR"), "/plugin.rs"));

#[cfg(feature = "composite-disk")]
include!(concat!(env!("OUT_DIR"), "/cdisk_spec.rs"));
