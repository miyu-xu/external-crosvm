// Copyright 2026 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Virtio minidump device backend for collecting guest memory regions.
//! This backend uses single-direction communication (output-only) to match
//! the frontend driver's virtqueue_add_outbuf() pattern.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::anyhow;
use anyhow::Context;
use base::error;
use base::info;
use base::warn;
use base::Event;
use base::EventToken;
use base::WaitContext;
use base::WorkerThread;
use data_model::Le32;
use data_model::Le64;
use snapshot::AnySnapshot;
use vm_memory::GuestAddress;
use vm_memory::GuestMemory;
use zerocopy::FromBytes;
use zerocopy::Immutable;
use zerocopy::IntoBytes;
use zerocopy::KnownLayout;
use sync::Mutex;

use super::DeviceType;
use super::Interrupt;
use super::Queue;
use super::VirtioDevice;

const QUEUE_SIZE: u16 = 64;
const QUEUE_SIZES: &[u16] = &[QUEUE_SIZE];

// Message types from frontend driver
const MD_SS_UPDATE_REGION: u32 = 0;
const MD_SS_ADD_REGION: u32 = 1;
const MD_SS_REMOVE_REGION: u32 = 2;

const MINIDUMP_MAX_NAME_LENGTH: usize = 14;
const MAX_ENTRY_NUM: usize = 200;

/// Virtio minidump message structure matching the frontend driver
#[derive(Copy, Clone, Debug, Default, FromBytes, Immutable, IntoBytes, KnownLayout)]
#[repr(C)]
struct VirtioMinidumpMsg {
    msg_type: Le32,
    name: [u8; MINIDUMP_MAX_NAME_LENGTH],
    _padding1: [u8; 6],  // Explicit padding to align phy_addr to 8-byte boundary
    phy_addr: Le64,
    size: Le64,
    result: Le32,
    _padding2: [u8; 4],  // Explicit padding at end of struct
}

pub use hypervisor::MinidumpRegion;

/// Shared state for minidump regions
#[derive(Clone)]
struct MinidumpState {
    regions: Arc<Mutex<HashMap<String, MinidumpRegion>>>,
    output_dir: Option<PathBuf>,
}

impl MinidumpState {
    fn new(output_dir: Option<PathBuf>) -> Self {
        MinidumpState {
            regions: Arc::new(Mutex::new(HashMap::new())),
            output_dir,
        }
    }

    fn add_region(&self, name: String, phys_addr: u64, size: u64) -> Result<(), String> {
        let mut regions = self.regions.lock();

        if regions.len() >= MAX_ENTRY_NUM {
            return Err(format!("Maximum number of regions ({}) reached", MAX_ENTRY_NUM));
        }

        if regions.contains_key(&name) {
            return Err(format!("Region '{}' already exists", name));
        }

        regions.insert(
            name.clone(),
            MinidumpRegion {
                name,
                phys_addr,
                size,
            },
        );

        Ok(())
    }

    fn remove_region(&self, name: &str) -> Result<(), String> {
        let mut regions = self.regions.lock();

        if regions.remove(name).is_some() {
            Ok(())
        } else {
            Err(format!("Region '{}' not found", name))
        }
    }

    fn update_region(&self, name: &str, phys_addr: u64, size: u64) -> Result<(), String> {
        let mut regions = self.regions.lock();

        if let Some(region) = regions.get_mut(name) {
            region.phys_addr = phys_addr;
            region.size = size;
            Ok(())
        } else {
            Err(format!("Region '{}' not found", name))
        }
    }

    fn get_available_regions(&self) -> usize {
        let regions = self.regions.lock();
        MAX_ENTRY_NUM - regions.len()
    }

    pub fn get_regions_arc(&self) -> Arc<Mutex<HashMap<String, MinidumpRegion>>> {
        Arc::clone(&self.regions)
    }
}

struct Worker {
    queue: Queue,
    mem: GuestMemory,
    state: MinidumpState,
    interrupt: Interrupt,
}

