use crate::system::{System, DmaTransfer, DmaDir};
use super::Peripheral;

// RA4M1 DMAC (8ch, real bases 0x40005000+ch*0x40) + DTC (0x40005400 stub).
// MVP: mem-to-mem transfers queue into the shared pending-DMA path that
// FlatMemory::service_sync_dma completes synchronously (same as STM32).
// Per-channel: SAR+0x00 DAR+0x04 CRB(size)+0x08 CHCR+0x0C (bit0 EN, bit1 DIR).
pub const DMAC_BASE: u32 = 0x4000_5000;
pub const DTC_BASE: u32 = 0x4000_5400;

pub struct RaDmac {
    sar: [u32; 8], dar: [u32; 8], size: [u32; 8], chcr: [u32; 8],
}

impl RaDmac {
    pub fn new_dmac() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { sar: [0; 8], dar: [0; 8], size: [0; 8], chcr: [0; 8] }))
    }
    pub fn new_dtc() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { sar: [0; 8], dar: [0; 8], size: [0; 8], chcr: [0; 8] }))
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
            0x08 => self.size[ch],
            0x0C => self.chcr[ch],
            _ => 0,
        }
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        let ch = (offset / 0x40) as usize;
        if ch >= 8 { return; }
        match offset % 0x40 {
            0x00 => self.sar[ch] = value,
            0x04 => self.dar[ch] = value,
            0x08 => self.size[ch] = value & 0xFFFF,
            0x0C => {
                let en = value & 1 != 0;
                self.chcr[ch] = value;
                if en && self.size[ch] > 0 {
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
                    });
                    self.chcr[ch] &= !1; // auto-clear EN once staged
                }
            }
            _ => {}
        }
    }
}
