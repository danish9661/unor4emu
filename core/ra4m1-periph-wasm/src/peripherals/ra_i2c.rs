use crate::system::System;
use super::Peripheral;

// RA4M1 IIC (RIIC) master: IIC0 0x40053000, IIC1 0x40053100 (R7FA4M1AB.h).
// ICCR1+0x00 (ICE b7 enable, IICRST b6), ICCR2+0x01 (ST b1 start,
//   RS b2 restart, SP b3 stop, TRS b5 dir, MST b6 master, BBSY b7 live),
// ICMR1+0x02, ICMR2+0x03, ICMR3+0x04, ICFER+0x05, ICSER+0x06,
// ICIER+0x07 (TIE b7, TEIE b6, RIE b5, NAKIE b4, SPIE b3, ALIE b1),
// ICSR1+0x08 (slave detect, retained 0), ICSR2+0x09 (TDRE b7 RO,
//   TEND b6, RDRF b5, NACKF b4 W0C, STOP b3 W0C, START b2 W0C,
//   AL b1 W0C, TMOF b0), SAR[3]+0x0A, ICBRL+0x10, ICBRH+0x11,
// ICDRT+0x12 (write starts a byte), ICDRR+0x13 (RO, pack read clears
// RDRF like the SCI RDR precedent).
// Model: master transactions against a virtual EEPROM slave at 0x50
// (the test jig, like the USB virtual host - SARs retain only). ST/RS
// are strobes (auto-cleared, so RMW never refires a start); SP is a
// strobe too (CCPL precedent). ICDRT write clears TDRE/TEND and stages
// the byte; the tick ships it: address phase ACKs 0x50 (NACKF+ERI
// otherwise) with TXI/TEI, W data bytes land in the EEPROM (first byte
// after the address is the mem pointer), R phases preload ICDRR from
// the EEPROM with RXI. SP sets STOP+TEND with ERI/TEI. Requires ICE;
// IICRST clears the live state. FIFO-less single-byte staging matches
// the FSP blocking master flow (poll TDRE/TEND/RDRF).
pub const IIC0_BASE: u32 = 0x4005_3000;
pub const IIC1_BASE: u32 = 0x4005_3100;
// ELC events (bsp_elc.h): RXI/TXI/TEI/ERI per channel. IIC2 has no
// event codes on this part: modeled polled-only (event 0 = none).
const EVTS: [[u32; 4]; 3] = [[53, 54, 55, 56], [58, 59, 60, 61], [0, 0, 0, 0]];
const SLAVE_ADDR: u8 = 0x50;

pub struct RaIic {
    ch: usize,
    cfg: [u8; 0x18],
    tdre: bool,
    tend: bool,
    rdrf: bool,
    rdr: u8,
    nackf: bool,
    stop: bool,
    start: bool,
    al: bool,
    bbsy: bool,
    pending: Option<u8>,
    addr_phase: bool,
    read_dir: bool,
    expect_ptr: bool,
    eeprom: [u8; 256],
    eptr: u8,
    /// Master side: slave channel currently addressed on the shared bus
    /// fabric (None = virtual-EEPROM jig path).
    fabric: Option<usize>,
    /// Slave side: latched while an external master addresses our SAR.
    slave_active: bool,
    /// Slave side: true while the master reads from us (we transmit).
    slave_tx: bool,
}

