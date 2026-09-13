use crate::system::{System, instruction_count};
use super::Peripheral;

// RA4M1 misc small blocks, real bases:
// ELC 0x40082000 (routing table), AGT0 0x400E8000 / AGT1 0x400E8100,
// WDT 0x40083400, IWDT 0x40083200, CRC 0x40108000, DOC 0x40109000.
// All accept-and-model: ELC dispatches link events, AGT counts, WDT/IWDT
// raise the shared watchdog flags, CRC/DOC compute.

// ---- ELC: real layout (R7FA4M1AB.h): ELCR+0x00, ELSEGR[2]+0x02,
// ELSR[23]+0x10 (16-bit HA each, stride 4, ELS = low 9 bits).
// ELSR writes mirror to the shared link table so icu_raise_event can
// route without peripheral borrows; ELSEGR software events (83/84)
// route through the same path. (An earlier revision used fictional
// offsets; remapped when SoftwareSerial needed real R_ELC_LinkSet.)
pub struct RaElc {
    elcr: u32,
    elsr: [u32; 32],
}

impl RaElc {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { elcr: 0, elsr: [0; 32] }))
    }
    fn set_elsr(&mut self, n: usize, event: u32) {
        if n < 32 {
            self.elsr[n] = event & 0x1FF;
            crate::system::elc_set_link(n, event & 0x1FF);
        }
    }
}

impl Peripheral for RaElc {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        match offset {
            0x00 => self.elcr,
            0x10..=0x8C => {
                let n = ((offset - 0x10) / 4) as usize;
                if (offset - 0x10) % 4 < 2 { self.elsr[n] } else { 0 }
            }
            _ => 0,
        }
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        // Word-path entry (also used by write_sized below after the
        // target bytes are merged into their 16-bit halves).
        match offset {
            0x00 => self.elcr = value & 0xFF,
            0x10..=0x8C => {
                let n = ((offset - 0x10) / 4) as usize;
                if (offset - 0x10) % 4 == 0 {
                    self.set_elsr(n, value & 0x1FF);
                }
            }
            _ => {}
        }
        let _ = sys;
    }
    fn write_sized(&mut self, sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        // Byte-exact: ELSEGR lives at bytes 2-3 of the +0x00 word and
        // ELSR halves at 4n-aligned slots.
        for i in 0..size as u32 {
            let addr = offset + byte_offset as u32 + i;
            let b = ((value >> (8 * (byte_offset as u32 + i))) & 0xFF) as u32;
            match addr {
                0x00 => self.elcr = b,
                0x02 | 0x03 => {
                    // ELSEGRn: SEG b0 fires software event n (WE b6
                    // gates it on silicon; accept SEG alone, like the
                    // old model, since FSP always sets both).
                    if b & 1 != 0 {
                        let ev = 83 + (addr - 0x02);
                        crate::system::elc_route(sys, ev);
                    }
                }
                0x10..=0x8D => {
                    let n = ((addr - 0x10) / 4) as usize;
                    let half = (addr - 0x10) % 4;
                    if half < 2 {
                        let cur = self.elsr[n];
                        let nv = if half == 0 {
                            (cur & 0x1F00) | b
                        } else {
                            (cur & 0xFF) | ((b & 1) << 8)
                        };
                        self.set_elsr(n, nv);
                    }
                }
                _ => {}
            }
        }
    }
}

// ---- AGT: 16-bit DOWN-counter (real layout: AGT+0x00 counter,
// AGTCMA+0x02 reload, AGTCMB+0x04, AGTCR+0x08 [TSTART b0, TCSTF b1,
// TSTOP b2 WO, TEDGF b4, TUNDF b5], ...). Underflow reloads AGTCMA,
// sets TUNDF and raises the channel event (AGT0_INT=30, AGT1_INT=33).
// Arduino millis() runs on AGT0 underflow IRQs every 1ms.
pub struct RaAgt {
    regs: [u8; 0x10],
    last_tick: u64,
    /// AGT source is PCLKB/8 (3MHz vs 48MHz core): 16 instructions per tick.
    div_acc: u64,
    /// Last value programmed into the counter: underflow reloads this.
    /// (FSP programs AGT=period-1 once at open; AGTCMA is untouched when
    /// output-compare is disabled, so the programmed counter IS the period.)
    reload: u16,
    underflow_event: u32,
}

impl RaAgt {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Self::new_ch(0)
    }
    pub fn new_ch(ch: u8) -> Option<Box<dyn Peripheral>> {
        let ev = match ch {
            0 => 30, // ELC_EVENT_AGT0_INT
            1 => 33, // ELC_EVENT_AGT1_INT
            _ => 0,
        };
        Some(Box::new(Self {
            regs: [0; 0x10],
            last_tick: instruction_count(),
            div_acc: 0,
            reload: 0,
            underflow_event: ev,
        }))
    }
    fn running(&self) -> bool { self.regs[0x08] & 1 != 0 }
    fn counter(&self) -> u16 { u16::from_le_bytes([self.regs[0], self.regs[1]]) }
    fn set_counter(&mut self, v: u16) {
        let b = v.to_le_bytes();
        self.regs[0] = b[0]; self.regs[1] = b[1];
    }
    fn reload(&self) -> u16 { self.reload }
    fn advance(&mut self, sys: &System) {
        let now = instruction_count();
        let dt = now.wrapping_sub(self.last_tick);
        self.last_tick = now;
        if !self.running() || dt == 0 { return; }
        self.div_acc += dt;
        let mut steps = self.div_acc / 16;
        self.div_acc %= 16;
        while steps > 0 {
            steps -= 1;
            let c = self.counter();
            if c == 0 {
                self.set_counter(self.reload);
                self.regs[0x08] |= 1 << 5; // TUNDF
                crate::system::icu_raise_event(sys, self.underflow_event);
            } else {
                self.set_counter(c - 1);
            }
        }
    }
}

