use crate::system::System;
use super::Peripheral;

// RA4M1 dataflash (8KB @ 0x40100000, erase block 0x400) + FACI_LP command
// interface (@ 0x407EC000). Drives the Arduino EEPROM library through the
// real FSP R_FLASH_LP driver (sequence proven by disassembly):
//   program: FASR.EXS=0, FSARH:FSARL = flash_addr + 0xBDF00000, FWBL0 = data,
//     FCR = 0x81; poll FSTATR1.FRDY (bit6, always ready here)
//   erase:   FSAR = block + 0xBDF00000, FCR = 0x84 (one 1KB block per cmd)
//   blankcheck: FSAR + count-1 @0x118, FCR = 0x83; verdict in FSTATR00.BCERR0
//     (bit3: 0 = blank, 1 = not blank)
// Direct guest writes to the window clear bits only (1->0); 0->1 needs an
// erase, exactly like silicon.
pub const DATAFLASH_BASE: u32 = 0x4010_0000;
pub const DATAFLASH_BLOCK: u32 = 0x400;
pub const FACI_BASE: u32 = 0x407E_C000;
const FSAR_BIAS: u32 = 0xBDF0_0000;

pub struct RaDataFlash;

impl RaDataFlash {
    pub fn new() -> Option<Box<dyn Peripheral>> { Some(Box::new(Self)) }
    fn pack(off: usize) -> u32 {
        let df = crate::system::dataflash().lock().unwrap();
        let mut b = [0xFFu8; 4];
        for i in 0..4 {
            if off + i < crate::system::DATAFLASH_SIZE { b[i] = df[off + i]; }
        }
        u32::from_le_bytes(b)
    }
}

impl Peripheral for RaDataFlash {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        Self::pack((offset & !3) as usize)
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        self.write_sized(_sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, _sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        let base = (offset & !3) as usize;
        let mut df = crate::system::dataflash().lock().unwrap();
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            let idx = base + byte_offset as usize + i;
            if idx >= crate::system::DATAFLASH_SIZE { continue; }
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            df[idx] &= v; // flash programming clears bits only
        }
    }
}

pub struct RaFaci {
    regs: [u8; 0x4000],
    prog_base: Option<u32>,
    prog_seq: u32,
    /// Sequencer busy latch: set by FCR+OPST (command issue), cleared by
    /// FCR OPST=0. FSTATR1.FRDY mirrors it: the FSP wait_for_ready spins
    /// for FRDY=1 after issuing, clears OPST, then spins for FRDY=0.
    busy: bool,
}

impl RaFaci {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { regs: [0; 0x4000], prog_base: None, prog_seq: 0, busy: false }))
    }
    fn u16(&self, off: usize) -> u32 {
        (self.regs[off] as u32) | ((self.regs[off + 1] as u32) << 8)
    }
    fn fsar(&self) -> u32 { self.u16(0x108) | (self.u16(0x110) << 16) }
    /// FACI address back to a dataflash byte index (None if outside).
    fn rel(&self) -> Option<u32> {
        let r = self.fsar().wrapping_sub(FSAR_BIAS).wrapping_sub(DATAFLASH_BASE);
        if r < crate::system::DATAFLASH_SIZE as u32 { Some(r) } else { None }
    }
    fn program_byte(&mut self) {
        let (base, seq, data) = match self.rel() {
            Some(r) => {
                let b = *self.prog_base.get_or_insert(r);
                (b, self.prog_seq, self.regs[0x130])
            }
            None => return,
        };
        self.prog_seq += 1;
        let idx = base.wrapping_add(seq);
        if idx < crate::system::DATAFLASH_SIZE as u32 {
            crate::system::dataflash().lock().unwrap()[idx as usize] &= data;
        }
    }
    fn erase_block(&mut self) {
        if let Some(r) = self.rel() {
            let start = (r & !(DATAFLASH_BLOCK - 1)) as usize;
            let mut df = crate::system::dataflash().lock().unwrap();
            for i in 0..DATAFLASH_BLOCK as usize {
                if start + i < crate::system::DATAFLASH_SIZE { df[start + i] = 0xFF; }
            }
        }
    }
    fn blank_check(&mut self) {
        // Verdict lives in FSTATR00.BCERR0 (bit3): 0 = blank, 1 = not blank.
        let n = self.u16(0x118).wrapping_add(1);
        let blank = match self.rel() {
            Some(r) => {
                let df = crate::system::dataflash().lock().unwrap();
                let mut ok = true;
                for i in 0..n {
                    let idx = r.wrapping_add(i) as usize;
                    if idx >= crate::system::DATAFLASH_SIZE || df[idx] != 0xFF {
                        ok = false;
                        break;
                    }
                }
                ok
            }
            None => false,
        };
        if blank { self.regs[0x128] &= !(1 << 3); } else { self.regs[0x128] |= 1 << 3; }
    }
}

impl Peripheral for RaFaci {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        let o = (offset & !3) as usize;
        if o + 4 > 0x4000 { return 0; }
        let mut b = [self.regs[o], self.regs[o + 1], self.regs[o + 2], self.regs[o + 3]];
        if o == 0x12C && self.busy {
            // FSTATR1.FRDY (bit6) = sequencer running.
            b[0] |= 1 << 6;
        }
        u32::from_le_bytes(b)
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        self.write_sized(_sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, _sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        let base = (offset & !3) as usize;
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            let idx = base + byte_offset as usize + i;
            if idx >= 0x4000 { continue; }
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            // FENTRYR reads back 0 once P/E mode is exited (0xAA00), so the
            // FSP pe_mode_exit poll terminates instead of timing out.
            if idx == 0x3FB2 || idx == 0x3FB3 {
                let cur = if idx == 0x3FB2 {
                    (v as u16) | ((self.regs[0x3FB3] as u16) << 8)
                } else {
                    (self.regs[0x3FB2] as u16) | ((v as u16) << 8)
                };
                if cur == 0xAA00 {
                    self.regs[0x3FB2] = 0;
                    self.regs[0x3FB3] = 0;
                    continue;
                }
            }
            self.regs[idx] = v;
        }
        // Snoop committed command strobes (byte lanes per the driver disasm).
        let touches = |off: u32| {
            let end = base as u32 + byte_offset as u32 + size as u32;
            off >= base as u32 && off < end
        };
        if touches(0x100) && self.regs[0x100] == 0x10 {
            // FPMCR enter-dataflash-P/E: new programming run starts here.
            self.prog_base = None;
            self.prog_seq = 0;
        }
        if touches(0x114) {
            if self.regs[0x114] & 0x80 != 0 {
                // Command issue: latch busy, act instantly (like HW the
                // wait loop then observes FRDY=1, clears OPST, sees FRDY=0).
                self.busy = true;
                match self.regs[0x114] & 0x0F {
                    1 => self.program_byte(),
                    3 => self.blank_check(),
                    4 => self.erase_block(),
                    _ => {}
                }
            } else {
                self.busy = false;
            }
        }
    }
}
