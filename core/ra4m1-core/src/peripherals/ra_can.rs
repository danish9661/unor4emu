use crate::system::System;
use super::Peripheral;

// RA4M1 CAN0 (mailbox CAN), real base 0x40050000 (R7FA4M1AB.h). CAN1 has
// no ELC-routable mailbox events on this part, so only CAN0 is mapped.
// Register file:
//   MB[32]+0x200 (16B each: ID+0x0 [SID bits 28:18], DL+0x4 [DLC 3:0],
//     D[8]+0x6, TS+0xE), MKR[8]+0x400, FIDCR[2]+0x420, MKIVLR+0x428,
//   MIER+0x42C (per-mailbox interrupt enables), MCTL[32]+0x820 (TX view:
//     SENTDATA b0 W0C, TRMABT b2 W0C, ONESHOT b4, RECREQ b6 mode,
//     TRMREQ b7 arm; RX view: NEWDATA b0 W0C, MSGLOST b2 W0C),
//   CTLR+0x840 (CANM[9:8]: 0 operation, 1 reset, 2/3 halt), STR+0x842
//     (RO: NDST b0, SDST b1, NMLST b4, TABST b6, RSTST b8, HLTST b9,
//     TRMST b13, RECST b14), BCR+0x844, RFCR+0x848, TFCR+0x84A,
//   EIER+0x84C, EIFR+0x84D, RECR+0x84E/ECR (RO, always 0), TSR+0x854
//   (RO free-running stamp), AFSR+0x856, TCR+0x858 (TSTE b0,
//     TSTM[2:1]: self-test loopback modes).
// Model: everything retains; TX request (TRMREQ on a TX mailbox)
// completes on the next tick in operation mode (the virtual wire always
// ACKs, like the USB virtual host eating instantly): TRMREQ clears,
// SENTDATA+SDST set, MIER-gated MAILBOX_TX (event 78). Receive
// delivery is loopback through self-test mode (TSTE=1, TSTM!=0, the
// FSP internal-loopback flow): the frame copies into the first
// RECREQ mailbox with NEWDATA+NDST (+TS stamp) and MIER-gated
// MAILBOX_RX (event 77); with no RECREQ mailbox NMLST sets. Leaving
// operation with a pending TX aborts it (TRMABT+TABST). FIFO mode
// (MBM), error counting, and mailbox search are accept-and-retain.
pub const CAN0_BASE: u32 = 0x4005_0000;
pub const CAN0_MBOX_RX_EVENT: u32 = 77;
pub const CAN0_MBOX_TX_EVENT: u32 = 78;

const MB_BASE: usize = 0x200;
const MCTL_BASE: usize = 0x820;
const CTLR: usize = 0x840;
const STR: usize = 0x842;

pub struct RaCan {
    mem: Vec<u8>,
    sent: [bool; 32],
    newdata: [bool; 32],
    msglost: [bool; 32],
    trmabt: [bool; 32],
    pending_tx: Option<usize>,
    nmlst: bool,
    ts: u16,
    last_search: u8,
}

impl RaCan {
    pub fn new_can0() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self {
            mem: vec![0; 0x860],
            sent: [false; 32],
            newdata: [false; 32],
            msglost: [false; 32],
            trmabt: [false; 32],
            pending_tx: None,
            nmlst: false,
            ts: 0,
            last_search: 0,
        }))
    }
    fn canm(&self) -> u16 {
        (u16::from_le_bytes([self.mem[CTLR], self.mem[CTLR + 1]]) >> 8) & 3
    }
    fn is_rx(&self, mb: usize) -> bool { self.mem[MCTL_BASE + mb] & (1 << 6) != 0 }
    fn mier(&self, mb: usize) -> bool {
        let w = u32::from_le_bytes([self.mem[0x42C], self.mem[0x42D], self.mem[0x42E], self.mem[0x42F]]);
        w & (1 << mb) != 0
    }
    fn test_loopback(&self) -> bool {
        self.mem[0x858] & 1 != 0 && self.mem[0x858] & (0b11 << 1) != 0
    }
    fn mctl_read(&self, mb: usize) -> u8 {
        let mut v = self.mem[MCTL_BASE + mb] & 0x50; // ONESHOT + RECREQ
        if self.is_rx(mb) {
            if self.newdata[mb] { v |= 1; }
            if self.msglost[mb] { v |= 1 << 2; }
        } else {
            if self.sent[mb] { v |= 1; }
            if self.trmabt[mb] { v |= 1 << 2; }
            if self.pending_tx == Some(mb) { v |= 1 << 7; }
        }
        v
    }
    fn abort_pending(&mut self) {
        if let Some(n) = self.pending_tx.take() {
            self.trmabt[n] = true;
        }
    }
    fn deliver_tx(&mut self, sys: &System, n: usize) {
        self.pending_tx = None;
        self.sent[n] = true;
        // Loopback: first RECREQ mailbox takes the frame.
        if self.test_loopback() {
            if let Some(rx) = (0..32).find(|&m| self.is_rx(m)) {
                let s = MB_BASE + n * 16;
                let d = MB_BASE + rx * 16;
                for k in 0..14 {
                    self.mem[d + k] = self.mem[s + k];
                }
                self.mem[d + 0x0E] = (self.ts & 0xFF) as u8;
                self.mem[d + 0x0F] = (self.ts >> 8) as u8;
                if self.newdata[rx] {
                    self.msglost[rx] = true;
                }
                self.newdata[rx] = true;
                if self.mier(rx) {
                    crate::system::icu_raise_event(sys, CAN0_MBOX_RX_EVENT);
                }
            } else {
                self.nmlst = true;
            }
        }
        if self.mier(n) {
            crate::system::icu_raise_event(sys, CAN0_MBOX_TX_EVENT);
        }
    }
    fn search_result(&mut self) -> u8 {
        let found = match self.mem[0x853] & 3 {
            0 => (0..32).find(|&m| self.is_rx(m) && self.newdata[m]),
            1 => (0..32).find(|&m| !self.is_rx(m) && self.sent[m]),
            _ => (0..32).find(|&m| self.msglost[m] || self.trmabt[m]),
        };
        match found {
            Some(mb) => {
                self.last_search = mb as u8;
                mb as u8
            }
            None => self.last_search,
        }
    }
    fn str_read(&self) -> u16 {
        let mut v = 0u16;
        if self.newdata.iter().any(|&b| b) { v |= 1 << 0; }
        if self.sent.iter().any(|&b| b) { v |= 1 << 1; }
        if self.nmlst { v |= 1 << 4; }
        if self.trmabt.iter().any(|&b| b) { v |= 1 << 6; }
        match self.canm() {
            1 => v |= 1 << 8,
            2 | 3 => v |= 1 << 9,
            _ => {}
        }
        if self.pending_tx.is_some() { v |= 1 << 13; }
        if self.newdata.iter().any(|&b| b) { v |= 1 << 14; }
        v
    }
}