impl Peripheral for RaAgt {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn tick(&mut self, sys: &System) { self.advance(sys); }
    fn read(&mut self, sys: &System, offset: u32) -> u32 {
        self.advance(sys);
        let o = (offset & !3) as usize;
        if o + 4 > 0x10 { return 0; }
        let mut b = [self.regs[o], self.regs[o+1], self.regs[o+2], self.regs[o+3]];
        // TCSTF (bit1 of AGTCR) live-follows TSTART (bit0).
        if o == 0x08 {
            if self.running() { b[0] |= 1 << 1; } else { b[0] &= !(1 << 1); }
        }
        u32::from_le_bytes(b)
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        self.write_sized(sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        self.advance(sys);
        let base = (offset & !3) as usize;
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            let idx = base + byte_offset as usize + i;
            if idx >= 0x10 { continue; }
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            if idx == 0x08 {
                // AGTCR: TSTART b0 R/W; TCSTF b1 RO (live on read); TSTOP b2
                // WO-stop; flag bits b4-b7 clear by writing 0, writing 1 is
                // ignored. So: TSTART from write, flags kept only where the
                // write has 1s.
                const FLAGS: u8 = (1 << 4) | (1 << 5) | (1 << 6) | (1 << 7);
                let cur = self.regs[0x08];
                let mut ncr = (v & 0x01) | (cur & FLAGS & v);
                let was_running = cur & 1 != 0;
                if v & (1 << 2) != 0 {
                    ncr &= !1; // TSTOP: force stop
                }
                self.regs[0x08] = ncr;
                // Counter (re)starts from the programmed value on 0->1 start.
                if !was_running && ncr & 1 != 0 {
                    self.set_counter(self.reload);
                }
                self.last_tick = instruction_count();
            } else {
                self.regs[idx] = v;
                // Explicit counter programs latch the reload value.
                if idx <= 0x01 {
                    self.reload = u16::from_le_bytes([self.regs[0], self.regs[1]]);
                }
            }
        }
    }
}

// ---- WDT / IWDT: countdown + shared reset flags ----
// WDTCR+0x02 TOPS[1:0] programs the virtual period (one tick ~= one
// test chunk here, not wall time); any WDTRR write refreshes to it.
pub struct RaWdt {
    wdtrr: u8, wdtsr: u16, down: u32, reload: u32,
}

impl RaWdt {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { wdtrr: 0, wdtsr: 0, down: 1_000_000, reload: 1_000_000 }))
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
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        self.write_sized(sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, _sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        // Real layout: WDTRR+0x00, WDTCR+0x02, WDTSR+0x04. The bus hands
        // merged words, so dispatch per byte (a WDTCR config write must
        // not look like a WDTRR refresh).
        let base = (offset & !3) as usize;
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            let idx = base + byte_offset as usize + i;
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            match idx {
                0x00 => {
                    self.wdtrr = v;
                    self.down = self.reload; // refresh
                    self.wdtsr &= !(1 << 7);
                }
                0x02 => {
                    // WDTCR TOPS[1:0] programs the virtual period (ticks
                    // here scale like test chunks, monotonic in timeout).
                    self.reload = match v & 3 {
                        0 => 64,
                        1 => 256,
                        2 => 1024,
                        _ => 4096,
                    };
                    self.down = self.reload;
                }
                _ => {}
            }
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

// RA SLCDC (segment LCD, real base 0x40082000): LCDM0+0x00, LCDM1+0x01,
// LCDC0+0x02, VLCD+0x03, SEG[64]+0x100 display RAM. No LCD panel on
// Minima and no Arduino consumer: configuration + display RAM retain,
// readable back. A panel would scan SEG on COM timing (unmodeled).
pub struct RaSlcdc {
    regs: [u8; 0x140],
}

impl RaSlcdc {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { regs: [0; 0x140] }))
    }
}

impl Peripheral for RaSlcdc {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        let o = (offset & !3) as usize;
        if o + 4 > 0x140 {
            return 0;
        }
        u32::from_le_bytes([self.regs[o], self.regs[o + 1], self.regs[o + 2], self.regs[o + 3]])
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        self.write_sized(_sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, _sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        let base = (offset & !3) as usize;
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 {
                continue;
            }
            let idx = base + byte_offset as usize + i;
            if idx >= 0x140 {
                continue;
            }
            self.regs[idx] = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
        }
    }
}
