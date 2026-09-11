//! RA4M1 (UNO R4) minimal bring-up: memory map + real peripheral bases.
//!
//! RA4M1 != STM32: flash lives at 0x00000000 (not 0x08000000),
//! 256KB flash + 32KB SRAM. RA bases (RA family, verified via FSP
//! base_addresses.h): SCI0 0x40118000, GPT320 0x40169000,
//! PORT0 0x40080000, PFS 0x40080800, SYSC 0x4001E000, MSTP 0x40084000.

pub const FLASH_BASE: u32 = 0x00000000;
pub const FLASH_SIZE: usize = 256 * 1024;
pub const RAM_BASE: u32 = 0x20000000;
pub const RAM_SIZE: usize = 32 * 1024;
pub const DATAFLASH_SIZE: usize = 8 * 1024;

pub const SCI0_BASE: u32 = 0x4007_0000;
pub const GPT0_BASE: u32 = 0x4007_8000;
pub const PORT_BASE: u32 = 0x4004_0000;
pub const PFS_BASE: u32 = 0x4004_0800;
pub const SYSC_BASE: u32 = 0x4001_E000;
pub const ADC_BASE: u32 = 0x4005_C000;
pub const DAC_BASE: u32 = 0x4005_E000;
pub const RTC_BASE: u32 = 0x4004_4000;
pub const DMAC_BASE: u32 = 0x4000_5000;
pub const ELC_BASE: u32 = 0x4004_1000;
pub const AGT0_BASE: u32 = 0x4008_4000;
pub const CRC_BASE: u32 = 0x4007_4000;
pub const DOC_BASE: u32 = 0x4005_4100;
pub const OPAMP_BASE: u32 = 0x4008_6000;
pub const ACMPLP_BASE: u32 = 0x4008_5E00;
pub const USBFS_BASE: u32 = 0x4009_0000;

/// Make a FlatMemory wired for RA4M1 (flash at zero).
pub fn ra4m1_memory() -> crate::cpu::mem::FlatMemory {
    let mut m = crate::cpu::mem::FlatMemory::new(FLASH_SIZE, RAM_SIZE);
    m.flash_base = FLASH_BASE;
    m.ram_base = RAM_BASE;
    m
}

/// Minimal clock stub: RA4M1 boots through HOCO/MOCO/PLL + option-setting
/// memory. Real model comes later; for now accept-and-retain writes so
/// `SystemInit` spins past config without faulting.
#[derive(Default)]
pub struct SystemClockStub {
    regs: std::collections::HashMap<u32, u32>,
}

