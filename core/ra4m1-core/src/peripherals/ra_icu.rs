use crate::system::System;
use super::Peripheral;

// RA4M1 ICU (real base 0x40006000): IRQCR[16] +0x00, NMICR +0x100,
// NMIER +0x120, WUPEN +0x1A0, IELEN +0x1C0, SELSR0 +0x200,
// DELSR[8] +0x280, IELSR[96] +0x300. The IELSR table is mirrored to the
// shared routing table so peripheral events pend the firmware-mapped IRQ.
pub const ICU_BASE: u32 = 0x4000_6000;

pub struct RaIcu {
    regs: std::collections::HashMap<u32, u32>,
}

impl RaIcu {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { regs: Default::default() }))
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
            crate::system::icu_set_ielsr(irq, value & 0x1FF);
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
