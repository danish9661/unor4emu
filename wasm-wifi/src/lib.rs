use wasm_bindgen::prelude::*;
use ra4m1_periph_wasm::{WasmCpu, ra4m1::FLASH_BASE};
use wifi_link::{WifiModule, NoWifi};

// Big WASM: RA4M1 + WiFi link stub. ESP32-S3 plugs in here later.
#[wasm_bindgen]
pub struct UnoR4Wifi {
    inner: WasmCpu,
    wifi: NoWifi,
}

#[wasm_bindgen]
impl UnoR4Wifi {
    #[wasm_bindgen(constructor)]
    pub fn new(sp: u32, pc: u32) -> Self {
        Self { inner: WasmCpu::new_ra4m1(sp, pc), wifi: NoWifi }
    }

    pub fn init_system() {
        ra4m1_periph_wasm::init();
    }

    pub fn load_firmware(&mut self, data: &[u8]) {
        self.inner.load_firmware(data, FLASH_BASE);
    }

    pub fn step(&mut self, budget: u32) -> u32 {
        let n = self.inner.step(budget);
        self.wifi.tick(n as u64);
        n
    }

    pub fn get_pc(&self) -> u32 { self.inner.get_pc() }
}
