// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::future::Future;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::task::Context;
use std::task::Poll;

use futures::pin_mut;
use futures::task::waker_ref;
use futures::task::ArcWake;

thread_local!(static PER_THREAD_WAKER: Arc<Waker> = Arc::new(Waker {
    ready: Mutex::new(false),
    cv: Condvar::new(),
}));

struct Waker {
    ready: Mutex<bool>,
    cv: Condvar,
}

impl ArcWake for Waker {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        let mut ready = arc_self.ready.lock().unwrap();
        *ready = true;
        arc_self.cv.notify_one();
    }
}

/// Run a future to completion on the current thread.
pub fn block_on<F: Future>(f: F) -> F::Output {
    pin_mut!(f);

    PER_THREAD_WAKER.with(|thread_waker| {
        let waker = waker_ref(thread_waker);
        let mut cx = Context::from_waker(&waker);

        loop {
            if let Poll::Ready(t) = f.as_mut().poll(&mut cx) {
                return t;
            }

            let mut ready = thread_waker.ready.lock().unwrap();
            while !*ready {
                ready = thread_waker.cv.wait(ready).unwrap();
            }
            *ready = false;
        }
    })
}
