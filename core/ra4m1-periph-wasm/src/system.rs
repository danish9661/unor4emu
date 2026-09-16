use std::sync::atomic::{AtomicU64, AtomicBool, AtomicI32, AtomicU8, AtomicU32, Ordering};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;
use crate::peripherals::{Peripherals, gpio::GpioPorts};
use crate::ext_devices::ExtDevices;

// UART output buffer: USART write_dr pushes chars here, JS reads via get_uart_output()
use std::sync::OnceLock;
static UART_OUTPUT: OnceLock<Mutex<String>> = OnceLock::new();
pub fn get_uart_output() -> &'static Mutex<String> {
    UART_OUTPUT.get_or_init(|| Mutex::new(String::new()))
}

// Global ExtDevices: populated by JS add_* calls before init
static EXT_DEVICES: OnceLock<Mutex<ExtDevices>> = OnceLock::new();
pub fn get_ext_devices() -> &'static Mutex<ExtDevices> {
    EXT_DEVICES.get_or_init(|| Mutex::new(ExtDevices::default()))
}

pub static INSTRUCTION_COUNT: AtomicU64 = AtomicU64::new(0);
pub fn instruction_count() -> u64 { INSTRUCTION_COUNT.load(Ordering::Relaxed) }

static WATCHDOG_RESET_EVENT: AtomicBool = AtomicBool::new(false);
// MPU master-enable latch (MPU_CTRL.ENABLE write). Level semantics follow
// the register: clearing ENABLE clears this. The driver halts while set —
// protection is not enforced, so running on would be silently wrong.
static MPU_ENABLED: AtomicBool = AtomicBool::new(false);
/// True while the guest holds MPU_CTRL.ENABLE (protection unmodeled).
pub fn is_mpu_enabled() -> bool { MPU_ENABLED.load(Ordering::Acquire) }
pub fn set_mpu_enabled(v: bool) { MPU_ENABLED.store(v, Ordering::Release); }
// Current CPU context for the MPU check (FlatMemory has no CPU handle).
// Updated at Cpu::new/reset, exception entry/return, and MSR CONTROL —
// the only points where (ipsr, CONTROL) change, so there is zero
// per-instruction cost. Relaxed: single-threaded producer/consumer.
static CURRENT_PRIV: AtomicBool = AtomicBool::new(true);
static CURRENT_HFNMI: AtomicBool = AtomicBool::new(false);
pub fn current_privileged() -> bool { CURRENT_PRIV.load(Ordering::Relaxed) }
pub fn current_hfnmi() -> bool { CURRENT_HFNMI.load(Ordering::Relaxed) }
pub fn set_cpu_context(priv_: bool, hfnmi: bool) {
    CURRENT_PRIV.store(priv_, Ordering::Relaxed);
    CURRENT_HFNMI.store(hfnmi, Ordering::Relaxed);
}
// Live exception number for ICSR.VECTACTIVE (SCB reads have no CPU
// handle, like the privilege cache above). Updated at every take/chain/
// return alongside ipsr; 0 in thread mode.
static CURRENT_IPSR: AtomicU32 = AtomicU32::new(0);
pub fn current_ipsr() -> u32 { CURRENT_IPSR.load(Ordering::Relaxed) }
pub fn set_current_ipsr(v: u32) { CURRENT_IPSR.store(v, Ordering::Relaxed); }
// Force the next MPU check(s) to unprivileged, for LDRT/STRT (which probe
// memory as-unprivileged even in handler mode). Set/cleared around the
// single access by a Drop guard in the decoder, so no path leaks it.
static MPU_FORCE_UNPRIV: AtomicBool = AtomicBool::new(false);
pub fn set_mpu_force_unpriv(v: bool) { MPU_FORCE_UNPRIV.store(v, Ordering::Relaxed); }
pub(crate) fn mpu_force_unpriv() -> bool { MPU_FORCE_UNPRIV.load(Ordering::Relaxed) }
// CCR.UNALIGN_TRP cache for the access hot path (mem.rs must not do a
// model read per access): refreshed on every SCB CCR write. Reset state
// is clear, matching CCR reset.
static UNALIGN_TRP: AtomicBool = AtomicBool::new(false);
pub fn set_unalign_trp(v: bool) { UNALIGN_TRP.store(v, Ordering::Relaxed); }
pub(crate) fn unalign_trp() -> bool { UNALIGN_TRP.load(Ordering::Relaxed) }
// Deferred alignment-fault channel: like the MPU data path, the faulting
// access completes dropped and the UsageFault raises before the next
// fetch (one-instruction imprecision, documented; flags exact).
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
// Deferred bus-fault channel (unmapped access = precise BusFault on
// silicon): same deferred shape as the MPU/align paths (access completes
// dummy, flags exact, PC one behind). Peripheral-space holes are NOT
// routed here — unlisted SVD devices read-as-0 by design (many are
// documented-reserved; HALs probe them), so only the mem.rs bad-arms
// (wild memory: null derefs, overruns, gaps) pend.
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
// Deferred MPU data-fault channel (see cpu/mod.rs): FlatMemory latches a
// violation (returning dummy/dropping the access); the run loop raises it
// before the next fetch. One instruction may complete with dummy data —
// documented imprecision; fault vector/flags/address are exact.
static MPU_FAULT_VALID: AtomicBool = AtomicBool::new(false);
static MPU_FAULT_ADDR: AtomicU32 = AtomicU32::new(0);
static MPU_FAULT_EXEC: AtomicBool = AtomicBool::new(false);
pub fn pend_mpu_fault(addr: u32, exec: bool) {
    // First fault wins: a split access (RAM write32 = 4x write8) pends once
    // per byte; the last byte must not overwrite the faulting address
    // (silicon reports the access; the PPB probe needs the base address).
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
/// Latch MemManage fault state (CFSR MMFSR bits + MMFAR) via read-modify-
/// write, preserving any BusFault/UsageFault bits already latched.
pub fn latch_memmanage_fault(sys: &WasmSystem, mmfsr_bits: u32, mmfar: Option<u32>) {
    let cfsr = sys.p.read(sys, 0xE000ED28, 4);
    sys.p.write(sys, 0xE000ED28, 4, cfsr | (mmfsr_bits & 0xFF));
    if let Some(a) = mmfar {
        sys.p.write(sys, 0xE000ED34, 4, a);
    }
}
// Persistent reset-cause bits, latched on watchdog expiry until the firmware
// clears them via RCC->CSR RMVF. Bit 29 (IWDGRSTF) / bit 30 (WWDGRSTF).
static IWDG_RESET_FLAG: AtomicBool = AtomicBool::new(false);
static WWDG_RESET_FLAG: AtomicBool = AtomicBool::new(false);

// Software SPI configs queued before init, registered after GPIO exists
static SOFTWARE_SPI_CONFIGS: OnceLock<Mutex<Vec<(String, Option<String>, String, String, String)>>> = OnceLock::new();
pub fn get_software_spi_configs() -> &'static Mutex<Vec<(String, Option<String>, String, String, String)>> {
    SOFTWARE_SPI_CONFIGS.get_or_init(|| Mutex::new(Vec::new()))
}
pub fn get_sys_for_cpu() -> &'static WasmSystem { crate::sys() }
pub fn is_watchdog_reset_requested() -> bool { WATCHDOG_RESET_EVENT.swap(false, Ordering::Acquire) }
/// Latch a watchdog reset event. cause: bit0 = IWDG, bit1 = WWDG (so a single
/// call can set both if needed). The event flag is consumed by the JS driver
/// (is_watchdog_reset_requested); the per-source flag persists for RCC->CSR.
pub fn request_watchdog_reset(cause: u8) {
    WATCHDOG_RESET_EVENT.store(true, Ordering::Release);
    if cause & 1 != 0 { IWDG_RESET_FLAG.store(true, Ordering::Release); }
    if cause & 2 != 0 { WWDG_RESET_FLAG.store(true, Ordering::Release); }
}
pub fn iwdg_reset_flag() -> bool { IWDG_RESET_FLAG.load(Ordering::Acquire) }
pub fn wwdg_reset_flag() -> bool { WWDG_RESET_FLAG.load(Ordering::Acquire) }
/// Clear the latched watchdog reset-cause bits (RCC->CSR RMVF write).
pub fn clear_watchdog_reset_flags() {
    IWDG_RESET_FLAG.store(false, Ordering::Release);
    WWDG_RESET_FLAG.store(false, Ordering::Release);
}

