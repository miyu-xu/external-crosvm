// Copyright 2021 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[macro_export]
macro_rules! gpu_debug {
    ($($args:tt)+) => {
        // Set true to enable logging.
        if false {
            base::debug!($($args)*);
        }
    };
}
