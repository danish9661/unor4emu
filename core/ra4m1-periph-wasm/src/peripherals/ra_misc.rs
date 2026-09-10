use crate::system::{System, instruction_count};
use super::Peripheral;

// RA4M1 misc small blocks, real bases:
// ELC 0x40082000 (routing table), AGT0 0x400E8000 / AGT1 0x400E8100,
// WDT 0x40083400, IWDT 0x40083200, CRC 0x40108000, DOC 0x40109000.
// All accept-and-model: ELC dispatches link events, AGT counts, WDT/IWDT
// raise the shared watchdog flags, CRC/DOC compute.

// ---- ELC: 32 link-select regs + software event gen ----
pub struct RaElc {
    elsr: [u32; 32],
    elsegr: u32,
    fired: u32, // bitmask of software-fired links (test-visible)
}

impl RaElc {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { elsr: [0; 32], elsegr: 0, fired: 0 }))
    }
}

impl Peripheral for RaElc {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        match offset {
            0x00..=0x7C => self.elsr[(offset / 4) as usize],
            0x80 => self.elsegr,
            0x84 => self.fired,
            _ => 0,
        }
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        match offset {
            0x00..=0x7C => self.elsr[(offset / 4) as usize] = value & 0xFF,
            0x80 => {
                // Software event: each set bit fires its link.
                self.elsegr = value;
                for i in 0..6u32 {
                    if value & (1 << i) != 0 {
                        self.fired |= 1 << i;
                        // Direct dispatch MVP: linked GPT start + ADC start.
                        // Link target encoding follows FSP ELSR values loosely:
                        // any nonzero ELSR on this index means "armed".
                        if self.elsr[i as usize] != 0 {
                            sys.p.nvic.borrow_mut().set_intr_pending(60 + i as i32);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

// ---- AGT: 16-bit low-power timer ----
pub struct RaAgt {
    agtcr: u32, agtcnt: u32, agtr: u32, agtior: u32,
    last_tick: u64,
}

impl RaAgt {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { agtcr: 0, agtcnt: 0, agtr: 0xFFFF, agtior: 0, last_tick: instruction_count() }))
    }
    fn running(&self) -> bool { self.agtcr & 1 != 0 }
    fn advance(&mut self) {
        let now = instruction_count();
        let dt = now.wrapping_sub(self.last_tick);
        self.last_tick = now;
        if !self.running() || dt == 0 { return; }
        let cnt = self.agtcnt.wrapping_add(dt as u32) & 0xFFFF;
        self.agtcnt = if cnt > (self.agtr & 0xFFFF) { 0 } else { cnt };
    }
}

impl Peripheral for RaAgt {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn tick(&mut self, _sys: &System) { self.advance(); }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        self.advance();
        match offset {
            0x00 => self.agtcr,
            0x04 => self.agtcnt,
            0x08 => self.agtr,
            0x0C => self.agtior,
            _ => 0,
        }
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        self.advance();
        match offset {
            0x00 => { self.agtcr = value; self.last_tick = instruction_count(); }
            0x04 => self.agtcnt = value & 0xFFFF,
            0x08 => self.agtr = value & 0xFFFF,
            0x0C => self.agtior = value,
            _ => {}
        }
    }
}

// ---- WDT / IWDT: countdown + shared reset flags ----
pub struct RaWdt {
    wdtrr: u8, wdtsr: u16, down: u32,
}

impl RaWdt {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { wdtrr: 0, wdtsr: 0, down: 1_000_000 }))
    }
}

impl Peripheral for RaWdt {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn tick(&mut self, _sys: &System) {
        if self.down > 0 {
            self.down -= 1;
            if self.down == 0 {
                crate::system::request_watchdog_reset(2);
                self.wdtsr |= 1 << 7;
            }
        }
    }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        match offset {
            0x00 => self.wdtrr as u32,
            0x04 => self.wdtsr as u32,
            _ => 0,
        }
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        match offset {
            0x00 => {
                self.wdtrr = (value & 0xFF) as u8;
                self.down = 1_000_000; // refresh
                self.wdtsr &= !(1 << 7);
            }
            _ => {}
        }
    }
}

// ---- CRC: IEEE-802.3 software-fed ----
pub struct RaCrc {
    crccr: u32, crcdir: u32, crcdor: u32, acc: u32,
}

impl RaCrc {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { crccr: 0, crcdir: 0, crcdor: 0, acc: 0xFFFF_FFFF }))
    }
}

impl Peripheral for RaCrc {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        match offset {
            0x00 => self.crccr,
            0x04 => self.crcdir,
            0x08 => self.crcdor,
            _ => 0,
        }
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        match offset {
            0x00 => { self.crccr = value; if value & 0x80 != 0 { self.acc = 0xFFFF_FFFF; } }
            0x04 => {
                self.crcdir = value;
                let mut c = self.acc ^ value;
                for _ in 0..32 {
                    c = if c & 1 != 0 { (c >> 1) ^ 0xEDB88320 } else { c >> 1 };
                }
                self.acc = c;
                self.crcdor = !self.acc;
            }
            _ => {}
        }
    }
}

// ---- DOC: data operation circuit (compare/mismatch IRQ) ----
pub struct RaDoc {
    docr: u32, dor: u32, dir: u32, dosr: u8,
}

impl RaDoc {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { docr: 0, dor: 0, dir: 0, dosr: 0 }))
    }
}

impl Peripheral for RaDoc {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        match offset {
            0x00 => self.docr as u32,
            0x04 => self.dor,
            0x08 => self.dir,
            0x0C => self.dosr as u32,
            _ => 0,
        }
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        match offset {
            0x00 => self.docr = value & 0xFF,
            0x04 => self.dor = value,
            0x08 => {
                self.dir = value;
                let mismatch = (value != self.dor) as u8;
                self.dosr = mismatch;
                if mismatch != 0 && self.docr & 4 != 0 {
                    sys.p.nvic.borrow_mut().set_intr_pending(67);
                }
            }
            _ => {}
        }
    }
}
