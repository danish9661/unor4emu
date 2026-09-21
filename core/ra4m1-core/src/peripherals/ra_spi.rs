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
    /// Slave mode (MSTR=0): firmware-staged transmit byte, shifted out
    /// when the bus master clocks us (taken, then None = shift 0xFF).
    tx_staged: Option<u8>,
}

impl RaSpi {
    pub fn new_spi(ch: usize) -> Option<Box<dyn Peripheral>> {
        if ch > 1 {
            return None;
        }
        let base = [SPI0_BASE, SPI1_BASE][ch];
        Some(Box::new(Self { ch, base, regs: [0; 0x100], rdr: 0, sprf: false, ovrf: false, tx_staged: None }))
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
    /// Slave select: enabled with MSTR=0 (an enabled master has MSTR=1,
    /// an untouched channel has SPE=0).
    pub(crate) fn is_slave(&self) -> bool {
        self.regs[0x00] & 0x48 == 0x40
    }
    /// Staged slave reply without consuming it (None = not a staged
    /// slave or nothing staged). Component polls use this; the guest
    /// exchange still shifts the byte out via `slave_clock_in`.
    pub(crate) fn slave_peek(&self) -> Option<u8> {
        if self.is_slave() {
            self.tx_staged
        } else {
            None
        }
    }
    /// Bus master clocks a byte through us: sample MOSI into RDR (overrun
    /// if unread, like HW) and shift out the staged byte (0xFF if none).
    pub(crate) fn slave_clock_in(&mut self, sys: &System, mosi: u8) -> u8 {
        if self.sprf {
            self.ovrf = true;
        } else {
            self.rdr = mosi;
            self.sprf = true;
        }
        self.update_irq(sys);
        self.tx_staged.take().unwrap_or(0xFF)
    }
    /// Component hook: clock one MOSI byte through this channel and pack
    /// `[miso, spsr]` (SPSR = live SPRF(b7)+SPTEF(b5)+OVRF(b0), same
    /// bits the MMIO proof reads at SPDR+0x03). Slave channels (MSTR=0)
    /// sample MOSI into RDR and shift out the staged byte (0xFF when
    /// empty); master channels run the normal SD/slave/jig/0xFF MISO
    /// path. Side effects are exactly one bus clock edge (RDR/SPRF/OVRF
    /// move like HW); polled firmware still reads its byte via the
    /// normal SPDR data read.
    pub fn component_exchange(&mut self, mosi: u8) -> Vec<u32> {
        let miso = if self.regs[0x00] & (1 << 3) != 0 {
            // Master path: duplicate the write_sized MISO selection
            // without consuming SPRF/RDR (component polls must not eat
            // the guest's byte).
            self.peek_miso(mosi)
        } else {
            // Slave path: same as a bus-master clock edge, but defer
            // the IRQ raise (runner polls state; see CAN hook rationale).
            if self.sprf {
                self.ovrf = true;
            } else {
                self.rdr = mosi;
                self.sprf = true;
            }
            self.tx_staged.take().unwrap_or(0xFF)
        };
        let spsr = (if self.sprf { 1 << 7 } else { 0 }) | (1 << 5) | if self.ovrf { 1 } else { 0 };
        vec![miso as u32, spsr as u32]
    }
    /// MISO answer for one MOSI byte without touching SPRF/RDR/OVRF.
    fn peek_miso(&self, mosi: u8) -> u8 {
        // NOTE: the SD engine is stateful (exchange advances it), so a
        // true peek is impossible while a card is armed; components that
        // need SD traffic should drive the guest master instead. With no
        // card armed this is side-effect-free.
        if let Some(b) = crate::system::spi_sd_peek(self.base) {
            return b;
        }
        if let Some(b) = crate::system::spi_slave_peek(self.ch) {
            return b;
        }
        if crate::system::sci_spi_loopback(self.base) {
            mosi
        } else {
            0xFF
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
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            let idx = base + byte_offset as usize + i;
            if idx >= 0x100 { continue; }
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            if idx == 0x04 {
                // SPDR data byte: master shifts now and samples MISO;
                // slave stages TX (the master clocks it out later).
                if self.regs[0x00] & (1 << 3) != 0 {
                    // Master: armed SD card eats the stream first, then a
                    // selected slave sources MISO, else the loopback jig
                    // echoes, else the bus pulls up 0xFF.
                    let miso = crate::system::spi_sd_exchange(self.base, v)
                        .or_else(|| crate::system::spi_slave_shift(sys, self.ch, v))
                        .unwrap_or_else(|| {
                            if crate::system::sci_spi_loopback(self.base) { v } else { 0xFF }
                        });
                    if self.sprf {
                        self.ovrf = true;
                    } else {
                        self.rdr = miso;
                        self.sprf = true;
                    }
                } else {
                    self.tx_staged = Some(v);
                }
                self.update_irq(sys);
            } else if idx == 0x03 {
                // SPSR flags are HW-driven; stores ignored.
            } else {
                self.regs[idx] = v;
            }
        }
    }
}

// SPDR reads drain through read(): clear on any pack containing +0x04.
// (FSP/Arduino only ever read the data byte itself.)
//
// Virtual SD card (SPI mode) behind an arming flag
// (`spi_set_sd_card`, same jig pattern as the loopback and the I2C
// EEPROM): when armed, the master's MOSI stream feeds an SD protocol
// engine and MISO comes from it instead of the slave/jig/0xFF. CS is
// NOT modeled (always selected); the sketch still toggles it like HW.
// Supported: CMD0 (reset -> idle), CMD8 (R7 echo), CMD55+ACMD41
// (init loop -> ready), CMD58 (OCR), CMD17 (512B block read with
// 0xFE token), CMD24 (block write, 0xE5 + busy). 16 x 512B blocks;
// block 0 carries an MBR signature (0x55AA), the rest a deterministic
// pattern. CRCs are accepted unchecked (only CMD0/8 need them on
// silicon); unknown commands answer R1 = illegal (0x04).
pub const SD_BLOCKS: usize = 16;

pub struct SpiSd {
    cmd: [u8; 6],
    cn: usize,
    out: std::collections::VecDeque<u8>,
    disk: Vec<u8>,
    app: bool,
    ready: bool,
    acmd41_n: u32,
    /// CMD24 write phase: skipping 0xFFs for the 0xFE token, then
    /// collecting 512 data + 2 CRC bytes into wbuf.
    writing: bool,
    wblock: usize,
    wskip_ff: bool,
    wbuf: Vec<u8>,
    busy_n: u32,
}

impl SpiSd {
    pub fn new() -> Self {        let mut disk = vec![0u8; SD_BLOCKS * 512];
        for b in 0..SD_BLOCKS {
            for i in 0..512 {
                disk[b * 512 + i] = ((b * 512 + i).wrapping_mul(7).wrapping_add(3)) as u8;
            }
        }
        disk[510] = 0x55;
        disk[511] = 0xAA;
        Self {
            cmd: [0; 6],
            cn: 0,
            out: std::collections::VecDeque::new(),
            disk,
            app: false,
            ready: false,
            acmd41_n: 0,
            writing: false,
            wblock: 0,
            wskip_ff: false,
            wbuf: Vec::new(),
            busy_n: 0,
        }
    }
    fn r1(&mut self, v: u8) { self.out.push_back(v); }
    fn block_of(&self, arg: u32) -> Option<usize> {
        // SDHC block addressing.
        let b = arg as usize;
        if b < SD_BLOCKS { Some(b) } else { None }
    }
    fn run_cmd(&mut self) {
        let n = self.cmd[0] & 0x3F;
        let arg = u32::from_be_bytes([self.cmd[1], self.cmd[2], self.cmd[3], self.cmd[4]]);
        let is_app = self.app;
        self.app = false;
        match (n, is_app) {
            (0, _) => {
                self.ready = false;
                self.acmd41_n = 0;
                self.writing = false;
                self.r1(0x01);
            }
            (8, _) => {
                self.r1(0x01);
                self.out.extend([0x00, 0x00, 0x01, 0xAA]);
            }
            (55, _) => {
                self.app = true;
                self.r1(if self.ready { 0x00 } else { 0x01 });
            }
            (41, true) => {
                self.acmd41_n += 1;
                if self.acmd41_n >= 2 {
                    self.ready = true;
                }
                self.r1(if self.ready { 0x00 } else { 0x01 });
            }
            (58, _) => {
                self.r1(if self.ready { 0x00 } else { 0x01 });
                self.out.extend([0xC0, 0xFF, 0x80, 0x00]);
            }
            (17, _) => match self.block_of(arg) {
                Some(b) => {
                    self.r1(0x00);
                    self.out.push_back(0xFF); // Ncr gap
                    self.out.push_back(0xFE); // data token
                    let s = b * 512;
                    self.out.extend(self.disk[s..s + 512].iter().copied());
                    self.out.extend([0xFF, 0xFF]); // CRC16 (unchecked)
                }
                None => self.r1(0x40),
            },
            (24, _) => match self.block_of(arg) {
                Some(b) => {
                    self.r1(0x00);
                    self.writing = true;
                    self.wblock = b;
                    self.wskip_ff = true;
                    self.wbuf.clear();
                }
                None => self.r1(0x40),
            },
            _ => self.r1(0x04),
        }
    }
    /// One MOSI byte in, one MISO byte out (0xFF when idle).
    pub fn exchange(&mut self, mosi: u8) -> u8 {
        if self.writing {
            if self.wskip_ff {
                if mosi == 0xFF {
                    // Host clocks while we answer R1; MISO below.
                } else if mosi == 0xFE {
                    self.wskip_ff = false;
                }
                // (any other byte while skipping: still waiting)
            } else {
                self.wbuf.push(mosi);
                if self.wbuf.len() >= 514 {
                    let s = self.wblock * 512;
                    self.disk[s..s + 512].copy_from_slice(&self.wbuf[..512]);
                    self.writing = false;
                    self.out.push_back(0x05); // data accepted
                    self.busy_n = 3; // busy 0x00s before idle 0xFF
                    // The response token belongs to a LATER byte (like
                    // the command-frame Ncr gap): hold MISO idle here.
                    return 0xFF;
                }
            }
            return self.out.pop_front().unwrap_or_else(|| if self.busy_emitting() { 0x00 } else { 0xFF });
        }
        if self.cn == 0 {
            if mosi & 0xC0 == 0x40 {
                self.cmd[0] = mosi;
                self.cn = 1;
            }
        } else {
            self.cmd[self.cn] = mosi;
            self.cn += 1;
            if self.cn >= 6 {
                self.cn = 0;
                self.run_cmd();
                // The card cannot answer inside the command frame
                // itself (Ncr gap on silicon): hold MISO idle here.
                return 0xFF;
            }
        }
        self.out.pop_front().unwrap_or(0xFF)
    }
    fn busy_emitting(&mut self) -> bool {
        if self.busy_n > 0 {
            self.busy_n -= 1;
            true
        } else {
            false
        }
    }
    /// Copy out one 512B block for host-side inspection (demo block
    /// viewer). Returns None for out-of-range blocks.
    pub fn read_block(&self, b: usize) -> Option<Vec<u8>> {
        if b < SD_BLOCKS {
            Some(self.disk[b * 512..b * 512 + 512].to_vec())
        } else {
            None
        }
    }
}
