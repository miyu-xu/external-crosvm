// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt;
use std::fmt::Display;
use std::io;
use std::io::Read;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::result;
use std::sync::Arc;

use anyhow::anyhow;
use anyhow::Context;
use base::add_fd_flags;
use base::error;
use base::info;
use base::warn;
use base::AsRawDescriptor;
use base::Error as SysError;
use base::Event;
use base::RawDescriptor;
use base::WorkerThread;
use cros_async::select3;
use cros_async::sync::RwLock;
use cros_async::AsyncError;
use cros_async::AsyncWrapper;
use cros_async::EventAsync;
use cros_async::Executor;
use cros_async::SelectResult;
use data_model::Le32;
use data_model::Le64;
use futures::channel::mpsc;
use futures::channel::oneshot;
use futures::pin_mut;
use futures::select;
use futures::select_biased;
use futures::stream::FuturesUnordered;
use futures::FutureExt;
use futures::SinkExt;
use futures::StreamExt;
use libc;
use remain::sorted;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error as ThisError;
use vm_memory::GuestMemory;
use zerocopy::AsBytes;
use zerocopy::FromBytes;
use zerocopy::FromZeroes;

use super::protocol::virtio_vsock_config;
use super::protocol::virtio_vsock_event;
use super::protocol::virtio_vsock_hdr;
use super::protocol::vsock_op;
use super::protocol::TYPE_STREAM_SOCKET;
use crate::virtio::async_utils;
use crate::virtio::copy_config;
use crate::virtio::create_stop_oneshot;
use crate::virtio::vsock::host_avf_bridge::macos_binder_rpc_uds_path;
use crate::virtio::DescriptorChain;
use crate::virtio::DeviceType;
use crate::virtio::Interrupt;
use crate::virtio::Queue;
use crate::virtio::StoppedWorker;
use crate::virtio::VirtioDevice;
use crate::virtio::Writer;
use crate::Suspendable;

#[sorted]
#[derive(ThisError, Debug)]
pub enum VsockError {
    #[error("Failed to await next descriptor chain from queue: {0}")]
    AwaitQueue(AsyncError),
    #[error("Failed to clone descriptor: {0}")]
    CloneDescriptor(SysError),
    #[error("Failed to create EventAsync: {0}")]
    CreateEventAsync(AsyncError),
    #[error("Failed to create wait context: {0}")]
    CreateWaitContext(SysError),
    #[error("Failed to read queue: {0}")]
    ReadQueue(io::Error),
    #[error("Failed to run executor: {0}")]
    RunExecutor(AsyncError),
    #[error("Failed to write to host socket, port {0}: {1}")]
    WriteFailed(PortPair, io::Error),
    #[error("Failed to write queue: {0}")]
    WriteQueue(io::Error),
}
pub type Result<T> = result::Result<T, VsockError>;

// Vsock has three virt IO queues: rx, tx, and event.
const QUEUE_SIZE: u16 = 256;
const QUEUE_SIZES: &[u16] = &[QUEUE_SIZE, QUEUE_SIZE, QUEUE_SIZE];
// We overload port numbers so that if message is to be received from
// CONNECTION_EVENT_PORT_NUM (invalid port number), we recognize that a
// new connection was set up.
const CONNECTION_EVENT_PORT_NUM: u32 = u32::MAX;

/// Number of bytes in a kilobyte. Used to simplify and clarify buffer size definitions.
const KB: usize = 1024;

/// Size of the buffer we read into from the host side named pipe. Note that data flows from the
/// host pipe -> this buffer -> rx queue.
/// This should be large enough to facilitate fast transmission of host data, see b/232950349.
const TEMP_READ_BUF_SIZE_BYTES: usize = 4 * KB;

/// In the case where the host side named pipe does not have a specified buffer size, we'll default
/// to telling the guest that this is the amount of extra RX space available (e.g. buf_alloc).
/// This should be larger than the volume of data that the guest will generally send at one time to
/// minimize credit update packtes (see MIN_FREE_BUFFER_PCT below).
const DEFAULT_BUF_ALLOC_BYTES: usize = 128 * KB;

/// Minimum free buffer threshold to notify the peer with a credit update
/// message. This is in case we are ingesting many messages without an
/// opportunity to send a message back to the peer with a buffer size update.
/// This value is a percentage of `buf_alloc`.
/// TODO(b/204246759): This value was chosen without much more thought than "it
/// works". It should probably be adjusted, along with DEFAULT_BUF_ALLOC, to a
/// value that makes empirical sense for the packet sizes that we expect to
/// receive.
/// TODO(b/239848326): Replace float with integer, in order to remove risk
/// of losing precision. Ie. change to `10` and perform
/// `FOO * MIN_FREE_BUFFER_PCT / 100`
const MIN_FREE_BUFFER_PCT: f64 = 0.1;

// Number of packets to buffer in the tx processing channels.
const CHANNEL_SIZE: usize = 256;

type VsockConnectionMap = RwLock<HashMap<PortPair, VsockConnection>>;

/// Virtio device for exposing entropy to the guest OS through virtio.
pub struct Vsock {
    guest_cid: u64,
    host_guid: Option<String>,
    features: u64,
    acked_features: u64,
    worker_thread: Option<WorkerThread<Option<(PausedQueues, VsockConnectionMap)>>>,
    /// Stores any active connections when the device sleeps. This allows us to sleep/wake
    /// without disrupting active connections, which is useful when taking a snapshot.
    sleeping_connections: Option<VsockConnectionMap>,
    /// If true, we should send a TRANSPORT_RESET event to the guest at the next opportunity.
    /// Used to inform the guest all connections are broken when we restore a snapshot.
    needs_transport_reset: bool,
}

/// Snapshotted state of Vsock. These fields are serialized in order to validate they haven't
/// changed when this device is restored.
#[derive(Serialize, Deserialize)]
struct VsockSnapshot {
    guest_cid: u64,
    features: u64,
    acked_features: u64,
}

impl Vsock {
    pub fn new(guest_cid: u64, host_guid: Option<String>, base_features: u64) -> Result<Vsock> {
        info!("vsock: macOS device init guest_cid={guest_cid}");
        Ok(Vsock {
            guest_cid,
            host_guid,
            features: base_features,
            acked_features: 0,
            worker_thread: None,
            sleeping_connections: None,
            needs_transport_reset: false,
        })
    }

    fn get_config(&self) -> virtio_vsock_config {
        virtio_vsock_config {
            guest_cid: Le64::from(self.guest_cid),
        }
    }

