use crate::system::System;
use super::Peripheral;

// RA4M1 PORT+PFS (real bases: PORTn 0x40040000+n*0x20, PFS 0x40040800).
// Real per-port layout, settled against the FSP disassembly (NOT the
// header's self-contradictory alias names):
// PCNTR1+0x00 word = PDR bits 15:0 + PODR bits 31:16 (halfword
// accesses: PDR@+0 / PODR@+2). turnLed's PCNTR1 &= ~16-bit-mask
// tristates for charlieplexing, so direction MUST be the LOW half;
// corroborated by R7FA4M1AB.h PCNTR1_b, the SVD field lsbs, and PAC
// pcntr1.rs. (The SVD 16-bit ALIAS names PODR@+0/PDR@+2 are swapped
// vs the word fields.)
// PCNTR3+0x08 word = PORR(high, RESET)+POSR(low, SET); aliases
// PORR@+0x08 / POSR@+0x0A. PCNTR4+0x0C (EORR+EOSR) same shape.
// R_IOPORT_PinWrite disasm proves it: HIGH halfword str for SET,
// (mask<<16) str for CLEAR, both to PCNTR3+8.
// FSP digitalWrite uses PCNTR3, so byte-exact write_sized is required:
// the bus merges sub-word stores into the aligned word and this model
// applies only the targeted bytes (a merged PCNTR3 write must NOT
// clobber PCNTR1 like a plain word store would).
pub const PORT_BASE: u32 = 0x4004_0000;
pub const PFS_BASE: u32 = 0x4004_0800;

pub struct RaPort {
    pdr: [u16; 12],
    podr: [u16; 12],
    pidr: [u16; 12],
    pfs: std::collections::HashMap<u32, u32>,
    /// True for the PFS-slot instance (0x40040800): PFS words live in
    /// the map keyed by slot offset. (Slot-relative offsets can never
    /// reach 0x800, so the old offset test silently routed every PFS
    /// access into the PORT arms - writes dropped, reads aliased.)
    is_pfs: bool,
}

impl Default for RaPort {
    fn default() -> Self {
        Self { pdr: [0; 12], podr: [0; 12], pidr: [0; 12], pfs: Default::default(), is_pfs: false }
    }
}

impl RaPort {
    pub fn new_port() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { is_pfs: false, ..Self::default() }))
    }
    pub fn new_pfs() -> Option<Box<dyn Peripheral>> {
        Some(Box::new(Self { is_pfs: true, ..Self::default() }))
    }
    pub fn read_output(&self, port: u8, pin: u8) -> bool {
        if (port as usize) < 12 && pin < 16 {
            (self.podr[port as usize] >> pin) & 1 != 0
        } else { false }
    }

    /// PFS PSEL field (bits 24-28) for a PFS-slot offset (plain retain
    /// storage; meaningful only on the PFS-slot instance).
    pub(crate) fn pfs_psel(&self, pfs_offset: u32) -> u8 {
        ((self.pfs.get(&pfs_offset).copied().unwrap_or(0) >> 24) & 0x1F) as u8
    }
    /// GPT-output level driving (port, pin): finds the PFS slot, checks
    /// its retained PSEL for a GPT-function code, maps the pin through
    /// the Minima PWM table, and returns the channel latch level.
    /// PFS PSEL codes 0x02/0x03/0x14/0x15/0x16 are the GPT groups.
    fn routed_gpt(sys: &System, port: u8, pin: u8) -> Option<bool> {
        for slot in sys.p.peripherals.iter() {
            if slot.start != 0x4004_0800 {
                continue;
            }
            let mut b = slot.peripheral.borrow_mut();
            if let Some(u) = b.as_any_mut().downcast_mut::<RaPort>() {
                let psel = u.pfs_psel(port as u32 * 0x40 + pin as u32 * 4);
                if matches!(psel, 0x02 | 0x03 | 0x14 | 0x15 | 0x16) {
                    if let Some((ch, ab)) = crate::system::gpt_pwm_route(port, pin) {
                        return Some(crate::system::gpt_out_level(ch, ab));
                    }
                }
            }
            break;
        }
        None
    }
    pub fn set_input(&mut self, port: u8, pin: u8, v: bool) {
        if (port as usize) < 12 && pin < 16 {
            if v { self.pidr[port as usize] |= 1 << pin; }
            else { self.pidr[port as usize] &= !(1 << pin); }
        }
    }
    /// Sync a PFS word's direction/level bits into the PORT instance.
    /// Slot offset maps as port*0x40 + pin*4 (R_PFS_PORT_Type layout).
    fn pfs_sync_pin(sys: &System, pfs_offset: u32, word: u32) {
        let port = (pfs_offset / 0x40) as u8;
        let pin = ((pfs_offset % 0x40) / 4) as u8;
        if port >= 12 || pin >= 16 {
            return;
        }
        for slot in sys.p.peripherals.iter() {
            if slot.start != PORT_BASE {
                continue;
            }
            if let Ok(mut b) = slot.peripheral.try_borrow_mut() {
                if let Some(u) = b.as_any_mut().downcast_mut::<RaPort>() {
                    if u.is_pfs {
                        continue;
                    }
                    let old = u.podr[port as usize];
                    if word & (1 << 2) != 0 {
                        u.pdr[port as usize] |= 1 << pin;
                    } else {
                        u.pdr[port as usize] &= !(1 << pin);
                    }
                    if word & 1 != 0 {
                        u.podr[port as usize] |= 1 << pin;
                    } else {
                        u.podr[port as usize] &= !(1 << pin);
                    }
                    if (old ^ u.podr[port as usize]) >> pin & 1 != 0 {
                        u.drive_wire(sys, port, pin);
                    }
                    matrix_trace_push(&u.pdr, &u.podr);
                }
            }
            break;
        }
    }
    /// SoftwareSerial loopback wire: (tx_port, tx_bit) -> (rx_port,
    /// rx_bit) with the RX pin's external-IRQ line. Any PODR change on
    /// the TX pin mirrors into the RX pin's PIDR and raises its edge
    /// event (CHANGE sensing matches either direction), standing in
    /// for the physical wire on the bench.
    pub fn soft_wire(tx_port: u8, tx_bit: u8, rx_port: u8, rx_bit: u8, irq_line: usize) {
        *SOFT_WIRE.lock().unwrap() = Some((tx_port, tx_bit, rx_port, rx_bit, irq_line));
    }
    pub fn soft_wire_reset() {
        *SOFT_WIRE.lock().unwrap() = None;
    }
    fn drive_wire(&mut self, sys: &System, port: u8, pin: u8) {
        if std::env::var("DMAEVLOG").is_ok() {
            eprintln!("WIRE t={} port={} pin={} podr={:#x}", crate::system::instruction_count(), port, pin, self.podr[port as usize]);
        }
        if let Some((txp, txb, rxp, rxb, irq)) = *SOFT_WIRE.lock().unwrap() {
            if port == txp && pin == txb && (rxp as usize) < 12 && rxb < 16 {
                let high = (self.podr[port as usize] >> pin) & 1 != 0;
                self.set_input(rxp, rxb, high);
                // CHANGE edge: the direction flag only matters for
                // rising/falling-only sensing; pass the actual edge.
                crate::system::icu_pin_edge(sys, irq, !high);
            }
        }
    }
}