impl RaIic {
    pub fn new_ch(ch: usize) -> Option<Box<dyn Peripheral>> {
        if ch > 2 {
            return None;
        }
        Some(Box::new(Self {
            ch,
            cfg: [0; 0x18],
            tdre: true,
            tend: false,
            rdrf: false,
            rdr: 0,
            nackf: false,
            stop: false,
            start: false,
            al: false,
            bbsy: false,
            pending: None,
            addr_phase: false,
            read_dir: false,
            expect_ptr: false,
            eeprom: [0; 256],
            eptr: 0,
            fabric: None,
            slave_active: false,
            slave_tx: false,
        }))
    }
    fn ev(&self, k: usize) -> u32 { EVTS[self.ch][k] } // 0 RXI 1 TXI 2 TEI 3 ERI
    fn icier(&self) -> u8 { self.cfg[0x07] }
    fn raise(&self, sys: &System, k: usize, enable_bit: u8) {
        if self.icier() & enable_bit != 0 {
            crate::system::icu_raise_event(sys, self.ev(k));
        }
    }
    fn reset_live(&mut self) {
        self.tdre = true;
        self.tend = false;
        self.rdrf = false;
        self.nackf = false;
        self.stop = false;
        self.start = false;
        self.al = false;
        self.bbsy = false;
        self.pending = None;
        self.addr_phase = false;
        self.fabric = None;
        self.slave_active = false;
        self.slave_tx = false;
    }
    /// Slave candidate: enabled, not master, and offering an address.
    /// (An idle master also has MST=0, but the master flow never programs
    /// SAR, so SAR!=0 means slave.)
    pub(crate) fn is_slave_candidate(&self) -> bool {
        self.cfg[0x00] & (1 << 7) != 0
            && self.cfg[0x01] & 0x40 == 0
            && (self.cfg[0x0A] | self.cfg[0x0C] | self.cfg[0x0E]) != 0
    }
    /// External-master address phase against our SARs. Latches AAS/BBSY;
    /// on slave-transmit raises TXI so firmware stages the first byte.
    pub(crate) fn slave_match(&mut self, sys: &System, addr: u8, read: bool) -> bool {
        let mut hit = None;
        for (k, off) in [(0u8, 0x0A), (1, 0x0C), (2, 0x0E)] {
            if self.cfg[off] >> 1 == addr {
                hit = Some(k);
                break;
            }
        }
        if let Some(k) = hit {
            self.slave_active = true;
            self.slave_tx = read;
            self.cfg[0x08] |= 1 << k; // ICSR1.AASx like HW
            self.bbsy = true;
            if read {
                self.tdre = true;
                self.raise(sys, 1, 1 << 7); // TXI: stage a byte
            }
            true
        } else {
            false
        }
    }
    /// External-master data byte arriving (slave-receive).
    pub(crate) fn slave_receive(&mut self, sys: &System, b: u8) {
        self.rdr = b;
        self.rdrf = true;
        self.raise(sys, 0, 1 << 5); // RXI
    }
    /// External-master clocking a byte out of us (slave-transmit): consume
    /// the firmware-staged ICDRT byte, ask for the next one.
    pub(crate) fn slave_take_tx(&mut self, sys: &System) -> Option<u8> {
        if let Some(b) = self.pending.take() {
            self.tdre = true;
            self.raise(sys, 1, 1 << 7); // TXI
            Some(b)
        } else {
            None
        }
    }
    /// External-master STOP: latch STOP, drop off the bus like HW.
    pub(crate) fn slave_stop(&mut self, sys: &System) {
        self.slave_active = false;
        self.slave_tx = false;
        self.bbsy = false;
        self.cfg[0x08] &= !0x07; // AAS clears on STOP
        self.stop = true;
        self.raise(sys, 3, 1 << 3); // ERI/SPIE
    }
    /// Ship the staged ICDRT byte (tick): address match/NACK or data.
    fn ship(&mut self, sys: &System) {
        let b = match self.pending.take() {
            Some(b) => b,
            None => return,
        };
        if std::env::var("I2CLOG").is_ok() {
            eprintln!("I2CLOG ship {:02x} ap={} rd={} rdrf={} rdr={:02x}", b, self.addr_phase, self.read_dir, self.rdrf, self.rdr);
        }
        if self.addr_phase {
            self.addr_phase = false;
            let addr = b >> 1;
            self.read_dir = b & 1 != 0;
            // A repeated START leaves the previous fabric slave: the new
            // address phase re-selects (old slave sees START, not STOP).
            if let Some(old) = self.fabric.take() {
                crate::system::i2c_slave_stop(sys, self.ch, old);
            }
            if let Some(ch) = crate::system::i2c_slave_match(sys, self.ch, addr, self.read_dir) {
                // Shared-bus slave answered: ACK like HW (no NACKF).
                self.fabric = Some(ch);
                if self.read_dir {
                    self.cfg[0x01] &= !(1 << 5);
                    self.rdrf = true;
                    self.raise(sys, 0, 1 << 5); // RXI (dummy, FSP discards)
                } else {
                    self.cfg[0x01] |= 1 << 5;
                }
            } else if addr == SLAVE_ADDR {
                if self.read_dir {
                    // SLA+R ACK drops the master to receive (TRS auto).
                    // RDRF+RXI fire now, but byte0 has NOT arrived yet:
                    // the FSP RXI ISR dummy-reads this first flag (stale
                    // ICDRR, discarded), and real bytes stream in on
                    // following ticks. Preloading here would shift the
                    // whole read by one (dummy eats byte0).
                    self.cfg[0x01] &= !(1 << 5);
                    self.rdrf = true;
                    self.raise(sys, 0, 1 << 5); // RXI
                } else {
                    self.cfg[0x01] |= 1 << 5;
                    self.expect_ptr = true;
                }
            } else {
                self.nackf = true;
                self.raise(sys, 3, 1 << 4); // ERI/NAKIE
            }
        } else if !self.read_dir {
            if let Some(ch) = self.fabric {
                // Shared-bus slave takes the data byte.
                crate::system::i2c_slave_receive(sys, self.ch, ch, b);
            } else if self.expect_ptr {
                self.eptr = b;
                self.expect_ptr = false;
            } else {
                self.eeprom[self.eptr as usize] = b;
                self.eptr = self.eptr.wrapping_add(1);
            }
        }
        self.tdre = true;
        self.tend = true;
        self.raise(sys, 1, 1 << 7); // TXI
        self.raise(sys, 2, 1 << 6); // TEI
    }
}

