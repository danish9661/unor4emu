//! ra4m1-core: Cortex-M4 + RA4M1 peripheral map. UNO R4 Minima only.

use std::sync::atomic::{AtomicPtr, Ordering};
use wasm_bindgen::prelude::*;

mod system;
pub mod peripherals;
pub mod cpu;
pub mod ra4m1;

use system::WasmSystem;

// Process-wide system instance (AtomicPtr to a leaked Box so `init_ra4m1()`
// can replace it while `sys()` still hands out a `&'static`. See the
// snapshot's lib.rs for the full rationale).
static SYS: AtomicPtr<WasmSystem> = AtomicPtr::new(std::ptr::null_mut());

pub(crate) fn sys() -> &'static WasmSystem {
    let p = SYS.load(Ordering::Acquire);
    assert!(!p.is_null(), "WasmSystem not initialized");
    // SAFETY: p came from Box::into_raw in set_sys and is never freed.
    unsafe { &*p }
}

/// Install a fresh system, leaking the previous one (see SYS above).
fn set_sys(s: WasmSystem) {
    SYS.store(Box::into_raw(Box::new(s)), Ordering::Release);
}

#[cfg(test)]
pub(crate) fn init_for_test(s: WasmSystem) {
    set_sys(s);
}

/// Initialize the emulator with the RA4M1 (UNO R4) peripheral map.
#[wasm_bindgen]
pub fn init_ra4m1() {
    console_error_panic_hook::set_once();
    set_sys(WasmSystem::new_ra4m1());
}

/// Initialize the emulator with the EK-RA4M1 target (same RA4M1 silicon
/// and peripheral map as Minima; board conventions differ: flash boots
/// at 0x00000000 with no Arduino bootloader, user LED1 is on P106).
#[wasm_bindgen]
pub fn init_ek_ra4m1() {
    console_error_panic_hook::set_once();
    set_sys(WasmSystem::new_ek_ra4m1());
}

/// Clear all process-lifetime globals so a NEW emulator instance starts
/// clean. Must be called before creating that instance.
#[wasm_bindgen]
pub fn reset_state() {
    system::reset_globals();
}

#[wasm_bindgen]
pub fn periph_read(addr: u32, width: u32) -> u32 {
    sys().p.read(&*sys(), addr, width as u8)
}

#[wasm_bindgen]
pub fn periph_write(addr: u32, width: u32, value: u32) {
    sys().p.write(&*sys(), addr, width as u8, value);
}

/// Advance the instruction clock by `delta` and tick peripherals once.
#[wasm_bindgen]
pub fn tick_n(delta: u32) {
    system::INSTRUCTION_COUNT.fetch_add(delta as u64, Ordering::Relaxed);
    sys().tick();
}

/// Run one peripheral-model tick WITHOUT advancing the instruction clock.
#[wasm_bindgen]
pub fn tick_peripherals() {
    sys().tick();
}

/// Check if any interrupt is pending (non-consuming).
#[wasm_bindgen]
pub fn has_pending_interrupt() -> bool {
    sys().p.nvic.borrow().has_pending()
}

#[wasm_bindgen]
pub fn get_next_pending_interrupt() -> i32 {
    sys().p.nvic.borrow_mut().get_and_clear_next_intr_pending()
        .unwrap_or(-255)
}

/// Inject a received byte into the SCI UART at the given base address.
#[wasm_bindgen]
pub fn uart_rx_byte(addr: u32, byte: u8) -> bool {
    sys().p.rx_byte(&*sys(), addr, byte)
}

/// Collect UART output since last call.
#[wasm_bindgen]
pub fn get_uart_output() -> String {
    use std::mem::take;
    take(&mut *system::get_uart_output().lock().unwrap())
}

/// Peek UART output WITHOUT consuming it (demo polling: the Rust proofs
/// use `.contains("got=A5")` on the live buffer; draining here would
/// eat the verdict before the check sees it).
#[wasm_bindgen]
pub fn peek_uart_output() -> String {
    system::get_uart_output().lock().unwrap().clone()
}

