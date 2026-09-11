//! RA4M1-only peripheral map (no STM32). See uno-r4 AGENTS.md §3 for bases.

pub mod systick;
pub mod nvic;
pub mod scb;
pub mod mpu;
pub mod fpu;
pub mod dwt;
pub mod itm;
pub mod stir;
pub mod ra_system;
pub mod ra_port;
pub mod ra_sci;
pub mod ra_gpt;
pub mod ra_analog;
pub mod ra_rtc;
pub mod ra_dma;
pub mod ra_misc;
pub mod ra_opamp;
pub mod ra_usb;
pub mod ra_ctsu;
pub mod ra_can;
pub mod ra_i2c;
pub mod ra_icu;

use std::cell::RefCell;
use crate::system::System;
use systick::SysTick;
use scb::Scb;
use mpu::Mpu;
use fpu::Fpu;
use dwt::{Dwt, Demcr};
use itm::Itm;
use stir::Stir;

pub trait Peripheral: std::any::Any {
    fn read(&mut self, sys: &System, offset: u32) -> u32;
    fn write(&mut self, sys: &System, offset: u32, value: u32);
    /// Precise sub-word write: aligned `offset`, merged `value`, target bytes
    /// `[byte_offset, byte_offset+size)`. Default preserves the legacy merged
    /// behavior. Byte-packed RA peripherals (SCI) override this so repeated
    /// writes of the same byte still register.
    fn write_sized(&mut self, sys: &System, offset: u32, value: u32, _byte_offset: u8, _size: u8) {
        self.write(sys, offset, value)
    }
    fn tick(&mut self, _sys: &System) {}
    fn rx_byte(&mut self, _sys: &System, _byte: u8) {}
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

pub struct PeripheralSlot<T> {
    pub start: u32,
    pub end: u32,
    pub peripheral: T,
}

pub struct Peripherals {
    pub(crate) peripherals: Vec<PeripheralSlot<RefCell<Box<dyn Peripheral>>>>,
    pub nvic: RefCell<nvic::Nvic>,
}

impl Peripherals {
    /// Device-memory query for the unaligned-Device rule (see Mpu).
    pub fn mpu_is_device(&self, addr: u32) -> bool {
        if !crate::system::is_mpu_enabled() {
            return false;
        }
        for slot in &self.peripherals {
            if slot.start == 0xE000_ED90 {
                if let Some(mpu) = slot.peripheral.borrow_mut().as_any_mut().downcast_mut::<Mpu>() {
                    return mpu.is_device(addr);
                }
                break;
            }
        }
        false
    }

    /// MPU access check for a CPU access range. Returns Some(true) on an
    /// execute violation, Some(false) on data, None when allowed.
    pub fn mpu_check(&self, addr: u32, size: u32, write: bool, exec: bool) -> Option<bool> {
        if !crate::system::is_mpu_enabled() {
            return None;
        }
        let priv_ = crate::system::current_privileged();
        let hfnmi = crate::system::current_hfnmi();
        let priv_ = priv_ && !crate::system::mpu_force_unpriv();
        for slot in &self.peripherals {
            if slot.start == 0xE000_ED90 {
                if let Some(mpu) = slot.peripheral.borrow_mut().as_any_mut().downcast_mut::<Mpu>() {
                    return mpu.check_range(addr, size, write, exec, priv_, hfnmi);
                }
                break;
            }
        }
        None
    }

    /// DWT EXCCNT tick: one exception entry. Gated on DEMCR.TRCENA.
    pub fn dwt_count_exc(&self, sys: &System) {
        if sys.p.read(sys, 0xE000EDFC, 4) & (1 << 24) == 0 {
            return;
        }
        for slot in &self.peripherals {
            if slot.start == 0xE000_1000 {
                use crate::peripherals::dwt::Dwt;
                if let Some(dwt) = slot.peripheral.borrow_mut().as_any_mut().downcast_mut::<Dwt>() {
                    dwt.count_exc();
                }
                break;
            }
        }
    }

