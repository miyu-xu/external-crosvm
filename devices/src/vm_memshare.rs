// Copyright 2018 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use base::warn;
use std::collections::HashMap;
use std::convert::TryInto;
use std::cmp::min;
use vm_memory::GuestAddress;

use crate::BusDevice;

const MEM_CONTROL: u64 = 0x0;
const MEM_SHARE_ID: u64 = 0x8;
const MEM_REGION_COUNT: u64 = 0x1c;
const MEM_REGION_BASE: u64 = 0x20;

const REGION_SIZE: u64 = 0x10;
const REGION_BASE: u64 = 0x0;
const REGION_PAGE_COUNT: u64 = 0x8;

const CMD_SHARE: u32 = 0xFEED;
const CMD_RETRIEVE: u32 = 0xF00D;

const MAX_REGIONS: u32 = 4;

#[derive(Clone, Copy)]
struct VmMemRegion {
    base: GuestAddress,
    page_count: u32,
}

#[derive(Clone, Default)]
struct VmMemDescriptor {
    region_count: u32,
    regions: Vec<VmMemRegion>,
}

/// A way to tell the VMM to share memory, styled off of FF-A.
pub struct VmMemShareDevice {
    // The memory regions that are actively being shared.
    shared_regions: HashMap<u64, VmMemDescriptor>,

    // The key of the active memory descriptor in the device.
    active_share_key: u64,

    // The memory descriptor active in the device.
    active_share: VmMemDescriptor,
}

enum VmMemShareMmioError {
    InvalidOffset,
    InvalidSize,
    InvalidCommand,
}

impl VmMemShareDevice {
    /// Constructs a VmMemShareDevice device
    pub fn new() -> VmMemShareDevice {
        VmMemShareDevice {
            shared_regions: HashMap::new(),
            active_share_key: 0,
            active_share: VmMemDescriptor::default(),
        }
    }

    fn mmioInput32(data: &[u8]) -> Result<u32, VmMemShareMmioError> {
        if data.len() != std::mem::size_of::<u32>() {
            return Err(VmMemShareMmioError::InvalidSize);
        }
        Ok(u32::from_be_bytes(data.try_into().unwrap()))
    }

    fn mmioInput64(data: &[u8]) -> Result<u64, VmMemShareMmioError> {
        if data.len() != std::mem::size_of::<u64>() {
            return Err(VmMemShareMmioError::InvalidSize);
        }
        Ok(u64::from_be_bytes(data.try_into().unwrap()))
    }

    fn mmioOutput32(data: &mut [u8], val: u32) -> Result<(), VmMemShareMmioError> {
        if data.len() != std::mem::size_of::<u32>() {
            return Err(VmMemShareMmioError::InvalidSize);
        }
        data.copy_from_slice(&val.to_be_bytes());
        Ok(())
    }

    fn mmioOutput64(data: &mut [u8], val: u64) -> Result<(), VmMemShareMmioError> {
        if data.len() != std::mem::size_of::<u64>() {
            return Err(VmMemShareMmioError::InvalidSize);
        }
        data.copy_from_slice(&val.to_be_bytes());
        Ok(())
    }

    fn mmioWrite(&mut self, offset: u64, data: &[u8]) -> Result<(), VmMemShareMmioError> {
        match offset {
            MEM_CONTROL => {
                match Self::mmioInput32(data)? {
                    CMD_SHARE => {
                        // TODO: generate a key and better handle collisions
                        let key = 0xBAD;
                        self.shared_regions.entry(key).or_insert(self.active_share.clone());
                        self.active_share_key = key;
                    }
                    CMD_RETRIEVE => {
                        if let Some(share) = self.shared_regions.get(&self.active_share_key) {
                            self.active_share = share.clone();
                        }
                    }
                    _ => {
                        return Err(VmMemShareMmioError::InvalidCommand);
                    }
                }
            }
            MEM_SHARE_ID => {
                self.active_share_key = Self::mmioInput64(data)?;
            }
            MEM_REGION_COUNT => {
                let count = min(Self::mmioInput32(data)?, MAX_REGIONS);
                self.active_share.region_count = count;
                self.active_share.regions.clear();
                self.active_share.regions.resize(count as usize, VmMemRegion { base: GuestAddress(0), page_count: 0 });
            }
            o if o >= MEM_REGION_BASE => {
                let region_index = ((o - MEM_REGION_BASE) / REGION_SIZE) as usize;
                let region_offset = (o - MEM_REGION_BASE) % REGION_SIZE;
                match region_offset {
                    REGION_BASE => {
                        self.active_share.regions[region_index].base = GuestAddress(Self::mmioInput64(data)?);
                    }
                    REGION_PAGE_COUNT => {
                        self.active_share.regions[region_index].page_count = Self::mmioInput32(data)?;
                    }
                    _ => {
                        return Err(VmMemShareMmioError::InvalidOffset);
                    }
                }
            }
            _ => {
                return Err(VmMemShareMmioError::InvalidOffset);
            }
        }
        Ok(())
    }

    fn mmioRead(&mut self, offset: u64, data: &mut [u8]) -> Result<(), VmMemShareMmioError> {
        match offset {
            MEM_SHARE_ID => {
                Self::mmioOutput64(data, self.active_share_key)?;
            }
            MEM_REGION_COUNT => {
                let count = self.active_share.regions.len() as u32;
                Self::mmioOutput32(data, count)?;
            }
            o if o >= MEM_REGION_BASE => {
                let region_index = ((o - MEM_REGION_BASE) / REGION_SIZE) as usize;
                let region_offset = (o - MEM_REGION_BASE) % REGION_SIZE;
                match region_offset {
                    REGION_BASE => {
                        let base = self.active_share.regions[region_index].base;
                        Self::mmioOutput64(data, base.offset())?;
                    }
                    REGION_PAGE_COUNT => {
                        let page_count = self.active_share.regions[region_index].page_count;
                        Self::mmioOutput32(data, page_count)?;
                    }
                    _ => {
                        return Err(VmMemShareMmioError::InvalidOffset);
                    }
                }
            }
            _ => {
                return Err(VmMemShareMmioError::InvalidOffset);
            }
        }
        Ok(())
    }
}

impl BusDevice for VmMemShareDevice {
    fn debug_label(&self) -> String {
        "VmMemShareDevice".to_owned()
    }

    fn write(&mut self, offset: u64, data: &[u8]) {
        match self.mmioWrite(offset, data) {
            Err(VmMemShareMmioError::InvalidOffset) => {
                warn!("VmMemShareDevice: bad write to {}", offset);
            }
            Err(VmMemShareMmioError::InvalidSize) => {
                warn!("VmMemShareDevice: bad write size of {} to {}",
                      data.len(), offset);
            }
            Err(VmMemShareMmioError::InvalidCommand) => {
                warn!("VmMemShareDevice: unknown command");
            }
            Ok(()) => ()
        }
    }

    fn read(&mut self, offset: u64, data: &mut [u8]) {
        match self.mmioRead(offset, data) {
            Err(VmMemShareMmioError::InvalidOffset) => {
                warn!("VmMemShareDevice: bad read from {}", offset);
            }
            Err(VmMemShareMmioError::InvalidSize) => {
                warn!("VmMemShareDevice: bad read size of {} from {}",
                      data.len(), offset);
            }
            Err(VmMemShareMmioError::InvalidCommand) => {
                panic!("VmMemShareDevice: read should not trigger commands");
            }
            Ok(()) => ()
        }
    }
}