// Ethernet MAC event flags
static ETH_TX_POLL: AtomicBool = AtomicBool::new(false);
static ETH_RX_POLL: AtomicBool = AtomicBool::new(false);
// 0=none, 1=TX done, 2=RX done, 3=both. Set by JS after descriptor processing.
static ETH_DONE: AtomicU8 = AtomicU8::new(0);
// TX/RX descriptor addresses captured when poll demand is written
static ETH_TX_DESC_ADDR: AtomicU32 = AtomicU32::new(0);
static ETH_RX_DESC_ADDR: AtomicU32 = AtomicU32::new(0);

pub fn eth_signal_tx_poll(desc_addr: u32) { ETH_TX_POLL.store(true, Ordering::Release); ETH_TX_DESC_ADDR.store(desc_addr, Ordering::Release); }
pub fn eth_signal_rx_poll(desc_addr: u32) { ETH_RX_POLL.store(true, Ordering::Release); ETH_RX_DESC_ADDR.store(desc_addr, Ordering::Release); }
pub fn eth_is_tx_poll() -> bool { ETH_TX_POLL.load(Ordering::Acquire) }
pub fn eth_clear_tx_poll() { ETH_TX_POLL.store(false, Ordering::Release); }
pub fn eth_is_rx_poll() -> bool { ETH_RX_POLL.load(Ordering::Acquire) }
pub fn eth_clear_rx_poll() { ETH_RX_POLL.store(false, Ordering::Release); }
pub fn eth_get_tx_desc_addr() -> u32 { ETH_TX_DESC_ADDR.load(Ordering::Acquire) }
pub fn eth_get_rx_desc_addr() -> u32 { ETH_RX_DESC_ADDR.load(Ordering::Acquire) }
pub fn eth_set_done(flags: u8) { ETH_DONE.fetch_or(flags, Ordering::Release); }
pub fn eth_take_done() -> u8 { ETH_DONE.swap(0, Ordering::Acquire) }

// ── RA ICU event routing (IELSR mirror) ────────────────────────────────────
// The RA ICU maps peripheral events to NVIC IRQs at runtime via IELSRn
// (FSP R_BSP_IrqCfg writes IELSR[irq] = event, e.g. ELC_EVENT_AGT0_INT).
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
    if std::env::var("EVLOG").is_ok() && event == 51 {
        static EV_N: AtomicU64 = AtomicU64::new(0);
        let n = EV_N.fetch_add(1, Ordering::Relaxed);
        if n < 40 {
            eprintln!("EVLOG ev={} cnt={} pend={:x}", event, instruction_count(),
                sys.p.nvic.borrow().pending_bits() as u64);
        }
    }
    if event == 0 {
        return;
    }
    // NOTE: no log_event here - producers that drive event DMA (GPT)
    // log directly with exact due times; logging here too would double
    // every transfer (observed as 2x CCMPA counts starving nothing but
    // doubling DMA work). Future DELSR users must log their own dues.
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

