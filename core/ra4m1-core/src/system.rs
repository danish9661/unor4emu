//! RA4M1-only system core: virtual clock, fault channels, DMA staging.

use std::sync::atomic::{AtomicU64, AtomicBool, AtomicU32, Ordering};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;
use crate::peripherals::Peripherals;

// UART output buffer: SCI TDR writes push chars here, JS reads them out.
use std::sync::OnceLock;
static UART_OUTPUT: OnceLock<Mutex<String>> = OnceLock::new();
pub fn get_uart_output() -> &'static Mutex<String> {
    UART_OUTPUT.get_or_init(|| Mutex::new(String::new()))
}

pub static INSTRUCTION_COUNT: AtomicU64 = AtomicU64::new(0);
pub fn instruction_count() -> u64 { INSTRUCTION_COUNT.load(Ordering::Relaxed) }

// MPU master-enable latch (MPU_CTRL.ENABLE write).
static MPU_ENABLED: AtomicBool = AtomicBool::new(false);
pub fn is_mpu_enabled() -> bool { MPU_ENABLED.load(Ordering::Acquire) }
pub fn set_mpu_enabled(v: bool) { MPU_ENABLED.store(v, Ordering::Release); }

// Current CPU context for the MPU check (FlatMemory has no CPU handle).
static CURRENT_PRIV: AtomicBool = AtomicBool::new(true);
static CURRENT_HFNMI: AtomicBool = AtomicBool::new(false);
pub fn current_privileged() -> bool { CURRENT_PRIV.load(Ordering::Relaxed) }
pub fn current_hfnmi() -> bool { CURRENT_HFNMI.load(Ordering::Relaxed) }
pub fn set_cpu_context(priv_: bool, hfnmi: bool) {
    CURRENT_PRIV.store(priv_, Ordering::Relaxed);
    CURRENT_HFNMI.store(hfnmi, Ordering::Relaxed);
}
// Live exception number for ICSR.VECTACTIVE.
static CURRENT_IPSR: AtomicU32 = AtomicU32::new(0);
pub fn current_ipsr() -> u32 { CURRENT_IPSR.load(Ordering::Relaxed) }
pub fn set_current_ipsr(v: u32) { CURRENT_IPSR.store(v, Ordering::Relaxed); }
// Force the next MPU check(s) to unprivileged (LDRT/STRT).
static MPU_FORCE_UNPRIV: AtomicBool = AtomicBool::new(false);
pub fn set_mpu_force_unpriv(v: bool) { MPU_FORCE_UNPRIV.store(v, Ordering::Relaxed); }
pub(crate) fn mpu_force_unpriv() -> bool { MPU_FORCE_UNPRIV.load(Ordering::Relaxed) }
// CCR.UNALIGN_TRP cache for the access hot path.
static UNALIGN_TRP: AtomicBool = AtomicBool::new(false);
pub fn set_unalign_trp(v: bool) { UNALIGN_TRP.store(v, Ordering::Relaxed); }
pub(crate) fn unalign_trp() -> bool { UNALIGN_TRP.load(Ordering::Relaxed) }
// Deferred alignment-fault channel.
static ALIGN_FAULT_VALID: AtomicBool = AtomicBool::new(false);
static ALIGN_FAULT_ADDR: AtomicU32 = AtomicU32::new(0);
pub fn pend_align_fault(addr: u32) {
    if ALIGN_FAULT_VALID.load(Ordering::Relaxed) {
        return;
    }
    ALIGN_FAULT_ADDR.store(addr, Ordering::Relaxed);
    ALIGN_FAULT_VALID.store(true, Ordering::Release);
}
pub fn take_align_fault() -> Option<u32> {
    if ALIGN_FAULT_VALID.swap(false, Ordering::Acquire) {
        Some(ALIGN_FAULT_ADDR.load(Ordering::Relaxed))
    } else {
        None
    }
}
// Deferred bus-fault channel.
static BUS_FAULT_VALID: AtomicBool = AtomicBool::new(false);
static BUS_FAULT_ADDR: AtomicU32 = AtomicU32::new(0);
static BUS_FAULT_EXEC: AtomicBool = AtomicBool::new(false);
pub fn pend_bus_fault(addr: u32, exec: bool) {
    if BUS_FAULT_VALID.load(Ordering::Relaxed) {
        return;
    }
    BUS_FAULT_ADDR.store(addr, Ordering::Relaxed);
    BUS_FAULT_EXEC.store(exec, Ordering::Relaxed);
    BUS_FAULT_VALID.store(true, Ordering::Release);
}
pub fn take_bus_fault() -> Option<(u32, bool)> {
    if BUS_FAULT_VALID.swap(false, Ordering::Acquire) {
        Some((BUS_FAULT_ADDR.load(Ordering::Relaxed), BUS_FAULT_EXEC.load(Ordering::Relaxed)))
    } else {
        None
    }
}
// Deferred MPU data-fault channel.
static MPU_FAULT_VALID: AtomicBool = AtomicBool::new(false);
static MPU_FAULT_ADDR: AtomicU32 = AtomicU32::new(0);
static MPU_FAULT_EXEC: AtomicBool = AtomicBool::new(false);
pub fn pend_mpu_fault(addr: u32, exec: bool) {
    if MPU_FAULT_VALID.load(Ordering::Relaxed) {
        return;
    }
    MPU_FAULT_ADDR.store(addr, Ordering::Relaxed);
    MPU_FAULT_EXEC.store(exec, Ordering::Relaxed);
    MPU_FAULT_VALID.store(true, Ordering::Release);
}
pub fn take_mpu_fault() -> Option<(u32, bool)> {
    if MPU_FAULT_VALID.swap(false, Ordering::Acquire) {
        Some((MPU_FAULT_ADDR.load(Ordering::Relaxed), MPU_FAULT_EXEC.load(Ordering::Relaxed)))
    } else {
        None
    }
}
/// Latch MemManage fault state (CFSR MMFSR bits + MMFAR).
pub fn latch_memmanage_fault(sys: &WasmSystem, mmfsr_bits: u32, mmfar: Option<u32>) {
    let cfsr = sys.p.read(sys, 0xE000ED28, 4);
    sys.p.write(sys, 0xE000ED28, 4, cfsr | (mmfsr_bits & 0xFF));
    if let Some(a) = mmfar {
        sys.p.write(sys, 0xE000ED34, 4, a);
    }
}

