use crate::system::System;
use super::Peripheral;

// RA4M1 CTSU (Capacitive Touch Sensing Unit), real base 0x40081000
// (R7FA4M1AB.h). Register file (byte regs unless noted):
//   CTSUCR0+0x00 (STRT b0 start trigger, CAP b1, SNZ b2, IOC b3,
//     INIT b4 init block, TXVSEL b7), CTSUCR1+0x01 (PON b0 power,
//     CSW b1, ATUNE0/1 b2/b3, CLK[5:4], MD[7:6]), CTSUSDPRS+0x02,
//   CTSUSST+0x03, CTSUMCH0+0x04 / CTSUMCH1+0x05 (channel, 6 bits),
//   CTSUCHAC[5]+0x06 / CTSUCHTRC[5]+0x0B (per-pin enable/TxRx),
//   CTSUDCLKC+0x10, CTSUST+0x11 (SOVF b5 sensor overflow,
//     ROVF b6 reference overflow, clear-by-0), CTSUSSC+0x12 (half),
//   CTSUSO0+0x14 / CTSUSO1+0x16 (offset/count), CTSUSC+0x18 (RO,
//     sensor counter), CTSURC+0x1A (RO, reference counter),
//   CTSUERRS+0x1C (RO, error status), CTSUTRMR+0x20.
// Model: accept-and-retain all config; a 0->1 STRT edge with PON set
// starts a measurement that completes on the next tick (like the ADC,
// conversion is instant on the virtual clock): SC fills from the JS
// override table (`ctsu_set_override`, else a deterministic per-channel
// default), RC fills a fixed reference, overflow sets SOVF for raw
// counts > 16 bits, and CTSU_END (event 68) raises. WRITE (66) / READ
// (67) are DTC transfer requests - unmodeled (polling works). STRT is
// retained (firmware clears it, like HW waiting for software); INIT
// resets the block (running + flags + counters).
pub const CTSU_BASE: u32 = 0x4008_1000;
// ELC events (bsp_elc.h).
pub const CTSU_END_EVENT: u32 = 68;

pub struct RaCtsu {
    cfg: [u8; 0x24],
    sc: u16,
    rc: u16,
    running: bool,
}

impl RaCtsu {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { cfg: [0; 0x24], sc: 0, rc: 0, running: false }))
    }
    fn pon(&self) -> bool { self.cfg[0x01] & 1 != 0 }
    fn channel(&self) -> u32 { (self.cfg[0x04] & 0x3F) as u32 }
    fn complete(&mut self, sys: &System) {
        let ch = self.channel();
        let raw = crate::system::ctsu_get_override(ch)
            .unwrap_or(0x0800 + ch * 0x41);
        if raw > 0xFFFF {
            self.sc = 0xFFFF;
            self.cfg[0x11] |= 1 << 5; // SOVF
        } else {
            self.sc = raw as u16;
        }
        self.rc = 0x3C00;
        self.running = false;
        crate::system::icu_raise_event(sys, CTSU_END_EVENT);
    }
}

impl Peripheral for RaCtsu {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        // Aligned 32-bit packs (the bus right-shifts sub-word reads).
        let o = (offset & !3) as usize;
        let mut w = [0u8; 4];
        for k in 0..4 {
            let i = o + k;
            w[k] = match i {
                0x00..=0x17 | 0x20..=0x23 => *self.cfg.get(i).unwrap_or(&0),
                0x18 | 0x19 => self.sc.to_le_bytes()[i - 0x18],
                0x1A | 0x1B => self.rc.to_le_bytes()[i - 0x1A],
                // ERRS + reserved read 0.
                _ => 0,
            };
        }
        u32::from_le_bytes(w)
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        self.write_sized(sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        // RO counters/ERRS ignore stores; config bytes retain.
        let base = (offset & !3) as usize;
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            let idx = base + byte_offset as usize + i;
            if idx >= 0x24 { continue; }
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            match idx {
                0x18..=0x1F => {} // SC/RC/ERRS/reserved: read-only
                0x11 => {
                    // CTSUST: SOVF/ROVF clear by writing 0, 1s ignored.
                    let cur = self.cfg[0x11];
                    self.cfg[0x11] = (cur & v) | (v & !(0x60));
                }
                _ => {
                    if idx == 0x00 {
                        let was = self.cfg[0x00];
                        // INIT resets the measurement block.
                        if v & (1 << 4) != 0 {
                            self.running = false;
                            self.cfg[0x11] &= !0x60;
                            self.sc = 0;
                            self.rc = 0;
                        }
                        self.cfg[0x00] = v;
                        // STRT 0->1 with power on starts a measurement.
                        if v & 1 != 0 && was & 1 == 0 && self.pon() {
                            self.running = true;
                        }
                        if v & 1 == 0 {
                            self.running = false;
                        }
                        let _ = sys;
                    } else {
                        self.cfg[idx] = v;
                    }
                }
            }
        }
    }
    fn tick(&mut self, sys: &System) {
        if self.running {
            self.complete(sys);
        }
    }
}