    fn stop_worker(&mut self) -> StoppedWorker<(PausedQueues, VsockConnectionMap)> {
        if let Some(worker_thread) = self.worker_thread.take() {
            if let Some(queues_and_conns) = worker_thread.stop() {
                StoppedWorker::WithQueues(Box::new(queues_and_conns))
            } else {
                StoppedWorker::MissingQueues
            }
        } else {
            StoppedWorker::AlreadyStopped
        }
    }

    fn start_worker(
        &mut self,
        mem: GuestMemory,
        interrupt: Interrupt,
        mut queues: VsockQueues,
        existing_connections: Option<VsockConnectionMap>,
    ) -> anyhow::Result<()> {
        let rx_queue = queues.rx;
        let tx_queue = queues.tx;
        let event_queue = queues.event;

        let host_guid = self.host_guid.clone();
        let guest_cid = self.guest_cid;
        let needs_transport_reset = self.needs_transport_reset;
        self.needs_transport_reset = false;
        self.worker_thread = Some(WorkerThread::start(
            "userspace_virtio_vsock",
            move |kill_evt| {
                let mut worker = Worker::new(
                    mem,
                    interrupt,
                    host_guid,
                    guest_cid,
                    existing_connections,
                    needs_transport_reset,
                );
                let result = worker.run(rx_queue, tx_queue, event_queue, kill_evt);

                match result {
                    Err(e) => {
                        error!("userspace vsock worker thread exited with error: {:?}", e);
                        None
                    }
                    Ok(paused_queues_and_connections_option) => {
                        paused_queues_and_connections_option
                    }
                }
            },
        ));

        Ok(())
    }
}

impl VirtioDevice for Vsock {
    fn keep_rds(&self) -> Vec<RawDescriptor> {
        Vec::new()
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let config = self.get_config();
        copy_config(data, 0, config.as_bytes(), offset);
        if offset < std::mem::size_of::<virtio_vsock_config>() as u64 {
            info!(
                "vsock: macOS read_config offset={} len={} bytes={:02x?} guest_cid={}",
                offset,
                data.len(),
                data,
                self.guest_cid
            );
        }
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Vsock
    }

    fn queue_max_sizes(&self) -> &[u16] {
        QUEUE_SIZES
    }

    fn features(&self) -> u64 {
        self.features
    }

    fn ack_features(&mut self, value: u64) {
        self.acked_features |= value;
    }

    fn activate(
        &mut self,
        mem: GuestMemory,
        interrupt: Interrupt,
        mut queues: BTreeMap<usize, Queue>,
    ) -> anyhow::Result<()> {
        if queues.len() != QUEUE_SIZES.len() {
            return Err(anyhow!(
                "Failed to activate vsock device. queues.len(): {} != {}",
                queues.len(),
                QUEUE_SIZES.len(),
            ));
        }

        let vsock_queues = VsockQueues {
            rx: queues.remove(&0).unwrap(),
            tx: queues.remove(&1).unwrap(),
            event: queues.remove(&2).unwrap(),
        };

        self.start_worker(mem, interrupt, vsock_queues, None)
    }

    fn virtio_sleep(&mut self) -> anyhow::Result<Option<BTreeMap<usize, Queue>>> {
        match self.stop_worker() {
            StoppedWorker::WithQueues(paused_queues_and_conns) => {
                let (queues, sleeping_connections) = *paused_queues_and_conns;
                self.sleeping_connections = Some(sleeping_connections);
                Ok(Some(queues.into()))
            }
            StoppedWorker::MissingQueues => {
                anyhow::bail!("vsock queue workers did not stop cleanly")
            }
            StoppedWorker::AlreadyStopped => {
                // The device isn't in the activated state.
                Ok(None)
            }
        }
    }

    fn virtio_wake(
        &mut self,
        queues_state: Option<(GuestMemory, Interrupt, BTreeMap<usize, Queue>)>,
    ) -> anyhow::Result<()> {
        if let Some((mem, interrupt, queues)) = queues_state {
            let connections = self.sleeping_connections.take();
            self.start_worker(
                mem,
                interrupt,
                queues
                    .try_into()
                    .expect("Failed to convert queue BTreeMap to VsockQueues"),
                connections,
            )?;
        }
        Ok(())
    }

    fn virtio_snapshot(&mut self) -> anyhow::Result<serde_json::Value> {
        serde_json::to_value(VsockSnapshot {
            guest_cid: self.guest_cid,
            features: self.features,
            acked_features: self.acked_features,
        })
        .context("failed to serialize vsock snapshot")
    }

    fn virtio_restore(&mut self, data: serde_json::Value) -> anyhow::Result<()> {
        let vsock_snapshot: VsockSnapshot =
            serde_json::from_value(data).context("error deserializing vsock snapshot")?;
        anyhow::ensure!(
            self.guest_cid == vsock_snapshot.guest_cid,
            "expected guest_cid to match, but they did not. Live: {}, snapshot: {}",
            self.guest_cid,
            vsock_snapshot.guest_cid
        );
        anyhow::ensure!(
            self.features == vsock_snapshot.features,
            "vsock: expected features to match, but they did not. Live: {}, snapshot: {}",
            self.features,
            vsock_snapshot.features
        );
        self.acked_features = vsock_snapshot.acked_features;
        self.needs_transport_reset = true;

        Ok(())
    }
}

#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
pub struct PortPair {
    host: u32,
    guest: u32,
}

impl Display for PortPair {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "(host port: {}, guest port: {})", self.host, self.guest)
    }
}

impl PortPair {
    fn from_tx_header(header: &virtio_vsock_hdr) -> PortPair {
        PortPair {
            host: header.dst_port.to_native(),
            guest: header.src_port.to_native(),
        }
    }
}

// Note: variables herein do not have to be atomic because this struct is guarded
// by a RwLock.
struct VsockConnection {
    guest_port: Le32,
    /// Connected host-side UDS (non-blocking) bridging to the guest virtio queue.
    host_stream: UnixStream,
    recv_cnt: usize,
    prev_recv_cnt: usize,
    buf_alloc: usize,
    peer_recv_cnt: usize,
    peer_buf_alloc: usize,
    tx_cnt: usize,
    is_buffer_full: bool,
}

struct Worker {
    mem: GuestMemory,
    interrupt: Interrupt,
    host_guid: Option<String>,
    guest_cid: u64,
    // Map of host port to a VsockConnection.
    connections: VsockConnectionMap,
    connection_event: Event,
    device_event_queue_tx: mpsc::Sender<virtio_vsock_event>,
    device_event_queue_rx: Option<mpsc::Receiver<virtio_vsock_event>>,
    send_protocol_reset: bool,
}