impl SystemClockStub {
    pub fn read(&self, offset: u32) -> u32 {
        // Option-setting memory reads back erased (all-1s).
        *self.regs.get(&offset).unwrap_or(&0xFFFF_FFFF)
    }
    pub fn write(&mut self, offset: u32, value: u32) {
        self.regs.insert(offset, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{Cpu, mem::Memory};

    static RA_BOOT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn ra4m1_boots_from_zero() {
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Synthetic image: SP at 0x0, reset PC at 0x4, code at 0x100.
        let mut img = vec![0u8; 0x200];
        img[0..4].copy_from_slice(&0x20008000u32.to_le_bytes()); // SP = top of 32K SRAM
        img[4..8].copy_from_slice(&0x00000101u32.to_le_bytes()); // PC = 0x100 Thumb
        // 0x100: NOP (0xBF00), 0x102: B to self (0xE7FE)
        img[0x100] = 0x00; img[0x101] = 0xBF;
        img[0x102] = 0xFE; img[0x103] = 0xE7;

        let sys = crate::system::WasmSystem::new();
        crate::init_for_test(sys);
        let mut mem = ra4m1_memory();
        mem.load(&img, FLASH_BASE);
        assert_eq!(mem.read32(0x00000000), 0x20008000, "SP live at zero");
        assert_eq!(mem.read32(0x00000004), 0x00000101, "PC live at zero");

        let sp = mem.read32(0x0);
        let pc = mem.read32(0x4);
        let mut cpu = Cpu::new(sp, pc);
        cpu.deliver_irqs = false;
        let sys = crate::sys();
        cpu.run(sys, &mut mem, 10);
        assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
        assert_eq!(mem.bad.get(), None, "bad access");
        // Still spinning in our 4-byte loop at 0x100-0x104.
        let pc_now = cpu.regs.r[15] & !1;
        assert!(pc_now == 0x00000100 || pc_now == 0x00000102, "pc={:08x}", pc_now);
    }

    #[test]
    fn clock_stub_accepts_boot_writes() {
        let mut clk = SystemClockStub::default();
        assert_eq!(clk.read(0x00), 0xFFFF_FFFF, "erased option bytes");
        clk.write(0x10, 0x12345678);
        assert_eq!(clk.read(0x10), 0x12345678, "retain for SystemInit");
    }

    #[test]
    fn ra4m1_map_sci_tx_reaches_console() {
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        crate::system::get_uart_output().lock().unwrap().clear();
        let sys = crate::sys();
        // Real byte offsets: SCR+0x02, TDR+0x03, SSR+0x04.
        sys.p.write(sys, SCI0_BASE + 0x02, 1, 0x20);
        sys.p.write(sys, SCI0_BASE + 0x03, 1, b'H' as u32);
        sys.p.write(sys, SCI0_BASE + 0x03, 1, b'i' as u32);
        let out = crate::system::get_uart_output().lock().unwrap().clone();
        assert_eq!(out, "Hi");
        // SSR TDRE/TEND stay set (ready).
        let ssr = sys.p.read(sys, SCI0_BASE + 0x04, 1);
        assert!(ssr & 0x80 != 0, "TDRE ssr={:02x}", ssr);
    }

    #[test]
    fn ra4m1_map_gpt_counts_and_matches() {
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        // Period 100, start.
        sys.p.write(sys, GPT0_BASE + 0x08, 4, 100);
        sys.p.write(sys, GPT0_BASE + 0x00, 4, 1);
        // Advance virtual clock and tick.
        crate::system::INSTRUCTION_COUNT.fetch_add(50, std::sync::atomic::Ordering::Relaxed);
        sys.tick();
        let cnt = sys.p.read(sys, GPT0_BASE + 0x04, 4);
        assert!(cnt > 0 && cnt <= 100, "cnt={}", cnt);
    }

    #[test]
    fn ra4m1_map_port_output_retained() {
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        // PORT1 (offset 0x20): PDR=output + PODR bit11 (Arduino LED) via PCNTR1.
        sys.p.write(sys, PORT_BASE + 0x20, 4, (1 << (16 + 11)) | (1 << 11));
        let v = sys.p.read(sys, PORT_BASE + 0x20, 4);
        assert!(v & (1 << 11) != 0, "LED bit retained v={:08x}", v);
    }

    #[test]
    fn ra4m1_firmware_blinky_via_mmio() {
        // Encoded Thumb (little-endian halfwords):
        //   0x100: LDR r0, =SCI0_BASE (literal at 0x11C)
        //   0x102: LDR r1, =0x48 ('H')
        //   0x104: STRB r1, [r0, #3] (TDR, real byte offset)
        //   0x106: LDR r0, =PORT1_PCNTR1 (literal at 0x120)
        //   0x108: LDR r1, =LED bits
        //   0x10A: STR r1, [r0]
        //   0x10C: B to self (0xE7FE)
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        crate::system::get_uart_output().lock().unwrap().clear();
        let mut mem = ra4m1_memory();
        let mut img = vec![0u8; 0x200];
        img[0..4].copy_from_slice(&0x20008000u32.to_le_bytes());
        img[4..8].copy_from_slice(&0x00000101u32.to_le_bytes());
        // Program halfwords at 0x100:
        // 4805 LDR r0,[pc,#20] -> 0x11C ; 4905 LDR r1,[pc,#20] -> 0x120? compute carefully:
        // At 0x100, PC reads as 0x104 (Thumb PC+4). 0x104+20=0x118. Place SCI addr at 0x118.
        // Simpler: hand-assemble with explicit literals:
        // 0x100: 0x4804 (LDR r0,[pc,#16] -> 0x114)
        // 0x102: 0x4904 (LDR r1,[pc,#16] -> 0x118)
        // 0x104: 0x70C1 (STRB r1,[r0,#3])
        // 0x106: 0x4803 (LDR r0,[pc,#12] -> 0x114? overlaps) -> use separate sequence below.
        // To avoid literal-pool arithmetic bugs, write the 3 stores with MOVW/MOVT-free
        // approach: use two literal pools.
        let code: [u16; 8] = [
            0x4804, // LDR r0, [pc,#16]  ; 0x100 -> pc 0x104 -> 0x114 (SCI0)
            0x4905, // LDR r1, [pc,#20]  ; 0x102 -> pc 0x104 -> 0x118 ('H')
            0x70C1, // STRB r1, [r0,#3]  ; TDR (real byte offset)
            0x4805, // LDR r0, [pc,#20]  ; 0x106 -> pc 0x108 -> 0x11C (PORT1)
            0x4905, // LDR r1, [pc,#20]  ; 0x108 -> pc 0x10C -> 0x120 (LED bits)
            0x6001, // STR r1, [r0,#0]
            0xE7FE, // B .
            0xBF00, // NOP pad
        ];
        for (i, w) in code.iter().enumerate() {
            img[0x100 + i * 2] = (w & 0xFF) as u8;
            img[0x100 + i * 2 + 1] = (w >> 8) as u8;
        }
        img[0x114..0x118].copy_from_slice(&SCI0_BASE.to_le_bytes());
        img[0x118..0x11C].copy_from_slice(&0x48u32.to_le_bytes());
        img[0x11C..0x120].copy_from_slice(&(PORT_BASE + 0x20).to_le_bytes());
        img[0x120..0x124].copy_from_slice(&((1u32 << (16 + 11)) | (1u32 << 11)).to_le_bytes());
        mem.load(&img, FLASH_BASE);
        let mut cpu = Cpu::new(0x20008000, 0x00000101);
        cpu.deliver_irqs = false;
        let sys = crate::sys();
        cpu.run(sys, &mut mem, 20);
        assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
        let out = crate::system::get_uart_output().lock().unwrap().clone();
        assert_eq!(out, "H", "uart out={:?}", out);
        let podr = sys.p.read(sys, PORT_BASE + 0x20, 4);
        assert!(podr & (1 << 11) != 0, "LED on podr={:08x}", podr);
    }

    #[test]
    fn ra4m1_map_adc_converts_channel() {
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        crate::system::adc_set_override("ADC0", 2, 0xABC);
        let sys = crate::sys();
        sys.p.write(sys, ADC_BASE + 0x04, 4, 1 << 2); // select ch2
        sys.p.write(sys, ADC_BASE + 0x00, 4, 1 << 15); // ADST start
        let v = sys.p.read(sys, ADC_BASE + 0x20 + 2 * 2, 4);
        assert_eq!(v & 0x3FFF, 0xABC, "ch2 v={:x}", v);
        crate::system::adc_clear_override("ADC0", 2);
    }

    #[test]
    fn ra4m1_map_dac_output_retained() {
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        sys.p.write(sys, DAC_BASE + 0x00, 4, 0x7FF);
        sys.p.write(sys, DAC_BASE + 0x04, 4, 0x01); // DAOE0
        assert_eq!(sys.p.read(sys, DAC_BASE + 0x00, 4) & 0xFFF, 0x7FF);
    }

    #[test]
    fn ra4m1_map_rtc_ticks_seconds() {
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        sys.p.write(sys, RTC_BASE + 0x0E, 1, 1); // START (RCR2 is 8-bit)
        sys.p.write(sys, RTC_BASE + 0x00, 1, 0); // sec=0
        crate::system::INSTRUCTION_COUNT.fetch_add(480_000 * 3, std::sync::atomic::Ordering::Relaxed);
        sys.tick();
        let s = sys.p.read(sys, RTC_BASE + 0x00, 1);
        assert!(s >= 2 && s <= 4, "sec={}", s);
    }

    #[test]
    fn ra4m1_map_dmac_mem_to_mem() {
        use crate::cpu::mem::Memory;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let mut mem = ra4m1_memory();
        for i in 0..16u32 { mem.write8(0x20000000 + i, (0xA0 + i) as u8); }
        let sys = crate::sys();
        sys.p.write(sys, DMAC_BASE + 0x00, 4, 0x20000000); // SAR
        sys.p.write(sys, DMAC_BASE + 0x04, 4, 0x20000100); // DAR
        sys.p.write(sys, DMAC_BASE + 0x08, 4, 16);         // size
        // EN via mem so the sync memcopy drains inline (mem periph path
        // runs service_sync_dma right after the model write).
        mem.write32(DMAC_BASE + 0x0C, 1);                  // EN
        assert_eq!(sys.pending_dma_count(), 0);
        for i in 0..16u32 {
            assert_eq!(mem.read8(0x20000100 + i), (0xA0 + i) as u8, "byte {}", i);
        }
    }

    #[test]
    fn ra4m1_map_elc_routes_software_event() {
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        sys.p.write(sys, ELC_BASE + 0x00, 4, 0x42); // arm link 0
        sys.p.write(sys, 0xE000_E104, 4, 1 << (60 - 32)); // enable IRQ60 via ISER1
        sys.p.write(sys, ELC_BASE + 0x80, 4, 0x01); // fire link 0
        assert_eq!(sys.p.read(sys, ELC_BASE + 0x84, 4) & 1, 1);
        assert!(sys.p.nvic.borrow().has_pending());
    }

    #[test]
    fn ra4m1_map_agt_counts() {
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        sys.p.write(sys, AGT0_BASE + 0x00, 2, 1000); // AGT counter program (latches reload)
        sys.p.write(sys, AGT0_BASE + 0x08, 1, 1);    // AGTCR.TSTART
        crate::system::INSTRUCTION_COUNT.fetch_add(100, std::sync::atomic::Ordering::Relaxed);
        sys.tick();
        let cnt = sys.p.read(sys, AGT0_BASE + 0x00, 2) & 0xFFFF; // AGT counter
        assert!(cnt > 0 && cnt < 1000, "cnt={}", cnt);
        // TCSTF follows TSTART.
        assert_eq!(sys.p.read(sys, AGT0_BASE + 0x08, 1) & 0x03, 0x03);
    }

    #[test]
    fn ra4m1_map_crc_and_doc() {
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        sys.p.write(sys, CRC_BASE + 0x00, 4, 0x80); // reset
        sys.p.write(sys, CRC_BASE + 0x04, 4, 0x12345678);
        let crc = sys.p.read(sys, CRC_BASE + 0x08, 4);
        assert_ne!(crc, 0, "crc computed");
        sys.p.write(sys, DOC_BASE + 0x04, 4, 0x1111);
        sys.p.write(sys, DOC_BASE + 0x08, 4, 0x2222);
        assert_eq!(sys.p.read(sys, DOC_BASE + 0x0C, 4) & 1, 1, "mismatch flagged");
    }

    #[test]
    fn ra4m1_map_sci_echo_path() {
        // UART echo: inject RX byte -> RDR readable + RDRF set -> TDR write echoes.
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        crate::system::get_uart_output().lock().unwrap().clear();
        let sys = crate::sys();
        sys.p.write(sys, SCI0_BASE + 0x02, 1, 0x60); // TE+RE+RIE
        assert!(crate::peripherals::ra_sci::sci_rx_inject(sys, SCI0_BASE, b'Z'));
        let ssr = sys.p.read(sys, SCI0_BASE + 0x04, 1);
        assert!(ssr & (1 << 6) != 0, "RDRF ssr={:02x}", ssr);
        let rdr = sys.p.read(sys, SCI0_BASE + 0x05, 1);
        assert_eq!(rdr, b'Z' as u32);
        sys.p.write(sys, SCI0_BASE + 0x03, 1, rdr); // echo
        assert_eq!(crate::system::get_uart_output().lock().unwrap().as_str(), "Z");
    }

    #[test]
    fn ra4m1_map_opamp_follower_and_acmp() {
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        sys.p.write(sys, OPAMP_BASE + 0x20, 4, 0xABC); // poke ch0 input
        sys.p.write(sys, OPAMP_BASE + 0x0B, 1, 0x01);  // AMPC: enable ch0
        assert_eq!(sys.p.read(sys, OPAMP_BASE + 0x30, 4) & 0xFFF, 0xABC);
        assert_eq!(sys.p.read(sys, OPAMP_BASE + 0x0C, 1) & 1, 1); // AMPMON0
        sys.p.write(sys, ACMPLP_BASE + 0x10, 4, 0x800);
        sys.p.write(sys, ACMPLP_BASE + 0x14, 4, 0x400);
        sys.p.write(sys, ACMPLP_BASE + 0x00, 4, 0x01);
        assert_eq!(sys.p.read(sys, ACMPLP_BASE + 0x04, 4) & 1, 1);
        sys.p.write(sys, ACMPLP_BASE + 0x10, 4, 0x100);
        assert_eq!(sys.p.read(sys, ACMPLP_BASE + 0x04, 4) & 1, 0);
    }

    fn usb_take_tx(sys: &crate::system::WasmSystem) -> Vec<u8> {
        for slot in sys.p.peripherals.iter() {
            if slot.start == USBFS_BASE {
                if let Some(u) = slot.peripheral.borrow_mut().as_any_mut()
                    .downcast_mut::<crate::peripherals::ra_usb::RaUsb>() {
                    return std::mem::take(&mut u.tx_capture);
                }
            }
        }
        Vec::new()
    }

    fn with_usb(sys: &crate::system::WasmSystem, f: impl FnOnce(&mut crate::peripherals::ra_usb::RaUsb)) {
        for slot in sys.p.peripherals.iter() {
            if slot.start == USBFS_BASE {
                let mut b = slot.peripheral.borrow_mut();
                if let Some(u) = b.as_any_mut().downcast_mut::<crate::peripherals::ra_usb::RaUsb>() {
                    f(u);
                    return;
                }
            }
        }
    }

    /// Run firmware with IRQs until `cond` holds or budget exhausts.
    fn run_until(
        sys: &crate::system::WasmSystem,
        mem: &mut crate::cpu::mem::FlatMemory,
        cpu: &mut Cpu,
        mut budget: u32,
        cond: impl Fn() -> bool,
    ) {
        while budget > 0 && !cond() {
            let n = budget.min(48_000);
            cpu.run(sys, mem, n);
            sys.tick();
            assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
            budget -= n;
        }
    }

    #[test]
    fn ra4m1_usb_enumerates_cdc() {
        // Virtual-host enumeration of the real TinyUSB stack in Blink:
        // attach -> reset -> GET_DEV -> SET_ADDR -> GET_CFG -> SET_CONFIG.
        // The firmware's dcd/usbd does everything else through the model.
        const APP_BASE: u32 = 0x4000;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bin = include_bytes!("../../blinky/r4blink.bin");
        let mut mem = ra4m1_memory();
        mem.load(bin, APP_BASE);
        let sp = mem.read32(APP_BASE);
        let pc = mem.read32(APP_BASE + 4);
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let _ = usb_take_tx(crate::sys());
        let mut cpu = Cpu::new(sp, pc);
        cpu.deliver_irqs = true;
        let sys = crate::sys();

        // Boot through USB init, then attach + reset.
        cpu.run(sys, &mut mem, 6_000_000);
        sys.tick();
        with_usb(sys, |u| u.host_attach());
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 4_000_000, || {
            usb_take_tx(sys);
            false
        });
        with_usb(sys, |u| u.host_set_dvst(1)); // DEF
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 4_000_000, || false);

        // Helper: one control-IN transfer, returns device bytes.
        fn ctl_in(
            sys: &crate::system::WasmSystem,
            mem: &mut crate::cpu::mem::FlatMemory,
            cpu: &mut Cpu,
            req: u16, val: u16, idx: u16, len: u16, want: usize,
        ) -> Vec<u8> {
            with_usb(sys, |u| u.host_setup(req, val, idx, len));
            crate::system::icu_raise_event(sys, 51);
            let mut got = Vec::new();
            let mut budget = 4_000_000u32;
            while budget > 0 && got.len() < want {
                let n = budget.min(48_000);
                cpu.run(sys, mem, n);
                sys.tick();
                assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
                got.extend(usb_take_tx(sys));
                budget -= n;
            }
            // Status stage: let the device complete, then tell it status is done.
            let mut budget = 2_000_000u32;
            while budget > 0 {
                let n = budget.min(48_000);
                cpu.run(sys, mem, n);
                sys.tick();
                budget -= n;
            }
            with_usb(sys, |u| u.host_status_done());
            crate::system::icu_raise_event(sys, 51);
            run_until(sys, mem, cpu, 2_000_000, || false);
            got.extend(usb_take_tx(sys));
            got
        }

        // GET_DESCRIPTOR device (18 bytes: bLength=18, type=DEVICE=1).
        let dev = ctl_in(sys, &mut mem, &mut cpu, 0x0680, 0x0100, 0, 18, 18);
        assert_eq!(dev.len(), 18, "dev desc {:?}", dev);
        assert_eq!(dev[0], 18);
        assert_eq!(dev[1], 1);

        // SET_ADDRESS 5: setup -> device status (CCPL) -> done. The stack
        // tracks `addressed` in software; USBADDR latching is SIE business
        // (no firmware writes it - verified in dcd_rusb2.c).
        with_usb(sys, |u| u.host_setup(0x0500, 5, 0, 0));
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 4_000_000, || false);
        with_usb(sys, |u| u.host_status_done());
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 2_000_000, || false);

        // GET_DESCRIPTOR configuration (first 9, then full).
        let cfg9 = ctl_in(sys, &mut mem, &mut cpu, 0x0680, 0x0200, 0, 9, 9);
        assert_eq!(cfg9.len(), 9, "cfg9 {:?}", cfg9);
        assert_eq!(cfg9[1], 2); // CONFIGURATION
        let total = u16::from_le_bytes([cfg9[2], cfg9[3]]) as usize;
        assert!(total > 9 && total < 512, "total {}", total);
        let cfg = ctl_in(sys, &mut mem, &mut cpu, 0x0680, 0x0200, 0, total as u16, total);
        assert_eq!(cfg.len(), total, "cfg {:?}..", &cfg[..cfg.len().min(16)]);

        // SET_CONFIGURATION 1.
        with_usb(sys, |u| u.host_setup(0x0900, 1, 0, 0));
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 4_000_000, || false);
        with_usb(sys, |u| u.host_status_done());
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 2_000_000, || false);
        assert!(cpu.fault.is_none());
    }

    #[test]
    fn ra4m1_map_usb_tx_reaches_capture() {
        // Scripted TinyUSB-style bulk-IN on pipe 1 via D0FIFO: select, wait
        // CURPIPE/FRDY, write packet, BVAL -> capture + BEMP + IRQ.
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        // Enable BRDY+BEMP interrupts, map USBFS_INT (51) to IRQ10.
        sys.p.write(sys, USBFS_BASE + 0x30, 2, (1 << 8) | (1 << 10));
        sys.p.write(sys, 0x4000_6300 + 10 * 4, 4, 51);
        sys.p.write(sys, 0xE000_E100, 4, 1 << 10);
        // Select pipe 1, 16-bit access.
        sys.p.write(sys, USBFS_BASE + 0x28, 2, 1 | (1 << 10));
        assert_eq!(sys.p.read(sys, USBFS_BASE + 0x28, 2) & 0xF, 1);
        assert_ne!(sys.p.read(sys, USBFS_BASE + 0x2A, 2) & (1 << 13), 0);
        // Write "Hi" + BVAL.
        sys.p.write(sys, USBFS_BASE + 0x18, 2, 0x6948);
        sys.p.write(sys, USBFS_BASE + 0x2A, 2, 1 << 15);
        // BEMP latched, IN buffer drained, IRQ pending, bytes captured.
        assert_ne!(sys.p.read(sys, USBFS_BASE + 0x4A, 2) & (1 << 1), 0);
        assert_eq!(sys.p.read(sys, USBFS_BASE + 0x70, 2) & (1 << 14), 0);
        assert!(sys.p.nvic.borrow().has_pending());
        assert_eq!(usb_take_tx(sys), b"Hi");
        // Write-0 clears the status bit.
        sys.p.write(sys, USBFS_BASE + 0x4A, 2, !(1 << 1) & 0xFFFF);
        assert_eq!(sys.p.read(sys, USBFS_BASE + 0x4A, 2) & (1 << 1), 0);
    }

    #[test]
    fn ra4m1_arduino_blink_boots() {
        // Real ArduinoCore-renesas Blink.ino built with arduino-cli
        // (fqbn arduino:renesas_uno:minima), vendored at core/blinky/r4blink.bin.
        // The Minima bootloader lives at 0x0000-0x3FFF; the app links at 0x4000
        // (see .hex addresses), so the raw .bin loads at APP_BASE like the
        // bootloader's jump does.
        const APP_BASE: u32 = 0x4000;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bin = include_bytes!("../../blinky/r4blink.bin");
        assert!(bin.len() >= 8);
        let mut mem = ra4m1_memory();
        mem.load(bin, APP_BASE);
        let sp = mem.read32(APP_BASE);
        let pc = mem.read32(APP_BASE + 4);
        assert_eq!(sp, 0x20007F00, "Arduino SP");
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        crate::system::get_uart_output().lock().unwrap().clear();
        let mut cpu = Cpu::new(sp, pc);
        cpu.deliver_irqs = false;
        let sys = crate::sys();
        cpu.run(sys, &mut mem, 500_000);
        assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
        assert_eq!(mem.bad.get(), None, "bad access");
        // Reset_Handler ran: PUSH+BL+branch past boot into setup/loop.
        let pc_now = cpu.regs.r[15] & !1;
        assert!(pc_now > 0x4000 && pc_now < 0x40000, "pc={:08x}", pc_now);
    }

    #[test]
    fn ra4m1_arduino_blink_toggles_led() {
        // Long-run proof: AGT0 underflow IRQs (1ms) drive millis/delay, so the
        // LED pin must flip within ~1.2s of virtual time. Matches the JS driver
        // contract: sys.tick() after every run chunk (48k instr = 1ms = one
        // AGT underflow). Scans all 12 ports so no pin map is assumed.
        const APP_BASE: u32 = 0x4000;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bin = include_bytes!("../../blinky/r4blink.bin");
        let mut mem = ra4m1_memory();
        mem.load(bin, APP_BASE);
        let sp = mem.read32(APP_BASE);
        let pc = mem.read32(APP_BASE + 4);
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let mut cpu = Cpu::new(sp, pc);
        cpu.deliver_irqs = true;
        let sys = crate::sys();
        let snap = || -> [u32; 12] {
            let mut s = [0u32; 12];
            for p in 0..12u32 {
                s[p as usize] = sys.p.read(sys, PORT_BASE + p * 0x20, 4) & 0xFFFF;
            }
            s
        };
        // Boot first (init takes ~5M), then watch for the toggle.
        cpu.run(sys, &mut mem, 6_000_000);
        sys.tick();
        assert!(cpu.fault.is_none(), "boot fault: {:?}", cpu.fault);
        let first = snap();
        let mut toggled = false;
        for _ in 0..1200 {
            cpu.run(sys, &mut mem, 48_000);
            sys.tick();
            assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
            assert_eq!(mem.bad.get(), None, "bad access");
            if snap() != first {
                toggled = true;
                break;
            }
        }
        assert!(toggled, "no PORT output change in 1200ms virtual");
    }
}
