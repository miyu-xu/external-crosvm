// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(target_os = "macos")]
use base::AsRawDescriptor;
use base::TimerTrait;

use crate::AsyncError;
use crate::AsyncResult;
use crate::IntoAsync;
use crate::TimerAsync;

#[cfg(any(target_os = "android", target_os = "linux"))]
impl<T: TimerTrait + IntoAsync> TimerAsync<T> {
    pub async fn wait_sys(&self) -> AsyncResult<()> {
        let (n, _) = self
            .io_source
            .read_to_vec(None, 0u64.to_ne_bytes().to_vec())
            .await?;
        if n != 8 {
            return Err(AsyncError::EventAsync(base::Error::new(libc::ENODATA)));
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl<T: TimerTrait + IntoAsync> TimerAsync<T> {
    pub async fn wait_sys(&self) -> AsyncResult<()> {
        // macOS Timer descriptors are kqueues. The descriptor becomes readable when its timer
        // event is pending, but unlike Linux timerfd it cannot be consumed with read(2).
        self.io_source.wait_readable().await?;
        let mut event = std::mem::MaybeUninit::<libc::kevent>::uninit();
        loop {
            // SAFETY: the timer owns a valid kqueue descriptor, the changelist is empty, and the
            // event output points to space for exactly one initialized kevent on success.
            let result = unsafe {
                libc::kevent(
                    self.io_source.as_source().as_raw_descriptor(),
                    std::ptr::null(),
                    0,
                    event.as_mut_ptr(),
                    1,
                    std::ptr::null(),
                )
            };
            if result == 1 {
                return Ok(());
            }
            let error = base::Error::last();
            if result < 0 && error.errno() == libc::EINTR {
                continue;
            }
            return Err(AsyncError::EventAsync(if result == 0 {
                base::Error::new(libc::ENODATA)
            } else {
                error
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;
    use std::time::Instant;

    use base::Timer;

    use super::super::fd_executor::EpollReactor;
    use super::super::uring_executor::UringReactor;
    use super::*;
    use crate::common_executor::RawExecutor;
    use crate::sys::linux::uring_executor::is_uring_stable;
    use crate::Executor;
    use crate::ExecutorTrait;

    impl TimerAsync<Timer> {
        pub(crate) fn new_poll(
            timer: Timer,
            ex: &Arc<RawExecutor<EpollReactor>>,
        ) -> AsyncResult<TimerAsync<Timer>> {
            ex.async_from(timer)
                .map(|io_source| TimerAsync { io_source })
        }

        pub(crate) fn new_uring(
            timer: Timer,
            ex: &Arc<RawExecutor<UringReactor>>,
        ) -> AsyncResult<TimerAsync<Timer>> {
            ex.async_from(timer)
                .map(|io_source| TimerAsync { io_source })
        }
    }

    #[test]
    fn timer() {
        async fn this_test(ex: &Executor) {
            let dur = Duration::from_millis(200);
            let now = Instant::now();
            TimerAsync::sleep(ex, dur).await.expect("unable to sleep");
            assert!(now.elapsed() >= dur);
        }

        let ex = Executor::new().expect("creating an executor failed");
        ex.run_until(this_test(&ex)).unwrap();
    }

    #[test]
    fn one_shot() {
        if !is_uring_stable() {
            return;
        }

        async fn this_test(ex: &Arc<RawExecutor<UringReactor>>) {
            let mut tfd = Timer::new().expect("failed to create timerfd");

            let dur = Duration::from_millis(200);
            let now = Instant::now();
            tfd.reset_oneshot(dur).expect("failed to arm timer");

            let t = TimerAsync::new_uring(tfd, ex).unwrap();
            t.wait().await.expect("unable to wait for timer");

            assert!(now.elapsed() >= dur);
        }

        let ex = RawExecutor::<UringReactor>::new().unwrap();
        ex.run_until(this_test(&ex)).unwrap();
    }

    #[test]
    fn one_shot_fd() {
        async fn this_test(ex: &Arc<RawExecutor<EpollReactor>>) {
            let mut tfd = Timer::new().expect("failed to create timerfd");

            let dur = Duration::from_millis(200);
            let now = Instant::now();
            tfd.reset_oneshot(dur).expect("failed to arm timer");

            let t = TimerAsync::new_poll(tfd, ex).unwrap();
            t.wait().await.expect("unable to wait for timer");

            assert!(now.elapsed() >= dur);
        }

        let ex = RawExecutor::<EpollReactor>::new().unwrap();
        ex.run_until(this_test(&ex)).unwrap();
    }
}
