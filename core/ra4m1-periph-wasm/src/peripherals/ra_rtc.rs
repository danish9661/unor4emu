use crate::system::{System, instruction_count};
use super::Peripheral;

// RA4M1 RTC (real base 0x40044000, R7FA4M1AB.h). Byte-packed BCD
// calendar counters: R64CNT+0x00 (RO, 64Hz), RSECCNT+0x02,
// RMINCNT+0x04, RHRCNT+0x06, RWKCNT+0x08, RDAYCNT+0x0A,
// RMONCNT+0x0C, RYRCNT+0x0E (16-bit year); alarm RSECAR+0x10 …
// RYRAR+0x1C (ENB b7 each); RCR1+0x22 (AIE b0, CIE b1, PIE b2),
// RCR2+0x24 (START b0, RESET b1), RCR4+0x28, RFRH+0x2A, RFRL+0x2C,
// RADJ+0x2E; capture regs retain only. Bus-correct via aligned packs
// + write_sized. Time advances on absolute instruction count
// (480000 instr = 1 second); alarm match raises event 38 when RCR1.AIE
// and all enabled alarm fields match.
pub const RTC_BASE: u32 = 0x4004_4000;

pub struct RaRtc {
    regs: [u8; 0x60],
    last_tick: u64,
}

impl RaRtc {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        let mut regs = [0u8; 0x60];
        regs[0x0A] = 1; // day
        regs[0x0C] = 1; // mon
        regs[0x0E] = 0x26; regs[0x0F] = 0x20; // 2026 BCD
        Some(Box::new(Self { regs, last_tick: instruction_count() }))
    }
    fn running(&self) -> bool { self.regs[0x24] & 1 != 0 }
    fn bump(field: &mut u8, top: u32) -> bool {
        // BCD increment with carry out at `top` (60/24/...).
        let v = (((*field >> 4) * 10 + (*field & 0xF)) + 1) as u32;
        if v >= top {
            *field = 0;
            true
        } else {
            *field = ((v / 10) << 4 | (v % 10)) as u8;
            false
        }
    }
    fn advance(&mut self, sys: &System) {
        let now = instruction_count();
        let dt = now.wrapping_sub(self.last_tick);
        if dt == 0 { return; }
        // Carry the sub-second remainder: small cadences (48k test
        // chunks) must accumulate instead of discarding the fraction.
        self.last_tick = now.wrapping_sub(dt % 480_000);
        if self.running() {
            let secs = (dt / 480_000) as u32;
            for _ in 0..secs {
                if Self::bump(&mut self.regs[0x02], 60) && Self::bump(&mut self.regs[0x04], 60) {
                    Self::bump(&mut self.regs[0x06], 24);
                }
            }
            // Alarm: all ENB-gated fields must match, plus RCR1.AIE.
            if self.regs[0x22] & 1 != 0 {
                let pairs = [(0x10, 0x02, 0x7F), (0x12, 0x04, 0x7F), (0x14, 0x06, 0x3F)];
                if pairs.iter().all(|&(a, c, m)| {
                    self.regs[a] & 0x80 == 0 || (self.regs[a] & m) == (self.regs[c] & m)
                }) {
                    crate::system::icu_raise_event(sys, 38); // ELC_EVENT_RTC_ALARM
                }
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
        if o == 0x00 {
            // R64CNT derives from the absolute sub-second phase (64Hz).
            let r64 = ((instruction_count() % 480_000) * 64 / 480_000) & 0x7F;
            return (r64 as u32) | ((self.regs[0x01] as u32) << 8)
                | ((self.regs[0x02] as u32) << 16) | ((self.regs[0x03] as u32) << 24);
        }
        if o + 4 > self.regs.len() { return 0; }
        u32::from_le_bytes([self.regs[o], self.regs[o+1], self.regs[o+2], self.regs[o+3]])
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        self.write_sized(sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, _sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        if std::env::var("RTCLOG").is_ok() {
            eprintln!("RTCLOG wr off={:#x} val={:#x} bo={} sz={}", offset, value, byte_offset, size);
        }
        let base = (offset & !3) as usize;
        for i in 0..size as usize {
            let idx = base + byte_offset as usize + i;
            if idx >= self.regs.len() { continue; }
            if byte_offset as usize + i >= 4 { continue; } // cross-word tail lives in next word
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            // R64CNT (0x00) is read-only; everything else retains.
            if idx == 0x00 { continue; }
            self.regs[idx] = v;
            if idx == 0x24 {
                self.last_tick = instruction_count();
                // RESET (b1) is a strobe: HW resets the counters and
                // self-clears (FSP polls for the clear after open).
                if v & (1 << 1) != 0 {
                    for b in [0x02u8, 0x04, 0x06, 0x08, 0x0A, 0x0C, 0x0E, 0x0F] {
                        self.regs[b as usize] = 0;
                    }
                    self.regs[0x24] &= !(1 << 1);
                }
            }
        }
    }
}
