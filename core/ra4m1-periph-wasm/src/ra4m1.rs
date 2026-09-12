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
pub const CTSU_BASE: u32 = 0x4008_1000;
pub const CAN0_BASE: u32 = 0x4005_0000;
pub const IIC0_BASE: u32 = 0x4005_3000;
pub const SPI0_BASE: u32 = 0x4007_2000;
pub const SPI1_BASE: u32 = 0x4007_2100;
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
        sys.p.write(sys, RTC_BASE + 0x24, 1, 1); // START (RCR2 b0)
        sys.p.write(sys, RTC_BASE + 0x02, 1, 0); // sec=0
        crate::system::INSTRUCTION_COUNT.fetch_add(480_000 * 3, std::sync::atomic::Ordering::Relaxed);
        sys.tick();
        let s = sys.p.read(sys, RTC_BASE + 0x02, 1);
        assert!(s >= 2 && s <= 4, "sec={}", s);
    }

    #[test]
    fn ra4m1_map_rtc_alarm() {
        // RTC alarm: second-alarm at :00 with RCR1.AIE raises ELC event
        // 38, routed here to IRQ5. Time starts at :58; three virtual
        // seconds cross the match.
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        sys.p.write(sys, 0x4000_6300 + 5 * 4, 4, 38); // IELSR5 = RTC_ALARM
        sys.p.write(sys, 0xE000_E100, 4, 1 << 5);     // ISER0: IRQ5
        sys.p.write(sys, RTC_BASE + 0x24, 1, 1);      // START (RCR2 b0)
        sys.p.write(sys, RTC_BASE + 0x02, 1, 0x58);   // sec=58 BCD
        sys.p.write(sys, RTC_BASE + 0x10, 1, 0x80);   // RSECAR: ENB + :00
        sys.p.write(sys, RTC_BASE + 0x22, 1, 0x01);   // RCR1.AIE
        // Step second by second: the alarm is evaluated against the
        // current second on each tick, so a multi-second jump would
        // skip the :00 match the way HW would miss a late check.
        for _ in 0..4 {
            crate::system::INSTRUCTION_COUNT.fetch_add(480_000, std::sync::atomic::Ordering::Relaxed);
            sys.tick();
        }
        assert!(sys.p.nvic.borrow().has_pending(), "ALARM event pending");
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

    #[test]
    fn ra4m1_opamp_firmware() {
        // End-to-end OPAMP on the real Arduino driver: the sketch calls
        // OPAMP.begin() (ch0, high-speed); the FSP read-modify-write of
        // AMPC must stick and AMPMON0 must report the channel running.
        // No USB involved; the verdict is the AMPMON register.
        const APP_BASE: u32 = 0x4000;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bin = include_bytes!("../../blinky/r4opamp.bin");
        let mut mem = ra4m1_memory();
        mem.load(bin, APP_BASE);
        let sp = mem.read32(APP_BASE);
        let pc = mem.read32(APP_BASE + 4);
        assert_eq!(sp, 0x20007F00, "Arduino SP");
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let mut cpu = Cpu::new(sp, pc);
        cpu.deliver_irqs = true;
        let sys = crate::sys();
        cpu.run(sys, &mut mem, 6_000_000);
        sys.tick();
        assert!(cpu.fault.is_none(), "boot fault: {:?}", cpu.fault);
        let mut ok = false;
        for _ in 0..400 {
            cpu.run(sys, &mut mem, 48_000);
            sys.tick();
            assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
            if sys.p.read(sys, OPAMP_BASE + 0x0C, 1) & 1 == 1 {
                ok = true;
                break;
            }
        }
        assert!(ok, "AMPMON0 never set by OPAMP.begin()");
    }

    #[test]
    fn ra4m1_map_ctsu_measures() {
        // FSP-like self-capacitance scan: PON, channel, pin enable,
        // STRT -> tick fills the sensor/reference counters and raises
        // CTSU_END (event 68, routed to IRQ11 here).
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        sys.p.write(sys, 0x4000_6300 + 11 * 4, 4, 68); // IELSR11 = CTSU_END
        sys.p.write(sys, 0xE000_E100, 4, 1 << 11);     // ISER0: IRQ11
        sys.p.write(sys, CTSU_BASE + 0x01, 1, 0x01);   // CTSUCR1.PON
        sys.p.write(sys, CTSU_BASE + 0x04, 1, 5);      // CTSUMCH0 = ch5
        sys.p.write(sys, CTSU_BASE + 0x06, 1, 1 << 5); // CHAC0: TS5 on
        sys.p.write(sys, CTSU_BASE + 0x14, 2, 0x000F); // SO0 count
        sys.p.write(sys, CTSU_BASE + 0x00, 1, 0x01);   // STRT
        sys.tick();
        assert_eq!(sys.p.read(sys, CTSU_BASE + 0x18, 2) & 0xFFFF, 0x0800 + 5 * 0x41, "SC");
        assert_eq!(sys.p.read(sys, CTSU_BASE + 0x1A, 2) & 0xFFFF, 0x3C00, "RC");
        assert_eq!(sys.p.read(sys, CTSU_BASE + 0x11, 1) & 0x60, 0, "no overflow");
        assert_eq!(sys.p.read(sys, CTSU_BASE + 0x00, 1) & 1, 1, "STRT retained");
        assert!(sys.p.nvic.borrow().has_pending(), "END event pending");
        // Oversize override clamps and sets SOVF (clear-by-0).
        crate::system::ctsu_set_override(5, 0x12345);
        sys.p.write(sys, CTSU_BASE + 0x00, 1, 0x00); // clear STRT
        sys.p.write(sys, CTSU_BASE + 0x00, 1, 0x01); // re-trigger
        sys.tick();
        assert_eq!(sys.p.read(sys, CTSU_BASE + 0x18, 2) & 0xFFFF, 0xFFFF, "clamped");
        assert_ne!(sys.p.read(sys, CTSU_BASE + 0x11, 1) & (1 << 5), 0, "SOVF");
        sys.p.write(sys, CTSU_BASE + 0x11, 1, !(1 << 5) & 0xFF);
        assert_eq!(sys.p.read(sys, CTSU_BASE + 0x11, 1) & 0x60, 0, "SOVF cleared");
        crate::system::ctsu_clear_override(5);
    }

    #[test]
    fn ra4m1_map_can_loopback() {
        // CAN0 self-test (internal loopback): reset -> halt -> operation
        // with STR tracking each mode, TX MB0 (SID 0x123, 8 bytes) into
        // RX MB8, MIER-gated mailbox events. Tick completes the frame.
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        sys.p.write(sys, 0x4000_6300 + 12 * 4, 4, 77); // IELSR12 = MBOX_RX
        sys.p.write(sys, 0xE000_E100, 4, 1 << 12);     // ISER0: IRQ12
        sys.p.write(sys, CAN0_BASE + 0x840, 2, 0x0100); // CANM = reset
        assert_ne!(sys.p.read(sys, CAN0_BASE + 0x842, 2) & (1 << 8), 0, "RSTST");
        sys.p.write(sys, CAN0_BASE + 0x844, 4, 0x0018_0009); // BCR retain
        sys.p.write(sys, CAN0_BASE + 0x840, 2, 0x0200); // CANM = halt
        assert_ne!(sys.p.read(sys, CAN0_BASE + 0x842, 2) & (1 << 9), 0, "HLTST");
        sys.p.write(sys, CAN0_BASE + 0x858, 1, 0x07); // TCR: TSTE + ST1 loopback
        sys.p.write(sys, CAN0_BASE + 0x840, 2, 0x0000); // CANM = operation
        assert_eq!(sys.p.read(sys, CAN0_BASE + 0x842, 2) & 0x0300, 0, "op mode");
        sys.p.write(sys, CAN0_BASE + 0x400, 4, 0); // MKR0: accept all
        sys.p.write(sys, CAN0_BASE + 0x428, 4, 0); // MKIVLR: masks valid
        sys.p.write(sys, CAN0_BASE + 0x200, 4, 0x123 << 18); // MB0 SID
        sys.p.write(sys, CAN0_BASE + 0x204, 2, 8); // DLC
        for i in 0..8u32 {
            sys.p.write(sys, CAN0_BASE + 0x206 + i, 1, 0xA0 + i);
        }
        sys.p.write(sys, CAN0_BASE + 0x820 + 0, 1, 0x80); // MB0 TRMREQ
        sys.p.write(sys, CAN0_BASE + 0x820 + 8, 1, 0x40); // MB8 RECREQ
        sys.p.write(sys, CAN0_BASE + 0x42C, 4, (1 << 0) | (1 << 8)); // MIER
        sys.tick();
        assert_eq!(sys.p.read(sys, CAN0_BASE + 0x820, 1) & 0x81, 0x01, "SENTDATA, TRMREQ clear");
        assert_ne!(sys.p.read(sys, CAN0_BASE + 0x842, 2) & (1 << 1), 0, "SDST");
        assert_ne!(sys.p.read(sys, CAN0_BASE + 0x828, 1) & 1, 0, "MB8 NEWDATA");
        assert_eq!(sys.p.read(sys, CAN0_BASE + 0x280, 4), 0x123 << 18, "MB8 ID");
        assert_eq!(sys.p.read(sys, CAN0_BASE + 0x284, 2) & 0xF, 8, "MB8 DLC");
        for i in 0..8u32 {
            assert_eq!(sys.p.read(sys, CAN0_BASE + 0x286 + i, 1) & 0xFF, 0xA0 + i, "MB8 D{}", i);
        }
        assert!(sys.p.nvic.borrow().has_pending(), "MBOX_RX pending");
        // Reading out clears NEWDATA (write-0) and drops NDST.
        sys.p.write(sys, CAN0_BASE + 0x828, 1, 0x40); // RECREQ kept, flags cleared
        assert_eq!(sys.p.read(sys, CAN0_BASE + 0x828, 1) & 1, 0, "NEWDATA clear");
        assert_eq!(sys.p.read(sys, CAN0_BASE + 0x842, 2) & 1, 0, "NDST clear");
    }

    #[test]
    fn ra4m1_map_i2c_eeprom() {
        // IIC0 master against the virtual EEPROM slave at 0x50, FSP
        // blocking-master shaped: START + SLA+W, pointer + two bytes,
        // STOP; then random-read back (START, SLA+W, pointer, RESTART,
        // SLA+R, two bytes, NACK + STOP). Ticks ship one byte each.
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        sys.p.write(sys, 0x4000_6300 + 13 * 4, 4, 54); // IELSR13 = TXI
        sys.p.write(sys, 0xE000_E100, 4, 1 << 13);     // ISER0: IRQ13
        sys.p.write(sys, IIC0_BASE + 0x00, 1, 0x80); // ICE
        sys.p.write(sys, IIC0_BASE + 0x02, 1, 0x30); // ICMR1 CKS
        sys.p.write(sys, IIC0_BASE + 0x07, 1, 0xF8); // ICIER: TIE+TEIE+RIE+NAKIE+SPIE
        sys.p.write(sys, IIC0_BASE + 0x01, 1, 0x62); // MST|TRS|ST
        sys.tick();
        assert_ne!(sys.p.read(sys, IIC0_BASE + 0x01, 1) & (1 << 7), 0, "BBSY");
        let wr = |sys: &crate::system::WasmSystem, b: u32| {
            sys.p.write(sys, IIC0_BASE + 0x12, 1, b);
            sys.tick();
        };
        wr(sys, 0xA0); // SLA+W to 0x50
        assert_eq!(sys.p.read(sys, IIC0_BASE + 0x09, 1) & 0x90, 0x80, "TDRE, no NACK");
        wr(sys, 0x10); // mem pointer
        wr(sys, 0x55);
        wr(sys, 0x66);
        assert!(sys.p.nvic.borrow().has_pending(), "TXI pending");
        sys.p.write(sys, IIC0_BASE + 0x01, 1, 0x68); // MST|TRS|SP
        sys.tick();
        assert_ne!(sys.p.read(sys, IIC0_BASE + 0x09, 1) & (1 << 3), 0, "STOP");
        assert_ne!(sys.p.read(sys, IIC0_BASE + 0x09, 1) & (1 << 6), 0, "TEND");
        assert_eq!(sys.p.read(sys, IIC0_BASE + 0x01, 1) & (1 << 7), 0, "bus free");
        // Random read back.
        sys.p.write(sys, IIC0_BASE + 0x01, 1, 0x62); // ST
        sys.tick();
        wr(sys, 0xA0);
        wr(sys, 0x10);
        sys.p.write(sys, IIC0_BASE + 0x01, 1, 0x64); // RS
        sys.tick();
        wr(sys, 0xA1); // SLA+R
        assert_ne!(sys.p.read(sys, IIC0_BASE + 0x09, 1) & (1 << 5), 0, "RDRF");
        let _ = sys.p.read(sys, IIC0_BASE + 0x13, 1); // dummy slot (FSP RXI discards the stale flag-read the same way)
        sys.tick(); // first byte streams in now
        assert_eq!(sys.p.read(sys, IIC0_BASE + 0x13, 1) & 0xFF, 0x55, "byte0");
        sys.tick();
        assert_eq!(sys.p.read(sys, IIC0_BASE + 0x13, 1) & 0xFF, 0x66, "byte1");
        sys.p.write(sys, IIC0_BASE + 0x04, 1, (1 << 3) | (1 << 4)); // ACKBT+NACK
        sys.p.write(sys, IIC0_BASE + 0x01, 1, 0x68); // SP
        sys.tick();
        assert_ne!(sys.p.read(sys, IIC0_BASE + 0x09, 1) & (1 << 3), 0, "STOP2");
        // Wrong address NACKs and clears by 0.
        sys.p.write(sys, IIC0_BASE + 0x01, 1, 0x62); // ST
        sys.tick();
        wr(sys, 0xC0); // SLA+W to 0x60: nobody home
        assert_ne!(sys.p.read(sys, IIC0_BASE + 0x09, 1) & (1 << 4), 0, "NACKF");
        sys.p.write(sys, IIC0_BASE + 0x09, 1, !(1 << 4) & 0xFF);
        assert_eq!(sys.p.read(sys, IIC0_BASE + 0x09, 1) & (1 << 4), 0, "NACKF clear");
        sys.p.write(sys, IIC0_BASE + 0x01, 1, 0x68); // SP
        sys.tick();
    }

    #[test]
    fn ra4m1_map_sci_spi_loopback() {
        // SCI1 in simple-SPI master mode (SMR.CM + SPMR.SSE/MSS, the
        // Arduino r_sci_spi shape): TDR shifts out while MISO samples
        // in the same clocks. Loopback jig echoes; open bus reads 0xFF;
        // overrun sticks ORER and keeps the old byte.
        const SCI1: u32 = SCI0_BASE + 0x20;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        crate::system::sci_set_spi_loopback(SCI1, true);
        sys.p.write(sys, SCI1 + 0x00, 1, 0x80); // SMR.CM = sync/SPI
        sys.p.write(sys, SCI1 + 0x0D, 1, 0x05); // SPMR: SSE + MSS master
        sys.p.write(sys, SCI1 + 0x02, 1, 0x30); // SCR: TE + RE
        sys.p.write(sys, SCI1 + 0x03, 1, 0xA5);
        assert_eq!(sys.p.read(sys, SCI1 + 0x05, 1) & 0xFF, 0xA5, "echo");
        assert_ne!(sys.p.read(sys, SCI1 + 0x04, 1) & (1 << 6), 0, "RDRF");
        assert_ne!(sys.p.read(sys, SCI1 + 0x04, 1) & (1 << 7), 0, "TDRE");
        // Second byte without reading overruns: ORER set, old kept.
        sys.p.write(sys, SCI1 + 0x03, 1, 0x5A);
        assert_ne!(sys.p.read(sys, SCI1 + 0x04, 1) & (1 << 5), 0, "ORER");
        assert_eq!(sys.p.read(sys, SCI1 + 0x05, 1) & 0xFF, 0xA5, "old kept");
        // Clear flags (write-0), take the new byte.
        sys.p.write(sys, SCI1 + 0x04, 1, 0x9F);
        assert_eq!(sys.p.read(sys, SCI1 + 0x04, 1) & 0x60, 0, "flags clear");
        sys.p.write(sys, SCI1 + 0x03, 1, 0x5A);
        assert_eq!(sys.p.read(sys, SCI1 + 0x05, 1) & 0xFF, 0x5A, "echo2");
        // Open bus pulls MISO up.
        crate::system::sci_set_spi_loopback(SCI1, false);
        sys.p.write(sys, SCI1 + 0x04, 1, 0x9F);
        sys.p.write(sys, SCI1 + 0x03, 1, 0x00);
        assert_eq!(sys.p.read(sys, SCI1 + 0x05, 1) & 0xFF, 0xFF, "pull-up");
    }

    #[test]
    fn ra4m1_map_spi_loopback() {
        // RSPI0 master (what Arduino SPI drives, polled): SPDR write
        // shifts out while MISO samples in. Loopback jig echoes; open
        // bus reads 0xFF; SPRF/RDRF-style flow via SPSR; overrun sticks.
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        crate::system::sci_set_spi_loopback(SPI0_BASE, true);
        sys.p.write(sys, SPI0_BASE + 0x00, 1, 0x48); // SPCR: MSTR + SPE
        sys.p.write(sys, SPI0_BASE + 0x04, 1, 0xA5);
        assert_ne!(sys.p.read(sys, SPI0_BASE + 0x03, 1) & (1 << 7), 0, "SPRF");
        assert_ne!(sys.p.read(sys, SPI0_BASE + 0x03, 1) & (1 << 5), 0, "SPTEF");
        assert_eq!(sys.p.read(sys, SPI0_BASE + 0x04, 1) & 0xFF, 0xA5, "echo");
        // SPRF cleared by the read; second write without read overruns.
        sys.p.write(sys, SPI0_BASE + 0x04, 1, 0x11);
        sys.p.write(sys, SPI0_BASE + 0x04, 1, 0x22);
        assert_ne!(sys.p.read(sys, SPI0_BASE + 0x03, 1) & 1, 0, "OVRF");
        assert_eq!(sys.p.read(sys, SPI0_BASE + 0x04, 1) & 0xFF, 0x11, "old kept");
        assert_eq!(sys.p.read(sys, SPI0_BASE + 0x03, 1) & 1, 0, "OVRF clear");
        sys.p.write(sys, SPI0_BASE + 0x04, 1, 0x22);
        assert_eq!(sys.p.read(sys, SPI0_BASE + 0x04, 1) & 0xFF, 0x22, "echo2");
        // Open bus pulls MISO up.
        crate::system::sci_set_spi_loopback(SPI0_BASE, false);
        sys.p.write(sys, SPI0_BASE + 0x04, 1, 0x00);
        assert_eq!(sys.p.read(sys, SPI0_BASE + 0x04, 1) & 0xFF, 0xFF, "pull-up");
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

    /// Run until the USB link is quiescent (no pending IRQ, no latched
    /// CTRT): real hosts never pipeline a new SETUP over an unfinished
    /// transfer, and neither must this driver.
    fn usb_quiesce(
        sys: &crate::system::WasmSystem,
        mem: &mut crate::cpu::mem::FlatMemory,
        cpu: &mut Cpu,
    ) {
        let mut budget = 4_000_000u32;
        while budget > 0 {
            let busy = sys.p.nvic.borrow().has_pending()
                || sys.p.read(sys, USBFS_BASE + 0x40, 2) & (1 << 11) != 0;
            if !busy {
                break;
            }
            let n = budget.min(48_000);
            cpu.run(sys, mem, n);
            sys.tick();
            assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
            budget -= n;
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
            usb_quiesce(sys, &mut *mem, &mut *cpu);
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
        run_until(sys, &mut mem, &mut cpu, 6_000_000, || false);

        // GET_DESCRIPTOR configuration (first 9, then full).
        let cfg9 = ctl_in(sys, &mut mem, &mut cpu, 0x0680, 0x0200, 0, 9, 9);
        assert_eq!(cfg9.len(), 9, "cfg9 {:?}", cfg9);
        assert_eq!(cfg9[1], 2); // CONFIGURATION
        let total = u16::from_le_bytes([cfg9[2], cfg9[3]]) as usize;
        assert!(total > 9 && total < 512, "total {}", total);
        let cfg = ctl_in(sys, &mut mem, &mut cpu, 0x0680, 0x0200, 0, total as u16, total);
        assert_eq!(cfg.len(), total, "cfg {:?}..", &cfg[..cfg.len().min(16)]);

        // SET_CONFIGURATION 1 (device self-completes status).
        usb_quiesce(sys, &mut mem, &mut cpu);
        with_usb(sys, |u| u.host_setup(0x0900, 1, 0, 0));
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 6_000_000, || false);
        assert!(cpu.fault.is_none());
        assert_eq!(mem.read8(0x20000C8D + 1), 1, "not configured");
    }

    fn usb_ctl_in(
        sys: &crate::system::WasmSystem,
        mem: &mut crate::cpu::mem::FlatMemory,
        cpu: &mut Cpu,
        req: u16, val: u16, idx: u16, len: u16, want: usize,
    ) -> Vec<u8> {
        usb_quiesce(sys, &mut *mem, &mut *cpu);
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
        run_until(sys, mem, cpu, 2_000_000, || false);
        with_usb(sys, |u| u.host_status_done());
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, mem, cpu, 2_000_000, || false);
        got.extend(usb_take_tx(sys));
        got
    }

    /// Standard enumeration + CDC bring-up through DTR (shared by the
    /// USB-serial firmware proofs): descriptors, address, config,
    /// 9600 8N1 line coding, DTR+RTS. Asserts each stage.
    fn usb_enumerate_cdc(
        sys: &crate::system::WasmSystem,
        mem: &mut crate::cpu::mem::FlatMemory,
        cpu: &mut Cpu,
    ) {
        let dev = usb_ctl_in(sys, mem, cpu, 0x0680, 0x0100, 0, 18, 18);
        assert_eq!((dev[0], dev[1]), (18, 1), "DEVICE descriptor");
        ctl_out(sys, mem, cpu, 0x0500, 5, 0, &[]);
        let cfg9 = usb_ctl_in(sys, mem, cpu, 0x0680, 0x0200, 0, 9, 9);
        assert_eq!(cfg9[1], 2, "CONFIGURATION descriptor");
        let total = u16::from_le_bytes([cfg9[2], cfg9[3]]) as usize;
        assert!(total > 9 && total < 512, "total {}", total);
        let cfg = usb_ctl_in(sys, mem, cpu, 0x0680, 0x0200, 0, total as u16, total);
        assert_eq!(cfg.len(), total);
        ctl_out(sys, mem, cpu, 0x0900, 1, 0, &[]);
        // DTR so `while (!Serial)` exits and writes are accepted.
        ctl_out(sys, mem, cpu, 0x2021, 0, 0,
            &[0x80, 0x25, 0x00, 0x00, 0x00, 0x00, 0x08]);
        ctl_out(sys, mem, cpu, 0x2221, 3, 0, &[]);
    }

    /// One control-OUT transfer. The OUT data stage is fed AFTER the setup
    /// is processed (the dcd clears the FIFO on setup receipt, like HW, so
    /// feeding beforehand would be wiped). Status is device-driven.
    fn ctl_out(
        sys: &crate::system::WasmSystem,
        mem: &mut crate::cpu::mem::FlatMemory,
        cpu: &mut Cpu,
        req: u16, val: u16, idx: u16, data: &[u8],
    ) {
        usb_quiesce(sys, &mut *mem, &mut *cpu);
        with_usb(sys, |u| u.host_setup(req, val, idx, data.len() as u16));
        crate::system::icu_raise_event(sys, 51);
        // Let the firmware arm the OUT stage first (setup -> BCLR passes).
        run_until(sys, mem, cpu, 2_000_000, || false);
        if !data.is_empty() {
            // Packet arrival raises BRDY in the model, like HW.
            with_usb(sys, |u| u.rx_inject(sys, 0, data));
        }
        run_until(sys, mem, cpu, 6_000_000, || false);
    }

    #[test]
    fn ra4m1_usb_serial_hello() {
        // End-to-end Serial.print: enumerate the Serial sketch, do CDC line
        // coding + DTR, run the loop, expect "hello" in the bulk capture.
        const APP_BASE: u32 = 0x4000;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bin = include_bytes!("../../blinky/r4serial.bin");
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

        cpu.run(sys, &mut mem, 6_000_000);
        sys.tick();
        with_usb(sys, |u| u.host_attach());
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 4_000_000, || false);
        with_usb(sys, |u| u.host_set_dvst(1));
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 4_000_000, || false);

        // Standard enumeration (short budgets: device is already up).
        let dev = usb_ctl_in(sys, &mut mem, &mut cpu, 0x0680, 0x0100, 0, 18, 18);
        assert_eq!(dev.len(), 18, "dev desc {:?}", dev);
        assert_eq!((dev[0], dev[1]), (18, 1), "DEVICE descriptor");
        ctl_out(sys, &mut mem, &mut cpu, 0x0500, 5, 0, &[]);
        let cfg9 = usb_ctl_in(sys, &mut mem, &mut cpu, 0x0680, 0x0200, 0, 9, 9);
        assert_eq!(cfg9.len(), 9, "cfg9 {:?}", cfg9);
        assert_eq!(cfg9[1], 2, "CONFIGURATION descriptor");
        let total = u16::from_le_bytes([cfg9[2], cfg9[3]]) as usize;
        assert!(total > 9 && total < 512, "total {}", total);
        let cfg = usb_ctl_in(sys, &mut mem, &mut cpu, 0x0680, 0x0200, 0, total as u16, total);
        assert_eq!(cfg.len(), total, "cfg {:?}..", &cfg[..cfg.len().min(16)]);
        ctl_out(sys, &mut mem, &mut cpu, 0x0900, 1, 0, &[]);

        // CDC bring-up: GET_LINE_CODING returns the default 115200 8N1,
        // SET_LINE_CODING programs 9600 8N1 (re-read proves the OUT data
        // stage landed), SET_CONTROL_LINE_STATE asserts DTR+RTS.
        let coding = usb_ctl_in(sys, &mut mem, &mut cpu, 0x21A1, 0, 0, 7, 7);
        assert_eq!(coding.len(), 7, "line coding {:?}", coding);
        ctl_out(sys, &mut mem, &mut cpu, 0x2021, 0, 0,
            &[0x80, 0x25, 0x00, 0x00, 0x00, 0x00, 0x08]);
        let coding = usb_ctl_in(sys, &mut mem, &mut cpu, 0x21A1, 0, 0, 7, 7);
        assert_eq!(coding, vec![0x80, 0x25, 0x00, 0x00, 0x00, 0x00, 0x08],
            "9600 8N1 not stored {:?}", coding);
        ctl_out(sys, &mut mem, &mut cpu, 0x2221, 3, 0, &[]);

        // Run the loop; the sketch prints every ~400ms.
        let mut all = Vec::new();
        for _ in 0..1200 {
            cpu.run(sys, &mut mem, 48_000);
            sys.tick();
            assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
            all.extend(usb_take_tx(sys));
            if all.windows(7).any(|w| w == b"hello\r\n" || w == b"hello\n") {
                break;
            }
        }
        let s = String::from_utf8_lossy(&all);
        assert!(s.contains("hello"), "no hello in {:?}..", &all[..all.len().min(64)]);
    }

    /// Read back a pipe's PIPECFG window (TYPE[15:14], DIR b4, EPNUM[3:0]).
    fn usb_pipe_cfg(sys: &crate::system::WasmSystem, pipe: u32) -> u16 {
        sys.p.write(sys, USBFS_BASE + 0x64, 2, pipe);
        sys.p.read(sys, USBFS_BASE + 0x68, 2) as u16
    }

    #[test]
    fn ra4m1_usb_cdc_echo() {
        // Native bulk endpoints, both directions through real pipe config:
        // enumerate the echo sketch, discover the CDC bulk pipes via
        // PIPECFG (no hardcoded pipe numbers), inject a multi-packet
        // message on bulk-OUT (Serial.read path), expect the exact bytes
        // back on bulk-IN (Serial.write path).
        const APP_BASE: u32 = 0x4000;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bin = include_bytes!("../../blinky/r4echo.bin");
        let mut mem = ra4m1_memory();
        // Erased flash reads 0xFF on silicon: pad the bootloader window
        // with 0xFF so any wild jump below the app faults precisely at
        // its landing (0xFFFF = undefined) instead of NOP-sliding on 0x00.
        let mut img = vec![0xFFu8; APP_BASE as usize];
        img.extend_from_slice(bin);
        mem.load(&img, FLASH_BASE);
        let sp = mem.read32(APP_BASE);
        let pc = mem.read32(APP_BASE + 4);
        assert_eq!(sp, 0x20007F00, "Arduino SP");
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let _ = usb_take_tx(crate::sys());
        let mut cpu = Cpu::new(sp, pc);
        cpu.deliver_irqs = true;
        let sys = crate::sys();

        cpu.run(sys, &mut mem, 6_000_000);
        sys.tick();
        with_usb(sys, |u| u.host_attach());
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 4_000_000, || false);
        with_usb(sys, |u| u.host_set_dvst(1));
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 4_000_000, || false);

        usb_enumerate_cdc(sys, &mut mem, &mut cpu);

        // Discover the CDC bulk pipes: TYPE==bulk(1), EPNUM==2, DIR=OUT/IN.
        let mut out_pipe = None;
        let mut in_pipe = None;
        for n in 1..10u32 {
            let c = usb_pipe_cfg(sys, n);
            if (c >> 14) & 3 == 1 && c & 0xF == 2 {
                if c & (1 << 4) == 0 { out_pipe = Some(n as usize); }
                else { in_pipe = Some(n as usize); }
            }
        }
        let (out_pipe, in_pipe) = (out_pipe.expect("bulk-OUT pipe"), in_pipe.expect("bulk-IN pipe"));

        // 100 bytes > 64B MPS: multi-packet both ways. The OUT side goes
        // in two host packets (64 + 36): after the first lands, run until
        // the firmware drains it, then feed the rest - like a real host,
        // which never pipelines a second packet over an unfinished one.
        let msg: Vec<u8> = (0..100u32).map(|i| (i.wrapping_mul(7).wrapping_add(3)) as u8).collect();
        let mut back = Vec::new();
        for (step, window) in [&msg[..64], &msg[64..]].iter().enumerate() {
            with_usb(sys, |u| u.rx_inject(sys, out_pipe, window));
            for _ in 0..1200 {
                cpu.run(sys, &mut mem, 48_000);
                sys.tick();
                assert!(cpu.fault.is_none(), "fault: {:?} step {}", cpu.fault, step);
                back.extend(usb_take_tx(sys));
                if back.len() >= msg.len() {
                    break;
                }
                // Let the second packet in once the first is fully echoed.
                if step == 0 && back.len() >= 64 {
                    break;
                }
            }
        }
        // Drain any stragglers, then compare the exact round-trip.
        for _ in 0..200 {
            cpu.run(sys, &mut mem, 48_000);
            sys.tick();
            assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
            back.extend(usb_take_tx(sys));
            if back.len() >= msg.len() {
                break;
            }
        }
        assert_eq!(back, msg, "echo mismatch ({}B)", back.len());
    }

    #[test]
    fn ra4m1_wire_ok() {
        // End-to-end Wire EEPROM round-trip on real FSP r_iic_master:
        // enumerate the Wire sketch, DTR, run the loop, expect "wire-ok"
        // in the bulk capture (write 0x55/0x66 to slave 0x50:0x10, read
        // back, compare on-device).
        const APP_BASE: u32 = 0x4000;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bin = include_bytes!("../../blinky/r4wire.bin");
        let mut mem = ra4m1_memory();
        mem.load(bin, APP_BASE);
        let sp = mem.read32(APP_BASE);
        let pc = mem.read32(APP_BASE + 4);
        assert_eq!(sp, 0x20007F00, "Arduino SP");
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let _ = usb_take_tx(crate::sys());
        let mut cpu = Cpu::new(sp, pc);
        cpu.deliver_irqs = true;
        let sys = crate::sys();

        cpu.run(sys, &mut mem, 6_000_000);
        sys.tick();
        with_usb(sys, |u| u.host_attach());
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 4_000_000, || false);
        with_usb(sys, |u| u.host_set_dvst(1));
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 4_000_000, || false);

        usb_enumerate_cdc(sys, &mut mem, &mut cpu);

        // The sketch does one round-trip in setup(), then idles.
        let mut all = Vec::new();
        for _ in 0..1200 {
            cpu.run(sys, &mut mem, 48_000);
            sys.tick();
            assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
            all.extend(usb_take_tx(sys));
            if all.windows(7).any(|w| w == b"wire-ok" || w == b"wire-ng") {
                break;
            }
        }
        let s = String::from_utf8_lossy(&all);
        assert!(s.contains("wire-ok"), "no wire-ok in {:?}..", &all[..all.len().min(96)]);
    }

    #[test]
    fn ra4m1_spi_ok() {
        // End-to-end SPI loopback on real FSP r_spi (Arduino drives
        // RSPI0 polled): enumerate the SPI sketch, arm the loopback jig
        // on SPI0, expect "spi-ok" (transfer(A5/5A/00) echoes).
        const APP_BASE: u32 = 0x4000;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bin = include_bytes!("../../blinky/r4spi.bin");
        let mut mem = ra4m1_memory();
        mem.load(bin, APP_BASE);
        let sp = mem.read32(APP_BASE);
        let pc = mem.read32(APP_BASE + 4);
        assert_eq!(sp, 0x20007F00, "Arduino SP");
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let _ = usb_take_tx(crate::sys());
        let mut cpu = Cpu::new(sp, pc);
        cpu.deliver_irqs = true;
        let sys = crate::sys();

        cpu.run(sys, &mut mem, 6_000_000);
        sys.tick();
        with_usb(sys, |u| u.host_attach());
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 4_000_000, || false);
        with_usb(sys, |u| u.host_set_dvst(1));
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 4_000_000, || false);

        usb_enumerate_cdc(sys, &mut mem, &mut cpu);

        // The sketch idles 200ms after SPI.begin: arm the RSPI
        // loopback jig (D11/D12/D13 probe to channel 1 = SPI1),
        // then collect the verdict.
        crate::system::sci_set_spi_loopback(SPI1_BASE, true);
        let mut all = Vec::new();
        for _ in 0..1200 {
            cpu.run(sys, &mut mem, 48_000);
            sys.tick();
            assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
            all.extend(usb_take_tx(sys));
            if all.windows(6).any(|w| w == b"spi-ok" || w == b"spi-ng") {
                break;
            }
        }
        let s = String::from_utf8_lossy(&all);
        crate::system::sci_set_spi_loopback(SPI1_BASE, false);
        assert!(s.contains("spi-ok"), "no spi-ok in {:?}..", &all[..all.len().min(64)]);
    }

    // CPU: predicated T1 ADD/SUB-immediate preserves flags (printNumber's
    // `ite le; addle r3,#48; addgt r3,#55` must not execute both arms:
    // addle must not kill N before addgt's test). Mirrors the existing
    // MOVS/ADD-reg/SUB-reg it_pred rule in the decoder.
    #[test]
    fn ra4m1_can_ok() {
        // End-to-end CAN self-test on real FSP + Arduino_CAN: enumerate
        // for the Serial verdict channel (the sketch pokes TCR loopback
        // itself), run the loop, expect "can-ok" in the bulk capture.
        const APP_BASE: u32 = 0x4000;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bin = include_bytes!("../../blinky/r4can.bin");
        let mut mem = ra4m1_memory();
        mem.load(bin, APP_BASE);
        let sp = mem.read32(APP_BASE);
        let pc = mem.read32(APP_BASE + 4);
        assert_eq!(sp, 0x20007F00, "Arduino SP");
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let _ = usb_take_tx(crate::sys());
        let mut cpu = Cpu::new(sp, pc);
        cpu.deliver_irqs = true;
        let sys = crate::sys();

        cpu.run(sys, &mut mem, 6_000_000);
        sys.tick();
        with_usb(sys, |u| u.host_attach());
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 4_000_000, || false);
        with_usb(sys, |u| u.host_set_dvst(1));
        crate::system::icu_raise_event(sys, 51);
        run_until(sys, &mut mem, &mut cpu, 4_000_000, || false);
        usb_enumerate_cdc(sys, &mut mem, &mut cpu);

        // The sketch sends + receives once in setup(), then idles.
        let mut all = Vec::new();
        for _ in 0..1200 {
            cpu.run(sys, &mut mem, 48_000);
            sys.tick();
            assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
            all.extend(usb_take_tx(sys));
            if all.windows(6).any(|w| w == b"can-ok" || w == b"can-ng") {
                break;
            }
        }
        let s = String::from_utf8_lossy(&all);
        assert!(s.contains("can-ok"), "no can-ok in {:?}..", &all[..all.len().min(64)]);
    }

    #[test]
    fn ra4m1_ite_add_imm_preserves_flags() {
        // Exact printNumber core for digit 4 (r3 starts 0 here instead of
        // the uxtb'd remainder, so correct is 0+48 = 48, not 52).
        // Both-arms bug gave 0+48+55 = 103.
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let mut mem = ra4m1_memory();
        let mut img = vec![0u8; 0x200];
        img[0..4].copy_from_slice(&0x20008000u32.to_le_bytes());
        img[4..8].copy_from_slice(&0x00000101u32.to_le_bytes());
        let code: [u16; 12] = [
            0x2300, // movs r3, #0
            0x2604, // movs r6, #4
            0x220A, // movs r2, #10
            0xFBB6, 0xF5F2, // udiv r5, r6, r2
            0xFB02, 0x6415, // mls r4, r2, r5, r6
            0x2C09, // cmp r4, #9
            0xBFD4, // ite le
            0x3330, // add r3, #48
            0x3337, // add r3, #55
            0xE7FE, // b .
        ];
        for (i, w) in code.iter().enumerate() {
            img[0x100 + i * 2] = (w & 0xFF) as u8;
            img[0x100 + i * 2 + 1] = (w >> 8) as u8;
        }
        mem.load(&img, FLASH_BASE);
        let mut cpu = Cpu::new(0x20008000, 0x00000101);
        cpu.deliver_irqs = false;
        let sys = crate::sys();
        cpu.run(sys, &mut mem, 20);
        assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
        assert_eq!(cpu.regs.r[3], 48, "ITE both-arms? r4={} r5={}", cpu.regs.r[4], cpu.regs.r[5]);
        assert_eq!(cpu.regs.r[4], 4, "mls");
        assert_eq!(cpu.regs.r[5], 0, "udiv");
    }

    #[test]
    fn ra4m1_map_icu_pin_irq() {
        // External pin interrupt routing: IRQCR sense-gates the virtual
        // button, ELC event 1 (IRQ0) pends the IELSR-mapped IRQ.
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        sys.p.write(sys, 0x4000_6300 + 13 * 4, 4, 1); // IELSR13 = IRQ0
        sys.p.write(sys, 0xE000_E100, 4, 1 << 13);    // ISER0: IRQ13
        sys.p.write(sys, 0x4000_6000, 1, 0x01); // IRQCR0 = rising
        assert!(!crate::system::icu_pin_edge(sys, 0, true), "falling ignored");
        assert!(!sys.p.nvic.borrow().has_pending(), "nothing pends");
        assert!(crate::system::icu_pin_edge(sys, 0, false), "rising fires");
        assert!(sys.p.nvic.borrow().has_pending(), "IRQ13 pends");
        sys.p.write(sys, 0x4000_6000, 1, 0x00); // IRQCR0 = falling
        assert!(crate::system::icu_pin_edge(sys, 0, true), "falling fires");
        assert!(!crate::system::icu_pin_edge(sys, 16, true), "bad line");
    }

    #[test]
    fn ra4m1_attach_interrupt() {
        // End-to-end attachInterrupt: real FSP external-IRQ setup on a
        // digital pin, virtual falling edges on every line (only the
        // firmware-routed one pends), ISR toggles the LED (PORT scan).
        const APP_BASE: u32 = 0x4000;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bin = include_bytes!("../../blinky/r4irq.bin");
        let mut mem = ra4m1_memory();
        mem.load(bin, APP_BASE);
        let sp = mem.read32(APP_BASE);
        let pc = mem.read32(APP_BASE + 4);
        assert_eq!(sp, 0x20007F00, "Arduino SP");
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let mut cpu = Cpu::new(sp, pc);
        cpu.deliver_irqs = true;
        let sys = crate::sys();
        cpu.run(sys, &mut mem, 6_000_000);
        sys.tick();
        assert!(cpu.fault.is_none(), "boot fault: {:?}", cpu.fault);
        let snap = || -> [u32; 12] {
            let mut s = [0u32; 12];
            for p in 0..12u32 {
                s[p as usize] = sys.p.read(sys, PORT_BASE + p * 0x20, 4) & 0xFFFF;
            }
            s
        };
        let first = snap();
        for line in 0..16usize {
            crate::system::icu_pin_edge(sys, line, true);
        }
        let mut toggled = false;
        for _ in 0..200 {
            cpu.run(sys, &mut mem, 48_000);
            sys.tick();
            assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
            if snap() != first {
                toggled = true;
                break;
            }
        }
        assert!(toggled, "no PORT output change after pin edges");
    }

    #[test]
    fn ra4m1_serial1_echo() {
        // End-to-end Serial1 (D0/D1 probe to SCI2): boot the echo sketch,
        // find the TE-enabled channel, inject bytes, expect the exact
        // echo on the UART console. No USB involved.
        const APP_BASE: u32 = 0x4000;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bin = include_bytes!("../../blinky/r4serial1.bin");
        let mut mem = ra4m1_memory();
        mem.load(bin, APP_BASE);
        let sp = mem.read32(APP_BASE);
        let pc = mem.read32(APP_BASE + 4);
        assert_eq!(sp, 0x20007F00, "Arduino SP");
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        crate::system::get_uart_output().lock().unwrap().clear();
        let mut cpu = Cpu::new(sp, pc);
        cpu.deliver_irqs = true;
        let sys = crate::sys();
        cpu.run(sys, &mut mem, 6_000_000);
        sys.tick();
        assert!(cpu.fault.is_none(), "boot fault: {:?}", cpu.fault);
        // Discover the channel FSP put in UART TX mode (SCR.TE).
        let mut ch_base = None;
        for _ in 0..200 {
            cpu.run(sys, &mut mem, 48_000);
            sys.tick();
            assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
            for hw in [0u32, 1, 2, 3, 4, 5, 6, 7, 8, 9] {
                let base = SCI0_BASE + hw * 0x20;
                if sys.p.read(sys, base + 0x02, 1) & (1 << 5) != 0 {
                    ch_base = Some(base);
                    break;
                }
            }
            if ch_base.is_some() {
                break;
            }
        }
        let base = ch_base.expect("no TE-enabled SCI channel");
        for b in [b'H', b'i'] {
            assert!(crate::peripherals::ra_sci::sci_rx_inject(sys, base, b));
            let mut echoed = false;
            for _ in 0..200 {
                cpu.run(sys, &mut mem, 48_000);
                sys.tick();
                assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
                if crate::system::get_uart_output().lock().unwrap().as_str().contains(b as char) {
                    echoed = true;
                    break;
                }
            }
            assert!(echoed, "no echo of {:?}", b as char);
        }
    }

    #[test]
    fn ra4m1_map_extra_channels() {
        // Same-stride siblings without ELC events (polled only): AGT2,
        // GPT8, SCI5, IIC2, DMAC ch5. Spot-check counters and data paths.
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let sys = crate::sys();
        // AGT2 down-counter.
        sys.p.write(sys, 0x4008_4200, 2, 500);
        sys.p.write(sys, 0x4008_4208, 1, 1);
        crate::system::INSTRUCTION_COUNT.fetch_add(100, std::sync::atomic::Ordering::Relaxed);
        sys.tick();
        let cnt = sys.p.read(sys, 0x4008_4200, 2) & 0xFFFF;
        assert!(cnt > 0 && cnt < 500, "agt2 cnt={}", cnt);
        // GPT8 16-bit counter with period.
        sys.p.write(sys, 0x4007_8808, 4, 200);
        sys.p.write(sys, 0x4007_8800, 4, 1);
        crate::system::INSTRUCTION_COUNT.fetch_add(100, std::sync::atomic::Ordering::Relaxed);
        sys.tick();
        let g = sys.p.read(sys, 0x4007_8804, 4);
        assert!(g > 0 && g <= 200, "gpt8 cnt={}", g);
        // SCI5 UART TX reaches the console.
        crate::system::get_uart_output().lock().unwrap().clear();
        sys.p.write(sys, 0x4007_00A0 + 0x02, 1, 0x20);
        sys.p.write(sys, 0x4007_00A0 + 0x03, 1, b'K' as u32);
        assert_eq!(crate::system::get_uart_output().lock().unwrap().as_str(), "K");
        // IIC2 flags accept a START.
        sys.p.write(sys, 0x4005_3200, 1, 0x80); // ICE
        sys.p.write(sys, 0x4005_3201, 1, 0x02); // ST
        assert_ne!(sys.p.read(sys, 0x4005_3201, 1) & (1 << 7), 0, "iic2 BBSY");
        assert_ne!(sys.p.read(sys, 0x4005_3201, 1) & (1 << 6), 0, "iic2 MST");
        sys.p.write(sys, 0x4005_3201, 1, 0x08); // SP
        sys.tick();
        assert_eq!(sys.p.read(sys, 0x4005_3201, 1) & (1 << 7), 0, "iic2 free");
        // DMAC ch5 mem-to-mem.
        let mut mem = ra4m1_memory();
        for i in 0..8u32 { mem.write8(0x20000000 + i, (0xC0 + i) as u8); }
        sys.p.write(sys, 0x4000_5000 + 5 * 0x40 + 0x00, 4, 0x20000000);
        sys.p.write(sys, 0x4000_5000 + 5 * 0x40 + 0x04, 4, 0x20000100);
        sys.p.write(sys, 0x4000_5000 + 5 * 0x40 + 0x08, 4, 8);
        mem.write32(0x4000_5000 + 5 * 0x40 + 0x0C, 1);
        assert_eq!(sys.pending_dma_count(), 0);
        for i in 0..8u32 {
            assert_eq!(mem.read8(0x20000100 + i), (0xC0 + i) as u8, "dma5 byte {}", i);
        }
    }

    #[test]
    fn ra4m1_rtc_firmware() {
        // End-to-end RTC on real FSP: the sketch sets 11:59:58, the core
        // advances ~4 virtual seconds, MMIO reads show the BCD rollover
        // to 12:00:0x. No USB involved.
        const APP_BASE: u32 = 0x4000;
        const RTC: u32 = 0x4004_4000;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bin = include_bytes!("../../blinky/r4rtc.bin");
        let mut mem = ra4m1_memory();
        mem.load(bin, APP_BASE);
        let sp = mem.read32(APP_BASE);
        let pc = mem.read32(APP_BASE + 4);
        assert_eq!(sp, 0x20007F00, "Arduino SP");
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let mut cpu = Cpu::new(sp, pc);
        cpu.deliver_irqs = true;
        let sys = crate::sys();
        cpu.run(sys, &mut mem, 6_000_000);
        sys.tick();
        assert!(cpu.fault.is_none(), "boot fault: {:?}", cpu.fault);
        // Wait for a minute rollover with seconds wrapping to 0
        // (proves the BCD second->minute carry on real FSP-set time,
        // whatever wall phase the boot landed in).
        let m0 = sys.p.read(sys, RTC + 0x04, 1) & 0xFF;
        let mut ok = false;
        for _ in 0..800 {
            cpu.run(sys, &mut mem, 48_000);
            sys.tick();
            assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
            let sec = sys.p.read(sys, RTC + 0x02, 1) & 0xFF;
            let min = sys.p.read(sys, RTC + 0x04, 1) & 0xFF;
            let hr = sys.p.read(sys, RTC + 0x06, 1) & 0xFF;
            assert!(hr <= 0x23 && (hr & 0x0F) <= 9, "hr BCD {:02x}", hr);
            if min != m0 && sec == 0x00 {
                ok = true;
                break;
            }
        }
        assert!(ok, "no minute rollover");
    }

    #[test]
    fn ra4m1_wdt_refresh() {
        // End-to-end WDT refresh on real FSP: the sketch refreshes every
        // 10ms forever; over >> one maximum period no reset may latch.
        // No USB involved; the verdict is the model reset flag.
        const APP_BASE: u32 = 0x4000;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bin = include_bytes!("../../blinky/r4wdtref.bin");
        let mut mem = ra4m1_memory();
        mem.load(bin, APP_BASE);
        let sp = mem.read32(APP_BASE);
        let pc = mem.read32(APP_BASE + 4);
        assert_eq!(sp, 0x20007F00, "Arduino SP");
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let mut cpu = Cpu::new(sp, pc);
        cpu.deliver_irqs = true;
        let sys = crate::sys();
        cpu.run(sys, &mut mem, 6_000_000);
        sys.tick();
        assert!(cpu.fault.is_none(), "boot fault: {:?}", cpu.fault);
        for _ in 0..5000 {
            cpu.run(sys, &mut mem, 48_000);
            sys.tick();
            assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
        }
        assert!(!crate::system::is_watchdog_reset_requested(), "reset despite refresh");
    }

    #[test]
    fn ra4m1_wdt_expire() {
        // End-to-end WDT timeout on real FSP: the sketch starts the
        // watchdog and never refreshes; the reset flag must latch.
        const APP_BASE: u32 = 0x4000;
        let _g = RA_BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bin = include_bytes!("../../blinky/r4wdtexp.bin");
        let mut mem = ra4m1_memory();
        mem.load(bin, APP_BASE);
        let sp = mem.read32(APP_BASE);
        let pc = mem.read32(APP_BASE + 4);
        assert_eq!(sp, 0x20007F00, "Arduino SP");
        let sys = crate::system::WasmSystem::new_ra4m1();
        crate::init_for_test(sys);
        let mut cpu = Cpu::new(sp, pc);
        cpu.deliver_irqs = true;
        let sys = crate::sys();
        cpu.run(sys, &mut mem, 6_000_000);
        sys.tick();
        assert!(cpu.fault.is_none(), "boot fault: {:?}", cpu.fault);
        let mut fired = false;
        for _ in 0..6000 {
            cpu.run(sys, &mut mem, 48_000);
            sys.tick();
            assert!(cpu.fault.is_none(), "fault: {:?}", cpu.fault);
            if crate::system::is_watchdog_reset_requested() {
                fired = true;
                break;
            }
        }
        assert!(fired, "no watchdog reset without refresh");
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
