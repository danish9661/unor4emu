use crate::system::System;
use super::Peripheral;

// RA4M1 ICU (real base 0x40006000): IRQCR[16] +0x00, NMICR +0x100,
// NMIER +0x120, WUPEN +0x1A0, IELEN +0x1C0, SELSR0 +0x200,
// DELSR[8] +0x280, IELSR[96] +0x300. The IELSR table is mirrored to the
// shared routing table so peripheral events pend the firmware-mapped IRQ.
// External pin interrupts are ELC events 1-16 (ICU_IRQ0-15): the
// test/JS side injects an edge per line via pin_edge (virtual button),
// gated on IRQCR.IRQMD like HW (00 falling, 01 rising, 10 either;
// 11 low-level is latched, not edged - unsupported, documented).
pub const ICU_BASE: u32 = 0x4000_6000;

pub struct RaIcu {
    regs: std::collections::HashMap<u32, u32>,
}

impl RaIcu {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { regs: Default::default() }))
    }
    fn irqcr(&self, line: usize) -> u8 {
        // IRQCR[line] is byte `line` of the word at aligned base.
        let w = self.regs.get(&((line / 4 * 4) as u32)).copied().unwrap_or(0);
        ((w >> (8 * (line % 4))) & 0xFF) as u8
    }
    /// Inject a pin edge on external IRQ `line` (0-15). Fires ELC event
    /// 1+line when IRQCR.IRQMD matches the edge direction.
    pub fn pin_edge(&mut self, sys: &System, line: usize, falling: bool) -> bool {
        if line >= 16 {
            return false;
        }
        let md = self.irqcr(line) & 3;
        if std::env::var("DMAEVLOG").is_ok() {
            eprintln!("PINEDGE t={} line={} falling={} md={}", crate::system::instruction_count(), line, falling, md);
        }
        let hit = if falling { md == 0 || md == 2 } else { md == 1 || md == 2 };
        if hit {
            crate::system::icu_raise_event(sys, 1 + line as u32);
        }
        hit
    }
}

impl Peripheral for RaIcu {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        // IELSR reset is all-ones on HW? FSP always programs before enabling,
        // so default 0 (NONE) is the safe emulation reset.
        *self.regs.get(&offset).unwrap_or(&0)
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        // IELSR[n]: low 8 bits select the event (retain full word too).
        if (0x300..0x300 + 96 * 4).contains(&offset) && offset % 4 == 0 {
            let irq = ((offset - 0x300) / 4) as usize;
            if std::env::var("ICULOG").is_ok() && irq < 12 {
                eprintln!("ICULOG ielsr[{}] = {}", irq, value & 0x1FF);
            }
            crate::system::icu_set_ielsr(irq, value);
        }
        // DELSR[n]: DMAC activation event (low 9 bits). Mirrored so the
        // DMAC engine can match events without peripheral borrows.
        if (0x280..0x280 + 8 * 4).contains(&offset) && offset % 4 == 0 {
            crate::system::delsr_set_link(((offset - 0x280) / 4) as usize, value & 0x1FF);
        }
        self.regs.insert(offset, value);
    }
    fn write_sized(&mut self, sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        let mut cur = self.read(sys, offset).to_le_bytes();
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            cur[byte_offset as usize + i] =
                ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
        }
        // Reuse the word path so IELSR mirroring stays in one place.
        self.write(sys, offset, u32::from_le_bytes(cur));
    }
}

// RA KINT (key return interrupt, real base 0x40080000): KRCTL+0x00
// (KREG b0 = key interrupt enable), KRF+0x04 (per-key flags, W0C),
// KRM+0x08 (key return mode). A falling edge on an enabled KR pin
// latches its KRF bit and raises KEY_INT (event 69). No Arduino
// consumer: keys arrive via the `kint_key_press` test jig (same shape
// as `icu_pin_edge`).
pub const KINT_BASE: u32 = 0x4008_0000;
pub const KINT_KEY_EVENT: u32 = 69;

pub struct RaKint {
    krctl: u8,
    krf: u8,
    krm: u8,
}

impl RaKint {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { krctl: 0, krf: 0, krm: 0 }))
    }
    fn enabled(&self) -> bool { self.krctl & 1 != 0 }
    /// Virtual key press on KR `key` (0-7): latch flag + KEY_INT event
    /// when the controller is enabled. Returns whether it fired.
    pub(crate) fn key_press(&mut self, sys: &System, key: usize) -> bool {
        if key > 7 || !self.enabled() {
            return false;
        }
        self.krf |= 1 << key;
        crate::system::icu_raise_event(sys, KINT_KEY_EVENT);
        true
    }
}

impl Peripheral for RaKint {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        match offset {
            0x00 => self.krctl as u32,
            0x04 => self.krf as u32,
            0x08 => self.krm as u32,
            _ => 0,
        }
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        self.write_sized(_sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, _sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        let base = (offset & !3) as usize;
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            let idx = base + byte_offset as usize + i;
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            match idx {
                0x00 => self.krctl = v,
                // KRF: write 0 clears latched flags, 1s ignored.
                0x04 => self.krf &= v,
                0x08 => self.krm = v,
                _ => {}
            }
        }
    }
}