/// Board identity for component runners (OpenHW-style platforms poll
/// this once instead of hardcoding silicon constants).
/// Layout: [chip_id, flash_bytes, ram_bytes, dataflash_bytes,
/// app_base, fcpu_hz, board_kind] where chip_id 0x4D31 = "M1" (RA4M1)
/// and board_kind 0 = Minima, 1 = EK-RA4M1.
#[wasm_bindgen]
pub fn board_info() -> Vec<u32> {
    vec![
        0x4D31,
        crate::ra4m1::FLASH_SIZE as u32,
        crate::ra4m1::RAM_SIZE as u32,
        crate::ra4m1::DATAFLASH_SIZE as u32,
        0x4000,
        48_000_000,
        0,
    ]
}

/// Minima Arduino pin -> (port, bit). Index = Arduino Dx/Ax number
/// (D0-D13, A0-A5 = 14-19). From MINIMA `variant.cpp` g_pin_cfg.
const MINIMA_PINS: [(u8, u8); 20] = [
    (3, 1), (3, 2), (1, 5), (1, 4), (1, 3), (1, 2), (1, 6), (1, 7),
    (3, 4), (3, 3), (1, 12), (1, 9), (1, 10), (1, 11),
    (0, 14), (0, 0), (0, 1), (0, 2), (1, 1), (1, 0),
];

/// Arduino pin number -> packed (port << 8 | bit), or -1 when unmapped.
/// Covers D0-D13 + A0-A5 (14-19); TX/RX LEDs and SWD pins are not
/// Arduino-numbered and return -1.
#[wasm_bindgen]
pub fn arduino_pin_to_port_bit(pin: u32) -> i32 {
    if (pin as usize) < MINIMA_PINS.len() {
        let (port, bit) = MINIMA_PINS[pin as usize];
        ((port as i32) << 8) | (bit as i32)
    } else {
        -1
    }
}

/// Read one Arduino pin's live output level (PODR bit). Returns 0/1,
/// or -1 for an unmapped pin number. This is the component-facing
/// GPIO read: direction-agnostic, no model side effects (plain MMIO
/// word read, PODR = high half of the settled PCNTR1 layout).
#[wasm_bindgen]
pub fn gpio_read_pin(pin: u32) -> i32 {
    if (pin as usize) >= MINIMA_PINS.len() {
        return -1;
    }
    let (port, bit) = MINIMA_PINS[pin as usize];
    let w = sys().p.read(
        &*sys(),
        crate::ra4m1::PORT_BASE + (port as u32) * 0x20,
        4,
    );
    (((w >> 16) >> bit) & 1) as i32
}

/// Drive one Arduino pin's input level (PIDR bit) from a virtual
/// component (button, jumper, wired peer). Returns false for an
/// unmapped pin. Direction-agnostic like the `set_input` hook the
/// SoftwareSerial wire uses; firmware observes it via `digitalRead`
/// (PIDR path) or `icu_pin_edge` (IRQ path).
#[wasm_bindgen]
pub fn gpio_set_pin_input(pin: u32, level: bool) -> bool {
    if (pin as usize) >= MINIMA_PINS.len() {
        return false;
    }
    let (port, bit) = MINIMA_PINS[pin as usize];
    // PORT and PFS are separate RaPort instances; only the PORT one
    // (is_pfs == false) owns the PIDR file. try_borrow skips a slot
    // that is mid-MMIO (never panic on reentry from a component poll).
    for slot in sys().p.peripherals.iter() {
        let mut b = match slot.peripheral.try_borrow_mut() {
            Ok(b) => b,
            Err(_) => continue,
        };
        if let Some(u) = b
            .as_any_mut()
            .downcast_mut::<peripherals::ra_port::RaPort>()
        {
            if u.is_port() {
                u.set_input(port, bit, level);
                return true;
            }
        }
    }
    false
}

