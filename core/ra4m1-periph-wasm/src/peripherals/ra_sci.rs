use crate::system::{System, get_uart_output};
use super::Peripheral;

// RA4M1 SCI (UART mode) + clock-synchronous/simple-SPI mode.
// Real bases: SCI0 0x40070000, stride 0x20; channels 0,1,2,9
// (BSP_FEATURE_SCI_CHANNELS=0x207). Arduino SPI runs on SCI in SPI
// mode (r_sci_spi), so this model covers both.
// Real byte-packed registers: SMR+0x00 (CM b7: 1 = sync/SPI), BRR+0x01,
// SCR+0x02, TDR+0x03, SSR+0x04, RDR+0x05, SEMR+0x07, SPMR+0x0D
// (SSE b0 SPI enable, MSS b2 master, CKPOL b6, CKPH b7). Byte-exact
// via write_sized.
// UART TX: TDR write -> console + SSR TDRE/TEND stay set. RX: inject
// via rx_byte. SPI: TDR write (TE=1) shifts a byte out while sampling
// MISO in: loopback jig (`sci_set_spi_loopback`) echoes MOSI, otherwise
// MISO reads pulled-up 0xFF; RDRF+RXI land, ORER sticks on overrun.
// IRQs use ICU event routing (ELC_EVENT_SCI*_RXI/TXI).
pub const SCI_BASE: u32 = 0x4007_0000; // ch stride 0x20

/// Test/driver helper: inject one RX byte into the SCI at `base`.
pub fn sci_rx_inject(sys: &crate::system::System, base: u32, b: u8) -> bool {
    sys.p.rx_byte(sys, base, b)
}

fn sci_events(hw_ch: u8) -> (u32, u32) {
    // ELC event numbers (bsp_elc.h): RXI/TXI per channel.
    match hw_ch {
        0 => (152, 153),
        1 => (158, 159),
        2 => (163, 164),
        9 => (168, 169),
        _ => (152, 153),
    }
}

pub struct RaSci {
    regs: [u8; 0x40],
    rx_buf: Vec<u8>,
    rxi_event: u32, txi_event: u32,
    base: u32,
}

impl RaSci {
    pub fn new(hw_ch: u8) -> Option<Box<dyn Peripheral>> {
        let (rxi, txi) = sci_events(hw_ch);
        let mut regs = [0u8; 0x40];
        regs[0x01] = 0xFF; // BRR reset
        regs[0x03] = 0xFF; // TDR reset
        regs[0x04] = 0x84; // SSR TDRE+TEND
        let base = SCI_BASE + (hw_ch as u32) * 0x20;
        Some(Box::new(Self { regs, rx_buf: Vec::new(), rxi_event: rxi, txi_event: txi, base }))
    }

    fn scr(&self) -> u8 { self.regs[0x02] }
    fn ssr(&self) -> u8 { self.regs[0x04] }
    fn set_ssr(&mut self, v: u8) { self.regs[0x04] = v; }
    /// Clock-synchronous mode (SMR.CM): with SPMR.SSE this is simple-SPI.
    fn spi_mode(&self) -> bool { self.regs[0x00] & 0x80 != 0 }

    fn update_irq(&self, sys: &System) {
        // RIE + RDRF -> RXI, TIE + TDRE -> TXI, via ICU routing.
        if self.scr() & (1 << 6) != 0 && self.ssr() & (1 << 6) != 0 {
            crate::system::icu_raise_event(sys, self.rxi_event);
        }
        if self.scr() & (1 << 7) != 0 && self.ssr() & (1 << 7) != 0 {
            crate::system::icu_raise_event(sys, self.txi_event);
        }
    }

    fn apply_byte(&mut self, sys: &System, idx: u32, v: u8) {
        match idx {
            0x03 => {
                // TDR: every write transmits, even same value twice.
                self.regs[0x03] = v;
                if self.spi_mode() {
                    // Simple-SPI shift (TE gates the clock): MOSI goes
                    // out while MISO samples in the same clocks.
                    if self.scr() & (1 << 5) != 0 {
                        let miso = if crate::system::sci_spi_loopback(self.base) {
                            v // loopback jig: MOSI tied to MISO
                        } else {
                            0xFF // idle bus pulls up
                        };
                        if self.ssr() & (1 << 6) != 0 {
                            self.set_ssr(self.ssr() | (1 << 5)); // ORER
                        } else {
                            self.regs[0x05] = miso;
                            self.set_ssr(self.ssr() | (1 << 6)); // RDRF
                        }
                        self.set_ssr(self.ssr() | 0xC0); // TDRE+TEND
                        self.update_irq(sys);
                    }
                } else {
                    get_uart_output().lock().unwrap().push(v as char);
                    self.set_ssr(self.ssr() | 0xC0);
                    self.update_irq(sys);
                }
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
