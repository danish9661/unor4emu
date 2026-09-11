use crate::system::System;
use super::Peripheral;

// RA4M1 RSPI (SPI0 0x40072000, SPI1 0x40072100 - R7FA4M1AB.h): this is
// what Arduino SPI uses on the Minima (D11/D12/D13 probe to channel 1,
// i.e. SPI1; polled transfer(), not SCI-SPI).
// SPCR+0x00 (SPMS b0, TXMD b1, MODFEN b2, MSTR b3, SPEIE b4, SPTIE b5,
//   SPE b6, SPRIE b7), SSLP+0x01, SPPCR+0x02, SPSR+0x03 (OVRF b0,
//   IDLNF b1, MODF b2, PERF b3, SPTEF b5 TX-empty, SPRF b7 RX-full),
// SPDR+0x04 (32-bit; 8-bit frames use the low byte, like Arduino),
// SPSCR+0x08, SPBR+0x09, SPDCR+0x0A, SPCKD+0x0B, SSLND+0x0C,
// SPND+0x0D, SPCR2+0x0E, SPCMD0-7+0x10, SPBFCR+0x3E...
// Model: 8-bit master transfers complete instantly (the FSP/Arduino
// polled flow waits on SPRF). SPDR byte write shifts MOSI out while
// MISO samples in: loopback jig (`spi_set_loopback`, shared with the
// SCI-SPI jig) echoes, otherwise the bus pulls up 0xFF. SPRF sets
// (+RXI if SPRIE), SPTEF stays set (+TXI if SPTIE); overrun (write
// while SPRF set) sticks OVRF and keeps the old byte. SPDR byte read
// returns RDR and clears SPRF (+OVRF). Everything else retains;
// SPCMD frame length is assumed 8-bit (what Arduino programs).
pub const SPI0_BASE: u32 = 0x4007_2000;
pub const SPI1_BASE: u32 = 0x4007_2100;
// ELC events: RXI/TXI per channel (bsp_elc.h).
const EVTS: [[u32; 2]; 2] = [[173, 174], [178, 179]];

pub struct RaSpi {
    ch: usize,
    base: u32,
    regs: [u8; 0x100],
    rdr: u8,
    sprf: bool,
    ovrf: bool,
}

impl RaSpi {
    pub fn new_spi(ch: usize) -> Option<Box<dyn Peripheral>> {
        if ch > 1 {
            return None;
        }
        let base = [SPI0_BASE, SPI1_BASE][ch];
        Some(Box::new(Self { ch, base, regs: [0; 0x100], rdr: 0, sprf: false, ovrf: false }))
    }
    pub fn new_spi0() -> Option<Box<dyn Peripheral>> {
        Self::new_spi(0)
    }
    fn update_irq(&self, sys: &System) {
        // SPRIE + SPRF -> RXI, SPTIE + SPTEF -> TXI, via ICU routing.
        if self.regs[0x00] & (1 << 7) != 0 && self.sprf {
            crate::system::icu_raise_event(sys, EVTS[self.ch][0]);
        }
        if self.regs[0x00] & (1 << 5) != 0 {
            crate::system::icu_raise_event(sys, EVTS[self.ch][1]);
        }
    }
}

impl Peripheral for RaSpi {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        let o = (offset & !3) as usize;
        if o == 0x04 {
            // SPDR pack read drains RX (only the data byte lives here;
            // SPCR..SPSR live in pack 0x00, so status polls never clear).
            self.sprf = false;
            self.ovrf = false;
        }
        let mut w = [0u8; 4];
        for k in 0..4 {
            let i = o + k;
            w[k] = if i >= 0x100 {
                0
            } else if i == 0x03 {
                // SPSR live: SPRF + SPTEF(always, instant shifts) + OVRF.
                (if self.sprf { 1 << 7 } else { 0 }) | (1 << 5) | if self.ovrf { 1 } else { 0 }
            } else if i == 0x04 {
                self.rdr
            } else {
                self.regs[i]
            };
        }
        u32::from_le_bytes(w)
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        self.write_sized(sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        let base = (offset & !3) as usize;
        let base = (offset & !3) as usize;
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            let idx = base + byte_offset as usize + i;
            if idx >= 0x100 { continue; }
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            if idx == 0x04 {
                // SPDR data byte: shift now, sample MISO simultaneously.
                let miso = if crate::system::sci_spi_loopback(self.base) { v } else { 0xFF };
                if self.sprf {
                    self.ovrf = true;
                } else {
                    self.rdr = miso;
                    self.sprf = true;
                }
                self.update_irq(sys);
            } else if idx == 0x03 {
                // SPSR flags are HW-driven; stores ignored.
            } else {
                self.regs[idx] = v;
            }
        }
    }
    // SPDR reads drain through read(): clear on any pack containing +0x04.
    // (FSP/Arduino only ever read the data byte itself.)
    fn tick(&mut self, _sys: &System) {}
}
