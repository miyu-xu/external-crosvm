// Copyright 2026 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! kqueue-backed [`EventContext`] for macOS (parity with Linux epoll).

use std::cmp::min;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Mutex;
use std::time::Duration;

use libc::ENOENT;
use smallvec::SmallVec;

use super::kqueue::Kqueue;
use crate::errno::Result;
use crate::AsRawDescriptor;
use crate::EventToken;
use crate::EventType;
use crate::RawDescriptor;
use crate::TriggeredEvent;

const EVENT_CONTEXT_MAX_EVENTS: usize = 16;

#[derive(Copy, Clone, Debug)]
struct FdState {
    event_type: EventType,
    token: u64,
}

/// Used to poll multiple objects that have file descriptors.
///
/// See [`crate::WaitContext`] for the cross-platform wrapper.
pub struct EventContext<T: EventToken> {
    queue: Kqueue,
    state: Mutex<HashMap<RawDescriptor, FdState>>,
    _marker: PhantomData<T>,
}

impl<T: EventToken> EventContext<T> {
    pub fn new() -> Result<EventContext<T>> {
        Ok(EventContext {
            queue: Kqueue::new()?,
            state: Mutex::new(HashMap::new()),
            _marker: PhantomData,
        })
    }

    pub fn build_with(fd_tokens: &[(&dyn AsRawDescriptor, T)]) -> Result<EventContext<T>> {
        let ctx = EventContext::new()?;
        ctx.add_many(fd_tokens)?;
        Ok(ctx)
    }

    pub fn add_many(&self, fd_tokens: &[(&dyn AsRawDescriptor, T)]) -> Result<()> {
        for (fd, token) in fd_tokens {
            self.add(*fd, T::from_raw_token(token.as_raw_token()))?;
        }
        Ok(())
    }

    pub fn add(&self, fd: &dyn AsRawDescriptor, token: T) -> Result<()> {
        self.add_for_event(fd, EventType::Read, token)
    }

    pub fn add_for_event(
        &self,
        descriptor: &dyn AsRawDescriptor,
        event_type: EventType,
        token: T,
    ) -> Result<()> {
        let fd = descriptor.as_raw_descriptor();
        let mut state = self.state.lock().expect("EventContext state lock poisoned");
        if let Entry::Vacant(v) = state.entry(fd) {
            v.insert(FdState {
                event_type,
                token: token.as_raw_token(),
            });
            drop(state);
            match self.apply_kevents(fd, event_type, token.as_raw_token()) {
                Ok(()) => Ok(()),
                Err(e) => {
                    let mut state = self.state.lock().expect("EventContext state lock poisoned");
                    state.remove(&fd);
                    Err(e)
                }
            }
        } else {
            Err(crate::Error::new(libc::EEXIST))
        }
    }

    pub fn modify(
        &self,
        fd: &dyn AsRawDescriptor,
        event_type: EventType,
        token: T,
    ) -> Result<()> {
        let fd = fd.as_raw_descriptor();
        let mut state = self.state.lock().expect("EventContext state lock poisoned");
        if let Entry::Occupied(mut o) = state.entry(fd) {
            let old = *o.get();
            o.insert(FdState {
                event_type,
                token: token.as_raw_token(),
            });
            drop(state);
            self.remove_filters(fd, old.event_type)?;
            self.apply_kevents(fd, event_type, token.as_raw_token())
        } else {
            Err(crate::Error::new(libc::ENOENT))
        }
    }

    pub fn delete(&self, fd: &dyn AsRawDescriptor) -> Result<()> {
        let fd = fd.as_raw_descriptor();
        let removed = {
            let mut state = self.state.lock().expect("EventContext state lock poisoned");
            state.remove(&fd)
        };
        if let Some(old) = removed {
            self.remove_filters(fd, old.event_type)?;
        }
        Ok(())
    }

    pub fn wait(&self) -> Result<SmallVec<[TriggeredEvent<T>; 16]>> {
        self.wait_timeout(Duration::new(i64::MAX as u64, 0))
    }

    pub fn wait_timeout(&self, timeout: Duration) -> Result<SmallVec<[TriggeredEvent<T>; 16]>> {
        let mut events: [libc::kevent64_s; EVENT_CONTEXT_MAX_EVENTS] =
            unsafe { std::mem::zeroed() };

        let timeout_arg = if timeout.as_secs() as i64 == i64::MAX {
            None
        } else {
            let millis = timeout
                .as_secs()
                .checked_mul(1_000)
                .and_then(|ms| ms.checked_add(u64::from(timeout.subsec_nanos()) / 1_000_000))
                .unwrap_or(i32::MAX as u64);
            let millis = min(i32::MAX as u64, millis) as i32;
            Some(std::time::Duration::from_millis(millis as u64))
        };

        let slice = loop {
            match self.queue.kevent(&[], &mut events[..], timeout_arg) {
                Ok(s) => break s,
                Err(e) if e.errno() == libc::EINTR => continue,
                Err(e) => return Err(e),
            }
        };

        let state = self.state.lock().expect("EventContext state lock poisoned");
        // Merge multiple kevents for the same FD (e.g. ReadWrite) into one `TriggeredEvent`, like
        // Linux epoll's combined `EPOLLIN | EPOLLOUT`.
        let mut merged: HashMap<RawDescriptor, TriggeredEvent<T>> = HashMap::new();
        for ev in slice {
            let fd = ev.ident as RawDescriptor;
            let Some(reg) = state.get(&fd) else {
                continue;
            };
            let token = T::from_raw_token(reg.token);
            let is_readable = ev.filter == libc::EVFILT_READ;
            let is_writable = ev.filter == libc::EVFILT_WRITE;
            let is_hungup = (ev.flags & libc::EV_EOF) != 0 || (ev.flags & libc::EV_ERROR) != 0;
            merged
                .entry(fd)
                .and_modify(|e| {
                    e.is_readable |= is_readable;
                    e.is_writable |= is_writable;
                    e.is_hungup |= is_hungup;
                })
                .or_insert(TriggeredEvent {
                    token,
                    is_readable,
                    is_writable,
                    is_hungup,
                });
        }
        Ok(merged.into_values().collect())
    }

