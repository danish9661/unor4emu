use crate::system::System;
use super::Peripheral;

// RA4M1 PORT + PFS (real bases, RA family).
// PORTn 0x40080000 + n*0x20 (PCNTR1/2/3), PFS 0x40080800, PMISC 0x40080D00.
// MVP: model PDR/PODR/PIDR + output tracking for LED matrix / Arduino D0-D13.
// PFS writes retained (pin function), reads return what was written.
pub const PORT_BASE: u32 = 0x4008_0000;
pub const PFS_BASE: u32 = 0x4008_0800;

pub struct RaPort {
    // per-port: PDR (direction), PODR (output), PIDR (input external)
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
        // PORT slot layout: per port n at +n*0x20: +0x00 PCNTR1(PDR+PODR), +0x02 EORR, +0x04 PORR...
        // MVP: decode port index + register.
        // PFS slot: flat retain map.
        if offset >= 0x800 && offset < 0xD00 {
            // PFS area when this instance serves PFS slot (offset relative to PFS_BASE)
            return *self.pfs.get(&offset).unwrap_or(&0);
        }
        let port = (offset / 0x20) as usize;
        let reg = offset % 0x20;
        if port >= 12 { return 0; }
        match reg {
            0x00 => ((self.pdr[port] as u32) << 16) | (self.podr[port] as u32),
            0x04 => self.pidr[port] as u32,
            _ => *self.pfs.get(&offset).unwrap_or(&0),
        }
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        if offset >= 0x800 && offset < 0x1000 {
            self.pfs.insert(offset, value);
            return;
        }
        let port = (offset / 0x20) as usize;
        let reg = offset % 0x20;
        if port >= 12 { return; }
        match reg {
            0x00 => {
                self.pdr[port] = (value >> 16) as u16;
                self.podr[port] = (value & 0xFFFF) as u16;
            }
            0x02 => self.podr[port] |= (value & 0xFFFF) as u16,   // EORR set bits
            0x04 => self.podr[port] &= !(value as u16),           // PORR clear bits
            _ => { self.pfs.insert(offset, value); }
        }
    }
}
