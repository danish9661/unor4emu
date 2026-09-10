use crate::system::System;
use super::Peripheral;

// RA4M1 OPAMP x4 (0x40085000+ch*0x100, FSP layout) + ACMPLP (0x40085E00).
// MVP: OPAMP enable/gain retain, output follows poked input; ACMPLP enable +
// monitor output from comparison of two poked inputs.
pub const OPAMP_BASE: u32 = 0x4008_5000;
pub const ACMPLP_BASE: u32 = 0x4008_5E00;

pub struct RaOpamp {
    ampc: [u32; 4],
    in_p: [u16; 4],
    in_m: [u16; 4],
}

impl RaOpamp {
    pub fn new(ch: u8) -> Option<Box<dyn Peripheral>> {
        if ch < 4 {
            Some(Box::new(Self { ampc: [0; 4], in_p: [0; 4], in_m: [0; 4] }))
        } else { None }
    }
}

impl Peripheral for RaOpamp {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        let ch = (offset / 0x100) as usize;
        if ch >= 4 { return 0; }
        match offset % 0x100 {
            0x00 => self.ampc[ch],
            // Output: enabled ? in_p (follower MVP) : 0. Test pokes in_p via 0x10.
            0x04 => if self.ampc[ch] & 1 != 0 { self.in_p[ch] as u32 } else { 0 },
            0x10 => self.in_p[ch] as u32,
            0x14 => self.in_m[ch] as u32,
            _ => 0,
        }
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        let ch = (offset / 0x100) as usize;
        if ch >= 4 { return; }
        match offset % 0x100 {
            0x00 => self.ampc[ch] = value,
            0x10 => self.in_p[ch] = (value & 0xFFF) as u16,
            0x14 => self.in_m[ch] = (value & 0xFFF) as u16,
            _ => {}
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
