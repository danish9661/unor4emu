use crate::system::System;
use super::Peripheral;

// RA4M1 USBFS device (real base 0x40090000) with endpoint FIFOs.
// Register file (R7FA4M1AB.h):
//   CFIFO+0x14 D0FIFO+0x18 D1FIFO+0x1C (FS: 16b halves at base, byte at
//   base), CFIFOSEL+0x20 CFIFOCTR+0x22 D0FIFOSEL+0x28 D0FIFOCTR+0x2A
//   D1FIFOSEL+0x2C D1FIFOCTR+0x2E, INTENB0+0x30 INTENB1+0x32 BRDYENB+0x36
//   NRDYENB+0x38 BEMPENB+0x3A, INTSTS0+0x40 (CTSQ[2:0] VALID b3 DVSQ[6:4]
//   VBSTS b7 BRDY b8 NRDY b9 BEMP b10 CTRT b11 DVST b12 VBINT b15)
//   INTSTS1+0x42 BRDYSTS+0x46 NRDYSTS+0x48 BEMPSTS+0x4A, USBREQ+0x54
//   USBVAL+0x56 USBINDX+0x58 USBLENG+0x5A, DCPMAXP+0x5E DCPCTR+0x60
//   (PID[1:0] CCPL b2 PBUSY b5), PIPESEL+0x64 PIPECFG+0x68 PIPEMAXP+0x6C
//   PIPE_CTR[9]+0x70 (INBUFM b14).
// Behavior matches the TinyUSB dcd_rusb2 sequences: FIFOSEL select +
// CURPIPE/FRDY wait, 16b+byte FIFO writes, BVAL finalize, BCLR, INBUFM
// empty-wait. Full MPS packets auto-flush (HW sends them without BVAL);
// BVAL terminates short packets. TX bytes land in `tx_capture` (the
// virtual host eats instantly); BEMPSTS/BRDYSTS latch + USBFS_INT
// (event 51) fires when enabled.
// Control transfers: the test/JS side plays host (setup regs + CTRT/DVST
// injection + IN drain/OUT feed); the firmware's dcd stack does the rest
// through this model (CFIFO data stage, DCPCTR status, descriptors).
pub const USBFS_BASE: u32 = 0x4009_0000;
pub const USBFS_INT_EVENT: u32 = 51; // ELC_EVENT_USBFS_INT

pub struct RaUsb {
    regs: std::collections::HashMap<u32, u32>,
    tx_buf: [Vec<u8>; 10],
    rx_buf: [Vec<u8>; 10],
    /// Virtual-host TX sink (bulk + control packets, in order).
    pub tx_capture: Vec<u8>,
    brdy: u16,
    bemp: u16,
    // Latched INTSTS0 device-event bits (write-0 clears, write-1 ignored).
    dvst: bool,
    dvsq: u8,
    ctrt: bool,
    ctsq: u8,
    vbint: bool,
    vbsts: bool,
    // Windowed pipe config (PIPESEL selects pipe for +0x68/+0x6C).
    pipesel: u16,
    pipecfg: [u16; 10],
    pipemaxp: [u16; 10],
}