    fn remove_filters(&self, fd: RawDescriptor, event_type: EventType) -> Result<()> {
        let mut changelist: SmallVec<[libc::kevent64_s; 2]> = SmallVec::new();
        match event_type {
            EventType::None => {}
            EventType::Read => changelist.push(kev_delete(fd, libc::EVFILT_READ)),
            EventType::Write => changelist.push(kev_delete(fd, libc::EVFILT_WRITE)),
            EventType::ReadWrite => {
                changelist.push(kev_delete(fd, libc::EVFILT_READ));
                changelist.push(kev_delete(fd, libc::EVFILT_WRITE));
            }
        }
        for kev in &changelist {
            let mut empty: [libc::kevent64_s; 0] = [];
            if let Err(e) = self.queue.kevent(std::slice::from_ref(kev), &mut empty, None) {
                if e.errno() != ENOENT {
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    fn apply_kevents(&self, fd: RawDescriptor, event_type: EventType, token: u64) -> Result<()> {
        let mut changelist: SmallVec<[libc::kevent64_s; 2]> = SmallVec::new();
        match event_type {
            EventType::None => {}
            EventType::Read => changelist.push(kev_add(fd, libc::EVFILT_READ, token)),
            EventType::Write => changelist.push(kev_add(fd, libc::EVFILT_WRITE, token)),
            EventType::ReadWrite => {
                changelist.push(kev_add(fd, libc::EVFILT_READ, token));
                changelist.push(kev_add(fd, libc::EVFILT_WRITE, token));
            }
        }
        if changelist.is_empty() {
            return Ok(());
        }
        let mut empty: [libc::kevent64_s; 0] = [];
        self.queue
            .kevent(&changelist, &mut empty, None)
            .map(|_| ())
    }
}

impl<T: EventToken> crate::AsRawDescriptor for EventContext<T> {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.queue.as_raw_descriptor()
    }
}

fn kev_add(fd: RawDescriptor, filter: i16, udata: u64) -> libc::kevent64_s {
    libc::kevent64_s {
        ident: fd as u64,
        filter,
        flags: libc::EV_ADD,
        fflags: 0,
        data: 0,
        udata,
        ext: [0, 0],
    }
}

fn kev_delete(fd: RawDescriptor, filter: i16) -> libc::kevent64_s {
    libc::kevent64_s {
        ident: fd as u64,
        filter,
        flags: libc::EV_DELETE,
        fflags: 0,
        data: 0,
        udata: 0,
        ext: [0, 0],
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use base_event_token_derive::EventToken;

    use super::*;
    use crate::Event;

    #[test]
    fn event_context() {
        let evt1 = Event::new().unwrap();
        let evt2 = Event::new().unwrap();
        evt1.signal().unwrap();
        evt2.signal().unwrap();
        let ctx: EventContext<u32> = EventContext::build_with(&[(&evt1, 1), (&evt2, 2)]).unwrap();

        let mut evt_count = 0;
        while evt_count < 2 {
            for event in ctx.wait().unwrap().iter().filter(|e| e.is_readable) {
                evt_count += 1;
                match event.token {
                    1 => {
                        evt1.wait().unwrap();
                        ctx.delete(&evt1).unwrap();
                    }
                    2 => {
                        evt2.wait().unwrap();
                        ctx.delete(&evt2).unwrap();
                    }
                    _ => panic!("unexpected token"),
                };
            }
        }
        assert_eq!(evt_count, 2);
    }

    #[test]
    fn event_context_timeout() {
        let ctx: EventContext<u32> = EventContext::new().unwrap();
        let dur = Duration::from_millis(10);
        let start_inst = Instant::now();
        ctx.wait_timeout(dur).unwrap();
        assert!(start_inst.elapsed() >= dur);
    }

    #[test]
    #[allow(dead_code)]
    fn event_token_derive() {
        #[derive(EventToken)]
        enum EmptyToken {}

        #[derive(PartialEq, Debug, EventToken)]
        enum Token {
            Alpha,
            Beta,
            Gamma(u32),
            Delta { index: usize },
            Omega,
        }

        assert_eq!(
            Token::from_raw_token(Token::Alpha.as_raw_token()),
            Token::Alpha
        );
        assert_eq!(
            Token::from_raw_token(Token::Beta.as_raw_token()),
            Token::Beta
        );
        assert_eq!(
            Token::from_raw_token(Token::Gamma(55).as_raw_token()),
            Token::Gamma(55)
        );
        assert_eq!(
            Token::from_raw_token(Token::Delta { index: 100 }.as_raw_token()),
            Token::Delta { index: 100 }
        );
        assert_eq!(
            Token::from_raw_token(Token::Omega.as_raw_token()),
            Token::Omega
        );
    }
}
