use crate::system::{System, instruction_count};
use super::Peripheral;

// RA4M1 GPT (General PWM Timer), real bases 0x40078000 stride 0x100
// (R7FA4M1AB.h). Register file (word regs):
//   GTWP+0x00 (write protect, accept), GTSTR+0x04 (CSTRT: FSP writes
//   the 1<<channel mask here - bit0-only checks wedge every channel
//   above 0, found via tone() landing on GPT4), GTSTP+0x08 (CSTOP mask,
//   same), GTCLR+0x0C (clear),
//   GTCR+0x2C (CST b0, MD b18:16), GTIOR+0x34 (GTIOA[4:0] + OAE b8,
//   GTIOB[20:16] + OBE b24), GTINTAD+0x38 (retain), GTST+0x3C (TCFA b0,
//   TCFB b1, W0C), GTBER+0x40 (CCRA b17:16, CCRB b19:18, PR b21:20),
//   GTCNT+0x48, GTCCR[6]+0x4C, GTPR+0x64.
// Compare buffering (the analogWrite find): FSP programs duties into
// the BUFFER registers (GTCCRC buffers A, GTCCRD buffers B) with
// GTBER.CCRA/CCRB enabled and relies on silicon to transfer C->A /
// D->B at each period wrap - R_GPT_DutyCycleSet never touches A/B.
// Without the transfer GTCCRA/B stay erased and the output never
// moves, though GTPR/GTIOR/GTSTR all look right.
// Counting is instruction-count driven. Compare-A/B matches and period
// wraps raise the channel's ICU events (GPT0_CCMPA=87 stride 8, OVF=+6)
// unconditionally - routing (IELSR) is the only gate, like silicon
// (the event exists whether or not its IRQ is armed). GPT8-13 have no
// ELC event codes on this part (event 0 = none).
// PWM output: GTIOA/B function 0 with OAE/OBE set means standard
// sawtooth PWM (output 0 on compare match, 1 on cycle end) - this is
// what the FSP default GTIOR (actions 0) yields on silicon, verified
// by duty measurement. Per-channel A/B latches publish to the shared
// output table; PORT pins whose PFS selects a GPT function follow the
// mapped channel (Arduino Minima PWM pin table). Other GTIOA/B
// functions retain without acting (no consumer programs them).
pub const GPT_BASE: u32 = 0x4007_8000; // ch stride 0x100

pub struct RaGpt {
    gtcr: u32,
    cstrt: bool,
    gtcnt: u32,
    gtpr: u32,
    gtccr: [u32; 6],
    gtior: u32,
    gtber: u32,
    gtssr: u32,
    gtcsr: u32,
    gtintad: u32,
    gtst: u32,
    last_tick: u64,
    ccmpa_event: u32,
    ovf_event: u32,
    out_a: bool,
    out_b: bool,
    is32: bool,
    ch: u8,
    started_fresh: bool,
}

impl RaGpt {
    pub fn new(ch: u8) -> Option<Box<dyn Peripheral>> {
        let is32 = ch < 2;
        Some(Box::new(Self {
            gtcr: 0, cstrt: false, gtcnt: 0, gtpr: 0xFFFF_FFFF,
            gtccr: [0; 6], gtior: 0, gtber: 0, gtssr: 0, gtcsr: 0,
            gtintad: 0, gtst: 0,
            last_tick: instruction_count(),
            ccmpa_event: if ch < 8 { 87 + ch as u32 * 8 } else { 0 },
            ovf_event: if ch < 8 { 93 + ch as u32 * 8 } else { 0 },
            out_a: false, out_b: false, is32, ch,
            started_fresh: false,
        }))
    }

    fn running(&self) -> bool { self.gtcr & 1 != 0 || self.cstrt }
    fn gtioa(&self) -> u32 { self.gtior & 0x1F }
    fn gtiob(&self) -> u32 { (self.gtior >> 16) & 0x1F }
    fn oae(&self) -> bool { self.gtior & (1 << 8) != 0 }
    fn obe(&self) -> bool { self.gtior & (1 << 24) != 0 }

