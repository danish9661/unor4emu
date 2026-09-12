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

/// Inject an external-pin edge on ICU IRQ line (virtual button press
/// for `attachInterrupt` sketches). Returns whether the line fired.
#[wasm_bindgen]
pub fn icu_pin_edge(line: u8, falling: bool) -> bool {
    system::icu_pin_edge(sys(), line as usize, falling)
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