/// Inject an external edge on an Arduino pin's IRQ line and report the
/// D13 LED level after it (convenience for button components: edge +
/// verdict in one call). `line` is the ICU IRQ line (e.g. 0 for D2),
/// `falling` the edge direction. Returns the PODR bit of P111 (1 = LED
/// on). Prefer `icu_pin_edge` + `gpio_read_pin(13)` when the runner
/// needs the steps separated.
#[wasm_bindgen]
pub fn gpio_button_press(line: u8, falling: bool) -> i32 {
    system::icu_pin_edge(sys(), line as usize, falling);
    gpio_read_pin(13)
}

/// Force an ADC channel to read `value` (14-bit) instead of the default.
#[wasm_bindgen]
pub fn adc_set_channel_value(channel: u32, value: u32) {
    system::adc_set_override("ADC0", channel, value);
}

/// Remove a channel override.
#[wasm_bindgen]
pub fn adc_clear_channel_value(channel: u32) {
    system::adc_clear_override("ADC0", channel);
}

/// Force a CTSU channel to read `value` (16-bit, saturating) instead of
/// the default touch count.
#[wasm_bindgen]
pub fn ctsu_set_channel_value(channel: u32, value: u32) {
    system::ctsu_set_override(channel, value);
}

/// Remove a CTSU channel override.
#[wasm_bindgen]
pub fn ctsu_clear_channel_value(channel: u32) {
    system::ctsu_clear_override(channel);
}

/// Tie an SCI channel's MOSI to MISO (SPI loopback jig) or not.
/// Off by default: idle MISO reads pulled-up 0xFF.
#[wasm_bindgen]
pub fn spi_set_loopback(addr: u32, on: bool) {
    system::sci_set_spi_loopback(addr, on);
}

/// Arm the virtual SD card (SPI mode) on an RSPI channel base, or not.
/// When armed, the channel's master MOSI stream feeds the SD engine.
/// Off by default.
#[wasm_bindgen]
pub fn spi_set_sd_card(addr: u32, on: bool) {
    system::spi_set_sd_card(addr, on);
}

/// Copy out one 512B virtual-SD block (empty when out of range).
#[wasm_bindgen]
pub fn sd_read_block(block: u32) -> Vec<u8> {
    system::sd_read_block(block)
}

/// Component helper: find a peripheral model by MMIO base and run `f`.
/// try_borrow skips a slot that is mid-MMIO (never panic on reentry).
fn with_periph<T>(base: u32, f: impl FnOnce(&mut T) -> Vec<u32>) -> Vec<u32>
where
    T: 'static,
{
    for slot in sys().p.peripherals.iter() {
        if slot.start != base {
            continue;
        }
        let mut b = match slot.peripheral.try_borrow_mut() {
            Ok(b) => b,
            Err(_) => return Vec::new(),
        };
        if let Some(u) = b.as_any_mut().downcast_mut::<T>() {
            return f(u);
        }
        return Vec::new();
    }
    Vec::new()
}

/// CAN mailbox exchange for bus components (Wokwi-style `onCanFrame`).
/// Programs MB`mbox` as TX with standard `id` + up to 8 `data` bytes,
/// arms self-test loopback + a RECREQ receiver, ticks once, and returns
/// the received frame `[id_hi, id_lo, dlc, d0..d7]` (empty when the TX
/// never completed or no receiver matched — same verdict the
/// `ra4m1_map_can_loopback` proof asserts via MMIO).
/// Requires operation mode setup by the guest (CTLR CANM); this only
/// stages one frame on the virtual wire.
#[wasm_bindgen]
pub fn can_send_frame(mbox: u8, id: u32, data: &[u8]) -> Vec<u32> {
    with_periph::<peripherals::ra_can::RaCan>(crate::ra4m1::CAN0_BASE, |u| {
        u.component_send(mbox, id, data)
    })
}

/// Drain CAN RX FIFO heads for bus components. Returns the queued
/// frames back-to-back as `[id, dlc, d0..d7]` 10-word groups (empty
/// when the FIFO is empty). Mirrors the MB24 + RFPCR=0xFF pop the
/// `ra4m1_map_can_fifo` proof performs via MMIO.
#[wasm_bindgen]
pub fn can_take_rx_fifo() -> Vec<u32> {
    with_periph::<peripherals::ra_can::RaCan>(crate::ra4m1::CAN0_BASE, |u| {
        u.component_take_rx_fifo()
    })
}