// FLASH programming/erase state shared with the JS driver (which applies the
// actual memory mutations to guest memory).
static FLASH_PROGRAMMING: AtomicBool = AtomicBool::new(false);
static FLASH_ERASE: Mutex<Option<(u32, u32)>> = Mutex::new(None);

pub fn set_flash_programming(v: bool) { FLASH_PROGRAMMING.store(v, Ordering::Release); }
pub fn flash_is_programming() -> bool { FLASH_PROGRAMMING.load(Ordering::Acquire) }
pub fn queue_flash_erase(start: u32, len: u32) {
    *FLASH_ERASE.lock().unwrap() = Some((start, len));
}
pub fn take_flash_erase() -> Option<(u32, u32)> {
    FLASH_ERASE.lock().unwrap().take()
}

impl WasmSystem {
    pub fn flash_erase_applied(&self) {
        self.p.flash_erase_applied();
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
    pub pinc: bool, // PINC: increment the peripheral address per transfer
    pub p_size: usize, // peripheral data width in bytes (PSIZE)
    /// Virtual timestamp (instruction_count) at which the unit becomes
    /// due. 0 = immediate (legacy/STM32/toy flows). Event-driven RA DMAC
    /// units carry their trigger's exact due time; the mem path defers
    /// them so waveforms keep phase (a 48k-instruction tick holds many
    /// baud periods - executing all at once would compress the wave and
    /// break TX/RX pairing).
    pub due: u64,
    /// Event sequence (tiebreak after channel).
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

// Per-stream DMA interrupt info: IRQ number (-1 = none) and flags (bit 0=TCIE, 1=HTIE, 2=TEIE)
static DMA_STREAM_IRQ: [AtomicI32; 8] = [
    AtomicI32::new(-1), AtomicI32::new(-1), AtomicI32::new(-1), AtomicI32::new(-1),
    AtomicI32::new(-1), AtomicI32::new(-1), AtomicI32::new(-1), AtomicI32::new(-1),
];
static DMA_STREAM_FLAGS: [AtomicU8; 8] = [
    AtomicU8::new(0), AtomicU8::new(0), AtomicU8::new(0), AtomicU8::new(0),
    AtomicU8::new(0), AtomicU8::new(0), AtomicU8::new(0), AtomicU8::new(0),
];

pub fn set_dma_intr_info(stream_idx: usize, irq: i32, flags: u8) {
    if stream_idx < 8 {
        DMA_STREAM_IRQ[stream_idx].store(irq, Ordering::Release);
        DMA_STREAM_FLAGS[stream_idx].store(flags, Ordering::Release);
    }
}

// --- CAN bus: staged transmit requests arbitrate globally across CAN1/CAN2.
// A TXRQ mailbox write stages a frame; the next system tick runs arbitration
// (lowest arbitration ID wins; ties broken by node, then mailbox index). The
// winner's mailbox completes (TSR TXOK|TME|RQCP) and the frame is delivered
// to every node's RX FIFO that passes its filter banks (the transmitter also
// receives its own frame, matching real CAN self-ACK traffic). Losers stay
// staged and complete on the next free round.
#[derive(Debug, Clone, Copy)]
pub struct CanFrame {
    pub node: u8,        // 1 = CAN1, 2 = CAN2
    pub mailbox: usize,  // 0..=2
    pub id: u32,         // 11-bit STID, or 29-bit value for extended frames
    pub ext: bool,
    pub rtr: bool,
    pub dlc: u8,
    pub data: [u8; 8],
    pub loopback: bool,  // BTR LBKM: deliver only to the transmitting node
}

static CAN_STAGED: OnceLock<Mutex<Vec<CanFrame>>> = OnceLock::new();
fn can_staged() -> &'static Mutex<Vec<CanFrame>> {
    CAN_STAGED.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn can_stage_tx(f: CanFrame) {
    can_staged().lock().unwrap().push(f);
}

pub(crate) fn can_take_staged() -> Vec<CanFrame> {
    std::mem::take(&mut *can_staged().lock().unwrap())
}

pub(crate) fn can_restage(frames: Vec<CanFrame>) {
    can_staged().lock().unwrap().extend(frames);
}

// --- Audio: WAV-backed sample source + TX capture FIFO --------------------
// JS loads a real WAV file (audio_load_wav) into the PCM source. I2S/SAI DR
// reads (RX/DMA PERIPH->MEM) consume the next source sample; DR writes (TX/
// DMA MEM->PERIPH) append to the capture FIFO, which JS drains with
// audio_take_capture (playback in the browser via WebAudio, or comparison
// against the firmware's intended stream in tests).
pub struct PcmSource {
    pub data: Vec<i16>,
    pub cursor: usize,
}

pub fn audio_clear() {
    if let Some(m) = AUDIO_SOURCE.get() {
        *m.lock().unwrap() = None;
    }
    if let Some(m) = AUDIO_CAPTURE.get() {
        m.lock().unwrap().clear();
    }
}

/// Parse a standard RIFF WAV (PCM 16-bit, mono or stereo — stereo sources
/// are downmixed by taking the left channel), returning an error string on
/// malformed input or unsupported formats.
pub fn audio_load_wav(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".to_string());
    }
    let mut pos = 12usize;
    let mut fmt: Option<(u16, u16, u16)> = None; // (format, channels, bits)
    let mut data: Option<(usize, usize)> = None; // (offset, len)
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]]) as usize;
        let body = pos + 8;
        if &id[..] == b"fmt " && body + 16 <= bytes.len() {
            fmt = Some((
                u16::from_le_bytes([bytes[body], bytes[body + 1]]),
                u16::from_le_bytes([bytes[body + 2], bytes[body + 3]]),
                u16::from_le_bytes([bytes[body + 14], bytes[body + 15]]),
            ));
        } else if &id[..] == b"data" {
            data = Some((body, size.min(bytes.len() - body)));
            break;
        }
        pos = body + size + (size & 1); // chunks are word-aligned
    }
    let (format, channels, bits) = fmt.ok_or("missing fmt chunk")?;
    let (off, len) = data.ok_or("missing data chunk")?;
    if format != 1 {
        return Err(format!("unsupported audio format {format} (only PCM)"));
    }
    if channels == 0 || channels > 2 {
        return Err("channels must be 1 or 2".to_string());
    }
    if bits != 16 {
        return Err(format!("unsupported bit depth {bits} (only 16-bit)"));
    }
    let mut samples = Vec::with_capacity(len / 2);
    let mut i = off;
    // stereo: take the left channel; mono: every sample
    let stride = if channels == 2 { 4 } else { 2 };
    while i + 1 < off + len {
        let s = i16::from_le_bytes([bytes[i], bytes[i + 1]]);
        samples.push(s);
        i += stride;
    }
    if samples.is_empty() {
        return Err("empty data chunk".to_string());
    }
    *AUDIO_SOURCE.get_or_init(|| Mutex::new(None)).lock().unwrap() =
        Some(PcmSource { data: samples, cursor: 0 });
    Ok(())
}