    fn publish(&self) {
        crate::system::gpt_out_set(self.ch as usize, 0, self.out_a);
        crate::system::gpt_out_set(self.ch as usize, 1, self.out_b);
    }

    fn fire_ccmpa(&mut self, sys: &System, due: u64) {
        self.gtst |= 1;
        if self.oae() && self.gtioa() == 0 {
            self.out_a = false;
        }
        self.publish();
        crate::system::dmac_notify(sys, self.ccmpa_event, due);
        crate::system::icu_raise_event(sys, self.ccmpa_event);
    }

    fn fire_ccmpb(&mut self, sys: &System, due: u64) {
        self.gtst |= 1 << 1;
        if self.obe() && self.gtiob() == 0 {
            self.out_b = false;
        }
        self.publish();
        crate::system::dmac_notify(sys, self.ccmpa_event + 1, due);
        crate::system::icu_raise_event(sys, self.ccmpa_event + 1);
    }

    fn fire_wrap(&mut self, sys: &System, due: u64) {
        // Period wrap (cycle end): buffer transfer C->A / D->B like
        // silicon when GTBER enables them, output 1 per function 0,
        // TCFPO (GTST b6) + OVF event, then a compare at 0 matches at
        // cycle start on silicon (FSP leaves A/B at reset 0 in PERIODIC
        // mode and relies on the start-edge sample, e.g. the
        // SoftwareSerial start bit; the fresh-start edge comes from
        // advance()'s started_fresh path).
        if self.gtber & (0x3 << 16) != 0 {
            self.gtccr[0] = self.gtccr[2];
        }
        if self.gtber & (0x3 << 18) != 0 {
            self.gtccr[1] = self.gtccr[3];
        }
        if self.oae() && self.gtioa() == 0 {
            self.out_a = true;
        }
        if self.obe() && self.gtiob() == 0 {
            self.out_b = true;
        }
        self.publish();
        self.gtst |= 1 << 6; // TCFPO
        crate::system::dmac_notify(sys, self.ovf_event, due);
        crate::system::icu_raise_event(sys, self.ovf_event);
        // A compare programmed at 0 matches at cycle start on silicon.
        if self.gtccr[0] == 0 {
            self.fire_ccmpa(sys, due);
        }
        if self.gtccr[1] == 0 {
            self.fire_ccmpb(sys, due);
        }
    }

    fn advance(&mut self, sys: &System) {
        let now = instruction_count();
        let dt = now.wrapping_sub(self.last_tick);
        self.last_tick = now;
        if !self.running() || dt == 0 { return; }
        // Prescaler ignored in MVP (counts instructions). Real PCLK/prescale later.
        let mask = if self.is32 { 0xFFFF_FFFF } else { 0xFFFF };
        let pr = (self.gtpr & mask) as u64;
        let cca = (self.gtccr[0] & mask) as u64;
        let ccb = (self.gtccr[1] & mask) as u64;
        // Exact-time multi-wrap walk: a coarse tick (e.g. 48k instructions
        // at 9600 baud = ~10 periods) must raise every compare/overflow
        // with its true virtual timestamp, or event-driven DMA starves.
        // Unprogrammed compares (all-ones, above any period) never fire.
        let base = now.wrapping_sub(dt);
        let mut cur = (self.gtcnt & mask) as u64;
        let mut elapsed: u64 = 0;
        let dt64 = dt as u64;
        // A compare at 0 matches at cycle start (counter == compare on
        // entry): fire once up front so a freshly started timer with
        // reset-zero compares produces its start-edge event. Later
        // zero-hits inside this walk come from fire_wrap's cascade.
        if elapsed == 0 && cur == 0 && self.started_fresh {
            self.started_fresh = false;
            let due0 = base;
            if cca == 0 {
                self.fire_ccmpa(sys, due0);
            }
            if ccb == 0 {
                self.fire_ccmpb(sys, due0);
            }
        }
        while elapsed < dt64 {
            let left = dt64 - elapsed;
            let to_wrap = pr - cur + 1;
            let mut step = left.min(to_wrap);
            // Ties fire together (FSP programs A and B to the same
            // 50% duty in PERIODIC mode - clearing the other flag on a
            // tie wedged CCMPA forever); a strictly earlier compare
            // takes over the step.
            let mut fa = false;
            let mut fb = false;
            if cca <= pr && cur < cca {
                let d = cca - cur;
                if d < step {
                    step = d;
                    fa = true;
                    fb = false;
                } else if d == step {
                    fa = true;
                }
            }
            if ccb <= pr && cur < ccb {
                let d = ccb - cur;
                if d < step {
                    step = d;
                    fb = true;
                    fa = false;
                } else if d == step {
                    fb = true;
                }
            }
            cur += step;
            elapsed += step;
            let due = base.wrapping_add(elapsed);
            if fa {
                self.fire_ccmpa(sys, due);
            }
            if fb {
                self.fire_ccmpb(sys, due);
            }
            if step == to_wrap {
                cur = 0;
                self.fire_wrap(sys, due);
            }
        }
        self.gtcnt = cur as u32;
    }
}

