use crate::system::System;
use super::Peripheral;

// RA4M1 SYSTEM / CGC / MSTP / option-setting memory.
// Real bases (RA family): SYSC 0x4001E000, MSTP 0x40084000.
// MVP: accept-and-retain so FSP SystemInit passes; option memory reads erased.
pub const SYSC_BASE: u32 = 0x4001_E000;
pub const MSTP_BASE: u32 = 0x4008_4000;

pub struct RaSystem {
    sysc: std::collections::HashMap<u32, u32>,
    mstp: std::collections::HashMap<u32, u32>,
}

impl Default for RaSystem {
    fn default() -> Self {
        Self { sysc: Default::default(), mstp: Default::default() }
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
        // Unwritten = erased (all-1s) for option-setting memory.
        self.sysc.get(&offset).or_else(|| self.mstp.get(&offset)).copied().unwrap_or(0xFFFF_FFFF)
    }
    fn write(&mut self, _sys: &System, offset: u32, value: u32) {
        // Accept-and-retain is enough to get past SystemInit clock setup.
        // Heuristic: low offsets belong to SYSC, high to MSTP - but since each
        // slot gets its own instance, just store in both maps via sysc.
        self.sysc.insert(offset, value);
        self.mstp.insert(offset, value);
    }
}
