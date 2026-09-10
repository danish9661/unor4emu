//! WiFi-link trait: Minima uses NoWifi (zero cost),
//! Uno R4 WiFi plugs a real ESP32-S3 impl later without touching RA4M1.

pub trait WifiModule {
    fn tick(&mut self, _cycles: u64) {}
    fn push_uart(&mut self, _b: u8) {}
    fn pop_uart(&mut self) -> Option<u8> { None }
}

/// Minima build: no radio, no cost.
pub struct NoWifi;
impl WifiModule for NoWifi {}
