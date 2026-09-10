use crate::system::{System, instruction_count};
use super::Peripheral;

// RA4M1 RTC (real base 0x40083000). Byte-packed registers, bus-correct via
// aligned packs + write_sized: SEC+0x00 MIN+0x02 HR+0x04 DAY+0x06 MON+0x08
// YR+0x0A RCR1+0x0C RCR2+0x0E ALRM+0x10 ALMEN+0x14.
// 100Hz sub-clock via instruction count; alarm IRQ.
pub const RTC_BASE: u32 = 0x4008_3000;

pub struct RaRtc {
    regs: [u8; 0x20],
    last_tick: u64,
    irq_alarm: i32,
}

impl RaRtc {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        let mut regs = [0u8; 0x20];
        regs[0x06] = 1; // day
        regs[0x08] = 1; // mon
        regs[0x0A] = 0xEA; regs[0x0B] = 0x07; // 2026
        Some(Box::new(Self { regs, last_tick: instruction_count(), irq_alarm: 60 }))
    }
    fn running(&self) -> bool { self.regs[0x0E] & 1 != 0 }
    fn advance(&mut self, sys: &System) {
        let now = instruction_count();
        let dt = now.wrapping_sub(self.last_tick);
        self.last_tick = now;
        if !self.running() || dt == 0 { return; }
        let secs = (dt / 480_000) as u32;
        for _ in 0..secs {
            self.regs[0x00] += 1;
            if self.regs[0x00] >= 60 {
                self.regs[0x00] = 0;
                self.regs[0x02] += 1;
                if self.regs[0x02] >= 60 {
                    self.regs[0x02] = 0;
                    self.regs[0x04] += 1;
                    if self.regs[0x04] >= 24 { self.regs[0x04] = 0; }
                }
            }
        }
        if self.regs[0x14] & 7 == 7 {
            let a = u32::from_le_bytes([self.regs[0x10], self.regs[0x11], self.regs[0x12], self.regs[0x13]]);
            if (a & 0x7F) as u8 == self.regs[0x00]
                && ((a >> 8) & 0x7F) as u8 == self.regs[0x02]
                && ((a >> 16) & 0x3F) as u8 == self.regs[0x04] {
                sys.p.nvic.borrow_mut().set_intr_pending(self.irq_alarm);
            }
        }
    }
}

impl Peripheral for RaRtc {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn tick(&mut self, sys: &System) { self.advance(sys); }
    fn read(&mut self, sys: &System, offset: u32) -> u32 {
        self.advance(sys);
        let o = (offset & !3) as usize;
        if o + 4 > self.regs.len() { return 0; }
        u32::from_le_bytes([self.regs[o], self.regs[o+1], self.regs[o+2], self.regs[o+3]])
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        self.write_sized(sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, _sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        let base = (offset & !3) as usize;
        for i in 0..size as usize {
            let idx = base + byte_offset as usize + i;
            if idx >= self.regs.len() { continue; }
            if byte_offset as usize + i >= 4 { continue; } // cross-word tail lives in next word
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            match idx {
                0x00 => self.regs[0x00] = v & 0x7F,
                0x02 => self.regs[0x02] = v & 0x7F,
                0x04 => self.regs[0x04] = v & 0x3F,
                0x06 | 0x08 | 0x0C => self.regs[idx] = v,
                0x0E => {
                    self.regs[0x0E] = v;
                    self.last_tick = instruction_count();
                }
                0x0A | 0x0B | 0x10 | 0x11 | 0x12 | 0x13 | 0x14 => self.regs[idx] = v,
                _ => self.regs[idx] = v,
            }
        }
    }
}