impl Worker {
    async fn write_host_bytes_to_guest(
        &self,
        port: PortPair,
        guest_port: Le32,
        buf_alloc: usize,
        recv_cnt: usize,
        data: &[u8],
        recv_queue: &Arc<RwLock<Queue>>,
        rx_queue_evt: &mut EventAsync,
        stop_rx: &mut oneshot::Receiver<()>,
    ) {
        let response_header = virtio_vsock_hdr {
            src_cid: 2.into(),
            dst_cid: self.guest_cid.into(),
            src_port: Le32::from(port.host),
            dst_port: guest_port,
            len: Le32::from(data.len() as u32),
            r#type: TYPE_STREAM_SOCKET.into(),
            op: vsock_op::VIRTIO_VSOCK_OP_RW.into(),
            buf_alloc: Le32::from(buf_alloc as u32),
            fwd_cnt: Le32::from(recv_cnt as u32),
            ..Default::default()
        };

        const HEADER_SIZE: usize = std::mem::size_of::<virtio_vsock_hdr>();
        let mut header_and_data = vec![0u8; HEADER_SIZE + data.len()];
        header_and_data[..HEADER_SIZE].copy_from_slice(response_header.as_bytes());
        header_and_data[HEADER_SIZE..].copy_from_slice(data);
        {
            let mut recv_queue_lock = recv_queue.lock().await;
            let write_fut = self
                .write_bytes_to_queue(
                    &mut recv_queue_lock,
                    &mut *rx_queue_evt,
                    &header_and_data[..],
                )
                .fuse();
            let stop_fut = stop_rx.fuse();
            pin_mut!(write_fut);
            pin_mut!(stop_fut);
            select_biased! {
                write = write_fut => {},
                _ = stop_fut => {}
            }
        }

        let mut connections = self.connections.lock().await;
        if let Some(connection) = connections.get_mut(&port) {
            connection.prev_recv_cnt = recv_cnt;
            connection.tx_cnt += data.len();
        }
    }