static SOFT_WIRE: std::sync::Mutex<Option<(u8, u8, u8, u8, usize)>> =
    std::sync::Mutex::new(None);

// ── LED-matrix GPIO trace jig (charlieplex reconstruction) ──────────
// The Arduino_LED_Matrix library multiplexes with adjacent on/off
// pairs (turnLed writes PFS anode/cathode words, then clears all via
// PCNTR1): each LED's on-window is ~15 instructions, so chunk-cadence
// MMIO sampling cannot catch them (duty ~0.2%). Instead every PORT
// write and every PFS->PORT sync pushes a full 12-port (PDR,PODR)
// snapshot here — the same "virtual capture" idea as the USB
// tx_capture and the SD card blocks. Tests drain it and decode the
// exact driven set; reset_globals clears it. Costs nothing when idle
// (no writes = no pushes) and is read-only for the guest.
static MATRIX_TRACE: std::sync::Mutex<Vec<[u32; 12]>> =
    std::sync::Mutex::new(Vec::new());

/// Push one 12-port snapshot (word layout: PDR low, PODR high).
fn matrix_trace_push(pdr: &[u16; 12], podr: &[u16; 12]) {
    let mut s = [0u32; 12];
    for p in 0..12 {
        s[p] = ((podr[p] as u32) << 16) | (pdr[p] as u32);
    }
    MATRIX_TRACE.lock().unwrap().push(s);
}

/// Drain all recorded snapshots since the last call (oldest first).
pub fn matrix_trace_take() -> Vec<[u32; 12]> {
    std::mem::take(&mut *MATRIX_TRACE.lock().unwrap())
}

/// Clear the trace (called from reset_globals).
pub fn matrix_trace_reset() {
    MATRIX_TRACE.lock().unwrap().clear();
}