    /// DWT FOLDCNT tick: one predicated-skipped instruction. Same TRCENA gate.
    pub fn dwt_count_fold(&self, sys: &System) {
        if sys.p.read(sys, 0xE000EDFC, 4) & (1 << 24) == 0 {
            return;
        }
        for slot in &self.peripherals {
            if slot.start == 0xE000_1000 {
                use crate::peripherals::dwt::Dwt;
                if let Some(dwt) = slot.peripheral.borrow_mut().as_any_mut().downcast_mut::<Dwt>() {
                    dwt.count_fold();
                }
                break;
            }
        }
    }

    /// FPEXC.EN shadow for the VMRS/VMSR path (see cpu/thumb.rs).
    pub fn fpu_fpexc_en(&self) -> bool {
        for slot in &self.peripherals {
            if slot.start == 0xE000_EF34 {
                if let Some(fpu) = slot.peripheral.borrow_mut().as_any_mut().downcast_mut::<Fpu>() {
                    return fpu.fpexc_en();
                }
                break;
            }
        }
        true
    }

    pub fn set_fpu_fpexc_en(&self, v: bool) {
        for slot in &self.peripherals {
            if slot.start == 0xE000_EF34 {
                if let Some(fpu) = slot.peripheral.borrow_mut().as_any_mut().downcast_mut::<Fpu>() {
                    fpu.set_fpexc_en(v);
                }
                break;
            }
        }
    }