impl Worker {
    /// Process a minidump message using single-direction communication.
    /// The frontend sends a buffer via virtqueue_add_outbuf(), and we modify
    /// it in-place in guest memory, then return the same buffer.
    fn process_message(
        &mut self,
        avail_desc: &mut super::DescriptorChain,
    ) -> anyhow::Result<()> {
        // Get the guest address before reading (we need it to write back)
        let guest_addr = avail_desc
            .reader
            .get_remaining_regions()
            .next()
            .map(|region| region.offset)
            .context("no regions available in descriptor")?;

        // Read the message from the descriptor
        let mut msg = VirtioMinidumpMsg::default();
        avail_desc
            .reader
            .read_exact(msg.as_mut_bytes())
            .context("failed to read minidump message")?;

        let msg_type = msg.msg_type.to_native();
        let name = std::str::from_utf8(&msg.name)
            .unwrap_or("<invalid>")
            .trim_end_matches('\0')
            .to_string();
        let phys_addr = msg.phy_addr.to_native();
        let size = msg.size.to_native();

        // Process the request and set result
        let result = match msg_type {
            MD_SS_ADD_REGION => {
                match self.state.add_region(name.clone(), phys_addr, size) {
                    Ok(()) => {
                        0
                    }
                    Err(e) => {
                        warn!("Failed to add region '{}': {}", name, e);
                        1
                    }
                }
            }
            MD_SS_REMOVE_REGION => {
                match self.state.remove_region(&name) {
                    Ok(()) => {
                        0
                    }
                    Err(e) => {
                        warn!("Failed to remove region '{}': {}", name, e);
                        1
                    }
                }
            }
            MD_SS_UPDATE_REGION => {
                match self.state.update_region(&name, phys_addr, size) {
                    Ok(()) => {
                        0
                    }
                    Err(e) => {
                        warn!("Failed to update region '{}': {}", name, e);
                        1
                    }
                }
            }
            _ => {
                warn!("Unknown message type: {}", msg_type);
                1
            }
        };

        // Update the result field in the message
        msg.result = Le32::from(result);

        // Write the modified message back to guest memory at the original buffer location
        self.mem
            .write_all_at_addr(msg.as_bytes(), GuestAddress(guest_addr))
            .context("failed to write response to guest memory")?;

        Ok(())
    }

    fn process_queue(&mut self) {
        let mut needs_interrupt = false;

        while let Some(mut avail_desc) = self.queue.pop() {
            // Get the message size before processing
            let msg_size = std::mem::size_of::<VirtioMinidumpMsg>() as u32;

            if let Err(e) = self.process_message(&mut avail_desc) {
                error!("Failed to process minidump message: {:#}", e);
            }

            // For single-direction (output-only) communication, we return the same buffer
            // with the result field updated. The "used length" is the message size.
            self.queue.add_used(avail_desc, msg_size);
            needs_interrupt = true;
        }

        if needs_interrupt {
            self.interrupt.signal_used_queue(self.queue.vector());
        }
    }

    fn run(&mut self, kill_evt: Event) -> anyhow::Result<()> {
        #[derive(Debug, EventToken)]
        enum Token {
            QueueAvailable,
            Kill,
        }

        let wait_ctx = WaitContext::build_with(&[
            (self.queue.event(), Token::QueueAvailable),
            (&kill_evt, Token::Kill),
        ])
        .context("failed creating WaitContext")?;

        // This handles the race condition where the frontend kicks the queue
        // before the backend worker thread enters the event loop.
        self.process_queue();

        let mut exiting = false;
        while !exiting {
            let events = wait_ctx.wait().context("failed polling for events")?;

            for event in events.iter().filter(|e| e.is_readable) {
                match event.token {
                    Token::QueueAvailable => {
                        self.queue
                            .event()
                            .wait()
                            .context("failed reading queue event")?;
                        self.process_queue();
                    }
                    Token::Kill => {
                        info!("minidump worker received kill event");
                        exiting = true;
                    }
                }
            }
        }

        Ok(())
    }
}

pub struct Minidump {
    state: MinidumpState,
    worker_thread: Option<WorkerThread<Worker>>,
    virtio_features: u64,
    acked_features: u64,
}

impl Minidump {
    pub fn new(
        base_features: u64,
        output_dir: Option<PathBuf>,
    ) -> anyhow::Result<Minidump> {
        Ok(Minidump {
            state: MinidumpState::new(output_dir),
            worker_thread: None,
            virtio_features: base_features,
            acked_features: 0,
        })
    }

    pub fn get_regions(&self) -> Arc<Mutex<HashMap<String, MinidumpRegion>>> {
        self.state.get_regions_arc()
    }

    pub fn get_regions_arc(&self) -> Arc<Mutex<HashMap<String, MinidumpRegion>>> {
        self.state.get_regions_arc()
    }
}

impl VirtioDevice for Minidump {
    fn keep_rds(&self) -> Vec<base::RawDescriptor> {
        Vec::new()
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Minidump
    }

    fn queue_max_sizes(&self) -> &[u16] {
        QUEUE_SIZES
    }

    fn features(&self) -> u64 {
        self.virtio_features
    }

    fn ack_features(&mut self, value: u64) {
        let mut value = value;
        if value & !self.virtio_features != 0 {
            warn!("virtio_minidump got unknown feature ack {:x}", value);
            value &= self.virtio_features;
        }
        self.acked_features |= value;
    }

