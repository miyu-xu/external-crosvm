// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

cfg_if::cfg_if! {
    if #[cfg(any(target_os = "android", target_os = "linux"))] {
        pub mod linux;
        pub use linux::{
            add_goldfish_battery,
            generate_platform_bus,
            PlatformBusResources,
        };
    } else if #[cfg(all(target_os = "macos", feature = "hvf"))] {
        pub mod macos;
        pub use macos::{
            add_goldfish_battery,
            generate_platform_bus,
            PlatformBusResources,
        };
    }
}