// Persistent reset-cause bits, latched on watchdog expiry.
static WATCHDOG_RESET_EVENT: AtomicBool = AtomicBool::new(false);
static IWDG_RESET_FLAG: AtomicBool = AtomicBool::new(false);
static WWDG_RESET_FLAG: AtomicBool = AtomicBool::new(false);
pub fn is_watchdog_reset_requested() -> bool { WATCHDOG_RESET_EVENT.swap(false, Ordering::Acquire) }
pub fn request_watchdog_reset(cause: u8) {
    WATCHDOG_RESET_EVENT.store(true, Ordering::Release);
    if cause & 1 != 0 { IWDG_RESET_FLAG.store(true, Ordering::Release); }
    if cause & 2 != 0 { WWDG_RESET_FLAG.store(true, Ordering::Release); }
}
pub fn iwdg_reset_flag() -> bool { IWDG_RESET_FLAG.load(Ordering::Acquire) }
pub fn wwdg_reset_flag() -> bool { WWDG_RESET_FLAG.load(Ordering::Acquire) }
pub fn clear_watchdog_reset_flags() {
    IWDG_RESET_FLAG.store(false, Ordering::Release);
    WWDG_RESET_FLAG.store(false, Ordering::Release);
}

// ── ADC channel-value injection (JS hardware layer plumbing) ───────────────
static ADC_OVERRIDES: OnceLock<Mutex<std::collections::HashMap<(String, u32), u32>>> = OnceLock::new();

fn adc_overrides() -> &'static Mutex<std::collections::HashMap<(String, u32), u32>> {
    ADC_OVERRIDES.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}
pub fn adc_set_override(peripheral: &str, channel: u32, value: u32) {
    adc_overrides().lock().unwrap().insert((peripheral.to_string(), channel), value & 0x3FFF);
}
pub fn adc_clear_override(peripheral: &str, channel: u32) {
    adc_overrides().lock().unwrap().remove(&(peripheral.to_string(), channel));
}
pub fn adc_get_override(peripheral: &str, channel: u32) -> Option<u32> {
    adc_overrides().lock().unwrap().get(&(peripheral.to_string(), channel)).copied()
}

// ── CTSU touch-count overrides (JS hardware layer plumbing) ───────────────
// Keyed channel -> raw sensor count. Values above 16 bits clamp to
// 0xFFFF and set SOVF, so overflow is testable through the same path.
static CTSU_OVERRIDES: OnceLock<Mutex<std::collections::HashMap<u32, u32>>> = OnceLock::new();

fn ctsu_overrides() -> &'static Mutex<std::collections::HashMap<u32, u32>> {
    CTSU_OVERRIDES.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}
pub fn ctsu_set_override(channel: u32, value: u32) {
    ctsu_overrides().lock().unwrap().insert(channel, value);
}
pub fn ctsu_clear_override(channel: u32) {
    ctsu_overrides().lock().unwrap().remove(&channel);
}
pub fn ctsu_get_override(channel: u32) -> Option<u32> {
    ctsu_overrides().lock().unwrap().get(&channel).copied()
}

// ── SCI-SPI loopback jig (MOSI tied to MISO) ───────────────────────────────
// Keyed by SCI base address. Off by default (idle MISO reads pulled-up
// 0xFF, like a real unterminated bus); tests enable it per channel.
static SCI_SPI_LOOPBACK: OnceLock<Mutex<std::collections::HashSet<u32>>> = OnceLock::new();

fn sci_spi_loopback_set() -> &'static Mutex<std::collections::HashSet<u32>> {
    SCI_SPI_LOOPBACK.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}
