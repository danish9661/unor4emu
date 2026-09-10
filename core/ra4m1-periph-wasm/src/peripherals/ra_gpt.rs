use crate::system::{System, instruction_count};
use super::Peripheral;

// RA4M1 GPT (General PWM Timer). Real bases: GPT0 0x40078000, stride 0x100.
// MVP subset: GTCR (control/start), GTCNT (counter), GTPR (period),
// GTCCRA/B (compare), GTIOR (IO), GTINTAD (IRQ enable), GTST (status).
// Counting is instruction-count driven like the STM32 Timer model.
// Compare IRQs use ICU event routing (GPT0_CCMPA=87, stride 8 per channel).
pub const GPT_BASE: u32 = 0x4007_8000; // ch stride 0x100

pub struct RaGpt {
    gtcr: u32,
    gtcnt: u32,
    gtpr: u32,
    gtccra: u32,
    gtccrb: u32,
    gtior: u32,
    gtintad: u32,
    gtst: u32,
    last_tick: u64,
    ccmpa_event: u32,
    is32: bool,
    ch: u8,
}

impl RaGpt {
    pub fn new(ch: u8) -> Option<Box<dyn Peripheral>> {
        let is32 = ch < 2;
        Some(Box::new(Self {
            gtcr: 0, gtcnt: 0, gtpr: 0xFFFF_FFFF, gtccra: 0xFFFF_FFFF,
            gtccrb: 0xFFFF_FFFF, gtior: 0, gtintad: 0, gtst: 0,
            last_tick: instruction_count(),
            ccmpa_event: 87 + ch as u32 * 8, is32, ch,
        }))
    }

    fn running(&self) -> bool { self.gtcr & 1 != 0 }

    fn advance(&mut self, sys: &System) {
        let now = instruction_count();
        let dt = now.wrapping_sub(self.last_tick);
        self.last_tick = now;
        if !self.running() || dt == 0 { return; }
        // Prescaler ignored in MVP (counts instructions). Real PCLK/prescale later.
        let mask = if self.is32 { 0xFFFF_FFFF } else { 0xFFFF };
        let mut cnt = self.gtcnt.wrapping_add(dt as u32) & mask;
        // Period wrap
        let pr = self.gtpr & mask;
        if cnt > pr {
            cnt = 0;
            self.gtst |= 1 << 8; // overflow
        }
        // Compare A match
        if cnt == (self.gtccra & mask) {
            self.gtst |= 1;
            if self.gtintad & 1 != 0 {
                crate::system::icu_raise_event(sys, self.ccmpa_event);
            }
        }
        self.gtcnt = cnt;
    }
}

impl Peripheral for RaGpt {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn tick(&mut self, sys: &System) { self.advance(sys); }
    fn read(&mut self, sys: &System, offset: u32) -> u32 {
        self.advance(sys);
        match offset & 0xFF {
            0x00 => self.gtcr,
            0x04 => self.gtcnt,
            0x08 => self.gtpr,
            0x0C => self.gtccra,
            0x10 => self.gtccrb,
            0x14 => self.gtior,
            0x18 => self.gtintad,
            0x1C => self.gtst,
            _ => 0,
        }
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        self.advance(sys);
        match offset & 0xFF {
            0x00 => { self.gtcr = value; self.last_tick = instruction_count(); }
            0x04 => self.gtcnt = value,
            0x08 => self.gtpr = value,
            0x0C => self.gtccra = value,
            0x10 => self.gtccrb = value,
            0x14 => self.gtior = value,
            0x18 => self.gtintad = value,
            0x1C => self.gtst &= !value, // write-1-to-clear style simplified to clear-by-mask
            _ => {}
        }
    }
}