    /// RA4M1 map: ARM core + RA peripherals only. Real bases (R7FA4M1AB.h).
    pub fn new_ra4m1() -> Self {
        let mut p = Peripherals {
            peripherals: Vec::new(),
            nvic: RefCell::new(nvic::Nvic::default()),
        };
        let mut add = |start: u32, end: u32, peri: Box<dyn Peripheral>| {
            p.peripherals.push(PeripheralSlot { start, end, peripheral: RefCell::new(peri) });
        };
        // ARM core
        if let Some(x) = nvic::NvicWrapper::new("NVIC") { add(0xE000_E100, 0xE000_E500, x); }
        if let Some(x) = SysTick::new("SysTick") { add(0xE000_E010, 0xE000_E020, x); }
        if let Some(x) = Scb::new("SCB") { add(0xE000_ED00, 0xE000_ED90, x); }
        if let Some(x) = Mpu::new("MPU") { add(0xE000_ED90, 0xE000_EDFC, x); }
        if let Some(x) = Fpu::new("FPU") { add(0xE000_EF34, 0xE000_EF4C, x); }
        if let Some(x) = Dwt::new("DWT") { add(0xE000_1000, 0xE000_1020, x); }
        if let Some(x) = Demcr::new("DEMCR") { add(0xE000_EDFC, 0xE000_EE00, x); }
        if let Some(x) = Stir::new("STIR") { add(0xE000_EF00, 0xE000_EF04, x); }
        if let Some(x) = Itm::new("ITM") { add(0xE000_0000, 0xE000_0F00, x); }
        // RA SYSTEM + MSTP
        if let Some(x) = ra_system::RaSystem::new_sysc() { add(0x4001_E000, 0x4001_F000, x); }
        if let Some(x) = ra_system::RaSystem::new_mstp() { add(0x4004_6FFC, 0x4004_8000, x); }
        // RA PORT + PFS (+PMISC tail)
        if let Some(x) = ra_port::RaPort::new_port() { add(0x4004_0000, 0x4004_0200, x); }
        if let Some(x) = ra_port::RaPort::new_pfs() { add(0x4004_0800, 0x4004_0E00, x); }
        // RA SCI0,1,2,9 (BSP_FEATURE_SCI_CHANNELS=0x207, stride 0x20)
        for hw in [0u8, 1, 2, 9] {
            if let Some(x) = ra_sci::RaSci::new(hw) {
                add(0x4007_0000 + (hw as u32) * 0x20, 0x4007_0000 + (hw as u32) * 0x20 + 0x20, x);
            }
        }
        // RA GPT0-7: 2x32-bit + 6x16-bit (stride 0x100)
        for ch in 0..8u8 {
            if let Some(x) = ra_gpt::RaGpt::new(ch) {
                let base = 0x4007_8000 + (ch as u32) * 0x100;
                add(base, base + 0x100, x);
            }
        }
        // RA ICU (IELSR event routing)
        if let Some(x) = ra_icu::RaIcu::new() { add(0x4000_6000, 0x4000_6400, x); }
        // RA ADC0/ADC1 + DAC + RTC
        if let Some(x) = ra_analog::RaAdc::new() { add(0x4005_C000, 0x4005_C200, x); }
        if let Some(x) = ra_analog::RaAdc::new() { add(0x4005_C200, 0x4005_C400, x); }
        if let Some(x) = ra_analog::RaDac::new() { add(0x4005_E000, 0x4005_E100, x); }
        if let Some(x) = ra_rtc::RaRtc::new() { add(0x4004_4000, 0x4004_4200, x); }
        // RA DMAC + DTC + ELC + AGT0-1 + WDT/IWDT + CRC + DOC
        if let Some(x) = ra_dma::RaDmac::new_dmac() { add(0x4000_5000, 0x4000_5100, x); }
        if let Some(x) = ra_dma::RaDmac::new_dtc() { add(0x4000_5400, 0x4000_5500, x); }
        if let Some(x) = ra_misc::RaElc::new() { add(0x4004_1000, 0x4004_1300, x); }
        if let Some(x) = ra_misc::RaAgt::new_ch(0) { add(0x4008_4000, 0x4008_4100, x); }
        if let Some(x) = ra_misc::RaAgt::new_ch(1) { add(0x4008_4100, 0x4008_4200, x); }
        if let Some(x) = ra_misc::RaWdt::new() { add(0x4004_4200, 0x4004_4300, x); }
        if let Some(x) = ra_misc::RaWdt::new() { add(0x4004_4400, 0x4004_4500, x); }
        if let Some(x) = ra_misc::RaCrc::new() { add(0x4007_4000, 0x4007_4100, x); }
        if let Some(x) = ra_misc::RaDoc::new() { add(0x4005_4100, 0x4005_4200, x); }
        // RA OPAMP (single block) + ACMPLP
        if let Some(x) = ra_opamp::RaOpamp::new() { add(0x4008_6000, 0x4008_6100, x); }
        if let Some(x) = ra_opamp::RaAcmplp::new() { add(0x4008_5E00, 0x4008_5F00, x); }
        // RA USBFS (retain file; endpoints later)
        if let Some(x) = ra_usb::RaUsb::new() { add(0x4009_0000, 0x4009_1000, x); }
        // RA CTSU (touch sensing: STRT->tick->counters + END event)
        if let Some(x) = ra_ctsu::RaCtsu::new() { add(0x4008_1000, 0x4008_1100, x); }
        // RA CAN0 (mailbox CAN; CAN1 has no routable mailbox events here)
        if let Some(x) = ra_can::RaCan::new_can0() { add(0x4005_0000, 0x4005_1000, x); }
        // RA IIC0/IIC1 (RIIC master + virtual EEPROM slave at 0x50)
        if let Some(x) = ra_i2c::RaIic::new_ch(0) { add(0x4005_3000, 0x4005_3100, x); }
        if let Some(x) = ra_i2c::RaIic::new_ch(1) { add(0x4005_3100, 0x4005_3200, x); }
        p.finish_registration();
        p
    }

    fn finish_registration(&mut self) {
        self.peripherals.sort_by_key(|p| p.start);
        let a = self.peripherals.iter();
        let mut b = self.peripherals.iter();
        b.next();
        for (p1, p2) in a.zip(b) {
            assert!(p1.end <= p2.start, "Overlap: 0x{:08x}-0x{:08x} vs 0x{:08x}-0x{:08x}",
                p1.start, p1.end, p2.start, p2.end);
        }
    }