pub fn audio_source_remaining() -> u32 {
    let Some(m) = AUDIO_SOURCE.get() else { return 0 };
    let g = m.lock().unwrap();
    g.as_ref().map_or(0, |s| (s.data.len() - s.cursor) as u32)
}

/// Consume the next source sample (None when no WAV is loaded or it is
/// exhausted — callers fall back to their synthetic generator).
pub fn audio_source_next() -> Option<i16> {
    let mut g = AUDIO_SOURCE.get()?.lock().unwrap();
    let src = g.as_mut()?;
    if src.cursor >= src.data.len() { return None; }
    let s = src.data[src.cursor];
    src.cursor += 1;
    Some(s)
}

pub fn audio_capture_push(v: u16) {
    AUDIO_CAPTURE.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap().push(v);
}

pub fn audio_take_capture() -> Vec<u16> {
    AUDIO_CAPTURE.get().map_or(Vec::new(), |m| std::mem::take(&mut *m.lock().unwrap()))
}

static AUDIO_SOURCE: OnceLock<Mutex<Option<PcmSource>>> = OnceLock::new();
static AUDIO_CAPTURE: OnceLock<Mutex<Vec<u16>>> = OnceLock::new();
pub(crate) fn audio_buses_ready() -> bool {
    AUDIO_SOURCE.get().is_some() && AUDIO_CAPTURE.get().is_some()
}

// ── ADC channel-value injection (JS hardware layer plumbing) ───────────────
// A global override table, not per-Adc-instance state: JS can set/clear a
// channel value at any time (unlike spi_tap/i2c_register_slave, which must
// run before init() because Spi/I2c snapshot their device list once at
// construction — see docs/components.md). Adc::start_conversion checks this
// before falling back to its synthetic temp/vref/vbat/random logic.
static ADC_OVERRIDES: OnceLock<Mutex<std::collections::HashMap<(String, u32), u32>>> = OnceLock::new();

fn adc_overrides() -> &'static Mutex<std::collections::HashMap<(String, u32), u32>> {
    ADC_OVERRIDES.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}
pub fn adc_set_override(peripheral: &str, channel: u32, value: u32) {
    adc_overrides().lock().unwrap().insert((peripheral.to_string(), channel), value & 0xFFF);
}
pub fn adc_clear_override(peripheral: &str, channel: u32) {
    adc_overrides().lock().unwrap().remove(&(peripheral.to_string(), channel));
}
pub fn adc_get_override(peripheral: &str, channel: u32) -> Option<u32> {
    adc_overrides().lock().unwrap().get(&(peripheral.to_string(), channel)).copied()
}

// ── CTSU touch-count overrides (JS/test plumbing) ──────────────────────────
// Keyed ("CTSU", channel) -> raw sensor count. Values above 16 bits clamp
// to 0xFFFF and set SOVF, so overflow is testable through the same path.
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

// ── Shared I2C bus fabric (master <-> slave across channels) ───────────────// Lets one channel's master talk to another channel's slave (SAR set,
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

// ── Shared SPI bus (master clocks a slave on the other channel) ───────────// Returns the byte the selected slave shifted out (None = no slave wired,
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

// ── Peripheral event log (DMAC activation, ELC links) ─────────────────────
// The DMAC drains activations synchronously: GPT compare/overflow edges
// call dmac_notify with their exact virtual timestamp, which matches the
// ICU DELSR activation sources and queues one transfer unit per armed
// channel. ELC links are mirrored from the ELC peripheral (ELSR writes)
// so routing needs no peripheral borrows in the raise path.
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