pub fn sci_set_spi_loopback(base: u32, on: bool) {
    let mut m = sci_spi_loopback_set().lock().unwrap();
    if on {
        m.insert(base);
    } else {
        m.remove(&base);
    }
}
pub fn sci_spi_loopback(base: u32) -> bool {
    sci_spi_loopback_set().lock().unwrap().contains(&base)
}

// ── Shared I2C bus fabric (master <-> slave across channels) ───────────────
// Lets one channel's master talk to another channel's slave (SAR set,
// MST clear) the way two pins would on a real bus: address match latches
// AAS/BBSY on the slave, data bytes route master->slave ICDRR and
// slave ICDRT->master ICDRR, STOP releases both. Falls back to nothing
// (the caller keeps its virtual-EEPROM jig) when no slave answers.
fn i2c_each_slave(sys: &WasmSystem, from_ch: Option<usize>, f: &mut dyn FnMut(usize, &mut crate::peripherals::ra_i2c::RaIic)) {
    for slot in sys.p.peripherals.iter() {
        if !(0x4005_3000..0x4005_3300).contains(&slot.start) || (slot.start - 0x4005_3000) % 0x100 != 0 {
            continue;
        }
        let ch = ((slot.start - 0x4005_3000) / 0x100) as usize;
        if Some(ch) == from_ch {
            continue;
        }
        let mut b = slot.peripheral.borrow_mut();
        if let Some(u) = b.as_any_mut().downcast_mut::<crate::peripherals::ra_i2c::RaIic>() {
            if u.is_slave_candidate() {
                f(ch, u);
            }
        }
    }
}
pub fn i2c_slave_match(sys: &WasmSystem, from_ch: usize, addr: u8, read: bool) -> Option<usize> {
    let mut hit = None;
    i2c_each_slave(sys, Some(from_ch), &mut |ch, u| {
        if hit.is_none() && u.slave_match(sys, addr, read) {
            hit = Some(ch);
        }
    });
    hit
}
pub fn i2c_slave_receive(sys: &WasmSystem, from_ch: usize, ch: usize, b: u8) {
    i2c_each_slave(sys, Some(from_ch), &mut |c, u| {
        if c == ch {
            u.slave_receive(sys, b);
        }
    });
}
pub fn i2c_slave_take_tx(sys: &WasmSystem, from_ch: usize, ch: usize) -> Option<u8> {
    let mut out = None;
    i2c_each_slave(sys, Some(from_ch), &mut |c, u| {
        if c == ch {
            out = u.slave_take_tx(sys);
        }
    });
    out
}
pub fn i2c_slave_stop(sys: &WasmSystem, from_ch: usize, ch: usize) {
    i2c_each_slave(sys, Some(from_ch), &mut |c, u| {
        if c == ch {
            u.slave_stop(sys);
        }
    });
}

/// Component hook for attached I2C parts (Wokwi-style onI2CWrite/
/// onI2CRead): an external master talks to `addr` (7-bit) with a
/// write-then-read transaction against the virtual EEPROM backing
/// (0x50 jig path on the Arduino Wire channel IIC1 — the same bytes
/// the `ra4m1_map_i2c_eeprom` proof moves via guest MMIO).
/// `write` bytes land from the current EEPROM pointer (first byte =
/// pointer set, like the guest-master flow); `read_len` bytes stream
/// back with pointer auto-increment. Returns the reply (empty when
/// the address NACKs). Guest-master live state is untouched (no
/// BBSY/pending/flag moves).
pub fn i2c_component_exchange(addr: u8, write: &[u8], read_len: usize) -> Vec<u8> {
    for slot in crate::sys().p.peripherals.iter() {
        if slot.start != crate::peripherals::ra_i2c::IIC1_BASE {
            continue;
        }
        let mut b = match slot.peripheral.try_borrow_mut() {
            Ok(b) => b,
            Err(_) => return Vec::new(),
        };
        if let Some(u) = b
            .as_any_mut()
            .downcast_mut::<crate::peripherals::ra_i2c::RaIic>()
        {
            return u.component_exchange(addr, write, read_len);
        }
        return Vec::new();
    }
    Vec::new()
}

// ── Shared SPI bus (master clocks a slave on the other channel) ───────────
// Returns the byte the selected slave shifted out (None = no slave wired,
// caller falls back to loopback jig / pulled-up 0xFF).
pub fn spi_slave_shift(sys: &WasmSystem, from_ch: usize, mosi: u8) -> Option<u8> {
    for slot in sys.p.peripherals.iter() {
        if slot.start != 0x4007_2000 && slot.start != 0x4007_2100 {
            continue;
        }
        let ch = if slot.start == 0x4007_2000 { 0 } else { 1 };
        if ch == from_ch {
            continue;
        }
        let mut b = slot.peripheral.borrow_mut();
        if let Some(u) = b.as_any_mut().downcast_mut::<crate::peripherals::ra_spi::RaSpi>() {
            if u.is_slave() {
                return Some(u.slave_clock_in(sys, mosi));
            }
        }
    }
    None
}