/// SSI audio sample bridge for speaker/mic components (Wokwi-style
/// `onI2SData`). Pushes guest TX-drained words into the returned tail
/// and feeds `rx` samples into the RX FIFO (pattern counter continues
/// after them). Layout: `[tx_drained..., 0xFFFF_FFFF, rx_depth]` —
/// the sentinel splits the two halves; empty halves are legal.
/// TX drains only while TEN runs and RX only fills while REN runs
/// (same gates the `ra4m1_map_ssi` proof drives via MMIO).
#[wasm_bindgen]
pub fn ssi_exchange(rx: &[u32]) -> Vec<u32> {
    with_periph::<peripherals::ra_ssi::RaSsi>(crate::ra4m1::SSI0_BASE, |u| {
        u.component_exchange(rx)
    })
}

/// RSPI byte exchange for attached SPI components (Wokwi-style
/// `onSPIByte`). Clocks one MOSI byte through the channel at `base`
/// (0x40072000 SPI0 / 0x40072100 SPI1): when the channel is a staged
/// slave (MSTR=0) the byte lands in its RDR and the staged reply
/// shifts out; when it is a master the normal SD/slave/jig/0xFF MISO
/// path answers. Returns `[miso, spsr]` where SPSR packs live
/// SPRF(b7)+SPTEF(b5, always set)+OVRF(b0) — same bits the MMIO proof
/// reads at SPDR+0x03. Side-effect profile matches a bus clock edge
/// (RDR/SPRF/OVRF move like HW); polled firmware still reads its byte
/// via the normal SPDR data read.
#[wasm_bindgen]
pub fn spi_exchange(base: u32, mosi: u8) -> Vec<u32> {
    with_periph::<peripherals::ra_spi::RaSpi>(base, |u| u.component_exchange(mosi))
}

/// I2C bus exchange for attached I2C components (Wokwi-style
/// `onI2CWrite/onI2CRead`). `addr` is the 7-bit slave address, `write`
/// the bytes the external master sends after the address, `read_len`
/// how many bytes it then clocks back. Returns the virtual-EEPROM
/// reply bytes (empty when the address NACKs — only 0x50 answers on
/// the jig path; shared-bus SAR slaves answer via the fabric when a
/// guest slave offers the address). Does not disturb guest-master
/// state: it runs purely on the EEPROM backing + fabric match.
#[wasm_bindgen]
pub fn i2c_exchange(addr: u8, write: &[u8], read_len: u32) -> Vec<u8> {
    crate::system::i2c_component_exchange(addr, write, read_len as usize)
}

/// Inject an external-pin edge on ICU IRQ line (virtual button press
/// for `attachInterrupt` sketches). Returns whether the line fired.
#[wasm_bindgen]
pub fn icu_pin_edge(line: u8, falling: bool) -> bool {
    system::icu_pin_edge(sys(), line as usize, falling)
}

/// Inject a key press on KINT KR `key` (virtual key matrix for the
/// key-return controller). Returns whether it fired (controller enabled).
#[wasm_bindgen]
pub fn kint_key_press(key: u8) -> bool {
    system::kint_key_press(sys(), key as usize)
}

/// Drain the LED-matrix GPIO trace (one 12-port snapshot per PORT/PFS
/// write, oldest first). Used by the Matrix demo to reconstruct the
/// charlieplex frame without sampling luck.
#[wasm_bindgen]
pub fn matrix_trace_take() -> Vec<u32> {
    crate::peripherals::ra_port::matrix_trace_take()
        .into_iter()
        .flat_map(|s| s.into_iter())
        .collect()
}

/// Test-jig CAN error injection: stuff `rx` receive / `tx` transmit
/// errors into CAN0's counters (EWF/EPF/BOEF + ERI event per EIER).
#[wasm_bindgen]
pub fn can_inject_errors(rx: u16, tx: u16) {
    system::can_inject_errors(sys(), rx, tx);
}