// ── Dataflash backing (8KB @ 0x40100000, erased 0xFF) ───────────────────────// Shared by the dataflash memory window and the FACI program/erase engine:
// the FSP R_FLASH_LP driver programs through FACI while Arduino reads hit
// the memory-mapped window directly. Reset restores the erased state.
pub const DATAFLASH_SIZE: usize = 8192;
static DATAFLASH: OnceLock<Mutex<[u8; 8192]>> = OnceLock::new();

pub fn dataflash() -> &'static Mutex<[u8; 8192]> {
    DATAFLASH.get_or_init(|| Mutex::new([0xFF; 8192]))
}

// ── SPI bus taps (JS hardware layer plumbing) ──────────────────────────────
// Event word layout: bit 31 = CS edge event, bit 30 = asserted (1) when CS
// is a CS event, bit 29 = DC level (1 = data) when the tap has a DC pin,
// bits 7..0 = the shifted byte. Byte and CS events interleave in the order
// the controller produced them.
static SPI_TAP_EVENTS: OnceLock<Mutex<std::collections::HashMap<String, Vec<u32>>>> = OnceLock::new();
static SPI_TAP_MISO: OnceLock<Mutex<std::collections::HashMap<String, Vec<u8>>>> = OnceLock::new();

fn spi_tap_events() -> &'static Mutex<std::collections::HashMap<String, Vec<u32>>> {
    SPI_TAP_EVENTS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}
fn spi_tap_miso() -> &'static Mutex<std::collections::HashMap<String, Vec<u8>>> {
    SPI_TAP_MISO.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

pub fn spi_tap_push_byte(peri: &str, v: u32) {
    spi_tap_events().lock().unwrap().entry(peri.to_string()).or_default().push(v & 0x2FF);
}
pub fn spi_tap_push_cs(peri: &str, asserted: bool) {
    let e = 0x8000_0000u32 | (if asserted { 1 << 30 } else { 0 });
    spi_tap_events().lock().unwrap().entry(peri.to_string()).or_default().push(e);
}
pub fn spi_tap_take_events(peri: &str) -> Vec<u32> {
    spi_tap_events().lock().unwrap().get_mut(peri).map(std::mem::take).unwrap_or_default()
}
pub fn spi_tap_miso_push(peri: &str, bytes: &[u8]) {
    spi_tap_miso().lock().unwrap().entry(peri.to_string()).or_default().extend_from_slice(bytes);
}
pub(crate) fn spi_tap_miso_pop(peri: &str) -> u8 {
    spi_tap_miso().lock().unwrap().get_mut(peri).and_then(|q| q.first().copied().map(|b| { q.remove(0); b })).unwrap_or(0xFF)
}

// ── I2C bus taps (JS hardware layer plumbing) ─────────────────────────────
// The TX queue carries u32 events: bit31 = boundary event (bit30 = 1 START /
// 0 STOP), otherwise the low byte is one master-write byte. START/STOP let
// the JS device parser find transaction group boundaries (SSD1306 needs
// them: a data group's length is only terminated by STOP).
static I2C_TAP_TX: OnceLock<Mutex<std::collections::HashMap<String, Vec<u32>>>> = OnceLock::new();
static I2C_TAP_RX: OnceLock<Mutex<std::collections::HashMap<String, Vec<u8>>>> = OnceLock::new();

fn i2c_tap_tx() -> &'static Mutex<std::collections::HashMap<String, Vec<u32>>> {
    I2C_TAP_TX.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}
fn i2c_tap_rx() -> &'static Mutex<std::collections::HashMap<String, Vec<u8>>> {
    I2C_TAP_RX.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

pub fn i2c_tap_push_tx(peri: &str, v: u8) {
    i2c_tap_tx().lock().unwrap().entry(peri.to_string()).or_default().push(v as u32);
}
pub fn i2c_tap_push_event(peri: &str, ev: u32) {
    i2c_tap_tx().lock().unwrap().entry(peri.to_string()).or_default().push(ev);
}
pub fn i2c_tap_take_tx(peri: &str) -> Vec<u32> {
    i2c_tap_tx().lock().unwrap().get_mut(peri).map(std::mem::take).unwrap_or_default()
}
pub fn i2c_tap_rx_push(peri: &str, bytes: &[u8]) {
    i2c_tap_rx().lock().unwrap().entry(peri.to_string()).or_default().extend_from_slice(bytes);
}
pub(crate) fn i2c_tap_rx_pop(peri: &str) -> u8 {
    i2c_tap_rx().lock().unwrap().get_mut(peri).and_then(|q| q.first().copied().map(|b| { q.remove(0); b })).unwrap_or(0xFF)
}

// ── FSMC bank taps (JS memory-mapped device plumbing) ─────────────────────
// Each access is TWO event words, so the JS device sees the address as well
// as the value (an 8080-mode display decodes one address line as RS/DC —
// command vs pixel data — so the offset is what distinguishes them):
//   word0: bit31 = 1 write / 0 read, bits 30..0 = byte offset in the bank
//   word1: value written, or value returned on a read
// Reads are answered from FSMC_TAP_DATA, a JS-pushed queue (`fsmc_push_data`)
// analogous to the SPI tap's MISO queue; an empty queue reads back 0.
static FSMC_TAP_EVENTS: OnceLock<Mutex<std::collections::HashMap<usize, Vec<u32>>>> = OnceLock::new();
static FSMC_TAP_DATA: OnceLock<Mutex<std::collections::HashMap<usize, Vec<u32>>>> = OnceLock::new();

fn fsmc_tap_events() -> &'static Mutex<std::collections::HashMap<usize, Vec<u32>>> {
    FSMC_TAP_EVENTS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}
