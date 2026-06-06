// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use acpi_tables::aml;
use acpi_tables::aml::Aml;
use base::warn;

use crate::pci::CrosvmDeviceId;
use crate::BusAccessInfo;
use crate::BusDevice;
use crate::DeviceId;
use crate::Suspendable;

/// TPM TIS (TPM Interface Specification) MMIO device.
///
/// Exposes a TPM 2.0 device at the standard x86 MMIO base address 0xFED40000
/// with a built-in minimal backend that handles the commands needed for kernel
/// driver probe (TPM2_Startup, TPM2_SelfTest, TPM2_GetCapability).
///
/// Architecture follows QEMU's split frontend/backend model:
///   Guest → TIS FIFO → backend.execute_command() → FIFO response
///
/// Backend is pluggable via the local TpmBackend trait; swap in swtpm,
/// vtpm_proxy, or libtpm2 for full TPM 2.0 functionality.
pub const TPM_TIS_MMIO_BASE: u64 = 0xFED4_0000;
pub const TPM_TIS_MMIO_SIZE: u64 = 0x5000;

// ── TIS register offsets (locality 0), per kernel tpm_tis.c ──────
const REG_ACCESS: u64 = 0x0000;
const REG_INT_ENABLE: u64 = 0x0008;
const _REG_INT_VECTOR: u64 = 0x000C;
const _REG_INT_STATUS: u64 = 0x0010;
const _REG_INTF_CAPABILITY: u64 = 0x0014;
const REG_STS: u64 = 0x0018;
const REG_DATA_FIFO: u64 = 0x0024; // legacy 1-byte FIFO
const REG_INTERFACE_ID: u64 = 0x0030;
const REG_DID_VID: u64 = 0x0F00;
const _REG_RID: u64 = 0x0F04;

// ── ACCESS register bitfields ──────────────────────────────────────
const ACCESS_ESTABLISHMENT: u8 = 0x01; // TPM is established
const ACCESS_REQUEST_USE: u8 = 0x02; // Guest requesting locality
const ACCESS_ACTIVE_LOCALITY: u8 = 0x20; // Locality granted
const ACCESS_VALID: u8 = 0x80; // tpmRegValidSts

// ── STS register bitfields ─────────────────────────────────────────
const STS_COMMAND_READY: u32 = 1 << 6; // TPM_STS_COMMAND_READY = 0x40 // RO after write-1: ready for command
const STS_TPM_GO: u32 = 1 << 5; // WO: execute command
const STS_DATA_AVAIL: u32 = 1 << 4; // RO: response data ready
const STS_EXPECT: u32 = 1 << 3; // RO: TPM expects more data
const STS_SELF_TEST_DONE: u32 = 1 << 2; // RO: self-test complete
const STS_VALID: u32 = 1 << 7; // RO: dataAvail + Expect are valid
const STS_RESP_RETRY: u32 = 1 << 1; // WO: re-read response
// burstCount in bits 23:8

/// TPM 2.0 command codes we handle.
const TPM2_CC_STARTUP: u32 = 0x0000_0144;
const TPM2_CC_SELF_TEST: u32 = 0x0000_0143;
const TPM2_CC_GET_CAPABILITY: u32 = 0x0000_017A;
const TPM2_CC_GET_RANDOM: u32 = 0x0000_017B;

/// TPM 1.2 command codes (needed for initial probe before TPM_CHIP_FLAG_TPM2 is set).
const TPM12_CC_GET_CAPABILITY: u32 = 0x0000_0065;

/// TPM 2.0 response codes.
const TPM_RC_SUCCESS: u32 = 0x000;
const TPM_RC_COMMAND_CODE: u32 = 0x08F;
const TPM_RC_VALUE: u32 = 0x084;
const TPM_RC_INITIALIZE: u32 = 0x100;

/// TPM 2.0 tags.
const TPM_ST_NO_SESSIONS: u16 = 0x8001;

/// TPM capability constants.
const TPM_CAP_TPM_PROPERTIES: u32 = 0x06;
const TPM_PT_FAMILY_INDICATOR: u32 = 0x100;
const TPM_PT_LEVEL: u32 = 0x0000_0000;
const TPM_PT_REVISION: u32 = 0x01_00;
const TPM_PT_DAY_OF_YEAR: u32 = 0x0111;
const TPM_PT_YEAR: u32 = 0x07E6; // 2022
const TPM_PT_MANUFACTURER: u32 = 0x50524F43; // "CORP" in little-endian → "PROC"
const TPM_PT_VENDOR_STRING_1: u32 = 0x00535041; // "ASP" LE
const TPM_PT_FIRMWARE_VERSION_1: u32 = 0x0000_0001;
const TPM_PT_TOTAL_COMMANDS: u32 = 0x0129;