impl Peripheral for RaCan {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        let o = (offset & !3) as usize;
        let mut w = [0u8; 4];
        for k in 0..4 {
            let i = o + k;
            w[k] = if i >= 0x860 {
                0
            } else if (MCTL_BASE..MCTL_BASE + 32).contains(&i) {
                self.mctl_read(i - MCTL_BASE)
            } else if (STR..STR + 2).contains(&i) {
                self.str_read().to_le_bytes()[i - STR]
            } else if i == 0x84E || i == 0x84F {
                0 // RECR/TECR: no errors on the virtual wire
            } else if (0x854..0x856).contains(&i) {
                self.ts.to_le_bytes()[i - 0x854]
            } else if i == 0x852 {
                // MSSR: mailbox search result for the MSMR mode (the FSP
                // ISRs search instead of scanning: 0 RX/NEWDATA, 1 TX done,
                // 2 message-lost/aborted). Lowest match wins; no match
                // keeps the previous value like HW latching.
                self.search_result()
            } else {
                self.mem[i]
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
            if idx >= 0x860 { continue; }
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            // Read-only: STR, RECR/TECR, MSSR, TSR.
            if (STR..STR + 2).contains(&idx) || idx == 0x84E || idx == 0x84F
                || idx == 0x852 || (0x854..0x856).contains(&idx)
            {
                continue;
            }
            if (MCTL_BASE..MCTL_BASE + 32).contains(&idx) {
                let mb = idx - MCTL_BASE;
                let was_rx = self.is_rx(mb);
                self.mem[idx] = (self.mem[idx] & !0x50) | (v & 0x50);
                if self.mem[idx] & (1 << 6) == 0 {
                    // TX mailbox.
                    if was_rx {
                        self.newdata[mb] = false;
                        self.msglost[mb] = false;
                    }
                    if v & (1 << 7) != 0 {
                        self.pending_tx = Some(mb);
                    } else if self.pending_tx == Some(mb) {
                        self.abort_pending();
                    }
                    if v & 1 == 0 { self.sent[mb] = false; }
                    if v & (1 << 2) == 0 { self.trmabt[mb] = false; }
                } else {
                    // RX mailbox.
                    if !was_rx {
                        self.sent[mb] = false;
                        self.trmabt[mb] = false;
                        if self.pending_tx == Some(mb) {
                            self.pending_tx = None;
                        }
                    }
                    if v & 1 == 0 { self.newdata[mb] = false; }
                    if v & (1 << 2) == 0 { self.msglost[mb] = false; }
                }
                continue;
            }
            if idx == CTLR || idx == CTLR + 1 {
                let oldm = self.canm();
                self.mem[idx] = v;
                // TSRC (b5) is a strobe: HW resets the stamp counter
                // and clears it (FSP polls for the clear after open).
                if idx == CTLR && v & (1 << 5) != 0 {
                    self.mem[CTLR] &= !(1 << 5);
                    self.ts = 0;
                }
                // Leaving operation with a queued TX aborts it.
                if oldm == 0 && self.canm() != 0 {
                    self.abort_pending();
                }
                let _ = sys;
                continue;
            }
            self.mem[idx] = v;
        }
    }
    fn tick(&mut self, sys: &System) {
        if self.canm() == 0 {
            self.ts = self.ts.wrapping_add(1);
        }
        if self.canm() == 0 {
            if let Some(n) = self.pending_tx {
                self.deliver_tx(sys, n);
            }
        }
    }
}
