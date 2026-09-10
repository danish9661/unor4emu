use crate::system::{System, instruction_count};
use super::Peripheral;

// RA4M1 SYSTEM / CGC / MSTP / option-setting memory.
// Real bases (RA family): SYSC 0x4001E000, MSTP 0x40084000.
// MVP: accept-and-retain so FSP SystemInit passes; option memory reads erased.
pub const SYSC_BASE: u32 = 0x4001_E000;
pub const MSTP_BASE: u32 = 0x4008_4000;

pub struct RaSystem {
    sysc: std::collections::HashMap<u32, u32>,
    mstp: std::collections::HashMap<u32, u32>,
    // OPCCR (+0xA0) / SOPCCR (+0xAA): power-on reset selects high-speed mode
    // (0x00), NOT erased 0xFF - FSP skips / short-waits on that. OPCM writes
    // raise the transition flag, which clears after a short spin like HW.
    opcm: u8,
    opc_deadline: u64,
    sopcm: u8,
    sopc_deadline: u64,
}

impl Default for RaSystem {
    fn default() -> Self {
        Self {
            sysc: Default::default(), mstp: Default::default(),
            opcm: 0, opc_deadline: 0, sopcm: 0, sopc_deadline: 0,
        }
    }
}

impl RaSystem {
    pub fn new_sysc() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self::default()))
    }
    pub fn new_mstp() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self::default()))
    }
}

impl Peripheral for RaSystem {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, _sys: &System, offset: u32) -> u32 {
        // Both blocks share this model; caller splits by slot base.
        // Unwritten = erased (all-1s) for option-setting memory, EXCEPT the
        // operating-power registers whose reset selects high-speed mode.
        let mut w = self.sysc.get(&offset)
            .or_else(|| self.mstp.get(&offset))
            .copied()
            .unwrap_or(0xFFFF_FFFF);
        let now = instruction_count();
        if offset == 0xA0 {
            let tsf = if now < self.opc_deadline { 0x10 } else { 0 };
            w = (w & !0xFF) | ((self.opcm & 0x03) as u32 | tsf);
        } else if offset == 0xA8 {
            let tsf = if now < self.sopc_deadline { 0x10 } else { 0 };
            w = (w & !(0xFF << 16)) | (((self.sopcm & 0x01) as u32 | tsf) << 16);
        }
        w
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        // Accept-and-retain is enough to get past SystemInit clock setup.
        // Heuristic: low offsets belong to SYSC, high to MSTP - but since each
        // slot gets its own instance, just store in both maps via sysc.
        if offset == 0xA0 {
            let m = (value & 0x03) as u8;
            if m != self.opcm {
                self.opcm = m;
                self.opc_deadline = instruction_count().wrapping_add(500);
            }
        } else if offset == 0xA8 {
            let m = ((value >> 16) & 0x01) as u8;
            if m != self.sopcm {
                self.sopcm = m;
                self.sopc_deadline = instruction_count().wrapping_add(500);
            }
        }
        self.sysc.insert(offset, value);
        self.mstp.insert(offset, value);
    }
}