/// DMA queue depth for bus/scheduler components (how many staged
/// transfers — memcopy units + DMAC_EV event units — are still waiting
/// in the sync path). Nonzero while event-driven DMA is in flight.
#[wasm_bindgen]
pub fn dma_pending_count() -> u32 {
    sys().pending_dma_count() as u32
}

/// Take one queued DMA transfer descriptor without executing it, as
/// `[dir, stream, src, dst, size, peri_addr, peripheral, pinc, psize]`
/// (same layout as `DmaTransfer::to_u32_vec`; dir 0=Read 1=Write
/// 2=MemCopy). Empty when the queue is empty or `index` is out of
/// range. Drains oldest-first like the sync path; the CPU-side drain
/// is unaffected (it takes only due memcopy units).
#[wasm_bindgen]
pub fn dma_take_pending(index: u32) -> Vec<u32> {
    sys().take_pending_dma_transfer(index as usize)
        .map(|t| t.to_u32_vec())
        .unwrap_or_default()
}

/// Mark a DMA stream completed from the component side (same latch the
/// sync path sets when a unit executes). `dma_check_completion` then
/// reports it once. Same name as the snapshot core's legacy helper so
/// runners use one spelling on both cores.
#[wasm_bindgen]
pub fn dma_set_completed(stream_idx: u32, success: bool) {
    sys().mark_dma_completed(stream_idx as usize, success)
}

/// Consume a completed-DMA latch (true once per completion). Same bit
/// the guest DMAC completion service raises via the sync path.
#[wasm_bindgen]
pub fn dma_check_completion(stream_idx: u32) -> bool {
    sys().dma_check_completion(stream_idx as usize)
}

/// DTC engine state for timer/scheduler components: is any DTC
/// activation still staged (waiting for the sync path to fire it)?
#[wasm_bindgen]
pub fn dtc_has_pending() -> bool {
    system::dtc_take_pending().map_or(false, |irq| {
        system::dtc_activate(irq);
        true
    })
}

/// SCI UART status poll for serial components (no MMIO decode needed).
/// Returns `[tx_ready, rx_ready, rx_byte]`: `tx_ready` = TDRE&&TEND
/// (firmware may write TDR), `rx_ready` = RDRF (a byte waits in RDR),
/// `rx_byte` = the staged RDR value (peeked, NOT consumed — the guest
/// still drains it via its own RDR read, like the SPI peek contract).
/// `base` is the channel base (SCI0 0x40070000 + ch*0x20).
#[wasm_bindgen]
pub fn sci_poll(base: u32) -> Vec<u32> {
    with_periph::<peripherals::ra_sci::RaSci>(base, |u| u.component_poll())
}

/// WDT/IWDT countdown poll for reset/scheduler components. Returns
/// `[down, reload]`: ticks remaining and the TOPS-programmed period
/// (64/256/1024/4096; FSP default TOPS=3 = 4096). `base` is
/// 0x40044200 (WDT) or 0x40044400 (IWDT). A latched expiry also raises
/// the shared watchdog flag (`is_watchdog_reset_requested`).
#[wasm_bindgen]
pub fn wdt_poll(base: u32) -> Vec<u32> {
    with_periph::<peripherals::ra_misc::RaWdt>(base, |u| u.component_poll())
}

/// RTC calendar poll for clock/date components (no BCD decode needed).
/// Returns `[sec, min, hour, day, mon, year_lo, year_hi]` as raw BCD
/// bytes straight from the counters (same values the RTC tab renders).
#[wasm_bindgen]
pub fn rtc_read_bcd() -> Vec<u32> {
    with_periph::<peripherals::ra_rtc::RaRtc>(crate::ra4m1::RTC_BASE, |u| {
        u.component_read_bcd()
    })
}