// ── Virtual SD card (SPI mode) behind a per-channel arming flag ───────────
// Same jig pattern as the loopback/EEPROM: when armed, the channel's
// master MOSI stream feeds the SD engine (CMD0/8/55/41/58/17/24) and
// MISO comes from it. CS is not modeled (always selected).
static SD_ARMED: OnceLock<Mutex<std::collections::HashSet<u32>>> = OnceLock::new();
static SD_CARD: OnceLock<Mutex<crate::peripherals::ra_spi::SpiSd>> = OnceLock::new();

fn sd_armed_set() -> &'static Mutex<std::collections::HashSet<u32>> {
    SD_ARMED.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}
fn sd_card() -> &'static Mutex<crate::peripherals::ra_spi::SpiSd> {
    SD_CARD.get_or_init(|| Mutex::new(crate::peripherals::ra_spi::SpiSd::new()))
}
pub fn spi_set_sd_card(base: u32, on: bool) {
    let mut m = sd_armed_set().lock().unwrap();
    if on {
        m.insert(base);
    } else {
        m.remove(&base);
    }
}
/// MISO byte for a master MOSI byte (None = no card armed here, caller
/// falls through to slave/jig/0xFF).
pub fn spi_sd_exchange(base: u32, mosi: u8) -> Option<u8> {
    if sd_armed_set().lock().unwrap().contains(&base) {
        Some(sd_card().lock().unwrap().exchange(mosi))
    } else {
        None
    }
}

/// Side-effect-free MISO peek for component polls (Some pulled-up 0xFF
/// when no slave answers and the loopback jig is off; None when an SD
/// card is armed — the SD engine is stateful, so components must drive
/// the guest master for SD traffic instead of peeking).
pub fn spi_sd_peek(base: u32) -> Option<u8> {
    if sd_armed_set().lock().unwrap().contains(&base) {
        None
    } else {
        Some(0xFF)
    }
}

/// Staged slave reply peek for component polls (None = channel is not
/// a staged slave; the master path falls through to jig/0xFF).
/// Never consumes the staged byte (guest exchange still shifts it out).
pub fn spi_slave_peek(ch: usize) -> Option<u8> {
    for slot in crate::sys().p.peripherals.iter() {
        if slot.start != 0x4007_2000 && slot.start != 0x4007_2100 {
            continue;
        }
        let c = if slot.start == 0x4007_2000 { 0 } else { 1 };
        if c != ch {
            continue;
        }
        let mut b = match slot.peripheral.try_borrow_mut() {
            Ok(b) => b,
            Err(_) => return None,
        };
        if let Some(u) = b
            .as_any_mut()
            .downcast_mut::<crate::peripherals::ra_spi::RaSpi>()
        {
            return u.slave_peek();
        }
        return None;
    }
    None
}

/// Copy out one 512B virtual-SD block for host-side inspection (empty
/// when out of range). Used by the demo block viewer.
pub fn sd_read_block(block: u32) -> Vec<u8> {
    sd_card().lock().unwrap().read_block(block as usize).unwrap_or_default()
}

/// Test-jig CAN error injection (the virtual wire never errors on its
/// own): stuff `rx` receive / `tx` transmit errors into CAN0's
/// counters (EWF/EPF/BOEF + ERI event per EIER, like a real storm).
pub fn can_inject_errors(sys: &WasmSystem, rx: u16, tx: u16) {
    for slot in sys.p.peripherals.iter() {
        if slot.start != crate::peripherals::ra_can::CAN0_BASE {
            continue;
        }
        let mut b = slot.peripheral.borrow_mut();
        if let Some(u) = b.as_any_mut().downcast_mut::<crate::peripherals::ra_can::RaCan>() {
            u.inject_errors(sys, rx, tx);
            return;
        }
    }
}


// ── DTC engine (event-driven transfers, no CPU) ────────────────────────────
// R_DTC_Open programs the SRAM vector table (DTCVBR + 4B per activation
// IRQ holding the transfer_info_t pointer) and R_DTC_Enable arms the
// source via IELSR.DTCE. Activations queue here; FlatMemory's sync-DMA
// path (which owns RAM access) resolves descriptors fresh from SRAM on
// every fire, so R_DTC_Reconfigure needs no modeling. Repeat state
// lives here, keyed by activation IRQ.
#[derive(Clone, Copy, Default)]
pub struct DtcCh {
    pub settings: u32,
    pub dest: u32,
    pub length: u16,
    pub base_src: u32,
    pub base_dest: u32,
    pub remaining: u16,
    pub done: bool,
}
static DTC_PENDING: OnceLock<Mutex<Vec<u32>>> = OnceLock::new();
static DTC_CH: OnceLock<Mutex<std::collections::HashMap<u32, DtcCh>>> = OnceLock::new();

fn dtc_pending() -> &'static Mutex<Vec<u32>> {
    DTC_PENDING.get_or_init(|| Mutex::new(Vec::new()))
}
fn dtc_ch() -> &'static Mutex<std::collections::HashMap<u32, DtcCh>> {
    DTC_CH.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}
