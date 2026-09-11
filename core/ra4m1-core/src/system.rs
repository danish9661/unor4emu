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

pub fn icu_raise_event(sys: &WasmSystem, event: u32) {
    if event == 0 {
        return;
    }
    for (irq, slot) in IELSR_MIRROR.iter().enumerate() {
        if slot.load(Ordering::Relaxed) == event {
            sys.p.nvic.borrow_mut().set_intr_pending(irq as i32);
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
    pub fn new_ra4m1() -> Self {
        let p = Rc::new(Peripherals::new_ra4m1());
        WasmSystem { p, pending_dma: RefCell::new(Vec::new()) }
    }

    pub fn queue_dma_transfer(&self, t: DmaTransfer) {
        self.pending_dma.borrow_mut().push(t);
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