    fn activate(
        &mut self,
        mem: GuestMemory,
        interrupt: Interrupt,
        mut queues: BTreeMap<usize, Queue>,
    ) -> anyhow::Result<()> {
        // If a worker thread is already running, stop it first
        // This can happen during re-probe scenarios (e.g., after EPROBE_DEFER)
        if let Some(worker_thread) = self.worker_thread.take() {
            let _worker = worker_thread.stop();
        }

        if queues.len() != 1 {
            return Err(anyhow!("expected 1 queue, got {}", queues.len()));
        }

        let queue = queues.remove(&0).unwrap();
        let state = self.state.clone();

        self.worker_thread = Some(WorkerThread::start("v_minidump", move |kill_evt| {
            let mut worker = Worker {
                queue,
                mem,
                state,
                interrupt,
            };
            if let Err(e) = worker.run(kill_evt) {
                error!("minidump worker thread failed: {:#}", e);
            }
            worker
        }));

        Ok(())
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        // Stop the worker thread if it's running
        if let Some(worker_thread) = self.worker_thread.take() {
            info!("stopping minidump worker thread");
            let _worker = worker_thread.stop();
        }

        // Reset acked features so device can be re-activated
        self.acked_features = 0;

        Ok(())
    }

    fn virtio_sleep(&mut self) -> anyhow::Result<Option<BTreeMap<usize, Queue>>> {
        if let Some(worker_thread) = self.worker_thread.take() {
            let worker = worker_thread.stop();
            return Ok(Some(BTreeMap::from([(0, worker.queue)])));
        }
        Ok(None)
    }

    fn virtio_wake(
        &mut self,
        queues_state: Option<(GuestMemory, Interrupt, BTreeMap<usize, Queue>)>,
    ) -> anyhow::Result<()> {
        if let Some((mem, interrupt, queues)) = queues_state {
            self.activate(mem, interrupt, queues)?;
        }
        Ok(())
    }

    fn virtio_snapshot(&mut self) -> anyhow::Result<AnySnapshot> {
        AnySnapshot::to_any(())
    }

    fn virtio_restore(&mut self, data: AnySnapshot) -> anyhow::Result<()> {
        let () = AnySnapshot::from_any(data)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minidump_state_new() {
        let state = MinidumpState::new(None);
        assert_eq!(state.get_available_regions(), MAX_ENTRY_NUM);
    }

    #[test]
    fn test_add_region() {
        let state = MinidumpState::new(None);
        let result = state.add_region("test_region".to_string(), 0x1000, 0x1000);
        assert!(result.is_ok());
        assert_eq!(state.get_available_regions(), MAX_ENTRY_NUM - 1);
    }

    #[test]
    fn test_add_duplicate_region() {
        let state = MinidumpState::new(None);
        state.add_region("test".to_string(), 0x1000, 0x1000).unwrap();
        let result = state.add_region("test".to_string(), 0x2000, 0x1000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already exists"));
    }

    #[test]
    fn test_remove_region() {
        let state = MinidumpState::new(None);
        state.add_region("test".to_string(), 0x1000, 0x1000).unwrap();
        let result = state.remove_region("test");
        assert!(result.is_ok());
        assert_eq!(state.get_available_regions(), MAX_ENTRY_NUM);
    }

    #[test]
    fn test_remove_nonexistent_region() {
        let state = MinidumpState::new(None);
        let result = state.remove_region("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_update_region() {
        let state = MinidumpState::new(None);
        state.add_region("test".to_string(), 0x1000, 0x1000).unwrap();
        let result = state.update_region("test", 0x2000, 0x2000);
        assert!(result.is_ok());

        let regions = state.regions.lock();
        let region = regions.get("test").unwrap();
        assert_eq!(region.phys_addr, 0x2000);
        assert_eq!(region.size, 0x2000);
    }

    #[test]
    fn test_update_nonexistent_region() {
        let state = MinidumpState::new(None);
        let result = state.update_region("nonexistent", 0x1000, 0x1000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_max_regions() {
        let state = MinidumpState::new(None);

        // Add MAX_ENTRY_NUM regions
        for i in 0..MAX_ENTRY_NUM {
            let name = format!("region_{}", i);
            state.add_region(name, 0x1000 * i as u64, 0x1000).unwrap();
        }

        assert_eq!(state.get_available_regions(), 0);

        // Try to add one more
        let result = state.add_region("overflow".to_string(), 0x1000, 0x1000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Maximum number"));
    }

    #[test]
    fn test_virtio_minidump_msg_size() {
        // Verify message structure size matches expectations
        use std::mem::size_of;
        let expected_size = 48; // With natural alignment: type(4) + name(14) + padding(6) + addr(8) + size(8) + result(4) + padding(4)
        assert_eq!(size_of::<VirtioMinidumpMsg>(), expected_size);
    }
}