pub fn dtc_activate(irq: u32) {
    dtc_pending().lock().unwrap().push(irq);
    dma_kick();
}
// Set whenever a DMA/DTC transfer is staged or an activation queues;
// the CPU run loop drains the sync paths while it is set (cheap atomic
// poll every 16 instructions, no mutex traffic when idle).
static DMA_ACTIVE: AtomicBool = AtomicBool::new(false);
pub fn dma_active() -> bool {
    DMA_ACTIVE.load(Ordering::Relaxed)
}
pub fn dma_kick() {
    DMA_ACTIVE.store(true, Ordering::Relaxed);
}
pub fn dma_idle() {
    DMA_ACTIVE.store(false, Ordering::Relaxed);
}
pub fn dtc_take_pending() -> Option<u32> {
    dtc_pending().lock().unwrap().pop()
}
pub fn dtc_get(irq: u32) -> Option<DtcCh> {
    dtc_ch().lock().unwrap().get(&irq).copied()
}
pub fn dtc_put(irq: u32, ch: DtcCh) {
    dtc_ch().lock().unwrap().insert(irq, ch);
}


static EVENT_SEQ: AtomicU64 = AtomicU64::new(0);
// ELC event links, mirrored from RaElc ELSR writes (0xFFFF = no link).
static ELC_LINKS: [AtomicU32; 32] = [const { AtomicU32::new(0xFFFF) }; 32];
pub fn elc_set_link(peripheral: usize, event: u32) {
    if peripheral < 32 {
        ELC_LINKS[peripheral].store(event, Ordering::Relaxed);
    }
}
// DMAC activation sources, mirrored from ICU DELSR writes (0xFFFF = none).
static DELSR_MIRROR: [AtomicU32; 8] = [const { AtomicU32::new(0xFFFF) }; 8];
pub fn delsr_set_link(channel: usize, event: u32) {
    if channel < 8 {
        DELSR_MIRROR[channel].store(event, Ordering::Relaxed);
    }
}
pub fn delsr_event(channel: usize) -> u32 {
    if channel < 8 {
        DELSR_MIRROR[channel].load(Ordering::Relaxed)
    } else {
        0xFFFF
    }
}
/// GPT channels whose compare/overflow events feed an armed DMAC
/// activation link. The mem path advances exactly these timers every
/// few instructions so event-driven DMA keeps phase even when the
/// test driver ticks coarsely (a 48k-instruction tick holds ~10 baud
/// periods at 9600).
pub fn dmac_listened_gpt() -> Vec<u8> {
    let mut out = Vec::new();
    for ch in 0..8 {
        let ev = DELSR_MIRROR[ch].load(Ordering::Relaxed);
        if (87..151).contains(&ev) {
            let g = ((ev - 87) / 8) as u8;
            if !out.contains(&g) {
                out.push(g);
            }
        }
    }
    out
}
/// Direct DMAC activation: match `event` against the DELSR links and
/// queue one unit per armed channel with remaining count. Called
/// synchronously from the raising timer (exact due time), so no
/// pending queue or drain pass is needed. A channel with no remaining
/// count ignores requests (silicon: DTE auto-clears at transfer end).
pub fn dmac_notify(sys: &WasmSystem, event: u32, due: u64) {
    if std::env::var("DMAEVLOG").is_ok() {
        eprintln!("DMACNQ ev={} due={} now={}", event, due, instruction_count());
    }
    let seq = EVENT_SEQ.fetch_add(1, Ordering::Relaxed);
    for slot in sys.p.peripherals.iter() {
        if slot.start != crate::peripherals::ra_dma::DMAC_BASE {
            continue;
        }
        if let Ok(mut b) = slot.peripheral.try_borrow_mut() {
            if let Some(d) = b
                .as_any_mut()
                .downcast_mut::<crate::peripherals::ra_dma::RaDmac>()
            {
                d.queue_event_unit(sys, event, due, seq);
            }
        }
        break;
    }
}
pub fn reset_event_routing() {
    for l in ELC_LINKS.iter() {
        l.store(0xFFFF, Ordering::Relaxed);
    }
    for d in DELSR_MIRROR.iter() {
        d.store(0xFFFF, Ordering::Relaxed);
    }
}
// DMAC event units queued-but-not-executed per channel. The mem path
// decrements on execution; the DMAC completes a channel (DTE clear +
// end event) on the tick after its last unit runs, so firmware
// busy-waits observe completion only after the data has moved.
static DMAC_OUTSTANDING: [AtomicU32; 8] = [const { AtomicU32::new(0) }; 8];
pub fn dmac_unit_queued(ch: usize) {
    if ch < 8 {
        DMAC_OUTSTANDING[ch].fetch_add(1, Ordering::Relaxed);
    }
}
pub fn dmac_unit_done(ch: usize) {
    if ch < 8 {
        DMAC_OUTSTANDING[ch].fetch_sub(1, Ordering::Relaxed);
    }
}
pub fn dmac_outstanding(ch: usize) -> u32 {
    if ch < 8 {
        DMAC_OUTSTANDING[ch].load(Ordering::Relaxed)
    } else {
        0
    }
}
// Route a raised event through the ELC links: any peripheral whose ELSR
// selects this event gets its elc_signal hook (used for the GPT_A link
// that starts/clears the SoftwareSerial RX timer on the pin edge).
pub fn elc_route(sys: &WasmSystem, event: u32) {
    for (p, link) in ELC_LINKS.iter().enumerate() {
        if link.load(Ordering::Relaxed) == event {
            sys.p.for_each_peripheral(&mut |peri| {
                peri.elc_signal(sys, p as u32, event);
            });
        }
    }
}



