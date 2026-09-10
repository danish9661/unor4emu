use crate::system::System;
use super::Peripheral;

// RA4M1 USBFS (real base 0x40090000). Bring-up stage: accept-and-retain
// register file so RMW init sequences (TinyUSB dcd_init: SYSCFG.SCKE,
// interrupt enable masks) stick and status polls observe their own writes.
// Endpoint FIFOs / SerialUSB streaming come later with the USB device model.
pub const USBFS_BASE: u32 = 0x4009_0000;

pub struct RaUsb {
    regs: std::collections::HashMap<u32, u32>,
}

impl RaUsb {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { regs: Default::default() }))
    }
}

impl Peripheral for RaUsb {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        // Unwritten USB regs read 0 (reset state); written ones retain.
        *self.regs.get(&offset).unwrap_or(&0)
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        self.regs.insert(offset, value);
    }
    fn write_sized(&mut self, sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        // Halfword RMW (LDRH/ORR/STRH init sequences) must stick per-halfword.
        let mut cur = self.read(sys, offset).to_le_bytes();
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            cur[byte_offset as usize + i] =
                ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
        }
        self.write(sys, offset, u32::from_le_bytes(cur));
    }
}