impl RaUsb {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self {
            regs: Default::default(),
            tx_buf: Default::default(),
            rx_buf: Default::default(),
            tx_capture: Vec::new(),
            brdy: 0,
            bemp: 0,
            dvst: false,
            dvsq: 0,
            ctrt: false,
            ctsq: 0,
            vbint: false,
            vbsts: false,
            pipesel: 0,
            pipecfg: [0; 10],
            pipemaxp: [64; 10],
        }))
    }

    /// Queue received bytes on a pipe (virtual-host RX for Serial.read /
    /// control OUT stages). DTLN reflects them; FIFO reads drain.
    pub fn rx_inject(&mut self, pipe: usize, data: &[u8]) {
        if pipe < 10 {
            self.rx_buf[pipe].extend_from_slice(data);
        }
    }

    /// Virtual-host: bus reset (DVST + DVSQ=DEF) or state move.
    pub fn host_set_dvst(&mut self, dvsq: u8) {
        self.dvst = true;
        self.dvsq = dvsq & 7;
    }

    /// Virtual-host: a SETUP packet arrived (loads setup regs, CTRT+RDATA).
    pub fn host_setup(&mut self, req: u16, val: u16, idx: u16, len: u16) {
        self.regs.insert(0x54, req as u32);
        self.regs.insert(0x56, val as u32);
        self.regs.insert(0x58, idx as u32);
        self.regs.insert(0x5A, len as u32);
        self.ctrt = true;
        self.ctsq = 1; // CTRL_RDATA
    }

    /// Virtual-host: status-stage completion (CTRT + CTSQ idle).
    pub fn host_status_done(&mut self) {
        self.ctrt = true;
        self.ctsq = 0;
    }

    /// Virtual-host: attach with VBUS (VBINT + VBSTS).
    pub fn host_attach(&mut self) {
        self.vbint = true;
        self.vbsts = true;
    }

    fn sel(&self, sel_reg: u32) -> usize {
        ((self.regs.get(&sel_reg).copied().unwrap_or(0)) & 0xF) as usize
    }

    fn pipe_mps(&self, pipe: usize) -> usize {
        if pipe == 0 {
            (self.regs.get(&0x5E).copied().unwrap_or(64) & 0x7F).max(8) as usize
        } else {
            (self.pipemaxp[pipe] & 0x7F).max(8) as usize
        }
    }

    fn auto_flush(&mut self, sys: &System, pipe: usize) {
        // HW ships full MPS packets without BVAL; BVAL ends short packets.
        let mps = self.pipe_mps(pipe);
        while self.tx_buf[pipe].len() >= mps {
            let pkt: Vec<u8> = self.tx_buf[pipe].drain(..mps).collect();
            self.tx_capture.extend_from_slice(&pkt);
            self.bemp |= 1 << pipe;
            self.brdy |= 1 << pipe;
        }
        if self.bemp & (1 << pipe) != 0 {
            self.raise_usb_int(sys);
        }
    }

    fn fifo_ctr_write(&mut self, sys: &System, sel_reg: u32, v: u16) {
        let pipe = self.sel(sel_reg);
        if pipe >= 10 {
            return;
        }
        if v & (1 << 14) != 0 {
            // BCLR: drop both directions.
            self.tx_buf[pipe].clear();
            self.rx_buf[pipe].clear();
        }
        if v & (1 << 15) != 0 {
            // BVAL: finalize TX packet -> virtual host.
            self.tx_capture.extend_from_slice(&self.tx_buf[pipe]);
            self.tx_buf[pipe].clear();
            self.bemp |= 1 << pipe;
            self.brdy |= 1 << pipe;
            self.raise_usb_int(sys);
        }
    }

    fn raise_usb_int(&self, sys: &System) {
        let enb = self.regs.get(&0x30).copied().unwrap_or(0);
        let brdye = enb & (1 << 8) != 0;
        let bempe = enb & (1 << 10) != 0;
        if (brdye && self.brdy != 0) || (bempe && self.bemp != 0) {
            crate::system::icu_raise_event(sys, USBFS_INT_EVENT);
        }
        // CTRT/DVST/VBINT share the same vector; fire when latched+enabled.
        let dvse = enb & (1 << 12) != 0;
        let ctre = enb & (1 << 11) != 0;
        if (ctre && self.ctrt) || (dvse && self.dvst) || self.vbint {
            crate::system::icu_raise_event(sys, USBFS_INT_EVENT);
        }
    }

    fn intsts0(&self) -> u32 {
        let enb = self.regs.get(&0x30).copied().unwrap_or(0);
        let mut v = 1 << 3; // VALID reads set while handled
        v |= (self.ctsq as u32) & 7;
        v |= ((self.dvsq as u32) & 7) << 4;
        if self.vbsts {
            v |= 1 << 7;
        }
        if enb & (1 << 8) != 0 && self.brdy != 0 {
            v |= 1 << 8;
        }
        if enb & (1 << 10) != 0 && self.bemp != 0 {
            v |= 1 << 10;
        }
        if self.ctrt {
            v |= 1 << 11;
        }
        if self.dvst {
            v |= 1 << 12;
        }
        if self.vbint {
            v |= 1 << 15;
        }
        v
    }

    fn intsts0_write(&mut self, w: u16) {
        // Write 0 clears latched device-event bits (VALID stays readable).
        if w & (1 << 11) == 0 {
            self.ctrt = false;
        }
        if w & (1 << 12) == 0 {
            self.dvst = false;
        }
        if w & (1 << 15) == 0 {
            self.vbint = false;
        }
    }
}

