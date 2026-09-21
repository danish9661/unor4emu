use crate::system::System;
use super::Peripheral;

// RA4M1 SSI0/SSI1 (serial sound interface), real bases 0x4004E000 /
// 0x4004E100, same R_SSI0_Type (R7FA4M1AB.h). SSI1 exists in silicon
// but BSP_FEATURE_SSI_VALID_CHANNEL_MASK = 1, so FSP/Arduino use SSI0
// only: SSI1 is modeled (own slot, own FIFOs) and proven register-level
// like the eventless extra channels elsewhere. Register file:
//   SSICR+0x00 (REN b0, TEN b1), SSISR+0x04 (RO, reads 0), SSIFCR+0x10
//   (RFRST b0, TFRST b1, RIE b2, TIE b3, RTRG/T... retained, SSIRST
//   b16), SSIFSR+0x14 (RDF b0, RDC[13:8], TDE b16, TDC[29:24]),
//   SSIFTDR+0x18 (WO TX FIFO), SSIFRDR+0x1C (RO RX FIFO),
//   SSIOFR+0x20 / SSISCR+0x24 (retain).
// Model: TX FIFO (depth 8) drains to the virtual codec on tick while
// TEN runs; RX FIFO fills with an incrementing pattern while REN runs
// (counter resets on RFRST, so polled reads are deterministic).
// TFRST/RFRST are strobes (clear FIFOs + self-clear). TXI (event 62)
// fires on the empty edge with TIE, RXI (63) on data arrival with RIE.
// The in-tree Arduino I2S library does not compile on this core
// (missing r_i2s_api.h), so this is proven bare-metal like the SPI
// slave and DTC proofs.
pub const SSI0_BASE: u32 = 0x4004_E000;
pub const SSI1_BASE: u32 = 0x4004_E100;
pub const SSI0_TXI_EVENT: u32 = 62;
pub const SSI0_RXI_EVENT: u32 = 63;

const FIFO_DEPTH: usize = 8;

pub struct RaSsi {
    ssicr: u32,
    ssifcr: u32,
    ssiofr: u32,
    ssiscr: u32,
    tx: std::collections::VecDeque<u32>,
    rx: std::collections::VecDeque<u32>,
    rx_next: u32,
    was_tde: bool,
    was_rdf: bool,
}

impl RaSsi {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self {
            ssicr: 0, ssifcr: 0, ssiofr: 0, ssiscr: 0,
            tx: std::collections::VecDeque::new(),
            rx: std::collections::VecDeque::new(),
            rx_next: 0, was_tde: true, was_rdf: false,
        }))
    }
    fn ten(&self) -> bool { self.ssicr & (1 << 1) != 0 }
    fn ren(&self) -> bool { self.ssicr & 1 != 0 }
    fn tie(&self) -> bool { self.ssifcr & (1 << 3) != 0 }
    fn rie(&self) -> bool { self.ssifcr & (1 << 2) != 0 }
    /// Component hook: drain the TX FIFO (words the guest wrote via
    /// SSIFTDR while TEN runs) and feed `rx` samples into the RX FIFO
    /// (pattern counter continues after them, same as RFRST sequencing).
    /// Returns `[tx_drained..., 0xFFFF_FFFF, rx_depth]`: the sentinel
    /// splits the halves; empty halves are legal. Respects the TEN/REN
    /// gates (drain/fill only while enabled) and FIFO_DEPTH backpressure
    /// (excess RX samples drop like overrun data loss); never raises
    /// TXI/RXI (the runner polls state, and unsolicited NVIC pends would
    /// perturb guest IRQ timing).
    pub fn component_exchange(&mut self, rx: &[u32]) -> Vec<u32> {
        let mut out: Vec<u32> = self.tx.drain(..).collect();
        out.push(0xFFFF_FFFF);
        if self.ren() {
            for &w in rx {
                if self.rx.len() < FIFO_DEPTH {
                    self.rx.push_back(w);
                } else {
                    break;
                }
            }
        }
        out.push(self.rx.len() as u32);
        out
    }
    fn tde(&self) -> bool { self.tx.is_empty() } // empty flag, like SCI TDRE
    fn rdf(&self) -> bool { !self.rx.is_empty() }
    fn fsr(&self) -> u32 {
        (if self.rdf() { 1 } else { 0 })
            | (((self.rx.len() as u32) & 0x3F) << 8)
            | (if self.tde() { 1 << 16 } else { 0 })
            | (((self.tx.len() as u32) & 0x3F) << 24)
    }
}

impl Peripheral for RaSsi {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        let o = (offset & !3) as usize;
        match o {
            0x00 => self.ssicr,
            0x04 => 0, // SSISR: no errors modeled
            0x10 => self.ssifcr & !(0x03), // reset strobes read back clear
            0x14 => self.fsr(),
            0x1C => self.rx.pop_front().unwrap_or(0),
            0x20 => self.ssiofr,
            0x24 => self.ssiscr,
            _ => 0,
        }
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        self.write_sized(sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        let base = (offset & !3) as usize;
        // TX FIFO data port: assemble the written bytes (any width)
        // into one entry, low-justified.
        if base == 0x18 {
            let mut w = 0u32;
            for i in 0..size as usize {
                if byte_offset as usize + i >= 4 { continue; }
                w |= (((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u32) << (8 * i);
            }
            if self.tx.len() < FIFO_DEPTH {
                self.tx.push_back(w);
            }
            let _ = sys;
            return;
        }
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            let idx = base + byte_offset as usize + i;
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            match idx {
                0x00..=0x03 => {
                    let mut cur = self.ssicr.to_le_bytes();
                    cur[idx] = v;
                    self.ssicr = u32::from_le_bytes(cur);
                }
                0x10..=0x13 => {
                    let mut cur = self.ssifcr.to_le_bytes();
                    cur[idx - 0x10] = v;
                    self.ssifcr = u32::from_le_bytes(cur);
                    if idx == 0x10 {
                        // Reset strobes act immediately, then clear.
                        if v & 1 != 0 {
                            self.rx.clear();
                            self.rx_next = 0;
                        }
                        if v & 2 != 0 {
                            self.tx.clear();
                        }
                        self.ssifcr &= !0x03;
                    }
                }
                0x20..=0x23 => {
                    let mut cur = self.ssiofr.to_le_bytes();
                    cur[idx - 0x20] = v;
                    self.ssiofr = u32::from_le_bytes(cur);
                }
                0x24..=0x27 => {
                    let mut cur = self.ssiscr.to_le_bytes();
                    cur[idx - 0x24] = v;
                    self.ssiscr = u32::from_le_bytes(cur);
                }
                _ => {}
            }
        }
        let _ = sys;
    }
    fn tick(&mut self, sys: &System) {
        // TX drains to the virtual codec while enabled.
        if self.ten() {
            self.tx.clear();
        }
        // RX streams the pattern while enabled (drops when full, like
        // overrun data loss; flag model stays simple).
        if self.ren() && self.rx.len() < FIFO_DEPTH {
            self.rx.push_back(self.rx_next);
            self.rx_next = self.rx_next.wrapping_add(1);
        }
        let tde = self.tde();
        if self.tie() && tde && !self.was_tde {
            crate::system::icu_raise_event(sys, SSI0_TXI_EVENT);
        }
        self.was_tde = tde;
        let rdf = self.rdf();
        if self.rie() && rdf && !self.was_rdf {
            crate::system::icu_raise_event(sys, SSI0_RXI_EVENT);
        }
        self.was_rdf = rdf;
    }
}
