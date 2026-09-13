use crate::system::{System, DmaTransfer, DmaDir};
use super::Peripheral;

// RA4M1 DMAC (8ch, real bases 0x40005000+ch*0x40) + DTC (0x40005400 stub).
// Two flows share the channel file:
// - Legacy mem-to-mem (dmac_mem_to_mem proof): SAR+0x00 DAR+0x04
//   CRB(size)+0x08 CHCR+0x0C (bit0 EN) queues an immediate full memcopy.
//   Gated on DMTMD-never-written so real FSP DMCRB writes can't misfire it.
// - Real RA DMAC (SoftwareSerial proof): DMSAR/DMDAR/DMCRA/DMCRB +
//   DMTMD+0x10 (SZ b9:8) + DMINT+0x13 (DTIE b4) + DMAMD+0x14 (DM b7:6
//   dst update, SM b15:14 src update) + DMCNT+0x1C (DTE b0 arm) +
//   DMSTS+0x1E (DTIF b0). Activation comes from ICU DELSR (event link);
//   each matching event moves ONE unit (SZ bytes), DMCRA--, and at zero
//   the channel completes (DTE clear, DTIF, DMACn_INT event 17+ch when
//   DTIE). Completion lags execution: units run in the mem path, so the
//   end event fires on the tick after the last unit executes (tracked
//   via the outstanding counter) - the firmware's DMCNT and TCFPO
//   busy-waits span ticks, like silicon spans bus cycles.
pub const DMAC_BASE: u32 = 0x4000_5000;
pub const DTC_BASE: u32 = 0x4000_5400;

pub struct RaDmac {
    sar: [u32; 8], dar: [u32; 8], size: [u32; 8], chcr: [u32; 8],
    cra: [u32; 8],
    dmtmd: [u32; 8], dmint: [u32; 8], dmamd: [u32; 8],
    dmofr: [u32; 8], dmsrr: [u32; 8], dmsts: [u32; 8],
    dte: [bool; 8],
    tmd_seen: [bool; 8],
}

impl RaDmac {
    pub fn new_dmac() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self {
            sar: [0; 8], dar: [0; 8], size: [0; 8], chcr: [0; 8], cra: [0; 8],
            dmtmd: [0; 8], dmint: [0; 8], dmamd: [0; 8],
            dmofr: [0; 8], dmsrr: [0; 8], dmsts: [0; 8],
            dte: [false; 8], tmd_seen: [false; 8],
        }))
    }
    pub fn new_dtc() -> Option<Box<dyn Peripheral>> {
        RaDtc::new()
    }
    fn unit_bytes(&self, ch: usize) -> usize {
        match (self.dmtmd[ch] >> 8) & 3 {
            0 => 1,
            1 => 2,
            _ => 4,
        }
    }
    fn queue_unit(&mut self, sys: &System, ch: usize, due: u64, seq: u64) {
        let n = self.unit_bytes(ch);
        if std::env::var("DMAEVLOG").is_ok() {
            eprintln!("DMACQ ch={} due={} sar={:#x} cra={:#x}", ch, due, self.sar[ch], self.cra[ch]);
        }
        sys.queue_dma_transfer(DmaTransfer {
            direction: DmaDir::MemCopy,
            stream_idx: ch,
            dma_name: "DMAC_EV".to_string(),
            src: self.sar[ch],
            dst: self.dar[ch],
            size: n,
            peri_addr: 0,
            peripheral: false,
            pinc: true,
            p_size: 1,
            due,
            seq,
        });
        crate::system::dmac_unit_queued(ch);
        crate::system::dma_kick();
        // Address update (DMAMD: 0b10 = increment, else fixed).
        if (self.dmamd[ch] >> 14) & 3 == 2 {
            self.sar[ch] = self.sar[ch].wrapping_add(n as u32);
        }
        if (self.dmamd[ch] >> 6) & 3 == 2 {
            self.dar[ch] = self.dar[ch].wrapping_add(n as u32);
        }
        // DMCRA low16 counts remaining units in normal mode.
        let c = (self.cra[ch] & 0xFFFF).saturating_sub(1);
        self.cra[ch] = (self.cra[ch] & 0xFFFF_0000) | c;
    }
    /// Queue one unit for an activation event (called synchronously
    /// from dmac_notify while the raising timer's event is in hand).
    /// A channel with no remaining count ignores requests (silicon:
    /// DTE auto-clears at transfer end) - without this the
    /// free-running timer queues forever and the completion that
    /// clears DMCNT can never fire.
    pub(crate) fn queue_event_unit(&mut self, sys: &System, event: u32, due: u64, seq: u64) {
        for ch in 0..8 {
            if self.dte[ch] && (self.cra[ch] & 0xFFFF) > 0
                && crate::system::delsr_event(ch) == event
            {
                self.queue_unit(sys, ch, due, seq);
            }
        }
    }
    fn service_completions(&mut self, sys: &System) {
        for ch in 0..8 {
            if self.dte[ch] && (self.cra[ch] & 0xFFFF) == 0
                && crate::system::dmac_outstanding(ch) == 0
            {
                self.dte[ch] = false;
                self.dmsts[ch] |= 1; // DTIF
                if self.dmint[ch] & 0x10 != 0 {
                    crate::system::icu_raise_event(sys, 17 + ch as u32);
                }
            }
        }
    }
}