impl Peripheral for RaUsb {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, sys: &System, offset: u32) -> u32 {
        let _ = sys;
        // All halfword regs are served as aligned packs (the bus
        // right-shifts sub-word reads into place).
        match offset {
            0x14 | 0x18 | 0x1C => {
                // FIFO port read: pop 16 bits from the selected pipe RX buf.
                let sel = match offset {
                    0x14 => self.sel(0x20),
                    0x18 => self.sel(0x28),
                    _ => self.sel(0x2C),
                };
                if sel >= 10 || self.rx_buf[sel].is_empty() {
                    return 0;
                }
                let b0 = self.rx_buf[sel].remove(0) as u32;
                let b1 = if self.rx_buf[sel].is_empty() { 0 } else { self.rx_buf[sel].remove(0) as u32 };
                b0 | (b1 << 8)
            }
            0x20 | 0x28 | 0x2C => {
                // SEL word + live CTR high half (DTLN + FRDY).
                let sel = self.regs.get(&offset).copied().unwrap_or(0) & 0xFFFF;
                let pipe = (sel & 0xF) as usize;
                let ctr = if pipe < 10 {
                    let dtln = (self.rx_buf[pipe].len().min(0xFFF)) as u32;
                    dtln | (1 << 13) // FRDY always set
                } else {
                    1 << 13
                };
                sel | (ctr << 16)
            }
            0x40 => {
                let hi = self.regs.get(&0x42).copied().unwrap_or(0) & 0xFFFF;
                self.intsts0() | (hi << 16)
            }
            // Setup packet block: the dcd reads these as 32-bit pairs
            // (LDRD), so serve both halves together.
            0x54 => {
                self.regs.get(&0x54).copied().unwrap_or(0) & 0xFFFF
                    | ((self.regs.get(&0x56).copied().unwrap_or(0) & 0xFFFF) << 16)
            }
            0x58 => {
                self.regs.get(&0x58).copied().unwrap_or(0) & 0xFFFF
                    | ((self.regs.get(&0x5A).copied().unwrap_or(0) & 0xFFFF) << 16)
            }
            0x44 => (self.brdy as u32) << 16, // [reserved, BRDYSTS]
            0x48 => (self.bemp as u32) << 16, // [NRDYSTS=0, BEMPSTS]
            0x60 => {
                // DCPCTR: retained PID + CCPL state. BSTS (b15, buffer
                // accessible) always reads set - the virtual link never
                // contends (pipe0_xfer asserts on it before every transfer).
                self.regs.get(&0x60).copied().unwrap_or(0) | (1 << 15)
            }
            0x64 => self.pipesel as u32,
            0x68 => {
                // PIPECFG windowed by PIPESEL.
                self.pipecfg[self.pipesel as usize] as u32
            }
            0x6C => self.pipemaxp[self.pipesel as usize] as u32,
            o if (0x70..0x70 + 9 * 2).contains(&o) && o % 4 == 0 => {
                // Two PIPE_CTR halves per word, INBUFM live per pipe.
                let mut w = 0u32;
                for k in 0..2u32 {
                    let idx = o + k * 2;
                    if idx >= 0x70 + 9 * 2 {
                        continue;
                    }
                    let n = ((idx - 0x70) / 2) as usize;
                    let retained =
                        self.regs.get(&idx).copied().unwrap_or(0) & !(1 << 14);
                    let half = retained
                        | (if self.tx_buf[n].is_empty() { 0 } else { 1 << 14 });
                    w |= (half & 0xFFFF) << (k * 16);
                }
                w
            }
            _ => *self.regs.get(&offset).unwrap_or(&0),
        }
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        self.write_sized(sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        // FIFO ports: append `size` low bytes to the selected pipe TX buffer,
        // auto-flushing full MPS packets like HW.
        if offset == 0x14 || offset == 0x18 || offset == 0x1C {
            let sel = match offset {
                0x14 => self.sel(0x20),
                0x18 => self.sel(0x28),
                _ => self.sel(0x2C),
            };
            if sel < 10 {
                for i in 0..size as usize {
                    if byte_offset as usize + i >= 4 { continue; }
                    let b = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
                    self.tx_buf[sel].push(b);
                }
                self.auto_flush(sys, sel);
            }
            return;
        }
        // FIFOCTR live in the high halfword of the SEL words
        // (CFIFOSEL+0x20/CFIFOCTR+0x22, D0 +0x28/+0x2A, D1 +0x2C/+0x2E):
        // route bytes 2-3 there, bytes 0-1 to the SEL retain cell.
        if offset == 0x20 || offset == 0x28 || offset == 0x2C {
            let mut sel_cur = self.regs.get(&offset).copied().unwrap_or(0).to_le_bytes();
            let mut hb = [0u8; 2];
            let mut have_ctr = false;
            for i in 0..size as usize {
                if byte_offset as usize + i >= 4 { continue; }
                let idx = byte_offset as usize + i;
                let b = ((value >> (8 * idx)) & 0xFF) as u8;
                if idx < 2 {
                    sel_cur[idx] = b;
                } else {
                    hb[idx - 2] = b;
                    have_ctr = true;
                }
            }
            self.regs.insert(offset, u32::from_le_bytes(sel_cur));
            if have_ctr {
                self.fifo_ctr_write(sys, offset, u16::from_le_bytes(hb));
            }
            return;
        }
        // Rebuild the merged word, then dispatch by register.
        let mut cur = self.read(sys, offset).to_le_bytes();
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            cur[byte_offset as usize + i] =
                ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
        }
        let w = u32::from_le_bytes(cur);
        match offset {
            // Setup pair writes split back into halves (firmware never
            // writes these, but keep storage consistent if it does).
            0x54 => {
                self.regs.insert(0x54, w & 0xFFFF);
                self.regs.insert(0x56, (w >> 16) & 0xFFFF);
            }
            0x58 => {
                self.regs.insert(0x58, w & 0xFFFF);
                self.regs.insert(0x5A, (w >> 16) & 0xFFFF);
            }            // BRDYSTS (high half of 0x44) / BEMPSTS (high half of 0x48):
            // write 0 clears, write 1 ignored.
            0x44 => {
                self.brdy &= (w >> 16) as u16;
            }
            0x48 => {
                self.bemp &= (w >> 16) as u16;
            }
            // INTSTS0: clear latched device events (VALID readable).
            0x40 => {
                self.intsts0_write((w & 0xFFFF) as u16);
                self.regs.insert(0x42, (w >> 16) & 0xFFFF);
            }
            // DCPCTR: CCPL=1 completes the status stage -> CTRT idle event.
            0x60 => {
                let old = self.regs.get(&0x60).copied().unwrap_or(0);
                self.regs.insert(0x60, w);
                if w & (1 << 2) != 0 && old & (1 << 2) == 0 {
                    self.ctrt = true;
                    self.ctsq = 0;
                    self.raise_usb_int(sys);
                }
            }
            // PIPESEL selects the window for PIPECFG/PIPEMAXP.
            0x64 => {
                self.pipesel = (w & 0xF) as u16;
                self.regs.insert(0x64, w);
            }
            0x68 => {
                self.pipecfg[self.pipesel as usize] = (w & 0xFFFF) as u16;
                self.regs.insert(offset, w);
            }
            0x6C => {
                // PIPEMAXP windowed: track MPS per pipe (default 64).
                self.pipemaxp[self.pipesel as usize] = (w & 0x7F) as u16;
                self.regs.insert(offset, w);
            }
            _ => {
                self.regs.insert(offset, w);
            }
        }
    }
}
