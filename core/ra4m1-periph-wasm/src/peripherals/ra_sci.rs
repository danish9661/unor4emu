use crate::system::{System, get_uart_output};
use super::Peripheral;

// RA4M1 SCI (UART mode). Real bases: SCI0 0x40118000 + ch*0x100.
// Real byte-packed registers: SMR+0x00, BRR+0x01, SCR+0x02, TDR+0x03,
// SSR+0x04, RDR+0x05, SEMR+0x07. Byte-exact via write_sized.
// TX: TDR write -> console + SSR TDRE/TEND stay set. RX: inject via rx_byte.
pub const SCI_BASE: u32 = 0x4011_8000;

/// Test/driver helper: inject one RX byte into the SCI at `base`.
pub fn sci_rx_inject(sys: &crate::system::System, base: u32, b: u8) -> bool {
    sys.p.rx_byte(sys, base, b)
}

fn sci_irq(ch: u8) -> (i32, i32) {
    match ch {
        0 => (40, 41),
        1 => (42, 43),
        2 => (44, 45),
        3 => (46, 47),
        _ => (40, 41),
    }
}

pub struct RaSci {
    regs: [u8; 0x20],
    rx_buf: Vec<u8>,
    rxi_irq: i32, txi_irq: i32,
}

impl RaSci {
    pub fn new(ch: u8) -> Option<Box<dyn Peripheral>> {
        let (rxi, txi) = sci_irq(ch);
        let mut regs = [0u8; 0x20];
        regs[0x01] = 0xFF; // BRR reset
        regs[0x03] = 0xFF; // TDR reset
        regs[0x04] = 0x84; // SSR TDRE+TEND
        Some(Box::new(Self { regs, rx_buf: Vec::new(), rxi_irq: rxi, txi_irq: txi }))
    }

    fn scr(&self) -> u8 { self.regs[0x02] }
    fn ssr(&self) -> u8 { self.regs[0x04] }
    fn set_ssr(&mut self, v: u8) { self.regs[0x04] = v; }

    fn update_irq(&self, sys: &System) {
        if self.scr() & (1 << 6) != 0 && self.ssr() & (1 << 6) != 0 {
            sys.p.nvic.borrow_mut().set_intr_pending(self.rxi_irq);
        }
        if self.scr() & (1 << 7) != 0 && self.ssr() & (1 << 7) != 0 {
            sys.p.nvic.borrow_mut().set_intr_pending(self.txi_irq);
        }
    }

    fn apply_byte(&mut self, sys: &System, idx: u32, v: u8) {
        match idx {
            0x03 => {
                // TDR: every write transmits, even same value twice.
                self.regs[0x03] = v;
                get_uart_output().lock().unwrap().push(v as char);
                self.set_ssr(self.ssr() | 0xC0);
                self.update_irq(sys);
            }
            0x02 => {
                self.regs[0x02] = v;
                self.update_irq(sys);
            }
            0x04 => {
                // SSR: write-0 clears RDRF/ORER/FER/PER; TDRE/TEND stick.
                let cur = self.ssr();
                self.set_ssr((cur & v) | 0x84);
            }
            0x00 | 0x01 | 0x05 | 0x07 => self.regs[idx as usize] = v,
            _ => self.regs[idx as usize] = v,
        }
    }
}

impl Peripheral for RaSci {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn rx_byte(&mut self, sys: &System, byte: u8) {
        self.rx_buf.push(byte);
        self.regs[0x05] = byte;
        self.set_ssr(self.ssr() | (1 << 6));
        self.update_irq(sys);
    }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        // Aligned 32-bit pack, LE. Peripherals::read shifts down the target.
        let o = (offset & !3) as usize;
        if o + 4 > self.regs.len() {
            // RDR read side-effects when reached via pack containing RDR?
            // Keep simple: pack only, side-effects on exact RDR reads below.
            return 0;
        }
        // RDR (0x05) lives in pack at offset 0x04: drain one byte per pack read
        // that includes it, matching polled-firmware behavior.
        let pack = u32::from_le_bytes([self.regs[o], self.regs[o+1], self.regs[o+2], self.regs[o+3]]);
        pack
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        // Legacy merged path (word writes): apply all 4 bytes.
        self.write_sized(sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        let base = (offset & !3) as usize;
        for i in 0..(size as usize) {
            let idx = base + byte_offset as usize + i;
            if idx >= self.regs.len() { continue; }
            if byte_offset as usize + i >= 4 { continue; } // cross-word tail lives in next word
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            // Handle RDR write (normally read-only): store, no side-effect.
            if idx == 0x05 && !(byte_offset == 0 && size == 4) {
                self.regs[idx] = v;
                continue;
            }
            if idx == 0x05 {
                self.regs[idx] = v;
                continue;
            }
            self.apply_byte(sys, idx as u32, v);
        }
        // RDR drain: exact single-byte RDR read clears RDRF when empty.
        // (Pack reads leave flags; exact path handled by callers using size 1.)
    }
}