fn fsmc_tap_data() -> &'static Mutex<std::collections::HashMap<usize, Vec<u32>>> {
    FSMC_TAP_DATA.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

pub(crate) fn fsmc_tap_push(bank: usize, write: bool, offset: u32, value: u32) {
    let hdr = (offset & 0x7FFF_FFFF) | if write { 1 << 31 } else { 0 };
    let mut m = fsmc_tap_events().lock().unwrap();
    let q = m.entry(bank).or_default();
    q.push(hdr);
    q.push(value);
}
pub fn fsmc_tap_take_events(bank: usize) -> Vec<u32> {
    fsmc_tap_events().lock().unwrap().get_mut(&bank).map(std::mem::take).unwrap_or_default()
}
pub fn fsmc_tap_data_push(bank: usize, values: &[u32]) {
    fsmc_tap_data().lock().unwrap().entry(bank).or_default().extend_from_slice(values);
}
pub(crate) fn fsmc_tap_data_pop(bank: usize) -> u32 {
    fsmc_tap_data().lock().unwrap().get_mut(&bank)
        .and_then(|q| if q.is_empty() { None } else { Some(q.remove(0)) })
        .unwrap_or(0)
}

// ── "this register read is coming from the DMA engine" ────────────────────
// Set around the peripheral-side reads the DMA driver issues. A peripheral
// register read is otherwise indistinguishable from a CPU load, but for a
// STREAMING peripheral the difference is the whole point: the DMA drains at
// bus rate and cannot overrun, while a CPU polling loop is far too slow and
// does. DCMI reads it in its DR handler.
static DMA_READ_ACTIVE: AtomicBool = AtomicBool::new(false);
pub fn set_dma_read_active(v: bool) { DMA_READ_ACTIVE.store(v, Ordering::Relaxed); }
pub(crate) fn dma_read_active() -> bool { DMA_READ_ACTIVE.load(Ordering::Relaxed) }

// ── DCMI frame source (JS camera sensor plumbing) ─────────────────────────
static DCMI_FRAME: OnceLock<Mutex<Option<(u32, u32, Vec<u8>)>>> = OnceLock::new();
pub fn dcmi_feed_frame(w: u32, h: u32, pixels: &[u8]) {
    *DCMI_FRAME.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some((w, h, pixels.to_vec()));
}
pub(crate) fn dcmi_frame() -> Option<(u32, u32, Vec<u8>)> {
    DCMI_FRAME.get()?.lock().unwrap().clone()
}
pub fn dcmi_clear() {
    if let Some(m) = DCMI_FRAME.get() {
        *m.lock().unwrap() = None;
    }
}

pub struct WasmSystem {
    pub p: Rc<Peripherals>,
    pending_dma: RefCell<Vec<DmaTransfer>>,
}

#[cfg(test)]
pub fn test_dummy_system() -> ::std::rc::Rc<crate::system::System> {
    use crate::ext_devices::ExtDevices;
    use crate::peripherals::Peripherals;
    let gpio = GpioPorts::default();
    // Empty ext devices: keeps tests independent of the global (shared,
    // Rc<RefCell>-based) device list, whose cross-thread borrows race when
    // tests run in parallel (see bug fix 2026-08-10).
    let empty = ExtDevices::default();
    let p = Rc::new(Peripherals::new_wasm(gpio, &empty));
    ::std::rc::Rc::new(WasmSystem { p, pending_dma: RefCell::new(Vec::new()) })
}

/// Like `test_dummy_system` but with a caller-supplied device list, for
/// tests that need a peripheral actually bound to an ext device (the
/// binding happens once, at construction).
#[cfg(test)]
pub fn test_system_with(ext: &crate::ext_devices::ExtDevices) -> ::std::rc::Rc<crate::system::System> {
    use crate::peripherals::Peripherals;
    let p = Rc::new(Peripherals::new_wasm(GpioPorts::default(), ext));
    ::std::rc::Rc::new(WasmSystem { p, pending_dma: RefCell::new(Vec::new()) })
}

#[cfg(test)]
pub fn dummy_gpio() -> crate::peripherals::gpio::GpioPorts {
    crate::peripherals::gpio::GpioPorts::default()
}

impl WasmSystem {
    pub fn new() -> Self {
        let gpio = GpioPorts::default();
        let ext = get_ext_devices().lock().unwrap();
        let p = Rc::new(Peripherals::new_wasm(gpio, &*ext));
        drop(ext);
        Self::register_software_spis(&p);
        WasmSystem { p, pending_dma: RefCell::new(Vec::new()) }
    }

    pub fn new_svd(svd_xml: &str) -> Self {
        let gpio = GpioPorts::default();
        let ext = get_ext_devices().lock().unwrap();
        let p = Rc::new(Peripherals::from_svd(svd_xml, gpio, &*ext));
        drop(ext);
        Self::register_software_spis(&p);
        WasmSystem { p, pending_dma: RefCell::new(Vec::new()) }
    }


    /// EK-RA4M1 evaluation-kit target (R7FA4M1AB3CFP, same RA4M1 silicon
    /// as Minima's R7FA4M1AB3CFM in a 100-pin LQFP): identical peripheral
    /// map and base addresses. The difference is board-level, not model
    /// level: no Arduino bootloader (flash boots at 0x00000000, APP_BASE
    /// is unused), the user LED is on P106 (not Minima's P111/D13), and
    /// P205 is TSCAP-A by default (E12 open). Constructor alias so
    /// bare-metal EK firmware (LED1 on P106, J2-header pins) boots
    /// without dragging the Arduino APP_BASE convention along.
    pub fn new_ra4m1() -> Self {
        let gpio = GpioPorts::default();
        let ext = get_ext_devices().lock().unwrap();
        let p = Rc::new(Peripherals::new_ra4m1(gpio, &*ext));
        drop(ext);
        WasmSystem { p, pending_dma: RefCell::new(Vec::new()) }
    }