impl Peripheral for RaDmac {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        let ch = (offset / 0x40) as usize;
        if ch >= 8 { return 0; }
        match offset % 0x40 {
            0x00 => self.sar[ch],
            0x04 => self.dar[ch],
            // DMCRA low16 = remaining units (what the DMCNT-wait polls
            // indirectly via DTE below; R_DMAC_Reset rewrites it).
            0x08 => self.cra[ch],
            0x0C => self.chcr[ch],
            // LE packs for the sub-word regs (bus shifts to the lane).
            0x10 => self.dmtmd[ch] | (self.dmint[ch] << 24),
            0x14 => self.dmamd[ch],
            0x18 => self.dmofr[ch],
            0x1C => (self.dte[ch] as u32) | (self.dmsts[ch] << 16),
            0x20 => self.dmsrr[ch],
            _ => 0,
        }
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        // Word-path entry; sub-word stores arrive via write_sized.
        self.write_sized(sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        let ch = (offset / 0x40) as usize;
        if ch >= 8 { return; }
        let base = offset % 0x40;
        for i in 0..size as u32 {
            let reg = base + byte_offset as u32 + i;
            let b = ((value >> (8 * (byte_offset as u32 + i))) & 0xFF) as u32;
            match reg {
                0x00..=0x03 => {
                    let mut c = self.sar[ch].to_le_bytes();
                    c[reg as usize] = b as u8;
                    self.sar[ch] = u32::from_le_bytes(c);
                }
                0x04..=0x07 => {
                    let mut c = self.dar[ch].to_le_bytes();
                    c[(reg - 4) as usize] = b as u8;
                    self.dar[ch] = u32::from_le_bytes(c);
                }
                0x08..=0x0B => {
                    let mut c = self.cra[ch].to_le_bytes();
                    c[(reg - 8) as usize] = b as u8;
                    self.cra[ch] = u32::from_le_bytes(c);
                    self.size[ch] = self.cra[ch] & 0xFFFF;
                }
                0x0C..=0x0F => {
                    let mut c = self.chcr[ch].to_le_bytes();
                    c[(reg - 0x0C) as usize] = b as u8;
                    self.chcr[ch] = u32::from_le_bytes(c);
                }
                0x10 | 0x11 => {
                    let mut c = self.dmtmd[ch].to_le_bytes();
                    c[(reg - 0x10) as usize] = b as u8;
                    self.dmtmd[ch] = u32::from_le_bytes(c);
                    self.tmd_seen[ch] = true;
                }
                0x13 => self.dmint[ch] = b,
                0x14 | 0x15 => {
                    let mut c = self.dmamd[ch].to_le_bytes();
                    c[(reg - 0x14) as usize] = b as u8;
                    self.dmamd[ch] = u32::from_le_bytes(c);
                }
                0x18..=0x1B => {
                    let mut c = self.dmofr[ch].to_le_bytes();
                    c[(reg - 0x18) as usize] = b as u8;
                    self.dmofr[ch] = u32::from_le_bytes(c);
                }
                // DMCNT: bit0 DTE arms/disarms the event-driven channel.
                0x1C => {
                    self.dte[ch] = b & 1 != 0;
                    crate::system::dma_kick();
                }
                0x1D => {}
                // DMSTS: write-0 clears flags.
                0x1E => self.dmsts[ch] &= !b,
                0x20..=0x23 => {
                    let mut c = self.dmsrr[ch].to_le_bytes();
                    c[(reg - 0x20) as usize] = b as u8;
                    self.dmsrr[ch] = u32::from_le_bytes(c);
                }
                _ => {}
            }
        }
        // Legacy immediate memcopy (kept for the dmac proof): fires on
        // the toy CHCR EN bit only when no real DMTMD was ever written,
        // so FSP DMCRB writes (same offset, block-count semantics) can't
        // misfire it.
        if base == 0x0C && self.chcr[ch] & 1 != 0 && !self.tmd_seen[ch] && self.size[ch] > 0 {
            sys.queue_dma_transfer(DmaTransfer {
                direction: DmaDir::MemCopy,
                stream_idx: ch,
                dma_name: "DMAC".to_string(),
                src: self.sar[ch],
                dst: self.dar[ch],
                size: self.size[ch] as usize,
                peri_addr: 0,
                peripheral: false,
                pinc: true,
                p_size: 1,
                due: 0,
                seq: 0,
            });
            crate::system::dma_kick();
            self.chcr[ch] &= !1; // auto-clear EN once staged
        }
    }
    fn tick(&mut self, sys: &System) {
        self.service_completions(sys);
    }
}

// RA DTC (0x40005400): DTCCR+0x00, DTCVBR+0x04, DTCST+0x0C, DTCSTS+0x0E.
// Accept-and-retain (R_DTC_Open programs DTCCR/DTCVBR/DTCST, Enable arms
// IELSR.DTCE, Reconfigure rewrites SRAM descriptors); the transfer
// engine itself lives in system.rs + FlatMemory's sync-DMA path, which
// resolves vector table + transfer_info fresh from SRAM on every
// activation. DTCSTS reads idle (no status poll in the FSP flow).
pub struct RaDtc {
    regs: [u8; 0x100],
}

impl RaDtc {
    pub fn new() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { regs: [0; 0x100] }))
    }
}

impl Peripheral for RaDtc {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        let o = (offset & !3) as usize;
        if o + 4 > 0x100 {
            return 0;
        }
        if o == 0x0C {
            // DTCSTS (RO idle) in the high half of the DTCST pack.
            return u32::from_le_bytes([self.regs[0x0C], self.regs[0x0D], 0, 0]);
        }
        u32::from_le_bytes([self.regs[o], self.regs[o + 1], self.regs[o + 2], self.regs[o + 3]])
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        self.write_sized(sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, _sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        let base = (offset & !3) as usize;
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 {
                continue;
            }
            let idx = base + byte_offset as usize + i;
            if idx >= 0x100 {
                continue;
            }
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            // DTCSTS is read-only; everything else retains.
            if idx == 0x0E || idx == 0x0F {
                continue;
            }
            self.regs[idx] = v;
        }
    }
}