impl Peripheral for RaPort {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn read(&mut self, sys: &System, offset: u32) -> u32 {
        // PFS-slot instance: flat retain map keyed by slot offset.
        if self.is_pfs {
            return *self.pfs.get(&offset).unwrap_or(&0);
        }
        let port = (offset / 0x20) as usize;
        let reg = (offset & !3) % 0x20;
        if port >= 12 { return 0; }
        // Peripheral-driven pins (GPT PWM outputs) follow the mapped
        // channel latch instead of PODR/PIDR.
        let mut podr = self.podr[port];
        let mut pidr = self.pidr[port];
        for pin in 0..16u8 {
            if let Some(level) = Self::routed_gpt(sys, port as u8, pin) {
                if level { podr |= 1 << pin; pidr |= 1 << pin; }
                else { podr &= !(1 << pin); pidr &= !(1 << pin); }
            }
        }
        match reg {
            // PCNTR1 word: PDR occupies bits 15:0, PODR bits 31:16.
            // Settled against the FSP disassembly (turnLed does
            // PCNTR1 &= ~16-bit-mask to tristate for charlieplexing:
            // that MUST clear direction, so direction is the LOW
            // half). Corroborated by R7FA4M1AB.h PCNTR1_b, the SVD
            // field lsbs (PDR 0:15, PODR 16:31), and PAC pcntr1.rs.
            // (The SVD 16-bit ALIAS names PODR@+0/PDR@+2 are swapped
            // vs the word fields — the aliases are NOT word-half
            // views; treat them as their field names state.)
            // Word-level LED idioms (1<<16 LED) are PODR-space.
            0x00 => ((podr as u32) << 16) | (self.pdr[port] as u32),
            0x04 => pidr as u32,
            // WO registers read 0.
            _ => 0,
        }
    }
    fn write(&mut self, sys: &System, offset: u32, value: u32) {
        self.write_sized(sys, offset, value, 0, 4);
    }
    fn write_sized(&mut self, sys: &System, offset: u32, value: u32, byte_offset: u8, size: u8) {
        if self.is_pfs {
            // PFS-slot instance: retain merged word.
            if byte_offset == 0 && size == 4 {
                self.pfs.insert(offset, value);
            } else {
                let mut cur = self.pfs.get(&offset).copied().unwrap_or(0).to_le_bytes();
                for i in 0..size as usize {
                    if byte_offset as usize + i >= 4 { continue; }
                    cur[byte_offset as usize + i] =
                        ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
                }
                self.pfs.insert(offset, u32::from_le_bytes(cur));
            }
            // PFS carries the live direction + output level (PODR b0,
            // PDR b2): R_IOPORT_PinCfg programs pins purely through PFS
            // words, so sync them into the PORT instance (found via the
            // SoftwareSerial TX-idle-HIGH miss: PODR never left reset).
            let word = self.pfs.get(&offset).copied().unwrap_or(0);
            Self::pfs_sync_pin(sys, offset, word);
            return;
        }
        let port = (offset / 0x20) as usize;
        if port >= 12 { return; }
        let base = (offset & !3) as usize % 0x20;
        let old_podr = self.podr[port];
        // PCNTR3/PCNTR4 set/reset need the FULL merged word (low 16 =
        // SET bits, high 16 = RESET bits) — but write_sized arrives one
        // byte at a time. Stage PCNTR3/4 bytes here, apply after the loop.
        let mut pcntr34: u32 = 0;
        let mut pcntr34_mask: u32 = 0;
        for i in 0..size as usize {
            if byte_offset as usize + i >= 4 { continue; }
            let reg = base + byte_offset as usize + i;
            let v = ((value >> (8 * (byte_offset as usize + i))) & 0xFF) as u8;
            match reg {
                // PCNTR1 halves: PDR occupies the LOW halfword
                // (+0/+1), PODR the HIGH halfword (+2/+3) — same
                // settled layout as the word read above. (The SVD
                // alias names PODR@+0/PDR@+2 are swapped vs the word
                // fields; the field positions rule.)
                0x00 | 0x01 => {
                    let mut cur = self.pdr[port].to_le_bytes();
                    cur[reg] = v;
                    self.pdr[port] = u16::from_le_bytes(cur);
                }
                0x02 | 0x03 => {
                    let mut cur = self.podr[port].to_le_bytes();
                    cur[reg - 0x02] = v;
                    self.podr[port] = u16::from_le_bytes(cur);
                }
                // PCNTR2 (PIDR/EIDR) is read-only: ignore.
                0x04..=0x07 => {}
                // PCNTR3 (+0x08 word: POSR low half = SET, PORR high
                // half = RESET) and PCNTR4 (+0x0C: EOSR/EORR same
                // shape): stage the byte; the merged word applies
                // below. Matches the R_IOPORT_PinWrite disasm (HIGH
                // halfword str for SET, (mask<<16) str for CLEAR, both
                // to PCNTR3+8) and the SVD field positions.
                0x08..=0x0F => {
                    let bit = (reg - 0x08) * 8;
                    pcntr34 |= (v as u32) << bit;
                    pcntr34_mask |= 0xFF << bit;
                }
                _ => {}
            }
        }
        if pcntr34_mask != 0 {
            let set = (pcntr34 & 0xFFFF) as u16;
            let rst = ((pcntr34 >> 16) & 0xFFFF) as u16;
            self.podr[port] |= set;
            self.podr[port] &= !rst;
            matrix_trace_push(&self.pdr, &self.podr);
        }
        // Direct PCNTR1 writes (RMW direction/level programs like
        // PortDirectionSet and turnLed's tristate clear) also change
        // the driven set: trace them too.
        if base < 0x04 {
            matrix_trace_push(&self.pdr, &self.podr);
        }
        // Loopback wire: mirror changed TX-pin bits to the RX pin.
        let changed = old_podr ^ self.podr[port];
        for pin in 0..16u8 {
            if changed >> pin & 1 != 0 {
                self.drive_wire(sys, port as u8, pin);
            }
        }
    }
}
