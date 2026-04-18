// Copyright 2021 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod async_types;
mod error;
pub mod event;
pub mod executor;
pub mod fd_executor;
pub mod poll_source;
mod timer;
#[cfg(feature = "tokio")]
pub mod tokio_source;
#[cfg(any(target_os = "android", target_os = "linux"))]
pub mod uring_executor;
#[cfg(any(target_os = "android", target_os = "linux"))]
pub mod uring_source;

pub use error::AsyncErrorSys;
pub use executor::ExecutorKindSys;
pub(crate) use fd_executor::EpollReactor;
pub use poll_source::Error as PollSourceError;
pub use poll_source::PollSource;
#[cfg(any(target_os = "android", target_os = "linux"))]
pub(crate) use uring_executor::UringReactor;
#[cfg(any(target_os = "android", target_os = "linux"))]
pub use uring_source::UringSource;

use crate::Error;

impl From<Error> for std::io::Error {
    fn from(e: Error) -> Self {
        use Error::*;
        match e {
            EventAsync(e) => e.into(),
            Io(e) => e,
            #[cfg(any(target_os = "android", target_os = "linux"))]
            URingExecutor(e) => e.into(),
            PollSource(e) => e.into(),
            Timer(e) => e.into(),
            TimerAsync(e) => e.into(),
        }
    }
}
