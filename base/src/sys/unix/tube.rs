// Copyright 2021 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::os::unix::prelude::AsRawFd;
use std::os::unix::prelude::RawFd;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;

use crate::descriptor::AsRawDescriptor;
use crate::descriptor_reflection::deserialize_with_descriptors;
use crate::descriptor_reflection::SerializeDescriptors;
use crate::handle_eintr;
use crate::tube::Error;
use crate::tube::RecvTube;
use crate::tube::Result;
use crate::tube::SendTube;
use crate::BlockingMode;
use crate::FramingMode;
use crate::RawDescriptor;
use crate::ReadNotifier;
use crate::SafeDescriptor;
use crate::ScmSocket;
use crate::StreamChannel;
use crate::UnixSeqpacket;
use crate::SCM_SOCKET_MAX_FD_COUNT;

// This size matches the inline buffer size of CmsgBuffer.
const TUBE_MAX_FDS: usize = 32;
const TUBE_FRAME_HEADER_SIZE: usize = std::mem::size_of::<u64>();
const TUBE_MAX_FRAME_SIZE: usize = 64 * 1024 * 1024;

/// Bidirectional tube that support both send and recv.
#[derive(Serialize, Deserialize)]
pub struct Tube {
    socket: ScmSocket<StreamChannel>,
    #[serde(skip)]
    send_lock: Arc<Mutex<()>>,
    #[serde(skip)]
    recv_lock: Arc<Mutex<()>>,
}

impl Tube {
    /// Create a pair of connected tubes. Request is sent in one direction while response is in the
    /// other direction.
    pub fn pair() -> Result<(Tube, Tube)> {
        // macOS does not support AF_UNIX SOCK_SEQPACKET. Use a framed Unix stream instead of
        // pretending that SOCK_STREAM preserves message boundaries.
        #[cfg(target_os = "macos")]
        let framing_mode = FramingMode::Byte;
        #[cfg(not(target_os = "macos"))]
        let framing_mode = FramingMode::Message;
        let (socket1, socket2) = StreamChannel::pair(BlockingMode::Blocking, framing_mode)
            .map_err(|errno| Error::Pair(std::io::Error::from(errno)))?;
        let tube1 = Tube::new(socket1)?;
        let tube2 = Tube::new(socket2)?;
        Ok((tube1, tube2))
    }

    /// Create a new `Tube` from a `StreamChannel`.
    /// The StreamChannel must use FramingMode::Message (meaning, must use a SOCK_SEQPACKET as the
    /// underlying socket type), otherwise, this method returns an error.
    pub fn new(socket: StreamChannel) -> Result<Tube> {
        match socket.get_framing_mode() {
            FramingMode::Message => {}
            #[cfg(target_os = "macos")]
            FramingMode::Byte => {}
            #[cfg(not(target_os = "macos"))]
            FramingMode::Byte => Err(Error::InvalidFramingMode),
        }
        Ok(Tube {
            socket: socket.try_into().map_err(Error::DupDescriptor)?,
            send_lock: Default::default(),
            recv_lock: Default::default(),
        })
    }

    /// Create a new `Tube` from a UnixSeqpacket. The StreamChannel is implicitly constructed to
    /// have the right FramingMode by being constructed from a UnixSeqpacket.
    pub fn new_from_unix_seqpacket(sock: UnixSeqpacket) -> Result<Tube> {
        Ok(Tube {
            socket: StreamChannel::from_unix_seqpacket(sock)
                .try_into()
                .map_err(Error::DupDescriptor)?,
            send_lock: Default::default(),
            recv_lock: Default::default(),
        })
    }

    /// DO NOT USE this method directly as it will become private soon (b/221484449). Use a
    /// directional Tube pair instead.
    #[deprecated]
    pub fn try_clone(&self) -> Result<Self> {
        let mut tube = self
            .socket
            .inner()
            .try_clone()
            .map(Tube::new)
            .map_err(Error::Clone)??;
        tube.send_lock = Arc::clone(&self.send_lock);
        tube.recv_lock = Arc::clone(&self.recv_lock);
        Ok(tube)
    }

    /// Sends a message via a Tube.
    /// The number of file descriptors that this method can send is limited to `TUBE_MAX_FDS`.
    /// If you want to send more descriptors, use `send_with_max_fds` instead.
    pub fn send<T: Serialize>(&self, msg: &T) -> Result<()> {
        self.send_with_max_fds(msg, TUBE_MAX_FDS)
    }