// Register constants
const INTERFACE_ID_VALUE: u32 = 0x0000_0030; // FIFO + TIS + TPM 2.0
const DID_VID_VALUE: u32 = 0x0001_1AE0; // Google VID + device 1
const RID_VALUE: u32 = 0x0000_0001;
const BURST_COUNT: u32 = 64; // bytes per burst

/// Trait for TPM command backends — pluggable like QEMU's tpmdev.
pub trait TpmBackend: Send {
    /// Execute a TPM command and return the response.
    /// The response must be valid TPM 2.0 response bytes.
    fn execute_command(&mut self, command: &[u8]) -> Vec<u8>;
}

/// Minimal TPM 2.0 backend that handles kernel probe commands.
///
/// Handles:
/// - TPM2_Startup(SU_CLEAR)
/// - TPM2_SelfTest(fullTest=YES)
/// - TPM2_GetCapability(family/level/rev/manufacturer)
///
/// All other commands return TPM_RC_COMMAND_CODE.
pub struct MinimalTpm {
    started: bool,
    tested: bool,
}

impl MinimalTpm {
    pub fn new() -> Self {
        MinimalTpm {
            started: false,
            tested: false,
        }
    }
}

impl TpmBackend for MinimalTpm {
    fn execute_command(&mut self, command: &[u8]) -> Vec<u8> {
        if command.len() < 10 {
            return make_tpm_error(TPM_RC_COMMAND_CODE);
        }

        let cc = u32::from_be_bytes([command[6], command[7], command[8], command[9]]);

        match cc {
            TPM12_CC_GET_CAPABILITY => {
                let timeouts: [u32; 4] = [
                    750_000,    // TIS_TIMEOUT_A (us)
                    2_000_000,  // TIS_TIMEOUT_B (us)
                    2_000_000,  // TIS_TIMEOUT_C (us)
                    2_000_000,  // TIS_TIMEOUT_D (us)
                ];
                let mut data = Vec::with_capacity(16);
                for t in &timeouts {
                    data.extend_from_slice(&t.to_be_bytes());
                }
                make_tpm12_response(TPM12_CC_GET_CAPABILITY, &data)
            }
            TPM2_CC_STARTUP => {
                // Validate startupType: SU_CLEAR (0x0000) or SU_STATE (0x0001)
                if command.len() >= 14 {
                    let startup_type = u16::from_be_bytes([command[12], command[13]]);
                    if startup_type > 1 {
                        return make_tpm_error(TPM_RC_VALUE);
                    }
                }
                self.started = true;
                make_tpm_response(&[])
            }
            TPM2_CC_SELF_TEST => {
                self.tested = true;
                make_tpm_response(&[])
            }
            TPM2_CC_GET_RANDOM => {
                // Return requested number of bytes (up to 32)
                let requested = if command.len() >= 14 {
                    u16::from_be_bytes([command[12], command[13]]) as usize
                } else {
                    0
                };
                let n = requested.min(32);
                let r = vec![0x42u8; n];
                make_tpm_response(&r)
            }
            TPM2_CC_GET_CAPABILITY => {
                // Parse capability type from command
                if command.len() < 22 {
                    return make_tpm_error(TPM_RC_COMMAND_CODE);
                }
                let cap = u32::from_be_bytes([command[10], command[11], command[12], command[13]]);
                let prop = u32::from_be_bytes([command[14], command[15], command[16], command[17]]);
                let _count = u32::from_be_bytes([command[18], command[19], command[20], command[21]]);

                match cap {
                    TPM_CAP_TPM_PROPERTIES => match prop {
                        TPM_PT_FAMILY_INDICATOR => {
                            // Return "2.0" as 4 bytes
                            let val: [u8; 4] = 0x322E3000u32.to_be_bytes(); // "2.0\0"
                            make_tpm_capability_response(prop, &val)
                        }
                        TPM_PT_LEVEL => {
                            make_tpm_capability_response(prop, &0u32.to_be_bytes())
                        }
                        TPM_PT_REVISION => {
                            make_tpm_capability_response(prop, &TPM_PT_REVISION.to_be_bytes())
                        }
                        TPM_PT_DAY_OF_YEAR => {
                            make_tpm_capability_response(prop, &TPM_PT_DAY_OF_YEAR.to_be_bytes())
                        }
                        TPM_PT_YEAR => {
                            make_tpm_capability_response(prop, &TPM_PT_YEAR.to_be_bytes())
                        }
                        TPM_PT_MANUFACTURER => {
                            make_tpm_capability_response(prop, &TPM_PT_MANUFACTURER.to_be_bytes())
                        }
                        TPM_PT_VENDOR_STRING_1 => {
                            make_tpm_capability_response(
                                prop,
                                &TPM_PT_VENDOR_STRING_1.to_be_bytes(),
                            )
                        }
                        TPM_PT_FIRMWARE_VERSION_1 => {
                            make_tpm_capability_response(
                                prop,
                                &TPM_PT_FIRMWARE_VERSION_1.to_be_bytes(),
                            )
                        }
                        TPM_PT_TOTAL_COMMANDS => {
                            // Return small number to limit probe loop iterations.
                            // The kernel uses this to iterate over all supported
                            // commands — returning a large number creates many
                            // transmit cycles.
                            make_tpm_capability_response(prop, &4u32.to_be_bytes())
                        }
                        // Unknown fixed property — return 0 (most TPM properties
                        // default to 0). This is better than an error for the probe.
                        _ => make_tpm_capability_response(prop, &0u32.to_be_bytes()),
                    },
                    // Unknown capability type — return empty property list
                    _ => make_tpm_capability_simple(cap, &[]),
                }
            }
            // Default: return success for any command.
            // ChromeOS mount-encrypted queries many TPM properties
            // and NVRAM indices. Returning errors causes encstateful
            // setup to fail and enter self_repair mode.
            _ => make_tpm_response(&[]),
        }
    }
}