    async fn drain_immediate_host_reads(
        &self,
        recv_queue: &Arc<RwLock<Queue>>,
        rx_queue_evt: &mut EventAsync,
        stop_rx: &mut oneshot::Receiver<()>,
    ) -> Result<bool> {
        let mut ready = Vec::new();
        {
            let mut connections = self.connections.lock().await;
            for (port, connection) in connections.iter_mut() {
                let peer_free_buf_size =
                    connection.peer_buf_alloc - (connection.tx_cnt - connection.peer_recv_cnt);
                let max_read = peer_free_buf_size.min(TEMP_READ_BUF_SIZE_BYTES);
                if max_read == 0 {
                    if !connection.is_buffer_full {
                        warn!(
                            "vsock: port {}: Peer has insufficient free buffer space ({} bytes available)",
                            port, peer_free_buf_size
                        );
                        connection.is_buffer_full = true;
                    }
                    continue;
                } else if connection.is_buffer_full {
                    connection.is_buffer_full = false;
                }

                let mut buf = [0u8; TEMP_READ_BUF_SIZE_BYTES];
                match connection.host_stream.read(&mut buf[..max_read]) {
                    Ok(0) => {
                        info!("vsock: port {}: host closed connection (EOF)", port);
                    }
                    Ok(n) => {
                        info!("vsock: port {}: host -> guest {} bytes", port, n);
                        ready.push((
                            *port,
                            connection.guest_port,
                            connection.buf_alloc,
                            connection.recv_cnt,
                            buf[..n].to_vec(),
                        ));
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => {
                        error!("vsock: port {}: read host socket: {}", port, e);
                    }
                }
            }
        }

        let found_ready = !ready.is_empty();
        for (port, guest_port, buf_alloc, recv_cnt, data) in ready {
            self.write_host_bytes_to_guest(
                port,
                guest_port,
                buf_alloc,
                recv_cnt,
                &data,
                recv_queue,
                rx_queue_evt,
                stop_rx,
            )
            .await;
        }
        Ok(found_ready)
    }

    fn new(
        mem: GuestMemory,
        interrupt: Interrupt,
        host_guid: Option<String>,
        guest_cid: u64,
        existing_connections: Option<VsockConnectionMap>,
        send_protocol_reset: bool,
    ) -> Worker {
        // Buffer size here is arbitrary, but must be at least one since we need
        // to be able to write a reset event to the channel when the device
        // worker is brought up on a VM restore. Note that we send exactly one
        // message per VM session, so we should never see these messages backing
        // up.
        let (device_event_queue_tx, device_event_queue_rx) = mpsc::channel(4);

        Worker {
            mem,
            interrupt,
            host_guid,
            guest_cid,
            connections: existing_connections.unwrap_or_default(),
            connection_event: Event::new().unwrap(),
            device_event_queue_tx,
            device_event_queue_rx: Some(device_event_queue_rx),
            send_protocol_reset,
        }
    }

    async fn process_rx_queue(
        &self,
        recv_queue: Arc<RwLock<Queue>>,
        mut rx_queue_evt: EventAsync,
        ex: &Executor,
        mut stop_rx: oneshot::Receiver<()>,
    ) -> Result<()> {
        'connections_changed: loop {
            if self
                .drain_immediate_host_reads(&recv_queue, &mut rx_queue_evt, &mut stop_rx)
                .await?
            {
                continue 'connections_changed;
            }

            let mut futures: FuturesUnordered<
                std::pin::Pin<
                    Box<
                        dyn std::future::Future<Output = result::Result<PortPair, VsockError>>
                            + Send,
                    >,
                >,
            > = FuturesUnordered::new();
            {
                let connections = self.connections.read_lock().await;
                for (port, connection) in connections.iter() {
                    let stream = connection.host_stream.try_clone().map_err(|e| {
                        error!("vsock: could not clone host socket fd: {}", e);
                        VsockError::CloneDescriptor(e.into())
                    })?;
                    let ex = ex.clone();
                    let port = *port;
                    futures.push(Box::pin(async move {
                        let io = ex.async_from(AsyncWrapper::new(stream)).map_err(|e| {
                            error!("vsock: async_from for rx wait: {:?}", e);
                            VsockError::RunExecutor(e)
                        })?;
                        io.wait_readable().await.map_err(|e| {
                            error!("vsock: wait_readable: {:?}", e);
                            VsockError::RunExecutor(e)
                        })?;
                        Ok(port)
                    }));
                }
            }
            let connection_evt_clone = self.connection_event.try_clone().map_err(|e| {
                error!("Could not clone connection_event.");
                VsockError::CloneDescriptor(e)
            })?;
            let connection_evt_async = EventAsync::new(connection_evt_clone, ex).map_err(|e| {
                error!("Could not create EventAsync.");
                VsockError::CreateEventAsync(e)
            })?;
            futures.push(Box::pin(async move {
                let _ = connection_evt_async.next_val().await.map_err(|e| {
                    error!("connection_event wait: {:?}", e);
                    VsockError::RunExecutor(e)
                })?;
                Ok(PortPair {
                    host: CONNECTION_EVENT_PORT_NUM,
                    guest: 0,
                })
            }));

            let futures_len = futures.len();
            let mut ready_chunks = futures.ready_chunks(futures_len);
            let ports = select_biased! {
                ports = ready_chunks.next() => {
                    ports.expect("failed to wait on vsock sockets")
                }
                _ = stop_rx => {
                    break;
                }
            };

            for port in ports {
                let port = match port {
                    Ok(p) => p,
                    Err(e) => {
                        error!("vsock: socket wait future failed: {:?}", e);
                        continue 'connections_changed;
                    }
                };
                if port.host == CONNECTION_EVENT_PORT_NUM {
                    #[cfg(not(target_os = "macos"))]
                    if let Err(e) = self.connection_event.reset() {
                        error!("vsock: could not reset connection_event: {:?}", e);
                    }
                    continue 'connections_changed;
                }
                let mut connections = self.connections.lock().await;
                let connection = if let Some(conn) = connections.get_mut(&port) {
                    conn
                } else {
                    continue 'connections_changed;
                };

                let peer_free_buf_size =
                    connection.peer_buf_alloc - (connection.tx_cnt - connection.peer_recv_cnt);
                let max_read = peer_free_buf_size.min(TEMP_READ_BUF_SIZE_BYTES);
                if max_read == 0 {
                    if !connection.is_buffer_full {
                        warn!(
                            "vsock: port {}: Peer has insufficient free buffer space ({} bytes available)",
                            port, peer_free_buf_size
                        );
                        connection.is_buffer_full = true;
                    }
                    continue;
                } else if connection.is_buffer_full {
                    connection.is_buffer_full = false;
                }

                let guest_port = connection.guest_port;
                let buf_alloc = connection.buf_alloc;
                let recv_cnt = connection.recv_cnt;
                let mut buf = [0u8; TEMP_READ_BUF_SIZE_BYTES];
                let data_size = match connection.host_stream.read(&mut buf[..max_read]) {
                    Ok(0) => {
                        info!("vsock: port {}: host closed connection (EOF)", port);
                        continue 'connections_changed;
                    }
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        continue 'connections_changed;
                    }
                    Err(e) => {
                        error!("vsock: port {}: read host socket: {}", port, e);
                        continue 'connections_changed;
                    }
                };
                info!("vsock: port {}: host -> guest {} bytes", port, data_size);
                let data_read = &buf[..data_size];
                drop(connections);
                self.write_host_bytes_to_guest(
                    port,
                    guest_port,
                    buf_alloc,
                    recv_cnt,
                    data_read,
                    &recv_queue,
                    &mut rx_queue_evt,
                    &mut stop_rx,
                )
                .await;
            }
        }
        Ok(())
    }

    async fn process_tx_queue(
        &self,
        mut queue: Queue,
        mut queue_evt: EventAsync,
        mut process_packets_queue: mpsc::Sender<(virtio_vsock_hdr, Vec<u8>)>,
        mut stop_rx: oneshot::Receiver<()>,
    ) -> Result<Queue> {
        loop {
            // Run continuously until exit evt
            let mut avail_desc = match queue
                .next_async_interruptable(&mut queue_evt, &mut stop_rx)
                .await
            {
                Ok(Some(d)) => d,
                Ok(None) => break,
                Err(e) => {
                    error!("vsock: Failed to read descriptor {}", e);
                    return Err(VsockError::AwaitQueue(e));
                }
            };

            let reader = &mut avail_desc.reader;
            while reader.available_bytes() >= std::mem::size_of::<virtio_vsock_hdr>() {
                let header = match reader.read_obj::<virtio_vsock_hdr>() {
                    Ok(hdr) => hdr,
                    Err(e) => {
                        error!("vsock: Error while reading header: {}", e);
                        break;
                    }
                };

                let len = header.len.to_native() as usize;
                if reader.available_bytes() < len {
                    error!("vsock: Error reading packet data");
                    break;
                }

                let mut data = vec![0_u8; len];
                if len > 0 {
                    if let Err(e) = reader.read_exact(&mut data) {
                        error!("vosck: failed to read data from tx packet: {:?}", e);
                    }
                }

                if let Err(e) = process_packets_queue.send((header, data)).await {
                    error!(
                        "Error while sending packet to queue, dropping packet: {:?}",
                        e
                    )
                };
            }

            queue.add_used(avail_desc, 0);
            queue.trigger_interrupt();
        }

        Ok(queue)
    }

    /// Processes a connection request and returns whether to return a response (true), or reset
    /// (false).
    async fn handle_vsock_connection_request(&self, header: virtio_vsock_hdr) -> bool {
        let port = PortPair::from_tx_header(&header);
        info!("vsock: port {}: Received connection request", port);

        if self.connections.read_lock().await.contains_key(&port) {
            // Connection exists, nothing for us to do.
            warn!(
                "vsock: port {}: accepting connection request on already connected port",
                port
            );
            return true;
        }

        // ChromeOS-style (host_guid set): UDS under /tmp (convention; host must listen).
        // AVF / Android: the guest connects to the host CID endpoint on the destination port.
        let path = if let Some(ref guid) = self.host_guid {
            format!(
                "/tmp/crosvm_vsock_{}_{}.sock",
                guid,
                header.dst_port.to_native()
            )
        } else {
            macos_binder_rpc_uds_path(self.guest_cid, header.dst_port.to_native())
        };

        match UnixStream::connect(&path) {
            Ok(stream) => {
                if let Err(e) = add_fd_flags(stream.as_raw_descriptor(), libc::O_NONBLOCK) {
                    error!("vsock: port {}: set O_NONBLOCK: {:?}", port, e);
                    return false;
                }
                info!("vsock: port {}: connected UDS {}", port, path);
                let buf_alloc = DEFAULT_BUF_ALLOC_BYTES;
                let connection = VsockConnection {
                    guest_port: header.src_port,
                    host_stream: stream,
                    peer_buf_alloc: header.buf_alloc.to_native() as usize,
                    peer_recv_cnt: header.fwd_cnt.to_native() as usize,
                    buf_alloc,
                    recv_cnt: 0_usize,
                    prev_recv_cnt: 0_usize,
                    tx_cnt: 0_usize,
                    is_buffer_full: false,
                };
                self.connections.lock().await.insert(port, connection);
                self.connection_event.signal().unwrap_or_else(|_| {
                    panic!(
                        "Failed to signal new connection event for vsock port {}.",
                        port
                    )
                });
                info!("vsock: port {}: signaled connection ready", port);
                true
            }
            Err(e) => {
                info!(
                    "vsock: No host listening on {} for port {} (err: {:?})",
                    path, port, e
                );
                false
            }
        }
    }

    async fn handle_vsock_guest_data(
        &self,
        header: virtio_vsock_hdr,
        data: &[u8],
        ex: &Executor,
    ) -> Result<()> {
        let port = PortPair::from_tx_header(&header);
        let stream = {
            let mut connections = self.connections.lock().await;
            if let Some(connection) = connections.get_mut(&port) {
                connection.peer_recv_cnt = header.fwd_cnt.to_native() as usize;
                connection.peer_buf_alloc = header.buf_alloc.to_native() as usize;
                connection
                    .host_stream
                    .try_clone()
                    .map_err(|e| VsockError::WriteFailed(port, e))?
            } else {
                error!(
                    "vsock: Guest attempted to send data packet over unconnected \
                            port ({}), dropping packet",
                    port
                );
                return Ok(());
            }
        };
        let io = ex
            .async_from(AsyncWrapper::new(stream))
            .map_err(VsockError::RunExecutor)?;
        let (written, _) = io
            .write_from_vec(None, data.to_vec())
            .await
            .map_err(VsockError::RunExecutor)?;
        info!("vsock: port {}: guest -> host {} bytes", port, written);
        if written != data.len() {
            return Err(VsockError::WriteFailed(
                port,
                io::Error::new(
                    io::ErrorKind::Other,
                    format!(
                        "port {} short write: expected {}, wrote {}",
                        port,
                        data.len(),
                        written
                    ),
                ),
            ));
        }
        let mut connections = self.connections.lock().await;
        if let Some(connection) = connections.get_mut(&port) {
            connection.recv_cnt += written;
        } else {
            error!(
                "vsock: Guest attempted to send data packet over unconnected \
                        port ({}), dropping packet",
                port
            );
        }
        if let Err(e) = self.connection_event.signal() {
            error!(
                "vsock: port {}: failed to signal rx rescan after guest write: {}",
                port, e
            );
        }
        Ok(())
    }

    async fn process_tx_packets(
        &self,
        send_queue: &Arc<RwLock<Queue>>,
        rx_queue_evt: Event,
        mut packet_recv_queue: mpsc::Receiver<(virtio_vsock_hdr, Vec<u8>)>,
        ex: &Executor,
        mut stop_rx: oneshot::Receiver<()>,
    ) {
        let mut packet_queues = HashMap::new();
        let mut futures = FuturesUnordered::new();
        // Push a pending future that will never complete into FuturesUnordered.
        // This will keep us from spinning on spurious notifications when we
        // don't have any open connections.
        futures.push(std::future::pending::<PortPair>().left_future());

        let mut stop_future = FuturesUnordered::new();
        stop_future.push(stop_rx);
        loop {
            let (new_packet, connection, stop_rx_res) =
                select3(packet_recv_queue.next(), futures.next(), stop_future.next()).await;
            match connection {
                SelectResult::Finished(Some(port)) => {
                    packet_queues.remove(&port);
                }
                SelectResult::Finished(_) => {
                    // This is only triggered when FuturesUnordered completes
                    // all pending futures. Right now, this can never happen, as
                    // we have a pending future that we push that will never
                    // complete.
                }
                SelectResult::Pending(_) => {
                    // Nothing to do.
                }
            };
            match new_packet {
                SelectResult::Finished(Some(packet)) => {
                    let port = PortPair::from_tx_header(&packet.0);
                    let queue = packet_queues.entry(port).or_insert_with(|| {
                        let (send, recv) = mpsc::channel(CHANNEL_SIZE);
                        let event_async = EventAsync::new(
                            rx_queue_evt.try_clone().expect("Failed to clone event"),
                            ex,
                        )
                        .expect("Failed to set up the rx queue event");
                        futures.push(
                            self.process_tx_packets_for_port(
                                port,
                                recv,
                                send_queue,
                                event_async,
                                ex,
                            )
                            .right_future(),
                        );
                        send
                    });
                    // Try to send the packet. Do not block other ports if the queue is full.
                    if let Err(e) = queue.try_send(packet) {
                        error!(
                            "vsock: port {}: error sending packet to queue, dropping packet: {:?}",
                            port, e
                        )
                    }
                }
                SelectResult::Finished(_) => {
                    // Triggers when the channel is closed; no more packets coming.
                    packet_recv_queue.close();
                    return;
                }
                SelectResult::Pending(_) => {
                    // Nothing to do.
                }
            }
            match stop_rx_res {
                SelectResult::Finished(_) => {
                    break;
                }
                SelectResult::Pending(_) => {
                    // Nothing to do.
                }
            }
        }
    }

    async fn process_tx_packets_for_port(
        &self,
        port: PortPair,
        mut packet_recv_queue: mpsc::Receiver<(virtio_vsock_hdr, Vec<u8>)>,
        send_queue: &Arc<RwLock<Queue>>,
        mut rx_queue_evt: EventAsync,
        ex: &Executor,
    ) -> PortPair {
        while let Some((header, data)) = packet_recv_queue.next().await {
            if !self
                .handle_tx_packet(header, &data, send_queue, &mut rx_queue_evt, ex)
                .await
            {
                packet_recv_queue.close();
                if let Ok(Some(_)) = packet_recv_queue.try_next() {
                    warn!("vsock: closing port {} with unprocessed packets", port);
                } else {
                    info!("vsock: closing port {} cleanly", port)
                }
                break;
            }
        }
        port
    }

    async fn handle_tx_packet(
        &self,
        header: virtio_vsock_hdr,
        data: &[u8],
        send_queue: &Arc<RwLock<Queue>>,
        rx_queue_evt: &mut EventAsync,
        ex: &Executor,
    ) -> bool {
        let mut is_open = true;
        let port = PortPair::from_tx_header(&header);
        info!(
            "vsock: port {}: guest tx op={} len={} src_cid={} dst_cid={}",
            port,
            header.op.to_native(),
            header.len.to_native(),
            header.src_cid.to_native(),
            header.dst_cid.to_native()
        );
        match header.op.to_native() {
            vsock_op::VIRTIO_VSOCK_OP_INVALID => {
                error!("vsock: Invalid Operation requested, dropping packet");
            }
            vsock_op::VIRTIO_VSOCK_OP_REQUEST => {
                let (resp_op, buf_alloc, fwd_cnt) =
                    if self.handle_vsock_connection_request(header).await {
                        let connections = self.connections.read_lock().await;

                        connections.get(&port).map_or_else(
                            || {
                                warn!("vsock: port: {} connection closed during connect", port);
                                is_open = false;
                                (vsock_op::VIRTIO_VSOCK_OP_RST, 0, 0)
                            },
                            |conn| {
                                (
                                    vsock_op::VIRTIO_VSOCK_OP_RESPONSE,
                                    conn.buf_alloc as u32,
                                    conn.recv_cnt as u32,
                                )
                            },
                        )
                    } else {
                        is_open = false;
                        (vsock_op::VIRTIO_VSOCK_OP_RST, 0, 0)
                    };

                let response_header = virtio_vsock_hdr {
                    src_cid: { header.dst_cid },
                    dst_cid: { header.src_cid },
                    src_port: { header.dst_port },
                    dst_port: { header.src_port },
                    len: 0.into(),
                    r#type: TYPE_STREAM_SOCKET.into(),
                    op: resp_op.into(),
                    buf_alloc: Le32::from(buf_alloc),
                    fwd_cnt: Le32::from(fwd_cnt),
                    ..Default::default()
                };
                // Safe because virtio_vsock_hdr is a simple data struct and converts cleanly to
                // bytes.
                self.write_bytes_to_queue(
                    &mut *send_queue.lock().await,
                    rx_queue_evt,
                    response_header.as_bytes(),
                )
                .await
                .expect("vsock: failed to write to queue");
                info!(
                    "vsock: port {}: replied {} to connection request",
                    port,
                    if resp_op == vsock_op::VIRTIO_VSOCK_OP_RESPONSE {
                        "success"
                    } else {
                        "reset"
                    },
                );
            }
            vsock_op::VIRTIO_VSOCK_OP_RESPONSE => {
                // TODO(b/237811512): Implement this for host-initiated connections
            }
            vsock_op::VIRTIO_VSOCK_OP_RST => {
                // TODO(b/237811512): Implement this for host-initiated connections
            }
            vsock_op::VIRTIO_VSOCK_OP_SHUTDOWN => {
                // While the header provides flags to specify tx/rx-specific shutdown,
                // we only support full shutdown.
                // TODO(b/237811512): Provide an optimal way to notify host of shutdowns
                // while still maintaining easy reconnections.
                let mut connections = self.connections.lock().await;
                if connections.remove(&port).is_some() {
                    let mut response = virtio_vsock_hdr {
                        src_cid: { header.dst_cid },
                        dst_cid: { header.src_cid },
                        src_port: { header.dst_port },
                        dst_port: { header.src_port },
                        len: 0.into(),
                        r#type: TYPE_STREAM_SOCKET.into(),
                        op: vsock_op::VIRTIO_VSOCK_OP_RST.into(),
                        // There is no buffer on a closed connection
                        buf_alloc: 0.into(),
                        // There is no fwd_cnt anymore on a closed connection
                        fwd_cnt: 0.into(),
                        ..Default::default()
                    };
                    // Safe because virtio_vsock_hdr is a simple data struct and converts cleanly to
                    // bytes
                    self.write_bytes_to_queue(
                        &mut *send_queue.lock().await,
                        rx_queue_evt,
                        response.as_bytes_mut(),
                    )
                    .await
                    .expect("vsock: failed to write to queue");
                    self.connection_event
                        .signal()
                        .expect("vsock: failed to write to event");
                    info!("vsock: port: {}: disconnected by the guest", port);
                } else {
                    error!("vsock: Attempted to close unopened port: {}", port);
                }
                is_open = false;
            }
            vsock_op::VIRTIO_VSOCK_OP_RW => {
                match self.handle_vsock_guest_data(header, data, ex).await {
                    Ok(()) => {
                        if self
                            .check_free_buffer_threshold(header)
                            .await
                            .unwrap_or(false)
                        {
                            // Send a credit update if we're below the minimum free
                            // buffer size. We skip this if the connection is closed,
                            // which could've happened if we were closed on the other
                            // end.
                            info!(
                                "vsock: port {}: Buffer below threshold; sending credit update.",
                                port
                            );
                            self.send_vsock_credit_update(send_queue, rx_queue_evt, header)
                                .await;
                        }
                    }
                    Err(e) => {
                        error!("vsock: port {}: resetting connection: {}", port, e);
                        self.send_vsock_reset(send_queue, rx_queue_evt, header)
                            .await;
                        is_open = false;
                    }
                }
            }
            // An update from our peer with their buffer state, which they are sending
            // (probably) due to a a credit request *we* made.
            vsock_op::VIRTIO_VSOCK_OP_CREDIT_UPDATE => {
                let port = PortPair::from_tx_header(&header);
                let mut connections = self.connections.lock().await;
                if let Some(connection) = connections.get_mut(&port) {
                    connection.peer_recv_cnt = header.fwd_cnt.to_native() as usize;
                    connection.peer_buf_alloc = header.buf_alloc.to_native() as usize;
                } else {
                    error!("vsock: port {}: got credit update on unknown port", port);
                    is_open = false;
                }
            }
            // A request from our peer to get *our* buffer state. We reply to the RX queue.
            vsock_op::VIRTIO_VSOCK_OP_CREDIT_REQUEST => {
                info!(
                    "vsock: port {}: Got credit request from peer; sending credit update.",
                    port,
                );
                self.send_vsock_credit_update(send_queue, rx_queue_evt, header)
                    .await;
            }
            _ => {
                error!(
                    "vsock: port {}: unknown operation requested, dropping packet",
                    port
                );
            }
        }
        is_open
    }

    // Checks if how much free buffer our peer thinks that *we* have available
    // is below our threshold percentage. If the connection is closed, returns `None`.
    async fn check_free_buffer_threshold(&self, header: virtio_vsock_hdr) -> Option<bool> {
        let mut connections = self.connections.lock().await;
        let port = PortPair::from_tx_header(&header);
        connections.get_mut(&port).map(|connection| {
            let threshold: usize = (MIN_FREE_BUFFER_PCT * connection.buf_alloc as f64) as usize;
            connection.buf_alloc - (connection.recv_cnt - connection.prev_recv_cnt) < threshold
        })
    }

    async fn send_vsock_credit_update(
        &self,
        send_queue: &Arc<RwLock<Queue>>,
        rx_queue_evt: &mut EventAsync,
        header: virtio_vsock_hdr,
    ) {
        let mut connections = self.connections.lock().await;
        let port = PortPair::from_tx_header(&header);

        if let Some(connection) = connections.get_mut(&port) {
            let mut response = virtio_vsock_hdr {
                src_cid: { header.dst_cid },
                dst_cid: { header.src_cid },
                src_port: { header.dst_port },
                dst_port: { header.src_port },
                len: 0.into(),
                r#type: TYPE_STREAM_SOCKET.into(),
                op: vsock_op::VIRTIO_VSOCK_OP_CREDIT_UPDATE.into(),
                buf_alloc: Le32::from(connection.buf_alloc as u32),
                fwd_cnt: Le32::from(connection.recv_cnt as u32),
                ..Default::default()
            };

            connection.prev_recv_cnt = connection.recv_cnt;

            // Safe because virtio_vsock_hdr is a simple data struct and converts cleanly
            // to bytes
            self.write_bytes_to_queue(
                &mut *send_queue.lock().await,
                rx_queue_evt,
                response.as_bytes_mut(),
            )
            .await
            .unwrap_or_else(|_| panic!("vsock: port {}: failed to write to queue", port));
        } else {
            error!(
                "vsock: port {}: error sending credit update on unknown port",
                port
            );
        }
    }

    async fn send_vsock_reset(
        &self,
        send_queue: &Arc<RwLock<Queue>>,
        rx_queue_evt: &mut EventAsync,
        header: virtio_vsock_hdr,
    ) {
        let mut connections = self.connections.lock().await;
        let port = PortPair::from_tx_header(&header);
        if let Some(connection) = connections.remove(&port) {
            let mut response = virtio_vsock_hdr {
                src_cid: { header.dst_cid },
                dst_cid: { header.src_cid },
                src_port: { header.dst_port },
                dst_port: { header.src_port },
                len: 0.into(),
                r#type: TYPE_STREAM_SOCKET.into(),
                op: vsock_op::VIRTIO_VSOCK_OP_RST.into(),
                buf_alloc: Le32::from(connection.buf_alloc as u32),
                fwd_cnt: Le32::from(connection.recv_cnt as u32),
                ..Default::default()
            };

            // Safe because virtio_vsock_hdr is a simple data struct and converts cleanly
            // to bytes
            self.write_bytes_to_queue(
                &mut *send_queue.lock().await,
                rx_queue_evt,
                response.as_bytes_mut(),
            )
            .await
            .expect("failed to write to queue");
        } else {
            error!("vsock: port {}: error closing unknown port", port);
        }
    }

    async fn write_bytes_to_queue(
        &self,
        queue: &mut Queue,
        queue_evt: &mut EventAsync,
        bytes: &[u8],
    ) -> Result<()> {
        let mut avail_desc = match queue.next_async(queue_evt).await {
            Ok(d) => d,
            Err(e) => {
                error!("vsock: failed to read descriptor {}", e);
                return Err(VsockError::AwaitQueue(e));
            }
        };
        self.write_bytes_to_queue_inner(queue, avail_desc, bytes)
    }

    async fn write_bytes_to_queue_interruptable(
        &self,
        queue: &mut Queue,
        queue_evt: &mut EventAsync,
        bytes: &[u8],
        mut stop_rx: &mut oneshot::Receiver<()>,
    ) -> Result<()> {
        let mut avail_desc = match queue.next_async_interruptable(queue_evt, stop_rx).await {
            Ok(d) => match d {
                Some(desc) => desc,
                None => return Ok(()),
            },
            Err(e) => {
                error!("vsock: failed to read descriptor {}", e);
                return Err(VsockError::AwaitQueue(e));
            }
        };
        self.write_bytes_to_queue_inner(queue, avail_desc, bytes)
    }

    fn write_bytes_to_queue_inner(
        &self,
        queue: &mut Queue,
        mut desc_chain: DescriptorChain,
        bytes: &[u8],
    ) -> Result<()> {
        let writer = &mut desc_chain.writer;
        let res = writer.write_all(bytes);

        if let Err(e) = res {
            error!(
                "vsock: failed to write {} bytes to queue, err: {:?}",
                bytes.len(),
                e
            );
            return Err(VsockError::WriteQueue(e));
        }

        let bytes_written = writer.bytes_written() as u32;
        if bytes_written > 0 {
            queue.add_used(desc_chain, bytes_written);
            queue.trigger_interrupt();
            Ok(())
        } else {
            error!("vsock: failed to write bytes to queue");
            Err(VsockError::WriteQueue(std::io::Error::new(
                std::io::ErrorKind::Other,
                "failed to write bytes to queue",
            )))
        }
    }

    async fn process_event_queue(
        &self,
        mut queue: Queue,
        mut queue_evt: EventAsync,
        mut stop_rx: oneshot::Receiver<()>,
        mut vsock_event_receiver: mpsc::Receiver<virtio_vsock_event>,
    ) -> Result<Queue> {
        loop {
            let vsock_event = select_biased! {
                vsock_event = vsock_event_receiver.next() => {
                    vsock_event
                }
                _ = stop_rx => {
                    break;
                }
            };
            let vsock_event = match vsock_event {
                Some(event) => event,
                None => break,
            };
            self.write_bytes_to_queue_interruptable(
                &mut queue,
                &mut queue_evt,
                vsock_event.as_bytes(),
                &mut stop_rx,
            )
            .await?;
        }
        Ok(queue)
    }

    fn run(
        mut self,
        rx_queue: Queue,
        tx_queue: Queue,
        event_queue: Queue,
        kill_evt: Event,
    ) -> Result<Option<(PausedQueues, VsockConnectionMap)>> {
        let rx_queue_evt = rx_queue
            .event()
            .try_clone()
            .map_err(VsockError::CloneDescriptor)?;

        // Note that this mutex won't ever be contended because the HandleExecutor is single
        // threaded. We need the mutex for compile time correctness, but technically it is not
        // actually providing mandatory locking, at least not at the moment. If we later use a
        // multi-threaded executor, then this lock will be important.
        let rx_queue_arc = Arc::new(RwLock::new(rx_queue));

        // Run executor / create futures in a scope, preventing extra reference to `rx_queue_arc`.
        let res = {
            let ex = Executor::new().unwrap();

            let rx_evt_async = EventAsync::new(
                rx_queue_evt
                    .try_clone()
                    .map_err(VsockError::CloneDescriptor)?,
                &ex,
            )
            .expect("Failed to set up the rx queue event");
            let mut stop_queue_oneshots = Vec::new();

            let vsock_event_receiver = self
                .device_event_queue_rx
                .take()
                .expect("event queue rx must be present");

            let stop_rx = create_stop_oneshot(&mut stop_queue_oneshots);
            let rx_handler =
                self.process_rx_queue(rx_queue_arc.clone(), rx_evt_async, &ex, stop_rx);
            let rx_handler = rx_handler.fuse();
            pin_mut!(rx_handler);

            let (send, recv) = mpsc::channel(CHANNEL_SIZE);

            let tx_evt_async = EventAsync::new(
                tx_queue
                    .event()
                    .try_clone()
                    .map_err(VsockError::CloneDescriptor)?,
                &ex,
            )
            .expect("Failed to set up the tx queue event");
            let stop_rx = create_stop_oneshot(&mut stop_queue_oneshots);
            let tx_handler = self.process_tx_queue(tx_queue, tx_evt_async, send, stop_rx);
            let tx_handler = tx_handler.fuse();
            pin_mut!(tx_handler);

            let stop_rx = create_stop_oneshot(&mut stop_queue_oneshots);
            let packet_handler =
                self.process_tx_packets(&rx_queue_arc, rx_queue_evt, recv, &ex, stop_rx);
            let packet_handler = packet_handler.fuse();
            pin_mut!(packet_handler);

            let event_evt_async = EventAsync::new(
                event_queue
                    .event()
                    .try_clone()
                    .map_err(VsockError::CloneDescriptor)?,
                &ex,
            )
            .expect("Failed to set up the event queue event");
            let stop_rx = create_stop_oneshot(&mut stop_queue_oneshots);
            let event_handler = self.process_event_queue(
                event_queue,
                event_evt_async,
                stop_rx,
                vsock_event_receiver,
            );
            let event_handler = event_handler.fuse();
            pin_mut!(event_handler);

            // Process any requests to resample the irq value.
            let resample_handler = async_utils::handle_irq_resample(&ex, self.interrupt.clone());
            let resample_handler = resample_handler.fuse();
            pin_mut!(resample_handler);

            let kill_evt = EventAsync::new(kill_evt, &ex).expect("Failed to set up the kill event");
            let kill_handler = kill_evt.next_val();
            pin_mut!(kill_handler);

            let mut device_event_queue_tx = self.device_event_queue_tx.clone();
            if self.send_protocol_reset {
                ex.run_until(async move { device_event_queue_tx.send(
                   virtio_vsock_event {
                       id: virtio_sys::virtio_vsock::virtio_vsock_event_id_VIRTIO_VSOCK_EVENT_TRANSPORT_RESET
                           .into(),
                   }).await
                }).expect("failed to write to empty mpsc queue.");
            }

            ex.run_until(async {
                select! {
                    _ = kill_handler.fuse() => (),
                    _ = rx_handler => return Err(anyhow!("rx_handler stopped unexpetedly")),
                    _ = tx_handler => return Err(anyhow!("tx_handler stop unexpectedly.")),
                    _ = packet_handler => return Err(anyhow!("packet_handler stop unexpectedly.")),
                    _ = event_handler => return Err(anyhow!("event_handler stop unexpectedly.")),
                    _ = resample_handler => return Err(anyhow!("resample_handler stop unexpectedly.")),
                }
                // kill_evt has fired

                for stop_tx in stop_queue_oneshots {
                    if stop_tx.send(()).is_err() {
                        return Err(anyhow!("failed to request stop for queue future"));
                    }
                }

                rx_handler.await.context("Failed to stop rx handler.")?;
                packet_handler.await;

                Ok((
                    tx_handler.await.context("Failed to stop tx handler.")?,
                    event_handler
                        .await
                        .context("Failed to stop event handler.")?,
                ))
            })
        };

        // At this point, a request to stop this worker has been sent or an error has happened in
        // one of the futures, which will stop this worker anyways.

        let queues_and_connections = match res {
            Ok(main_future_res) => match main_future_res {
                Ok((tx_queue, event_handler_queue)) => {
                    let rx_queue = match Arc::try_unwrap(rx_queue_arc) {
                        Ok(queue_rw_lock) => queue_rw_lock.into_inner(),
                        Err(_) => panic!("failed to recover queue from worker"),
                    };
                    let paused_queues = PausedQueues::new(rx_queue, tx_queue, event_handler_queue);
                    Some((paused_queues, self.connections))
                }
                Err(e) => {
                    error!("Error happened in a vsock future: {}", e);
                    None
                }
            },
            Err(e) => {
                error!("error happened in executor: {}", e);
                None
            }
        };

        Ok(queues_and_connections)
    }
}