/// Wire a TX pin to an RX pin for the SoftwareSerial loopback demo
/// (TX PODR changes mirror into RX PIDR + edge IRQ). Must be called
/// after `init_ra4m1()` and before booting the `r4sser` firmware:
/// `soft_wire(1, 4, 1, 5, 0)` = P104 (D3 TX) -> P105 (D2 RX, IRQ0).
#[wasm_bindgen]
pub fn soft_wire(tx_port: u8, tx_bit: u8, rx_port: u8, rx_bit: u8, irq_line: u8) {
    crate::peripherals::ra_port::RaPort::soft_wire(
        tx_port, tx_bit, rx_port, rx_bit, irq_line as usize,
    );
}

#[wasm_bindgen]
pub fn is_watchdog_reset_requested() -> bool {
    system::is_watchdog_reset_requested()
}

/// Drain captured USB TX bytes (virtual-host sink) since last call.
#[wasm_bindgen]
pub fn usb_take_tx() -> Vec<u8> {
    let p = sys().p.clone();
    for slot in &p.peripherals {
        if slot.start == crate::ra4m1::USBFS_BASE {
            let mut b = slot.peripheral.borrow_mut();
            if let Some(u) = b.as_any_mut().downcast_mut::<peripherals::ra_usb::RaUsb>() {
                return std::mem::take(&mut u.tx_capture);
            }
        }
    }
    Vec::new()
}

fn with_usb(f: impl FnOnce(&mut peripherals::ra_usb::RaUsb)) {
    let p = sys().p.clone();
    for slot in &p.peripherals {
        if slot.start == crate::ra4m1::USBFS_BASE {
            let mut b = slot.peripheral.borrow_mut();
            if let Some(u) = b.as_any_mut().downcast_mut::<peripherals::ra_usb::RaUsb>() {
                f(u);
                return;
            }
        }
    }
}

/// Virtual-host: attach with VBUS.
#[wasm_bindgen]
pub fn usb_host_attach() {
    with_usb(|u| u.host_attach());
    crate::system::icu_raise_event(sys(), 51);
}

/// Virtual-host: bus reset (DVST + DVSQ=DEF).
#[wasm_bindgen]
pub fn usb_host_reset() {
    with_usb(|u| u.host_set_dvst(1));
    crate::system::icu_raise_event(sys(), 51);
}

/// Virtual-host: deliver a SETUP packet (CTRT + stage=RDATA).
#[wasm_bindgen]
pub fn usb_host_setup(req: u16, val: u16, idx: u16, len: u16) {
    with_usb(|u| u.host_setup(req, val, idx, len));
    crate::system::icu_raise_event(sys(), 51);
}

/// Virtual-host: status-stage completion (CTRT + stage idle).
#[wasm_bindgen]
pub fn usb_host_status_done() {
    with_usb(|u| u.host_status_done());
    crate::system::icu_raise_event(sys(), 51);
}

/// Virtual-host: 3ms bus idle (DVSQ -> SUSPx, TinyUSB suspend path).
#[wasm_bindgen]
pub fn usb_host_suspend() {
    with_usb(|u| u.host_suspend(sys()));
    crate::system::icu_raise_event(sys(), 51);
}

/// Virtual-host: bus activity again (RESM latches, DVSQ restored).
#[wasm_bindgen]
pub fn usb_host_resume() {
    with_usb(|u| u.host_resume(sys()));
    crate::system::icu_raise_event(sys(), 51);
}

/// Virtual-host: queue received bytes on a pipe (Serial.read path).
#[wasm_bindgen]
pub fn usb_rx_inject(pipe: u8, data: &[u8]) {
    with_usb(|u| u.rx_inject(sys(), pipe as usize, data));
}

