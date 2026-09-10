use crate::system::System;
use super::Peripheral;

// RA4M1 ADC14 (real base 0x40170000) + DAC12 (0x40171000).
// MVP: ADC single-scan polled conversion; channel values synthetic with
// JS override via adc_set_override (reuse system table, keyed "ADC0").
// DAC: DADR retained, output readable.
pub const ADC_BASE: u32 = 0x4017_0000;
pub const DAC_BASE: u32 = 0x4017_1000;

pub struct RaAdc {
    adcsr: u16,   // +0x00 control/status (ADST start, ADCS group)
    adansa: u32,  // +0x04 channel select A
    addr: [u16; 26], // result regs +0x20..
    cer: u8,
}

impl RaAdc {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { adcsr: 0, adansa: 0, addr: [0; 26], cer: 0 }))
    }
    fn convert(&mut self) {
        for ch in 0..26u32 {
            if self.adansa & (1 << ch) != 0 || ch == 0 {
                let v = crate::system::adc_get_override("ADC0", ch).unwrap_or_else(|| {
                    // Synthetic default: temp/vref pattern per channel.
                    ((ch * 137 + 512) & 0x3FFF) as u32
                });
                self.addr[ch as usize] = (v & 0x3FFF) as u16;
            }
        }
        self.adcsr |= 1 << 7; // ADST stays set while converting; complete instantly
        self.adcsr |= 1 << 5; // ESY? no - set conversion-end flag bit (ADF)
        self.adcsr &= !(1 << 7);
    }
}

impl Peripheral for RaAdc {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        match offset {
            0x00 => self.adcsr as u32,
            0x04 => self.adansa,
            // ADDR area: return aligned pack of two halfwords so odd channels
            // work through the bus right-shift (bus aligns 0x22 -> 0x20, >>16).
            o if (0x20..0x20 + 26 * 2).contains(&o) && o % 4 == 0 => {
                let a = ((o - 0x20) / 2) as usize;
                let lo = *self.addr.get(a).unwrap_or(&0) as u32;
                let hi = *self.addr.get(a + 1).unwrap_or(&0) as u32;
                lo | (hi << 16)
            }
            o if (0x20..0x20 + 26 * 2).contains(&o) => {
                self.addr[((o - 0x20) / 2) as usize] as u32
            }
            _ => 0,
        }
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        match offset {
            0x00 => {
                let start = value & (1 << 7) != 0;
                self.adcsr = (value & 0xFFFF) as u16;
                if start { self.convert(); }
            }
            0x04 => self.adansa = value,
            _ => {}
        }
    }
    fn write_sized(&mut self, sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        // Halfword/byte accesses to ADCSR/ADDR packs.
        let base = (offset & !3) as usize;
        for i in 0..size as usize {
            let idx = base + byte_offset as usize + i;
            if byte_offset as usize + i >= 4 { continue; } // cross-word tail lives in next word
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            match idx {
                0x00 | 0x01 => {
                    let mut cur = self.adcsr.to_le_bytes();
                    cur[idx] = v;
                    let was = self.adcsr;
                    self.adcsr = u16::from_le_bytes(cur);
                    if idx == 0 && v & 0x80 != 0 && was & 0x80 == 0 { self.convert(); }
                    let _ = sys;
                }
                0x04..=0x07 => {
                    let mut cur = self.adansa.to_le_bytes();
                    cur[idx - 0x04] = v;
                    self.adansa = u32::from_le_bytes(cur);
                }
                _ => {}
            }
        }
    }
}

pub struct RaDac {
    dadr: [u16; 2], // +0x00/+0x02 data
    dacr: u8,       // +0x04 control (DAOE0/1)
}

impl RaDac {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { dadr: [0; 2], dacr: 0 }))
    }
}

impl Peripheral for RaDac {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        match offset {
            0x00 => self.dadr[0] as u32,
            0x02 => self.dadr[1] as u32,
            0x04 => self.dacr as u32,
            _ => 0,
        }
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        match offset {
            0x00 => self.dadr[0] = (value & 0xFFF) as u16,
            0x02 => self.dadr[1] = (value & 0xFFF) as u16,
            0x04 => self.dacr = (value & 0xFF) as u8,
            _ => {}
        }
    }
    fn write_sized(&mut self, sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        let base = (offset & !3) as usize;
        for i in 0..size as usize {
            let idx = base + byte_offset as usize + i;
            if byte_offset as usize + i >= 4 { continue; } // cross-word tail lives in next word
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            match idx {
                0x00 | 0x01 => {
                    let mut cur = self.dadr[0].to_le_bytes();
                    cur[idx] = v;
                    self.dadr[0] = u16::from_le_bytes(cur) & 0xFFF;
                }
                0x02 | 0x03 => {
                    let mut cur = self.dadr[1].to_le_bytes();
                    cur[idx - 0x02] = v;
                    self.dadr[1] = u16::from_le_bytes(cur) & 0xFFF;
                }
                0x04 => self.dacr = v,
                _ => { let _ = sys; }
            }
        }
    }
}
