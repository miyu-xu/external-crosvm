// Copyright 2026 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! WHPX/OVMF virtio bring-up helpers shared between queue setup and the vCPU loop.

use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use base::debug;

static USED_IDX_GPA: AtomicU64 = AtomicU64::new(0);
static VCPU_TLB_FLUSH_REQUESTED: AtomicBool = AtomicBool::new(false);

type GpaRefreshFn = Box<dyn Fn(u64, usize) + Send + Sync>;

static GPA_REFRESH: Mutex<Option<GpaRefreshFn>> = Mutex::new(None);

/// Records the guest GPA of the virtio used-ring `idx` field (used_ring + 2).
pub fn set_used_idx_gpa(gpa: u64) {
    if gpa != 0 {
        debug!("whpx_ovmf: used.idx GPA = 0x{:016x}", gpa);
        USED_IDX_GPA.store(gpa, Ordering::Release);
    }
}

/// Returns the last published used-ring `idx` GPA, or 0 if unknown.
pub fn used_idx_gpa() -> u64 {
    USED_IDX_GPA.load(Ordering::Acquire)
}

/// Registers a callback that remaps a GPA range in WHPX after host-side guest RAM writes.
pub fn register_gpa_refresh(f: GpaRefreshFn) {
    *GPA_REFRESH.lock().unwrap() = Some(f);
}

/// Remaps `gpa..gpa+len` in WHPX so vCPUs observe recent host writes (virtio rings/buffers).
pub fn refresh_gpa_range(gpa: u64, len: usize) {
    if gpa == 0 || len == 0 {
        return;
    }
    debug!("whpx_ovmf: refresh_gpa gpa=0x{:x} len=0x{:x}", gpa, len);
    if let Some(f) = GPA_REFRESH.lock().unwrap().as_ref() {
        f(gpa, len);
    }
}

/// Ask each vCPU to reload CR3 (TLB flush) before the next guest run.
pub fn request_vcpu_tlb_flush() {
    VCPU_TLB_FLUSH_REQUESTED.store(true, Ordering::Release);
}

/// Returns true once per request; consumed by the vCPU run loop.
pub fn take_vcpu_tlb_flush_request() -> bool {
    VCPU_TLB_FLUSH_REQUESTED.swap(false, Ordering::AcqRel)
}
