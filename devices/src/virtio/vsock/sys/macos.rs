// Copyright 2026 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[path = "windows/protocol.rs"]
pub mod protocol;

mod vsock;

pub(crate) use protocol::*;
use serde::Deserialize;
use serde::Serialize;
use serde_keyvalue::FromKeyValues;
pub use vsock::Vsock;
pub use vsock::VsockError;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, FromKeyValues)]
#[serde(deny_unknown_fields)]
pub struct VsockConfig {
    pub cid: u64,
}

impl VsockConfig {
    pub fn new(cid: u64) -> Self {
        Self { cid }
    }
}

#[cfg(test)]
mod tests {
    use serde_keyvalue::from_key_values;
    use serde_keyvalue::ErrorKind;
    use serde_keyvalue::ParseError;

    use super::*;

    fn from_vsock_arg(options: &str) -> Result<VsockConfig, ParseError> {
        from_key_values(options)
    }

    #[test]
    fn params_from_key_values() {
        assert_eq!(from_vsock_arg("cid=56").unwrap(), VsockConfig { cid: 56 });
        assert_eq!(
            from_vsock_arg("").unwrap_err(),
            ParseError {
                kind: ErrorKind::SerdeError("missing field `cid`".into()),
                pos: 0
            }
        );
    }
}