impl Peripheral for RaIic {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        let o = (offset & !3) as usize;
        if std::env::var("I2CLOG").is_ok() && o == 0x10 {
            eprintln!("I2CLOG rd10 rdrf={} rdr={:02x} rd={}", self.rdrf, self.rdr, self.read_dir);
        }
        let mut w = [0u8; 4];
        for k in 0..4 {
            let i = o + k;
            w[k] = match i {
                0x00 => self.cfg[0x00],
                0x01 => {
                    // ICCR2 with live BBSY; ST/RS/SP are strobes (read 0).
                    // HW auto-manages MST/TRS: ST on a free bus enters
                    // master-transmit (FSP writes ST alone, then polls
                    // MST), SLA+R ACK drops to receive, SP returns to
                    // slave. The retained bits follow the same path.
                    (self.cfg[0x01] & 0x60) | if self.bbsy { 1 << 7 } else { 0 }
                }
                0x02..=0x08 | 0x0A..=0x11 | 0x14..=0x17 => *self.cfg.get(i).unwrap_or(&0),
                0x09 => {
                    // ICSR2 live flags.
                    (if self.tdre { 1 << 7 } else { 0 })
                        | (if self.tend { 1 << 6 } else { 0 })
                        | (if self.rdrf { 1 << 5 } else { 0 })
                        | (if self.nackf { 1 << 4 } else { 0 })
                        | (if self.stop { 1 << 3 } else { 0 })
                        | (if self.start { 1 << 2 } else { 0 })
                        | (if self.al { 1 << 1 } else { 0 })
                }
                0x12 => 0xFF, // ICDRT reads back all-ones when idle
                0x13 => {
                    // ICDRR pack read consumes RDRF (SCI RDR precedent:
                    // FSP always reads the data with the flag set).
                    self.rdrf = false;
                    self.rdr
                }
                _ => 0,
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
            if idx >= 0x18 { continue; }
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            match idx {
                0x00 => {
                    let was_rst = self.cfg[0x00] & (1 << 6) != 0;
                    self.cfg[0x00] = v;
                    if v & (1 << 6) != 0 && !was_rst {
                        self.reset_live();
                    }
                }
                0x01 => {
                    // Keep MST/TRS/state bits; ST/RS/SP strobe (auto-clear).
                    self.cfg[0x01] = (self.cfg[0x01] & 0x60) | (v & 0x60);
                    if v & (1 << 1) != 0 && !self.bbsy {
                        // START on a free bus: enter master-transmit
                        // (HW sets MST+TRS; FSP polls MST after ST).
                        // ICDRT is empty, so TDRE+TXI fire for the slave
                        // address - the interrupt-driven FSP flow waits
                        // on this, not on a flag.
                        self.cfg[0x01] |= 0x60;
                        self.bbsy = true;
                        self.start = true;
                        self.addr_phase = true;
                        self.tdre = true;
                        self.raise(sys, 1, 1 << 7); // TXI
                    }
                    if v & (1 << 2) != 0 {
                        // RESTART: next ICDRT byte is the address.
                        self.start = true;
                        self.addr_phase = true;
                    }
                    if v & (1 << 3) != 0 {
                        // STOP: release the bus back to slave, latch
                        // STOP+TEND. A fabric slave sees the STOP too.
                        if let Some(ch) = self.fabric.take() {
                            crate::system::i2c_slave_stop(sys, self.ch, ch);
                        }
                        self.bbsy = false;
                        self.cfg[0x01] &= !0x40; // MST auto-clear
                        self.stop = true;
                        self.tend = true;
                        self.addr_phase = false;
                        self.raise(sys, 3, 1 << 3); // ERI/SPIE
                        self.raise(sys, 2, 1 << 6); // TEI
                    }
                }
                0x09 => {
                    // W0C event flags (TDRE/TEND/RDRF clear through their
                    // data registers, not here).
                    if v & (1 << 4) == 0 { self.nackf = false; }
                    if v & (1 << 3) == 0 { self.stop = false; }
                    if v & (1 << 2) == 0 { self.start = false; }
                    if v & (1 << 1) == 0 { self.al = false; }
                    if v & (1 << 6) == 0 { self.tend = false; }
                    if v & (1 << 5) == 0 { self.rdrf = false; }
                }
                0x12 => {
                    // ICDRT: stage a byte (TDRE/TEND clear till the tick).
                    if self.cfg[0x00] & (1 << 7) != 0 {
                        self.pending = Some(v);
                        self.tdre = false;
                        self.tend = false;
                    }
                }
                0x13 => {} // ICDRR read-only
                0x07 => {
                    self.cfg[0x07] = v;
                    // Like HW, enabling an interrupt with its flag already
                    // latched fires it (the FSP flow enables TXI mid-
                    // transfer and waits on the ISR, not on a flag edge).
                    // Gated on bus-busy: an idle enable must not summon a
                    // transfer out of thin air.
                    if self.bbsy {
                        if self.tdre { self.raise(sys, 1, 1 << 7); }
                        if self.tend { self.raise(sys, 2, 1 << 6); }
                        if self.rdrf { self.raise(sys, 0, 1 << 5); }
                        if self.nackf { self.raise(sys, 3, 1 << 4); }
                        if self.stop { self.raise(sys, 3, 1 << 3); }
                    }
                }
                _ => {
                    self.cfg[idx] = v;
                }
            }
        }
    }
    fn tick(&mut self, sys: &System) {
        if self.cfg[0x00] & (1 << 7) == 0 {
            return;
        }
        if self.slave_active || self.is_slave_candidate() {
            // Slave side: clocked entirely by the external master (its
            // ship/stream calls our slave_* entry points directly). A
            // SAR-offering channel must never run the master ship path,
            // or it would consume its own staged ICDRT byte.
            return;
        }
        // Multi-byte staging matches the FSP poll rate (one byte/tick);
        // the proof ticks between bytes.
        self.ship(sys);
        // Master-receive streams one byte per tick while the flag is
        // clear (backpressure: an unread byte is never overwritten,
        // like HW holding SCL). The FSP RXI ISR dummy-reads the stale
        // flag first, then takes real bytes - same order here.
        if self.read_dir && self.bbsy && !self.addr_phase && !self.rdrf {
            if let Some(ch) = self.fabric {
                // Fabric slave feeds us when it has staged a byte; an
                // empty slave holds SCL (no byte, no flag) like HW.
                if let Some(b) = crate::system::i2c_slave_take_tx(sys, self.ch, ch) {
                    self.rdr = b;
                    self.rdrf = true;
                    self.raise(sys, 0, 1 << 5); // RXI
                }
            } else {
                self.rdr = self.eeprom[self.eptr as usize];
                self.eptr = self.eptr.wrapping_add(1);
                self.rdrf = true;
                self.raise(sys, 0, 1 << 5); // RXI
            }
        }
    }
}