    /// Sends a message with at most `max_fds` file descriptors via a Tube.
    /// Note that `max_fds` must not exceed `SCM_SOCKET_MAX_FD_COUNT` (= 253).
    pub fn send_with_max_fds<T: Serialize>(&self, msg: &T, max_fds: usize) -> Result<()> {
        if max_fds > SCM_SOCKET_MAX_FD_COUNT {
            return Err(Error::SendTooManyFds);
        }
        let msg_serialize = SerializeDescriptors::new(&msg);
        let msg_json = serde_json::to_vec(&msg_serialize).map_err(Error::Json)?;
        let msg_descriptors = msg_serialize.into_descriptors();

        if msg_descriptors.len() > max_fds {
            return Err(Error::SendTooManyFds);
        }

        self.send_packet(&msg_json, &msg_descriptors)?;
        Ok(())
    }

    /// Recieves a message from a Tube.
    /// If the sender sent file descriptors more than TUBE_MAX_FDS with `send_with_max_fds`, use
    /// `recv_with_max_fds` instead.
    pub fn recv<T: DeserializeOwned>(&self) -> Result<T> {
        self.recv_with_max_fds(TUBE_MAX_FDS)
    }

    /// Recieves a message with at most `max_fds` file descriptors from a Tube.
    pub fn recv_with_max_fds<T: DeserializeOwned>(&self, max_fds: usize) -> Result<T> {
        if max_fds > SCM_SOCKET_MAX_FD_COUNT {
            return Err(Error::RecvTooManyFds);
        }

        let (msg_json, msg_descriptors) = self.recv_packet(max_fds)?;

        deserialize_with_descriptors(|| serde_json::from_slice(&msg_json), msg_descriptors)
            .map_err(Error::Json)
    }

    fn send_packet(&self, msg: &[u8], descriptors: &[RawFd]) -> Result<()> {
        let _guard = self.send_lock.lock().unwrap_or_else(|e| e.into_inner());
        match self.socket.inner().get_framing_mode() {
            FramingMode::Message => {
                handle_eintr!(self.socket.send_with_fds(msg, descriptors)).map_err(Error::Send)?;
            }
            FramingMode::Byte => {
                let mut frame = Vec::with_capacity(TUBE_FRAME_HEADER_SIZE + msg.len());
                frame.extend_from_slice(&(msg.len() as u64).to_be_bytes());
                frame.extend_from_slice(msg);
                let mut written = 0;
                while written < frame.len() {
                    let fds = if written == 0 { descriptors } else { &[] };
                    let count = handle_eintr!(self.socket.send_with_fds(&frame[written..], fds))
                        .map_err(Error::Send)?;
                    if count == 0 {
                        return Err(Error::Disconnected);
                    }
                    written += count;
                }
            }
        }
        Ok(())
    }

    fn recv_packet(&self, max_fds: usize) -> Result<(Vec<u8>, Vec<SafeDescriptor>)> {
        let _guard = self.recv_lock.lock().unwrap_or_else(|e| e.into_inner());
        match self.socket.inner().get_framing_mode() {
            FramingMode::Message => {
                let msg_size =
                    handle_eintr!(self.socket.inner().peek_size()).map_err(Error::Recv)?;
                let mut msg = vec![0u8; msg_size];
                let (size, descriptors) =
                    handle_eintr!(self.socket.recv_with_fds(&mut msg, max_fds))
                        .map_err(Error::Recv)?;
                if size == 0 {
                    return Err(Error::Disconnected);
                }
                msg.truncate(size);
                Ok((msg, descriptors))
            }
            FramingMode::Byte => {
                let mut header = [0u8; TUBE_FRAME_HEADER_SIZE];
                let mut descriptors = Vec::new();
                self.recv_exact_with_fds(&mut header, max_fds, &mut descriptors)?;
                let msg_size = usize::try_from(u64::from_be_bytes(header))
                    .map_err(|_| Error::Recv(std::io::Error::from_raw_os_error(libc::EOVERFLOW)))?;
                if msg_size == 0 {
                    return Err(Error::Disconnected);
                }
                if msg_size > TUBE_MAX_FRAME_SIZE {
                    return Err(Error::Recv(std::io::Error::from_raw_os_error(
                        libc::EMSGSIZE,
                    )));
                }
                let mut msg = vec![0u8; msg_size];
                self.recv_exact_with_fds(&mut msg, max_fds, &mut descriptors)?;
                Ok((msg, descriptors))
            }
        }
    }