// ── GPT PWM output levels ───────────────────────────────────────────────────
// Per-channel A/B output latches (GTIOR function 0: 0 on compare, 1 on
// cycle end). PORT pins whose PFS selects a GPT function follow the
// mapped channel (Arduino Minima PWM pin table below). Reset with the
// globals; tests clear explicitly (register tests run no reset).
static GPT_OUT: [[AtomicBool; 2]; 14] = [const { [const { AtomicBool::new(false) }; 2] }; 14];

pub fn gpt_out_set(ch: usize, ab: usize, level: bool) {
    if ch < 14 && ab < 2 {
        GPT_OUT[ch][ab].store(level, Ordering::Relaxed);
    }
}
pub fn gpt_out_reset() {
    for ch in GPT_OUT.iter() {
        for ab in ch.iter() {
            ab.store(false, Ordering::Relaxed);
        }
    }
}

/// Minima PWM-capable pins (from Arduino's own pinmux table): (port, pin)
/// -> (GPT channel, A=0/B=1). Routed when the pin's PFS PSEL selects a
/// GPT function (codes 0x02/0x03/0x14/0x15/0x16).
pub const GPT_PWM_PINS: [((u8, u8), (usize, usize)); 32] = [
    ((4, 0), (6, 0)), ((4, 1), (6, 1)), ((2, 13), (0, 0)), ((2, 12), (0, 1)),
    ((4, 11), (6, 0)), ((4, 10), (6, 1)), ((4, 9), (5, 0)), ((4, 8), (5, 1)),
    ((2, 5), (4, 0)), ((2, 4), (4, 1)), ((3, 4), (7, 0)), ((3, 3), (7, 1)),
    ((3, 2), (4, 0)), ((3, 1), (4, 1)), ((3, 0), (0, 0)), ((1, 8), (0, 1)),
    ((1, 9), (1, 0)), ((1, 10), (1, 1)), ((1, 11), (3, 0)), ((1, 12), (3, 1)),
    ((1, 13), (2, 0)), ((1, 7), (0, 0)), ((1, 6), (0, 1)), ((1, 5), (1, 0)),
    ((1, 4), (1, 1)), ((1, 3), (2, 0)), ((1, 2), (2, 1)), ((1, 1), (5, 0)),
    ((1, 0), (5, 1)), ((5, 0), (2, 0)), ((5, 1), (2, 1)), ((5, 2), (3, 1)),
];
pub fn gpt_pwm_route(port: u8, pin: u8) -> Option<(usize, usize)> {
    GPT_PWM_PINS.iter().find(|&&(p, _)| p == (port, pin)).map(|&(_, c)| c)
}
pub fn gpt_out_level(ch: usize, ab: usize) -> bool {
    if ch < 14 && ab < 2 {
        GPT_OUT[ch][ab].load(Ordering::Relaxed)
    } else {
        false
    }
}

// ── Dataflash backing (8KB @ 0x40100000, erased 0xFF) ───────────────────────
// Shared by the dataflash memory window and the FACI program/erase engine.
pub const DATAFLASH_SIZE: usize = 8192;
static DATAFLASH: OnceLock<Mutex<[u8; 8192]>> = OnceLock::new();

pub fn dataflash() -> &'static Mutex<[u8; 8192]> {
    DATAFLASH.get_or_init(|| Mutex::new([0xFF; 8192]))
}

// ── RA ICU event routing (IELSR mirror) ────────────────────────────────────
// The RA ICU maps peripheral events to NVIC IRQs at runtime via IELSRn.
// Peripherals raise EVENTS here; every IRQ whose IELSR selects that event
// pends in the NVIC. Event 0 (ELC_EVENT_NONE) never fires.
static IELSR_MIRROR: [AtomicU32; 96] = [const { AtomicU32::new(0) }; 96];

pub fn icu_set_ielsr(irq: usize, event: u32) {
    if irq < 96 {
        IELSR_MIRROR[irq].store(event, Ordering::Relaxed);
    }
}

/// Inject an external-pin edge on ICU IRQ `line` (virtual button press
/// for `attachInterrupt` sketches). Returns whether the line's IRQCR
/// sense matched (and the event fired).
pub fn icu_pin_edge(sys: &WasmSystem, line: usize, falling: bool) -> bool {
    for slot in sys.p.peripherals.iter() {
        if slot.start == crate::peripherals::ra_icu::ICU_BASE {
            let mut b = slot.peripheral.borrow_mut();
            if let Some(u) = b.as_any_mut().downcast_mut::<crate::peripherals::ra_icu::RaIcu>() {
                return u.pin_edge(sys, line, falling);
            }
        }
    }
    false
}