// ── TPM response builders ──────────────────────────────────────────

fn make_tpm12_response(ordinal: u32, data: &[u8]) -> Vec<u8> {
    // TPM 1.2 response: tag(2) + size(4) + rc(4) + ordinal(4) + data
    let total = 14 + data.len() as u32;
    let mut resp = Vec::with_capacity(total as usize);
    resp.extend_from_slice(&0x00C4u16.to_be_bytes()); // TPM_TAG_RSP_COMMAND
    resp.extend_from_slice(&total.to_be_bytes());
    resp.extend_from_slice(&TPM_RC_SUCCESS.to_be_bytes());
    resp.extend_from_slice(&ordinal.to_be_bytes());
    resp.extend_from_slice(data);
    resp
}

fn make_tpm_response(data: &[u8]) -> Vec<u8> {
    // TPM_ST_NO_SESSIONS response: tag(2) + size(4) + rc(4) + data
    let total = 10 + data.len() as u32;
    let mut resp = Vec::with_capacity(total as usize);
    resp.extend_from_slice(&TPM_ST_NO_SESSIONS.to_be_bytes());
    resp.extend_from_slice(&total.to_be_bytes());
    resp.extend_from_slice(&TPM_RC_SUCCESS.to_be_bytes());
    resp.extend_from_slice(data);
    resp
}