use cpu::{Cpu, mem::{FlatMemory, Memory}};
#[wasm_bindgen]
pub struct WasmCpu { cpu: Cpu, mem: FlatMemory }
#[wasm_bindgen]
impl WasmCpu {
    /// EK-RA4M1 CPU+memory: same RA4M1 map as Minima (256KB flash at
    /// 0x00000000, 32KB SRAM at 0x20000000). Boot from the zero vector
    /// table (no APP_BASE offset); LED1 is P106.
    pub fn new_ek_ra4m1(sp: u32, pc: u32) -> Self {
        Self::new_ra4m1(sp, pc)
    }
    /// RA4M1 CPU+memory: 256KB flash at 0x00000000, 32KB SRAM at 0x20000000.
    #[wasm_bindgen(constructor)]
    pub fn new_ra4m1(sp: u32, pc: u32) -> Self {
        let mut mem = FlatMemory::new(
            crate::ra4m1::FLASH_SIZE,
            crate::ra4m1::RAM_SIZE,
        );
        mem.flash_base = crate::ra4m1::FLASH_BASE;
        mem.ram_base = crate::ra4m1::RAM_BASE;
        Self { cpu: Cpu::new(sp, pc), mem }
    }
    /// Load firmware bytes (writes through flash protection).
    pub fn load_firmware(&mut self, data: &[u8], base: u32) { self.mem.load(data, base); }
    pub fn read8(&self, addr: u32) -> u8 { self.mem.read8(addr) }
    pub fn write8(&mut self, addr: u32, v: u8) { self.mem.write8(addr, v) }
    pub fn read32(&self, addr: u32) -> u32 { self.mem.read32(addr) }
    pub fn write32(&mut self, addr: u32, v: u32) { self.mem.write32(addr, v) }
    pub fn mem_read(&self, addr: u32, len: u32) -> Vec<u8> {
        (0..len).map(|i| self.mem.read8(addr.wrapping_add(i))).collect()
    }
    pub fn mem_write(&mut self, addr: u32, data: &[u8]) {
        for (i, &b) in data.iter().enumerate() { self.mem.write8(addr.wrapping_add(i as u32), b); }
    }
    pub fn reset_cpu(&mut self, sp: u32, pc: u32) { self.cpu.reset(sp, pc); }
    /// Enable/disable inline guest exception delivery. Off by default.
    pub fn set_deliver_irqs(&mut self, v: bool) { self.cpu.deliver_irqs = v; }
    pub fn sleeping(&self) -> bool { self.cpu.sleeping }
    pub fn wake(&mut self) { self.cpu.sleeping = false; }
    pub fn get_ipsr(&self) -> u32 { self.cpu.ipsr }
    pub fn get_pc(&self) -> u32 { self.cpu.regs.r[15] }
    pub fn get_sp(&self) -> u32 { self.cpu.regs.r[13] }
    pub fn get_regs(&self) -> Vec<u32> { self.cpu.regs.r.to_vec() }
    pub fn get_xpsr(&self) -> u32 { self.cpu.regs.xpsr }
    pub fn get_sregs(&self) -> Vec<u32> { self.cpu.regs.s.to_vec() }
    pub fn get_fpscr(&self) -> u32 { self.cpu.regs.fpscr }
    pub fn get_primask(&self) -> u32 { self.cpu.regs.primask }
    /// Fault program counter, or 0xFFFF_FFFF when running clean.
    pub fn fault_pc(&self) -> u32 { self.cpu.fault.map(|f| f.pc).unwrap_or(0xFFFF_FFFF) }
    pub fn fault_op1(&self) -> u32 { self.cpu.fault.map(|f| f.op1 as u32).unwrap_or(0) }
    pub fn fault_op2(&self) -> u32 { self.cpu.fault.map(|f| f.op2 as u32).unwrap_or(0) }
    pub fn fault_len(&self) -> u32 { self.cpu.fault.map(|f| f.len as u32).unwrap_or(0) }
    /// Last unmapped-memory access address, or 0xFFFF_FFFF when none.
    pub fn mem_fault(&self) -> u32 { self.mem.bad.get().unwrap_or(0xFFFF_FFFF) }
    pub fn step(&mut self, budget: u32) -> u32 { self.cpu.run(sys(), &mut self.mem, budget) }
    /// PC-trace control for differential debugging (see cpu::trace_*).
    pub fn trace_start(&mut self) { cpu::trace_start(); }
    pub fn trace_stop(&mut self) { cpu::trace_stop(); }
    pub fn take_trace(&mut self) -> Vec<u32> { cpu::take_trace() }
}