/// Inject a key press on KINT KR `key` (virtual key matrix for the
/// key-return controller). Returns whether the controller was enabled
/// (and the KEY_INT event fired).
pub fn kint_key_press(sys: &WasmSystem, key: usize) -> bool {
    for slot in sys.p.peripherals.iter() {
        if slot.start == crate::peripherals::ra_icu::KINT_BASE {
            let mut b = slot.peripheral.borrow_mut();
            if let Some(u) = b.as_any_mut().downcast_mut::<crate::peripherals::ra_icu::RaKint>() {
                return u.key_press(sys, key);
            }
        }
    }
    false
}

pub fn icu_raise_event(sys: &WasmSystem, event: u32) {
    if event == 0 {
        return;
    }
    elc_route(sys, event);
    for (irq, slot) in IELSR_MIRROR.iter().enumerate() {
        let w = slot.load(Ordering::Relaxed);
        if w & 0x1FF == event {
            if w & (1 << 24) != 0 {
                // IELSR.DTCE (bit24, set by R_DTC_Enable) hands the event
                // to the DTC engine; the transfer completes in the mem
                // path. With TRANSFER_IRQ_END the source IRQ stays quiet
                // until completion/wrap (the mode is known from the last
                // serviced descriptor; unknown the first time = raise).
                // IRQ_EACH and unstarted channels raise alongside.
                dtc_activate(irq as u32);
                let quiet = matches!(dtc_get(irq as u32),
                    Some(ch) if ch.settings & (1 << 21) == 0 && !ch.done);
                if !quiet {
                    sys.p.nvic.borrow_mut().set_intr_pending(irq as i32);
                }
            } else {
                sys.p.nvic.borrow_mut().set_intr_pending(irq as i32);
            }
        }
    }
}

