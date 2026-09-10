use crate::system::System;
use super::Peripheral;

// RA4M1 PORT+PFS (real bases: PORTn 0x40040000+n*0x20, PFS 0x40040800).
// Real per-port layout: PCNTR1+0x00 (PODR+PDR), PCNTR2+0x04 (EIDR+PIDR, RO),
// PCNTR3+0x08 (PORR set + POSR reset, WO), PCNTR4+0x0C (EORR+EOSR, WO).
// FSP digitalWrite uses PORR/POSR, so byte-exact write_sized is required:
// the bus merges sub-word stores into the aligned word and this model
// applies only the targeted bytes (a merged PORR write must NOT clobber
// PCNTR1 like a plain word store would).
pub const PORT_BASE: u32 = 0x4004_0000;
pub const PFS_BASE: u32 = 0x4004_0800;

pub struct RaPort {
    pdr: [u16; 12],
    podr: [u16; 12],
    pidr: [u16; 12],
    pfs: std::collections::HashMap<u32, u32>,
}

impl Default for RaPort {
    fn default() -> Self {
        Self { pdr: [0; 12], podr: [0; 12], pidr: [0; 12], pfs: Default::default() }
    }
}

impl RaPort {
    pub fn new_port() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self::default()))
    }
    pub fn new_pfs() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self::default()))
    }
    pub fn read_output(&self, port: u8, pin: u8) -> bool {
        if (port as usize) < 12 && pin < 16 {
            (self.podr[port as usize] >> pin) & 1 != 0
        } else { false }
    }
    pub fn set_input(&mut self, port: u8, pin: u8, v: bool) {
        if (port as usize) < 12 && pin < 16 {
            if v { self.pidr[port as usize] |= 1 << pin; }
            else { self.pidr[port as usize] &= !(1 << pin); }
        }
    }
}

impl Peripheral for RaPort {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        // PFS slot: flat retain map.
        if offset >= 0x800 {
            return *self.pfs.get(&offset).unwrap_or(&0);
        }
        let port = (offset / 0x20) as usize;
        let reg = (offset & !3) % 0x20;
        if port >= 12 { return 0; }
        match reg {
            0x00 => ((self.pdr[port] as u32) << 16) | (self.podr[port] as u32),
            0x04 => self.pidr[port] as u32,
            // WO registers read 0.
            _ => 0,
        }
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        self.write_sized(sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, _sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        if offset >= 0x800 {
            // PFS slot: retain merged word.
            if byte_offset == 0 && size == 4 {
                self.pfs.insert(offset, value);
            } else {
                let mut cur = self.pfs.get(&offset).copied().unwrap_or(0).to_le_bytes();
                for i in 0..size as usize {
                    if byte_offset as usize + i >= 4 { continue; }
                    cur[byte_offset as usize + i] =
                        ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
                }
                self.pfs.insert(offset, u32::from_le_bytes(cur));
            }
            return;
        }
        let port = (offset / 0x20) as usize;
        if port >= 12 { return; }
        let base = (offset & !3) as usize % 0x20;
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            let reg = base + byte_offset as usize + i;
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            match reg {
                0x00 | 0x01 => {
                    let mut cur = self.podr[port].to_le_bytes();
                    cur[reg] = v;
                    self.podr[port] = u16::from_le_bytes(cur);
                }
                0x02 | 0x03 => {
                    let mut cur = self.pdr[port].to_le_bytes();
                    cur[reg - 0x02] = v;
                    self.pdr[port] = u16::from_le_bytes(cur);
                }
                // PCNTR2 (PIDR/EIDR) is read-only: ignore.
                0x04..=0x07 => {}
                // PORR/EORR: set PODR bits.
                0x08 | 0x09 | 0x0C | 0x0D => {
                    let sh = ((reg % 4) * 8) as u16;
                    self.podr[port] |= (v as u16) << sh;
                }
                // POSR/EOSR: clear PODR bits.
                0x0A | 0x0B | 0x0E | 0x0F => {
                    let bit = (reg - if reg < 0x0C { 0x0A } else { 0x0E }) * 8;
                    self.podr[port] &= !((v as u16) << bit);
                }
                _ => {}
            }
        }
    }
}