    fn get_peripheral<T>(slots: &[PeripheralSlot<T>], addr: u32) -> Option<&PeripheralSlot<T>> {
        let index = slots.binary_search_by_key(&addr, |p| p.start)
            .map_or_else(|e| e.checked_sub(1), |v| Some(v));
        index.map(|i| slots.get(i).filter(|p| addr <= p.end)).flatten()
    }

    fn bitbanding(addr: u32) -> Option<(u32, u8)> {
        if (0x4200_0000..0x4400_0000).contains(&addr) {
            let bit_number = (addr % 32) / 4;
            let mapped = 0x4000_0000 + (addr - 0x4200_0000) / 32;
            Some((mapped, bit_number as u8))
        } else { None }
    }

    fn is_register(addr: u32) -> bool { !(0x6000_0000..0xA000_0000).contains(&addr) }

    fn align_addr_4(addr: u32) -> (u32, u8) {
        let byte_offset = (addr % 4) as u8;
        (addr - byte_offset as u32, byte_offset)
    }

    pub fn read(&self, sys: &System, addr: u32, size: u8) -> u32 {
        if let Some((addr, bit_number)) = Self::bitbanding(addr) {
            return (self.read(sys, addr, 1) >> bit_number) & 1;
        }
        let is_reg = Self::is_register(addr);
        let (addr, byte_offset) = if is_reg {
            Self::align_addr_4(addr)
        } else { (addr, 0) };
        let value = if Self::NVIC_REGS_BASE <= addr && addr < Self::NVIC_REGS_END {
            self.nvic.borrow_mut().read(sys, addr - Self::NVIC_REGS_BASE)
        } else if let Some(p) = Self::get_peripheral(&self.peripherals, addr) {
            p.peripheral.borrow_mut().read(sys, addr - p.start)
        } else { 0 };
        // Sub-word register access: peripheral returns the aligned 32-bit pack,
        // the target byte(s) shift down to the low bits.
        if is_reg { value >> (8 * byte_offset) } else { value }
    }

    pub fn write(&self, sys: &System, addr: u32, size: u8, mut value: u32) {
        if let Some((addr, bit_number)) = Self::bitbanding(addr) {
            let mut v = self.read(sys, addr, 1);
            v &= !(1 << bit_number);
            v |= (value & 1) << bit_number;
            return self.write(sys, addr, 1, v);
        }
        let (addr, byte_offset) = if Self::is_register(addr) {
            Self::align_addr_4(addr)
        } else { (addr, 0) };
        if byte_offset != 0 && Self::is_register(addr) {
            let v = self.read(sys, addr, 4);
            value = (value << 8 * byte_offset) | (v & (0xFFFF_FFFF >> (32 - 8 * byte_offset)));
        }
        if Self::NVIC_REGS_BASE <= addr && addr < Self::NVIC_REGS_END {
            self.nvic.borrow_mut().write(sys, addr - Self::NVIC_REGS_BASE, value);
        } else if let Some(p) = Self::get_peripheral(&self.peripherals, addr) {
            p.peripheral.borrow_mut().write_sized(sys, addr - p.start, value, byte_offset, size);
        }
    }

    pub fn rx_byte(&self, sys: &System, addr: u32, byte: u8) -> bool {
        if let Some(p) = Self::get_peripheral(&self.peripherals, addr) {
            p.peripheral.borrow_mut().rx_byte(sys, byte);
            true
        } else { false }
    }

    pub fn addr_desc(&self, addr: u32) -> String {
        format!("addr=0x{:08x}", addr)
    }
}

impl Peripherals {
    pub const NVIC_REGS_BASE: u32 = 0xE000_E100;
    pub const NVIC_REGS_END: u32 = 0xE000_E500;
}