/// Queues & events for the vsock device.
struct VsockQueues {
    rx: Queue,
    tx: Queue,
    event: Queue,
}

impl TryFrom<BTreeMap<usize, Queue>> for VsockQueues {
    type Error = anyhow::Error;
    fn try_from(mut queues: BTreeMap<usize, Queue>) -> result::Result<Self, Self::Error> {
        if queues.len() < 3 {
            anyhow::bail!(
                "{} queues were found, but an activated vsock must have at 3 active queues.",
                queues.len()
            );
        }

        Ok(VsockQueues {
            rx: queues.remove(&0).context("the rx queue is required.")?,
            tx: queues.remove(&1).context("the tx queue is required.")?,
            event: queues.remove(&2).context("the event queue is required.")?,
        })
    }
}

impl From<PausedQueues> for BTreeMap<usize, Queue> {
    fn from(queues: PausedQueues) -> Self {
        let mut ret = BTreeMap::new();
        ret.insert(0, queues.rx_queue);
        ret.insert(1, queues.tx_queue);
        ret.insert(2, queues.event_queue);
        ret
    }
}

struct PausedQueues {
    rx_queue: Queue,
    tx_queue: Queue,
    event_queue: Queue,
}

impl PausedQueues {
    fn new(rx_queue: Queue, tx_queue: Queue, event_queue: Queue) -> Self {
        PausedQueues {
            rx_queue,
            tx_queue,
            event_queue,
        }
    }
}
