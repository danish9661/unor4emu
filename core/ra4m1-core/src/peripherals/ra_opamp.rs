use crate::system::System;
use super::Peripheral;

// RA4M1 OPAMP (single block 0x40086000, AMP[4] at +0x0E stride 3) + ACMPLP
// (0x40085E00). MVP: AMPC enable bits gate per-channel loopback outputs;
// AMPMON reports enabled channels; ACMPLP compares poked inputs.
pub const OPAMP_BASE: u32 = 0x4008_6000;
pub const ACMPLP_BASE: u32 = 0x4008_5E00;

pub struct RaOpamp {
    regs: [u8; 0x40],
}

impl RaOpamp {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { regs: [0; 0x40] }))
    }
    fn enabled(&self, ch: usize) -> bool {
        // AMPC at +0x0B: AMPE0-3 channel enables.
        self.regs[0x0B] & (1 << ch) != 0
    }
}

impl Peripheral for RaOpamp {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        let o = (offset & !3) as usize;
        if o + 4 > 0x40 { return 0; }
        if o == 0x0C {
            // AMPMON (+0x0C): live status of enabled channels in low nibble.
            let mut mon = 0u8;
            for ch in 0..4 {
                if self.enabled(ch) { mon |= 1 << ch; }
            }
            let b0 = (self.regs[0x0C] & 0xF0) | mon;
            return u32::from_le_bytes([b0, self.regs[0x0D], self.regs[0x0E], self.regs[0x0F]]);
        }
        // Channel output loopback at +0x30+ch*2: follows poked input +0x20+ch*2.
        if (0x30..0x38).contains(&o) {
            let ch = (o - 0x30) / 2;
            let inp = (self.regs[0x20 + ch * 2] as u16) | ((self.regs[0x20 + ch * 2 + 1] as u16) << 8);
            let v = if self.enabled(ch) { inp & 0xFFF } else { 0 };
            let mut b = [self.regs[o], self.regs[o+1], self.regs[o+2], self.regs[o+3]];
            if o % 2 == 0 {
                b[0] = (v & 0xFF) as u8; b[1] = (v >> 8) as u8;
            }
            return u32::from_le_bytes(b);
        }
        u32::from_le_bytes([self.regs[o], self.regs[o+1], self.regs[o+2], self.regs[o+3]])
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        self.write_sized(_sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, _sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        let base = (offset & !3) as usize;
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            let idx = base + byte_offset as usize + i;
            if idx >= 0x40 { continue; }
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            self.regs[idx] = v;
        }
    }
}

pub struct RaAcmplp {
    cmpcr: u8, cmpmon: u8, in_p: u16, in_m: u16,
}

impl RaAcmplp {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { cmpcr: 0, cmpmon: 0, in_p: 0, in_m: 0 }))
    }
    fn eval(&mut self) {
        self.cmpmon = if self.cmpcr & 1 != 0 && self.in_p > self.in_m { 1 } else { 0 };
    }
}

impl Peripheral for RaAcmplp {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        match offset {
            0x00 => self.cmpcr as u32,
            0x04 => { self.eval(); self.cmpmon as u32 }
            0x10 => self.in_p as u32,
            0x14 => self.in_m as u32,
            _ => 0,
        }
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        match offset {
            0x00 => { self.cmpcr = (value & 0xFF) as u8; self.eval(); }
            0x10 => { self.in_p = (value & 0xFFF) as u16; self.eval(); }
            0x14 => { self.in_m = (value & 0xFFF) as u16; self.eval(); }
            _ => {}
        }
    }
}