    fn register_software_spis(p: &Peripherals) {
        use crate::peripherals::sw_spi::{SoftwareSpi, SoftwareSpiConfig};
        let configs = get_software_spi_configs().lock().unwrap();
        let ext_devices = get_ext_devices().lock().unwrap();
        for (name, cs, clk, miso, mosi) in configs.iter() {
            let config = SoftwareSpiConfig {
                name: name.clone(),
                cs: cs.clone(),
                clk: clk.clone(),
                miso: miso.clone(),
                mosi: mosi.clone(),
            };
            SoftwareSpi::register(config, &mut p.gpio.borrow_mut(), &ext_devices);
        }
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

    /// Is a queued DMA transfer aimed at a peripheral register inside
    /// `[start, end)`? A streaming peripheral uses this to tell "the DMA is
    /// my consumer" from "the CPU is polling me", which are very different
    /// flow-control situations — see the DCMI tick.
    pub fn dma_pending_for_range(&self, start: u32, end: u32) -> bool {
        self.pending_dma.borrow().iter()
            .any(|t| t.peripheral && t.peri_addr >= start && t.peri_addr < end)
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
    /// a pure memory-to-memory move (MemCopy direction, no peripheral side).
    /// The CPU core drains these synchronously right after the guest's EN
    /// store, so polling firmware observes completion (data + TCIF/HTIF)
    /// without waiting for the JS driver round-trip. Peripheral-involving
    /// transfers always stay staged for the driver (it owns the data path).
    pub fn take_memcopy_dma_transfer(&self) -> Option<DmaTransfer> {
        let mut pending = self.pending_dma.borrow_mut();
        match pending.first() {
            Some(t) if t.direction == DmaDir::MemCopy && !t.peripheral => Some(pending.remove(0)),
            _ => None,
        }
    }

    /// Due-time ordered take for the mem path: among MemCopy units whose
    /// due time has arrived, pick the smallest (due bucket, channel,
    /// sequence). The 64-instruction bucket absorbs trigger-to-execution
    /// latency so coincident TX/RX requests order by DMAC fixed priority
    /// (lower channel first = RX read-before-write on a bit boundary),
    /// like silicon.
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

    /// True while any MemCopy unit is still queued (due or not) - the
    /// mem path stays active instead of idling with future units left.
    pub fn has_pending_memcopy(&self) -> bool {
        self.pending_dma
            .borrow()
            .iter()
            .any(|t| t.direction == DmaDir::MemCopy && !t.peripheral)
    }

    pub fn mark_dma_completed(&self, stream_idx: usize, _success: bool) {
        DMA_COMPLETED[stream_idx].store(true, Ordering::Release);
        // Fire NVIC interrupt after transfer completes
        if stream_idx < 8 {
            let irq = DMA_STREAM_IRQ[stream_idx].swap(-1, Ordering::Acquire);
            if irq >= 0 {
                let flags = DMA_STREAM_FLAGS[stream_idx].swap(0, Ordering::Acquire);
                if flags & 0x7 != 0 {
                    self.p.nvic.borrow_mut().set_intr_pending(irq);
                }
            }
        }
    }

    pub fn dma_check_completion(&self, stream_idx: usize) -> bool {
        DMA_COMPLETED[stream_idx].swap(false, Ordering::Acquire)
    }

    pub fn tick(&self) {
        let p = self.p.clone();
        for slot in &p.peripherals {
            slot.peripheral.borrow_mut().tick(self);
        }
        crate::peripherals::can::arbitrate_bus(self);
        p.nvic.borrow_mut().maybe_set_systick_intr_pending();
    }

    pub fn addr_desc(&self, addr: u32) -> String {
        self.p.addr_desc(addr)
    }
}

pub type System = WasmSystem;

// SAFETY: WasmSystem contains Rc<RefCell> peripherals and is single-system
// (SYS AtomicPtr). On wasm32-unknown-unknown the module is single-threaded
// — Send/Sync are never exercised. wasm-bindgen requires them for exported
// types, so we assert unsafely. Native `cargo test` (multi-threaded) is
// guarded by per-suite Mutex locks (CAN_TEST_LOCK, AUDIO_TEST_LOCK, etc.)
// to avoid `already borrowed` panics. Do not share WasmSystem across OS
// threads in a native build; use the WASM artifact for multi-instance.
#[cfg(target_arch = "wasm32")]
unsafe impl Sync for WasmSystem {}
#[cfg(target_arch = "wasm32")]
unsafe impl Send for WasmSystem {}
#[cfg(not(target_arch = "wasm32"))]
unsafe impl Sync for WasmSystem {}
#[cfg(not(target_arch = "wasm32"))]
unsafe impl Send for WasmSystem {}

#[cfg(test)]
mod audio_tests {
    use super::*;

    // AUDIO_SOURCE / AUDIO_CAPTURE are process-global (the model is
    // single-system), so audio tests must run serially.
    static AUDIO_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub fn make_pcm16_wav(samples: &[i16], channels: u16) -> Vec<u8> {
        let data_len = samples.len() * 2 * channels as usize;
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
        w.extend_from_slice(b"WAVE");
        w.extend_from_slice(b"fmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes()); // PCM
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&44100u32.to_le_bytes());
        w.extend_from_slice(&(44100 * 2 * channels as u32).to_le_bytes());
        w.extend_from_slice(&(2 * channels as u16).to_le_bytes());
        w.extend_from_slice(&16u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&(data_len as u32).to_le_bytes());
        for s in samples {
            for _ in 0..channels {
                w.extend_from_slice(&s.to_le_bytes());
            }
        }
        w
    }

    #[test]
    fn wav_parse_mono_pcm16() {
        let _g = AUDIO_TEST_LOCK.lock().unwrap();
        audio_clear();
        let src: Vec<i16> = (0..8).map(|i| i * 100 + 1).collect();
        let wav = make_pcm16_wav(&src, 1);
        assert!(audio_load_wav(&wav).is_ok());
        assert_eq!(audio_source_remaining(), 8);
        for (i, s) in src.iter().enumerate() {
            assert_eq!(audio_source_next(), Some(*s), "sample {i}");
        }
        assert_eq!(audio_source_next(), None);
        assert_eq!(audio_source_remaining(), 0);
    }

    #[test]
    fn wav_stereo_downmix_takes_left() {
        let _g = AUDIO_TEST_LOCK.lock().unwrap();
        audio_clear();
        let wav = make_pcm16_wav(&[7, 99], 2);
        assert!(audio_load_wav(&wav).is_ok());
        assert_eq!(audio_source_next(), Some(7));
        assert_eq!(audio_source_next(), Some(99));
    }

    #[test]
    fn wav_rejects_garbage_and_bad_format() {
        let _g = AUDIO_TEST_LOCK.lock().unwrap();
        audio_clear();
        assert!(audio_load_wav(b"nope").is_err());
        let mut wav = make_pcm16_wav(&[1, 2, 3], 1);
        wav[20] = 3; // corrupt audio format -> not PCM
        assert!(audio_load_wav(&wav).is_err());
    }

    #[test]
    fn capture_fifo_roundtrip() {
        let _g = AUDIO_TEST_LOCK.lock().unwrap();
        audio_clear();
        assert_eq!(audio_take_capture(), Vec::<u16>::new());
        audio_capture_push(0x1234);
        audio_capture_push(0x5678);
        assert_eq!(audio_take_capture(), vec![0x1234, 0x5678]);
        assert_eq!(audio_take_capture(), Vec::<u16>::new());
    }
}

// ── process-wide state reset ────────────────────────────────────────────────
/// Clear every process-lifetime global so a fresh emulator instance starts
/// clean.  Without this, creating a second instance in the same process is
/// broken in a subtle way: `ExtDevices` ACCUMULATES, and the peripheral
/// constructors use `find_*_device(name)`, which returns the FIRST match —
/// so instance 2 silently binds to instance 1's devices (measured: a regfile
/// seeded 0x22 read back 0x11 from the previous instance, and rtc_test hung
/// right after its first UART line when run after another firmware).
///
/// Call this BEFORE registering devices for a new instance (emulator.js does
/// it immediately after the wasm module is ready).  It is safe to call when
/// no instance exists — every table is lazily created.
pub fn reset_globals() {
    use std::sync::atomic::Ordering::Relaxed;
    if let Some(m) = EXT_DEVICES.get() { *m.lock().unwrap() = ExtDevices::default(); }
    if let Some(m) = SOFTWARE_SPI_CONFIGS.get() { m.lock().unwrap().clear(); }
    if let Some(m) = UART_OUTPUT.get() { m.lock().unwrap().clear(); }
    if let Some(m) = SPI_TAP_EVENTS.get() { m.lock().unwrap().clear(); }
    if let Some(m) = SPI_TAP_MISO.get() { m.lock().unwrap().clear(); }
    if let Some(m) = FSMC_TAP_EVENTS.get() { m.lock().unwrap().clear(); }
    if let Some(m) = FSMC_TAP_DATA.get() { m.lock().unwrap().clear(); }
    if let Some(m) = I2C_TAP_TX.get() { m.lock().unwrap().clear(); }
    if let Some(m) = I2C_TAP_RX.get() { m.lock().unwrap().clear(); }
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
    if let Some(m) = CAN_STAGED.get() { m.lock().unwrap().clear(); }
    if let Some(m) = AUDIO_SOURCE.get() { *m.lock().unwrap() = None; }
    if let Some(m) = AUDIO_CAPTURE.get() { m.lock().unwrap().clear(); }
    if let Some(m) = DCMI_FRAME.get() { *m.lock().unwrap() = None; }
    *FLASH_ERASE.lock().unwrap() = None;
    DMA_READ_ACTIVE.store(false, Relaxed);
    FLASH_PROGRAMMING.store(false, Relaxed);
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
    ETH_TX_POLL.store(false, Relaxed);
    ETH_RX_POLL.store(false, Relaxed);
    ETH_DONE.store(0, Relaxed);
    ETH_TX_DESC_ADDR.store(0, Relaxed);
    ETH_RX_DESC_ADDR.store(0, Relaxed);
    icu_reset_mirror();
    for i in 0..8 {
        DMA_COMPLETED[i].store(false, Relaxed);
        DMA_STREAM_IRQ[i].store(0, Relaxed);
        DMA_STREAM_FLAGS[i].store(0, Relaxed);
    }
    // NOTE: deliberately NOT resetting INSTRUCTION_COUNT here — peripherals
    // capture last_tick at construction; zeroing the global afterwards makes
    // elapsed = now.wrapping_sub(last_tick) enormous and breaks tick logic.
    // INSTRUCTION_COUNT.store(0, Relaxed);
}