impl Peripheral for RaGpt {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn tick(&mut self, sys: &System) { self.advance(sys); }
    fn read(&mut self, sys: &System, offset: u32) -> u32 {
        self.advance(sys);
        match offset & 0xFF {
            0x00 => 0, // GTWP write-only
            0x04 => if self.cstrt { 1 << self.ch } else { 0 },
            0x08 => 0, // GTSTP write-only
            0x0C => 0, // GTCLR write-only
            0x2C => self.gtcr,
            0x34 => self.gtior,
            0x38 => self.gtintad,
            0x3C => self.gtst,
            0x40 => self.gtber,
            0x10 => self.gtssr,
            0x18 => self.gtcsr,
            0x48 => self.gtcnt,
            0x4C | 0x50 | 0x54 | 0x58 | 0x5C | 0x60 => self.gtccr[((offset & 0xFF) - 0x4C) as usize / 4],
            0x64 => self.gtpr,
            _ => 0,
        }
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        self.advance(sys);
        match offset & 0xFF {
            0x00 => {} // GTWP protect key: accept
            0x04 => {
                // GTSTR takes the 1<<channel mask (silicon: start bits).
                if value & (1 << self.ch) != 0 {
                    if !self.running() {
                        self.started_fresh = true;
                    }
                    self.cstrt = true;
                    self.last_tick = instruction_count();
                }
            }
            0x08 => {
                // GTSTP: stop clears start + CST (channel mask, as GTSTR).
                if value & (1 << self.ch) != 0 {
                    self.cstrt = false;
                    self.gtcr &= !1;
                }
            }
            0x0C => {
                // GTCLR: clear resets the counter.
                if value & 1 != 0 {
                    self.gtcnt = 0;
                    self.last_tick = instruction_count();
                }
            }
            0x2C => {
                if value & 1 != 0 && !self.running() {
                    self.started_fresh = true;
                }
                self.gtcr = value;
                self.last_tick = instruction_count();
            }
            0x34 => self.gtior = value,
            0x38 => self.gtintad = value,
            0x3C => self.gtst &= !value, // W0C
            0x40 => self.gtber = value,
            0x10 => self.gtssr = value,
            0x18 => self.gtcsr = value,
            0x48 => self.gtcnt = value,
            0x4C | 0x50 | 0x54 | 0x58 | 0x5C | 0x60 => {
                self.gtccr[((offset & 0xFF) - 0x4C) as usize / 4] = value;
            }
            0x64 => self.gtpr = value,
            _ => {}
        }
    }
    fn elc_signal(&mut self, _sys: &System, peripheral: u32, _event: u32) {
        // ELC link 0 (GPT_A): start + clear timers whose GTSSR/GTCSR
        // select the GPT_A source (GPT_SOURCE_GPT_A = 1<<16). This is
        // how the SoftwareSerial RX timer starts on the pin edge.
        if peripheral == 0 {
            if self.gtssr & (1 << 16) != 0 {
                if !self.running() {
                    self.started_fresh = true;
                }
                self.cstrt = true;
            }
            if self.gtcsr & (1 << 16) != 0 {
                self.gtcnt = 0;
            }
            self.last_tick = instruction_count();
        }
    }
}
