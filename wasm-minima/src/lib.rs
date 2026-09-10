use wasm_bindgen::prelude::*;
use ra4m1_core::{WasmCpu, ra4m1::FLASH_BASE};

// Small WASM: RA4M1 Minima only. No STM32 and no ESP32 code linked.
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

    pub fn init_system() {
        ra4m1_core::init_ra4m1();
    }

    pub fn load_firmware(&mut self, data: &[u8]) {
        self.inner.load_firmware(data, FLASH_BASE);
    }

    pub fn reset_cpu(&mut self, sp: u32, pc: u32) {
        self.inner.reset_cpu(sp, pc);
    }

    pub fn step(&mut self, budget: u32) -> u32 {
        self.inner.step(budget)
    }

    pub fn get_pc(&self) -> u32 { self.inner.get_pc() }
    pub fn fault_pc(&self) -> u32 { self.inner.fault_pc() }
}