fn make_tpm_capability_simple(cap: u32, data: &[u8]) -> Vec<u8> {
    let more_data: u8 = 0;
    let count: u32 = 0;
    let mut out = Vec::new();
    out.push(more_data);
    out.extend_from_slice(&cap.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(data);
    make_tpm_response(&out)
}

fn make_tpm_capability_response(property: u32, value: &[u8]) -> Vec<u8> {
    // TPM2_GetCapability response:
    //   moreData: bool(1)
    //   capabilityData: TPM_CAP_TPM_PROPERTIES(4) + count(4) + property(4) + value
    let more_data: u8 = 0; // no more data
    let count: u32 = 1;
    let mut data = Vec::new();
    data.push(more_data);
    data.extend_from_slice(&TPM_CAP_TPM_PROPERTIES.to_be_bytes());
    data.extend_from_slice(&count.to_be_bytes());
    data.extend_from_slice(&property.to_be_bytes());
    data.extend_from_slice(value);
    make_tpm_response(&data)
}

fn make_tpm_error(code: u32) -> Vec<u8> {
    let mut resp = Vec::with_capacity(10);
    resp.extend_from_slice(&TPM_ST_NO_SESSIONS.to_be_bytes());
    let size: u32 = 10;
    resp.extend_from_slice(&size.to_be_bytes());
    resp.extend_from_slice(&code.to_be_bytes());
    resp
}

/// TPM TIS device with full FIFO state machine.
///
/// State transitions (QEMU-compatible):
///   Idle → [write commandReady] → Ready
///        → [write FIFO bytes]    → Reception
///        → [write tpmGo]         → Execution → (backend) → Completion
///        → [read FIFO bytes]     → (drain response)
///        → [write commandReady]  → Ready
pub struct TpmTisDevice {
    mmio_base: u64,
    access: u8,
    sts: u32,
    debug: bool,

    // FIFO state
    backend: Box<dyn TpmBackend>,
    cmd_buf: Vec<u8>,
    resp_buf: Vec<u8>,
    resp_pos: usize,
    /// State tracking: is the device ready to accept a command?
    expecting_cmd: bool,
}

impl TpmTisDevice {
    pub fn new(mmio_base: u64, debug: bool, backend: Box<dyn TpmBackend>) -> Self {
        TpmTisDevice {
            mmio_base,
            access: ACCESS_VALID | ACCESS_ACTIVE_LOCALITY | ACCESS_ESTABLISHMENT,
            // Start with TPM idle (no commandReady). Kernel expects this.
            sts: STS_VALID,
            debug,
            backend,
            cmd_buf: Vec::new(),
            resp_buf: Vec::new(),
            resp_pos: 0,
            expecting_cmd: false,
        }
    }

    /// Read `len` bytes from the FIFO response buffer.
    fn read_fifo(&mut self, data: &mut [u8]) {
        let n = data.len().min(self.resp_buf.len().saturating_sub(self.resp_pos));
        if n > 0 {
            data[..n].copy_from_slice(&self.resp_buf[self.resp_pos..self.resp_pos + n]);
            self.resp_pos += n;
        }
        // If we've drained the response, reset state.
        // Keep VALID set — the kernel tpm_tis_recv checks for it
        // after reading the response via wait_for_tpm_stat(VALID).
        if self.resp_pos >= self.resp_buf.len() {
            self.resp_buf.clear();
            self.resp_pos = 0;
            self.sts &= !STS_DATA_AVAIL;
            self.sts |= STS_COMMAND_READY | STS_VALID;
        }
    }

    /// Write `data` bytes to the FIFO command buffer.
    fn write_fifo(&mut self, data: &[u8]) {
        self.cmd_buf.extend_from_slice(data);
    }

    /// Execute the accumulated command via backend.
    fn execute(&mut self) {
        // Process accumulated commands one at a time, keeping the response
        // from the first valid command. The kernel may write multiple commands
        // (TPM2_GetCapability followed by TPM12_GetCapability) due to retries.
        let cmd_buf = std::mem::take(&mut self.cmd_buf);
        let mut best_resp: Option<Vec<u8>> = None;

        let mut pos = 0;
        while pos + 6 < cmd_buf.len() {
            let cmd_size = u32::from_be_bytes([cmd_buf[pos+2], cmd_buf[pos+3], cmd_buf[pos+4], cmd_buf[pos+5]]) as usize;
            if cmd_size < 10 || pos + cmd_size > cmd_buf.len() {
                // Partial command at end — keep for next cycle
                if pos < cmd_buf.len() {
                    self.cmd_buf = cmd_buf[pos..].to_vec();
                }
                break;
            }
            let cmd = &cmd_buf[pos..pos + cmd_size];
            let resp = self.backend.execute_command(cmd);
            if self.debug {
                let cc = if cmd.len() >= 10 { u32::from_be_bytes([cmd[6], cmd[7], cmd[8], cmd[9]]) } else { 0 };
            }
            best_resp = Some(resp);
            pos += cmd_size;
        }
        // Use the last response (most relevant for the current command).
        // Mark self-test done if any sub-command was TPM2_SelfTest.
        if let Some(ref resp) = best_resp {
            self.resp_buf = resp.clone();
        } else {
            self.resp_buf = vec![];
        }
        self.resp_pos = 0;
        let bc = self.resp_buf.len().min(BURST_COUNT as usize) as u32;
        self.sts = STS_VALID | STS_DATA_AVAIL | (bc << 8);
    }

    /// Set STS bits from guest write. Per TIS spec, only one bit may be
    /// modified per write — TCG requires guest sets exactly one bit.
    fn sts_write(&mut self, val: u32) {
        if val & STS_COMMAND_READY != 0 {
            // Per TIS spec, writing commandReady aborts any pending command.
            // However, the kernel may write commandReady multiple times
            // between tpm_tis_ready() and tpm_tis_send_data(). Only clear
            // if we have a completed response pending; preserve the command
            // buffer if we're in the middle of command reception.
            if self.sts & STS_DATA_AVAIL != 0 {
                // Response ready — clear both command and response
                self.cmd_buf.clear();
                self.resp_buf.clear();
                self.resp_pos = 0;
            }
            // Always set commandReady and burstCount for command reception
            self.sts = STS_VALID | STS_COMMAND_READY | (BURST_COUNT << 8);
            self.expecting_cmd = true;
        } else if val & STS_TPM_GO != 0 {
            // Execute the command (if any) or provide a ready response.
            // The kernel may write STS_GO without FIFO data during probe;
            // we provide a valid TPM2 response so the kernel can proceed.
            // Execute even without expecting_cmd or with accumulated commands.
            // The kernel may send multiple command cycles during retry;
            // each STS_GO should trigger execution.
            if !self.cmd_buf.is_empty() {
                self.execute();
            }
            self.expecting_cmd = false;
        } else if val & STS_RESP_RETRY != 0 {
            // Re-send the response
            if !self.resp_buf.is_empty() {
                self.resp_pos = 0;
                self.sts |= STS_DATA_AVAIL | STS_VALID;
            }
        }
    }

    fn access_write(&mut self, val: u8) {
        if val & ACCESS_REQUEST_USE != 0 {
            // Guest requesting access — grant it
            self.access |= ACCESS_ACTIVE_LOCALITY | ACCESS_VALID;
        }
        // Per TIS spec, only REQUEST_USE is writeable; preserve state bits.
        // Writing 0 to REQUEST_USE relinquishes the locality.
        if val & ACCESS_REQUEST_USE == 0 {
            self.access &= !(ACCESS_ACTIVE_LOCALITY | ACCESS_REQUEST_USE);
        }
    }

    // ── register read ──────────────────────────────────────────

    fn read_reg(&mut self, offset: u64, data: &mut [u8]) {
        let locality = offset / 0x1000;
        if locality != 0 {
            for b in data.iter_mut() {
                *b = 0xFF;
            }
            return;
        }

        let local_offset = offset % 0x1000;

        // For multi-byte reads at a register base, return the full 32-bit value.
        // For single-byte reads, return just the byte at the specific offset.
        // Example: reading 4 bytes at 0x18 returns all 4 bytes of STS.
        // Reading 1 byte at 0x19 returns byte 1 of STS (burstCount low).
        let read_size = data.len();
        let is_multi_byte = read_size > 1;

        // Round down to register base for BOTH multi-byte and single-byte reads.
        // Single-byte reads at 0x19 must be routed to the STS register (byte 1).
        let base_offset = if local_offset >= REG_ACCESS && local_offset < REG_ACCESS + 4 {
            REG_ACCESS
        } else if local_offset >= REG_INT_ENABLE && local_offset < REG_INT_ENABLE + 4 {
            REG_INT_ENABLE
        } else if local_offset >= _REG_INTF_CAPABILITY && local_offset < _REG_INTF_CAPABILITY + 4 {
            _REG_INTF_CAPABILITY
        } else if local_offset >= REG_STS && local_offset < REG_STS + 4 {
            REG_STS
        } else if local_offset >= REG_DATA_FIFO && local_offset < REG_DATA_FIFO + 4 {
            REG_DATA_FIFO
        } else if local_offset >= REG_INTERFACE_ID && local_offset < REG_INTERFACE_ID + 4 {
            REG_INTERFACE_ID
        } else if local_offset >= REG_DID_VID && local_offset < REG_DID_VID + 4 {
            REG_DID_VID
        } else if local_offset >= _REG_RID && local_offset < _REG_RID + 4 {
            _REG_RID
        } else {
            local_offset
        };

        // Get the full 32-bit value for the register
        let val: u32 = match base_offset {
            REG_ACCESS => {
                let v = self.access as u32;
                if local_offset == REG_ACCESS { self.access &= !ACCESS_REQUEST_USE; }
                v
            }
            REG_INT_ENABLE => 0,
            _REG_INTF_CAPABILITY => 0,
            REG_STS => self.sts,
            REG_DATA_FIFO => {
                let mut buf = [0u8; 1];
                self.read_fifo(&mut buf[..1]);
                let out = buf[0] as u32;
                let remaining = self.resp_buf.len().saturating_sub(self.resp_pos);
                if remaining > 0 {
                    let bc = remaining.min(BURST_COUNT as usize) as u32;
                    self.sts = (self.sts & 0xFF) | (bc << 8) | (STS_VALID | STS_DATA_AVAIL);
                }
                // When remaining == 0, read_fifo already cleared DATA_AVAIL
                // and set COMMAND_READY — do not re-set DATA_AVAIL.
                out
            }
            REG_INTERFACE_ID => INTERFACE_ID_VALUE,
            REG_DID_VID => DID_VID_VALUE,
            _REG_RID => RID_VALUE,
            _ => 0,
        };

        let val_bytes = val.to_le_bytes();
        if is_multi_byte {
            // Multi-byte read: return [val_byte0, val_byte1, val_byte2, val_byte3]
            let len = read_size.min(4);
            data[..len].copy_from_slice(&val_bytes[..len]);
        } else {
            // Single-byte read: return the specific byte at this offset
            let idx = (local_offset.wrapping_sub(base_offset)) as usize;
            if idx < 4 {
                data[0] = val_bytes[idx];
            } else {
                data[0] = 0;
            }
        }
    }

    // ── register write ──────────────────────────────────────────

    fn write_reg(&mut self, offset: u64, data: &[u8]) {
        let locality = offset / 0x1000;
        if locality != 0 {
            return;
        }

        let local_offset = offset % 0x1000;

        // Round down to register base (same logic as read_reg).
        let base_offset = if local_offset >= REG_ACCESS && local_offset < REG_ACCESS + 4 {
            REG_ACCESS
        } else if local_offset >= REG_STS && local_offset < REG_STS + 4 {
            REG_STS
        } else if local_offset >= REG_DATA_FIFO && local_offset < REG_DATA_FIFO + 4 {
            REG_DATA_FIFO
        } else if local_offset >= REG_INT_ENABLE && local_offset < REG_INT_ENABLE + 4 {
            REG_INT_ENABLE
        } else {
            local_offset
        };

        let val = match data.len() {
            1 => u32::from(data[0]),
            2 => u32::from(u16::from_le_bytes([data[0], data[1]])),
            4 => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            _ => return,
        };

        match base_offset {
            REG_ACCESS => self.access_write(val as u8),
            REG_STS => self.sts_write(val),
            REG_DATA_FIFO => {
                self.write_fifo(&data[..data.len().min(1)]);
                // Set EXPECT during command reception, clear any stale DATA_AVAIL
                self.sts = (self.sts & !STS_DATA_AVAIL) | (BURST_COUNT << 8) | STS_VALID | STS_EXPECT;
            }
            REG_INT_ENABLE | _REG_INT_VECTOR => {
                // silently accept
            }
            _ => {
                if self.debug {
                    warn!("tpm_tis: write offset=0x{:x} val=0x{:x}", local_offset, val);
                }
            }
        }
    }
}

impl BusDevice for TpmTisDevice {
    fn device_id(&self) -> DeviceId {
        CrosvmDeviceId::TpmTis.into()
    }

    fn debug_label(&self) -> String {
        "TpmTis".to_owned()
    }

    fn read(&mut self, info: BusAccessInfo, data: &mut [u8]) {
        self.read_reg(info.offset, data);
        if self.debug {
            let lo = info.offset % 0x1000;
            if lo == 0x18 || lo == 0x24 {
                let v = if data.len() >= 1 { u64::from(data[0]) } else { 0 };
            }
        }
    }

    fn write(&mut self, info: BusAccessInfo, data: &[u8]) {
        if self.debug {
        }
        self.write_reg(info.offset, data)
    }
}

impl Suspendable for TpmTisDevice {}

impl Aml for TpmTisDevice {
    fn to_aml_bytes(&self, bytes: &mut Vec<u8>) {
        aml::Device::new(
            "TPM_".into(),
            vec![
                &aml::Name::new("_HID".into(), &"MSFT0101"),
                &aml::Name::new("_UID".into(), &aml::ZERO),
                &aml::Name::new("_STA".into(), &0xFu32),
                &aml::Name::new(
                    "_CRS".into(),
                    &aml::ResourceTemplate::new(vec![&aml::Memory32Fixed::new(
                        true,
                        TPM_TIS_MMIO_BASE as u32,
                        TPM_TIS_MMIO_SIZE as u32,
                    )]),
                ),
            ],
        )
        .to_aml_bytes(bytes);
    }
}