fn icu_reset_mirror() {
    for s in IELSR_MIRROR.iter() {
        s.store(0, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaDir { Read, Write, MemCopy }

#[derive(Debug, Clone)]
pub struct DmaTransfer {
    pub direction: DmaDir,
    pub stream_idx: usize,
    pub dma_name: String,
    pub src: u32,
    pub dst: u32,
    pub size: usize,
    pub peri_addr: u32,
    pub peripheral: bool,
    pub pinc: bool,
    pub p_size: usize,
    pub due: u64,
    pub seq: u64,
}

impl DmaTransfer {
    pub fn to_u32_vec(&self) -> Vec<u32> {
        vec![
            self.direction as u32,
            self.stream_idx as u32,
            self.src,
            self.dst,
            self.size as u32,
            self.peri_addr,
            self.peripheral as u32,
            self.pinc as u32,
            self.p_size as u32,
        ]
    }
}

static DMA_COMPLETED: [AtomicBool; 8] = [
    AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false),
    AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false),
];

pub struct WasmSystem {
    pub p: Rc<Peripherals>,
    pending_dma: RefCell<Vec<DmaTransfer>>,
}

impl WasmSystem {

    /// EK-RA4M1 evaluation-kit target (R7FA4M1AB3CFP, same RA4M1 silicon
    /// as Minima's R7FA4M1AB3CFM in a 100-pin LQFP): identical peripheral
    /// map and base addresses. The difference is board-level, not model
    /// level: no Arduino bootloader (flash boots at 0x00000000, APP_BASE
    /// is unused), the user LED is on P106 (not Minima's P111/D13), and
    /// P205 is TSCAP-A by default (E12 open). Constructor alias so
    /// bare-metal EK firmware (LED1 on P106, J2-header pins) boots
    /// without dragging the Arduino APP_BASE convention along.
    pub fn new_ra4m1() -> Self {
        let p = Rc::new(Peripherals::new_ra4m1());
        WasmSystem { p, pending_dma: RefCell::new(Vec::new()) }
    }

    /// EK-RA4M1 target: same silicon/map as Minima; board differences
    /// (zero-boot, P106 LED, TSCAP P205) are caller conventions, not
    /// model changes. See new_ra4m1 docs.
    pub fn new_ek_ra4m1() -> Self {
        Self::new_ra4m1()
    }

    pub fn queue_dma_transfer(&self, t: DmaTransfer) {
        self.pending_dma.borrow_mut().push(t);
        dma_kick();
    }

    pub fn pending_dma_count(&self) -> usize {
        self.pending_dma.borrow().len()
    }

    pub fn take_pending_dma_transfer(&self, index: usize) -> Option<DmaTransfer> {
        let mut pending = self.pending_dma.borrow_mut();
        if index < pending.len() {
            Some(pending.remove(index))
        } else {
            None
        }
    }

    /// Atomically remove and return the oldest queued DMA transfer iff it is
    /// a pure memory-to-memory move. The CPU core drains these synchronously
    /// right after the guest's EN store, so polling firmware observes
    /// completion without waiting for the JS driver round-trip.
    pub fn take_memcopy_dma_transfer(&self) -> Option<DmaTransfer> {
        let mut pending = self.pending_dma.borrow_mut();
        match pending.first() {
            Some(t) if t.direction == DmaDir::MemCopy && !t.peripheral => Some(pending.remove(0)),
            _ => None,
        }
    }

    pub fn take_due_memcopy_dma_transfer(&self, now: u64) -> Option<DmaTransfer> {
        let mut pending = self.pending_dma.borrow_mut();
        let mut best: Option<usize> = None;
        for (i, t) in pending.iter().enumerate() {
            if t.direction != DmaDir::MemCopy || t.peripheral || t.due > now {
                continue;
            }
            let key = (t.due >> 6, t.stream_idx, t.seq);
            let is_best = match best {
                None => true,
                Some(b) => {
                    let o = &pending[b];
                    key < (o.due >> 6, o.stream_idx, o.seq)
                }
            };
            if is_best {
                best = Some(i);
            }
        }
        best.map(|i| pending.remove(i))
    }

    pub fn has_pending_memcopy(&self) -> bool {
        self.pending_dma
            .borrow()
            .iter()
            .any(|t| t.direction == DmaDir::MemCopy && !t.peripheral)
    }

    pub fn mark_dma_completed(&self, stream_idx: usize, _success: bool) {
        if stream_idx < 8 {
            DMA_COMPLETED[stream_idx].store(true, Ordering::Release);
        }
    }

    pub fn dma_check_completion(&self, stream_idx: usize) -> bool {
        if stream_idx < 8 {
            DMA_COMPLETED[stream_idx].swap(false, Ordering::Acquire)
        } else {
            false
        }
    }

    pub fn tick(&self) {
        let p = self.p.clone();
        for slot in &p.peripherals {
            slot.peripheral.borrow_mut().tick(self);
        }
        p.nvic.borrow_mut().maybe_set_systick_intr_pending();
    }

    pub fn addr_desc(&self, addr: u32) -> String {
        self.p.addr_desc(addr)
    }
}

pub type System = WasmSystem;

// SAFETY: single-system Wasm (see snapshot system.rs for the full rationale).
#[cfg(target_arch = "wasm32")]
unsafe impl Sync for WasmSystem {}
#[cfg(target_arch = "wasm32")]
unsafe impl Send for WasmSystem {}
#[cfg(not(target_arch = "wasm32"))]
unsafe impl Sync for WasmSystem {}
#[cfg(not(target_arch = "wasm32"))]
unsafe impl Send for WasmSystem {}

// ── process-wide state reset ────────────────────────────────────────────────
/// Clear every process-lifetime global so a fresh emulator instance starts
/// clean. Call this BEFORE creating a new instance.
pub fn reset_globals() {
    use std::sync::atomic::Ordering::Relaxed;
    if let Some(m) = UART_OUTPUT.get() { m.lock().unwrap().clear(); }
    if let Some(m) = ADC_OVERRIDES.get() { m.lock().unwrap().clear(); }
    if let Some(m) = CTSU_OVERRIDES.get() { m.lock().unwrap().clear(); }
    if let Some(m) = SCI_SPI_LOOPBACK.get() { m.lock().unwrap().clear(); }
    gpt_out_reset();
    crate::peripherals::ra_port::RaPort::soft_wire_reset();
    crate::peripherals::ra_port::matrix_trace_reset();
    reset_event_routing();
    for d in DMAC_OUTSTANDING.iter() {
        d.store(0, Relaxed);
    }
    if let Some(m) = DTC_PENDING.get() { m.lock().unwrap().clear(); }
    if let Some(m) = DTC_CH.get() { m.lock().unwrap().clear(); }
    if let Some(m) = SD_ARMED.get() { m.lock().unwrap().clear(); }
    if let Some(m) = SD_CARD.get() { *m.lock().unwrap() = crate::peripherals::ra_spi::SpiSd::new(); }
    *dataflash().lock().unwrap() = [0xFF; 8192];
    icu_reset_mirror();
    for i in 0..8 {
        DMA_COMPLETED[i].store(false, Relaxed);
    }
    WATCHDOG_RESET_EVENT.store(false, Relaxed);
    MPU_ENABLED.store(false, Relaxed);
    MPU_FAULT_VALID.store(false, Relaxed);
    ALIGN_FAULT_VALID.store(false, Relaxed);
    BUS_FAULT_VALID.store(false, Relaxed);
    UNALIGN_TRP.store(false, Relaxed);
    set_mpu_force_unpriv(false);
    CURRENT_PRIV.store(true, Relaxed);
    CURRENT_HFNMI.store(false, Relaxed);
    CURRENT_IPSR.store(0, Relaxed);
    // NOTE: deliberately NOT resetting INSTRUCTION_COUNT here — peripherals
    // capture last_tick at construction; zeroing the global afterwards makes
    // elapsed = now.wrapping_sub(last_tick) enormous and breaks tick logic.
}