    fn recv_exact_with_fds(
        &self,
        mut buf: &mut [u8],
        max_fds: usize,
        descriptors: &mut Vec<SafeDescriptor>,
    ) -> Result<()> {
        while !buf.is_empty() {
            let remaining_fds = max_fds.saturating_sub(descriptors.len());
            let (size, mut received) = handle_eintr!(self.socket.recv_with_fds(buf, remaining_fds))
                .map_err(Error::Recv)?;
            if size == 0 {
                return Err(Error::Disconnected);
            }
            descriptors.append(&mut received);
            buf = &mut buf[size..];
        }
        Ok(())
    }

    pub fn set_send_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        self.socket
            .inner()
            .set_write_timeout(timeout)
            .map_err(Error::SetSendTimeout)
    }

    pub fn set_recv_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        self.socket
            .inner()
            .set_read_timeout(timeout)
            .map_err(Error::SetRecvTimeout)
    }

    #[cfg(feature = "proto_tube")]
    fn send_proto<M: protobuf::Message>(&self, msg: &M) -> Result<()> {
        let bytes = msg.write_to_bytes().map_err(Error::Proto)?;
        let no_fds: [RawFd; 0] = [];

        self.send_packet(&bytes, &no_fds)
    }

    #[cfg(feature = "proto_tube")]
    fn recv_proto<M: protobuf::Message>(&self) -> Result<M> {
        let (msg_bytes, _) = self.recv_packet(TUBE_MAX_FDS)?;
        protobuf::Message::parse_from_bytes(&msg_bytes).map_err(Error::Proto)
    }
}

impl AsRawDescriptor for Tube {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.socket.as_raw_descriptor()
    }
}

impl AsRawFd for Tube {
    fn as_raw_fd(&self) -> RawFd {
        self.socket.inner().as_raw_fd()
    }
}

impl ReadNotifier for Tube {
    fn get_read_notifier(&self) -> &dyn AsRawDescriptor {
        &self.socket
    }
}

impl AsRawDescriptor for SendTube {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.0.as_raw_descriptor()
    }
}

impl AsRawDescriptor for RecvTube {
    fn as_raw_descriptor(&self) -> RawDescriptor {
        self.0.as_raw_descriptor()
    }
}

/// Wrapper for Tube used for sending and receiving protos - avoids extra overhead of serialization
/// via serde_json. Since protos should be standalone objects we do not support sending of file
/// descriptors as a normal Tube would.
#[cfg(feature = "proto_tube")]
pub struct ProtoTube(Tube);

#[cfg(feature = "proto_tube")]
impl ProtoTube {
    pub fn pair() -> Result<(ProtoTube, ProtoTube)> {
        Tube::pair().map(|(t1, t2)| (ProtoTube(t1), ProtoTube(t2)))
    }

    pub fn send_proto<M: protobuf::Message>(&self, msg: &M) -> Result<()> {
        self.0.send_proto(msg)
    }

    pub fn recv_proto<M: protobuf::Message>(&self) -> Result<M> {
        self.0.recv_proto()
    }

    pub fn new_from_unix_seqpacket(sock: UnixSeqpacket) -> Result<ProtoTube> {
        Ok(ProtoTube(Tube::new_from_unix_seqpacket(sock)?))
    }
}

#[cfg(all(feature = "proto_tube", test))]
#[allow(unused_variables)]
mod tests {
    // not testing this proto specifically, just need an existing one to test the ProtoTube.
    use protos::cdisk_spec::ComponentDisk;

    use super::*;

    #[test]
    fn tube_serializes_and_deserializes() {
        let (pt1, pt2) = ProtoTube::pair().unwrap();
        let proto = ComponentDisk {
            file_path: "/some/cool/path".to_string(),
            offset: 99,
            ..ComponentDisk::new()
        };

        pt1.send_proto(&proto).unwrap();

        let recv_proto = pt2.recv_proto().unwrap();

        assert!(proto.eq(&recv_proto));
    }
}
