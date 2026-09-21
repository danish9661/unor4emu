use wasm_bindgen::prelude::*;
use ra4m1_core::WasmCpu;

// Small WASM: RA4M1 Minima only. No STM32 and no ESP32 code linked.
//
// This is the component-facing board class (what OpenHW-style runners
// hold: one board instance + full delegation to the proven core).
// Board conventions: the Arduino bootloader occupies 0x0000-0x3FFF, so
// app images link at APP_BASE=0x4000 and load_firmware defaults there;
// EK-RA4M1 images zero-boot at flash base. See AGENTS.md §7.
pub const APP_BASE: u32 = 0x4000;

#[wasm_bindgen]
pub struct UnoR4Minima {
    inner: WasmCpu,
}

#[wasm_bindgen]
impl UnoR4Minima {
    #[wasm_bindgen(constructor)]
    pub fn new(sp: u32, pc: u32) -> Self {
        Self { inner: WasmCpu::new_ra4m1(sp, pc) }
    }

    /// EK-RA4M1 constructor: same silicon, zero-boot (no APP_BASE).
    pub fn new_ek_ra4m1(sp: u32, pc: u32) -> Self {
        Self { inner: WasmCpu::new_ek_ra4m1(sp, pc) }
    }

    pub fn init_system() {
        ra4m1_core::init_ra4m1();
    }

    /// Load an Arduino app image at APP_BASE=0x4000 (bootloader offset).
    /// Pass explicit bases via `load_firmware_at` for EK/zero-boot images.
    pub fn load_firmware(&mut self, data: &[u8]) {
        self.inner.load_firmware(data, APP_BASE);
    }

    /// Load firmware bytes at an explicit base (EK images use 0x00000000).
    pub fn load_firmware_at(&mut self, data: &[u8], base: u32) {
        self.inner.load_firmware(data, base);
    }

    pub fn reset_cpu(&mut self, sp: u32, pc: u32) {
        self.inner.reset_cpu(sp, pc);
    }

    pub fn set_deliver_irqs(&mut self, v: bool) {
        self.inner.set_deliver_irqs(v);
    }

    pub fn step(&mut self, budget: u32) -> u32 {
        self.inner.step(budget)
    }

    pub fn sleeping(&self) -> bool { self.inner.sleeping() }
    pub fn wake(&mut self) { self.inner.wake() }

    pub fn get_pc(&self) -> u32 { self.inner.get_pc() }
    pub fn get_sp(&self) -> u32 { self.inner.get_sp() }
    pub fn get_regs(&self) -> Vec<u32> { self.inner.get_regs() }
    pub fn get_xpsr(&self) -> u32 { self.inner.get_xpsr() }
    pub fn get_ipsr(&self) -> u32 { self.inner.get_ipsr() }
    pub fn get_primask(&self) -> u32 { self.inner.get_primask() }
    pub fn fault_pc(&self) -> u32 { self.inner.fault_pc() }
    pub fn fault_op1(&self) -> u32 { self.inner.fault_op1() }
    pub fn fault_op2(&self) -> u32 { self.inner.fault_op2() }
    pub fn fault_len(&self) -> u32 { self.inner.fault_len() }
    pub fn mem_fault(&self) -> u32 { self.inner.mem_fault() }

    pub fn mem_read(&self, addr: u32, len: u32) -> Vec<u8> {
        self.inner.mem_read(addr, len)
    }
    pub fn mem_write(&mut self, addr: u32, data: &[u8]) {
        self.inner.mem_write(addr, data)
    }
    pub fn read8(&self, addr: u32) -> u8 { self.inner.read8(addr) }
    pub fn write8(&mut self, addr: u32, v: u8) { self.inner.write8(addr, v) }
    pub fn read32(&self, addr: u32) -> u32 { self.inner.read32(addr) }
    pub fn write32(&mut self, addr: u32, v: u32) { self.inner.write32(addr, v) }

    pub fn trace_start(&mut self) { self.inner.trace_start(); }
    pub fn trace_stop(&mut self) { self.inner.trace_stop(); }
    pub fn take_trace(&mut self) -> Vec<u32> { self.inner.take_trace() }
}
