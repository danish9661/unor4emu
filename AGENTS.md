# AGENTS.md — Arduino UNO R4 (RA4M1) emulator

## 0. Working agreement

- Scope is `uno r4/` ONLY. Never touch `microbit-v2/`, `stm32 F4/`, background
  processes, or `pkill` anything.
- Keep going non-stop toward a working end result. Do not stall on questions;
  decide and build. No "limitations" - fix the bus/model until hardware-exact.
- Every change must keep `cargo test -- --test-threads=1` green in
  `core/ra4m1-periph-wasm` (currently 184) and `cargo build` green in the
  top workspace (Minima only; WiFi is parked).
- Shared globals (`SYS`, `INSTRUCTION_COUNT`, UART buffer) mean parallel
  `cargo test` flakes (notably LTDC timing). Always verify with
  `--test-threads=1`. Do not "fix" flakes by editing the CPU to suit a board.
- CPU bugs go upstream to `Documents/stm32 F4/cpu_bug.md`, never worked around
  in board code. The decoder is proven; peripherals are the work.

## 1. What this is

Emulator for the **Arduino UNO R4 Minima** (RA4M1 `R7FA4M1AB3CFM`,
Cortex-M4F @ 48 MHz, 256 KB flash @ `0x00000000`, 32 KB SRAM @ `0x20000000`,
8 KB data flash). WiFi (`wasm-wifi/`, ESP32-S3) is PARKED - folder stays on
disk but is excluded from the workspace until its code is ready.

- `core/ra4m1-periph-wasm/` - the full snapshot (isolated `[workspace]`,
  excluded from the top build). CPU `src/cpu/` is the proven M4F decoder;
  STM32 peripherals remain as reference until their RA replacements land.
  184 tests must stay green (128 legacy CPU + 56 R4 proofs).
- `core/ra4m1-core/` - the SMALL R4-only core (top-workspace member): same
  CPU + ARM + RA peripherals, NO STM32 code, deps are only
  `wasm-bindgen`+`console_error_panic_hook` (no aes/sha/des/svd/regex/serde).
  Release WASM is ~225KB vs ~2.1MB for the snapshot core (~9.4x smaller).
  Its 50 `ra4m1.rs` proofs mirror the snapshot's register-level ones and must stay green.
- `wasm-minima/` (`uno-r4-minima-wasm`) - the small WASM output. Calls
  `init_ra4m1()`, uses `WasmCpu::new_ra4m1()`. Depends on `ra4m1-core` only.
  No STM32 and no ESP32 code may ever link here.
- `wasm-wifi/` - parked, excluded from workspace members.
- `crates/wifi-link/` - `WifiModule` trait + `NoWifi` (zero cost). ESP32-S3
  plugs in here later without touching RA4M1.
- `core/blinky/blinky.bin`, `core/monox/stm32f407.svd`, `core/docs/*.s` -
  legacy CPU-test assets. `blinky.bin` is still `include_bytes!`'d by 20+
  CPU tests; do not delete until CPU tests are R4-native.

## 2. Build / test

```bash
# snapshot core (184 tests, always single-threaded)
cargo test --manifest-path core/ra4m1-periph-wasm/Cargo.toml --lib -- --test-threads=1
# small core (50 R4 proofs)
cargo test --manifest-path core/ra4m1-core/Cargo.toml --lib -- --test-threads=1
# top workspace (Minima only)
cargo build --manifest-path Cargo.toml
```

`core/ra4m1-periph-wasm` needs `crate-type = ["cdylib","rlib"]` (rlib so the
wasm wrappers can path-depend on it).

## 3. RA4M1 map (real bases, via FSP R7FA4M1AB.h - NOT RA6!)

| Block | Base | Model |
|---|---|---|
| SYSC | `0x4001_E000` | `ra_system.rs` accept-and-retain, erased reads `0xFFFFFFFF` |
| MSTP | `0x4004_6FFC` | same model, own slot (MSTPCRB @ `0x4004_7000`) |
| ICU | `0x4000_6000` | stub (same model) |
| PORT0.. | `0x4004_0000`+n*`0x20` | `ra_port.rs` PDR/PODR/PIDR + EORR/PORR |
| PFS | `0x4004_0800` | retain map |
| ELC | `0x4004_1000` | `ra_misc.rs` routing table + soft trigger |
| RTC | `0x4004_4000` | `ra_rtc.rs` instruction-count seconds |
| WDT/IWDT | `0x4004_4200`/`0x4004_4400` | `ra_misc.rs` + shared reset flags |
| DMAC/DTC | `0x4000_5000`/`0x4000_5400` | `ra_dma.rs` queues shared sync-memcopy path + DTC repeat engine (IELSR.DTCE, SRAM vector table) |
| DOC/ADC/DAC | `0x4005_4100`/`0x4005_C000`/`0x4005_E000` | `ra_misc.rs`/`ra_analog.rs` |
| SCI0-9 | `0x4007_0000`+ch*`0x20` | `ra_sci.rs` byte-exact UART + simple-SPI mode (SMR.CM/SPMR, loopback jig; ch4-9 eventless polled) |
| SPI0/1 | `0x4007_2000`/`0x4007_2100` | `ra_spi.rs` RSPI master (polled SPRF + loopback, Arduino D11-13 probe to ch1) + slave (MSTR=0 staging + exchange) + virtual SD card |
| IIC0-2 | `0x4005_3000`+ch*`0x100` | `ra_i2c.rs` RIIC master + virtual EEPROM slave at 0x50 + shared-bus slave (SAR/AAS/RXI/TXI/STOP; IIC2 eventless) |
| CRC | `0x4007_4000` | `ra_misc.rs` IEEE-802.3 |
| GPT0-13 | `0x4007_8000`+ch*`0x100` | `ra_gpt.rs` instruction-count driven (0-1: 32-bit; 8-13 eventless polled) |
| AGT0-5 | `0x4008_4000`+ch*`0x100` | `ra_misc.rs` 16-bit count (2-5 eventless polled) |
| ACMPLP/OPAMP | `0x4008_5E00`/`0x4008_6000` | `ra_opamp.rs` loopback + compare |
| USBFS | `0x4009_0000` | `ra_usb.rs` endpoint FIFOs + TX capture + IRQs (CDC + HID, suspend/resume) |
| CTSU | `0x4008_1000` | `ra_ctsu.rs` STRT->tick counters + END event (self + mutual MD=2) |
| CAN0 | `0x4005_0000` | `ra_can.rs` mailbox TX/RX + self-test loopback + RX/TX FIFO via MB24 (CAN1: no routable events, unmapped) |
| DATAFLASH | `0x4010_0000` | `ra_flash.rs` 8KB window, erased `0xFF`, bit-clear writes |
| FACI_LP | `0x407E_C000` | `ra_flash.rs` program/erase/blankcheck engine + FENTRYR |
| ARM | `0xE000_xxxx` | reuse NVIC/SysTick/SCB/MPU/FPU/DWT/STIR/ITM |

`Peripherals::new_ra4m1()` builds this map. `WasmSystem::new_ra4m1()` +
`init_ra4m1()` install it. Legacy `new()`/`init()` (STM32 map) stay for the
128 legacy CPU tests - do not delete until replacements are proven.

## 4. Bus rules (byte-exact, no hacks)

- `Peripherals::read` aligns to 4, asks the model for the 32-bit pack, then
  returns `pack >> (8*byte_offset)`. Models return LE packs; word accesses
  shift 0 (no-op for STM32).
- `Peripherals::write` merges sub-word stores into the aligned pack, then
  calls `write_sized(offset_aligned, merged, byte_offset, size)`. Default
  impl forwards to `write` (STM32 word path). Byte-packed RA models override
  `write_sized` and apply only bytes in range - this is what makes repeated
  `STRB TDR,#3` with the same value transmit twice instead of diffing to zero.
- New RA peripherals MUST use real byte offsets (SCI `TDR+0x03`, not word
  aliases) and prove them with a firmware-bytes test in `ra4m1.rs`
  (hand-assembled Thumb with exact LDR-literal `PC=(addr+4)&~3` arithmetic).

## 5. Peripheral status

- DONE: SYSTEM/MSTP/ICU(+IELSR routing)/ELC stubs, PORT+PFS (real PCNTR
  layout), SCI UART (TX console, RX inject, TXI/RXI events), GPT
  (count/compare/event), ADC14 (ADST=bit15, HW-cleared), DAC12, RTC,
  DMAC/DTC memcopy, AGT down-counter (latched reload, TUNDF, underflow
  events), WDT/IWDT, CRC, DOC, OPAMP/ACMP, USBFS endpoint FIFOs + TX
  capture + IRQs (virtual-host enumeration proven against TinyUSB),
  `ra4m1_memory()` flash-at-zero, `WasmCpu::new_ra4m1()`, Arduino Blink
  boots AND toggles the LED (AGT0 1ms IRQs drive millis), native
  USB-Serial bulk endpoints (echo sketch round-trips 100B via PIPECFG-
  discovered pipes, `rx_inject` raises BRDY like HW), CTSU
  (STRT->tick counters + END event), CAN0 (mailbox TX/RX + self-test
  loopback), IIC0/1 (RIIC master vs virtual EEPROM slave at 0x50, Wire
  sketch round-trips on real FSP), SCI simple-SPI (shift + loopback jig),
  SPI0/1 (RSPI master, polled SPRF + loopback; Arduino D11-13 probe to
  ch1, SPI firmware proof round-trips on real FSP), channel loops
  (SCI0-9, GPT0-13, AGT0-5, IIC2, DMAC0-7 mapped; extras eventless polled),
  attachInterrupt (ICU IRQCR sense + `icu_pin_edge` injection, all 16 lines
  proven with a button sketch), Serial1 (SCI2 TE discovery + echo),
  CAN firmware proof (Arduino_CAN self-test loopback via MSSR search +
  TSRC strobe), RTC alarm (event 38) + RTC lib firmware proof (BCD minute
  rollover), WDT lib proofs (refresh holds 5000 chunks, expiry latches;
  FSP uses TOPS=3 = 4096 ticks), OPAMP lib firmware proof (`OPAMP.begin()`
  -> AMPMON0), dataflash + FACI_LP (FSP R_FLASH_LP program/erase/
  blankcheck, Arduino EEPROM round-trip), I2C slave (shared-bus fabric
  vs master channels, AAS/RXI/TXI/STOP, Wire-master/bare-slave proof),
  SPI slave (MSTR=0 staging + exchange, dual-channel proof), CAN FIFO
  (RX/TX depth-4, MB24 port, RFPCR/TFPCR, loopback proof), CTSU mutual
  (MD=2 pair default + overrides), USB HID keyboard (PluggableUSB report
  descriptor + INT-IN report proof), USB suspend/resume (DVSQ SUSPx,
  RESM, WKUP, callback proof), virtual SD in SPI mode (CMD0/8/55/41/58/
  17/24, MBR + write/read-back proof), CAN error counting (RECR/TECR +
  EIFR + ERI event 74, Arduino_CAN isError proof), DTC (IELSR.DTCE +
  SRAM vector table + repeat engine serviced in the mem path, GPT->DAC
  firmware proof).
- Hard-won truths: RA4M1 bases differ from RA6 everywhere (this §3 is from
  R7FA4M1AB.h, never assume); SYSC OPCCR resets 0x00 with timed TSF;
  AGT reload latches the programmed counter (AGTCMA untouched when output
  support is off); AGT flag bits clear-by-0 (start value 0xF1 keeps it
  running); `MRS PSR` must include live IPSR or FSP routes every IRQ wrong
  (fixed in snapshot thumb.rs - report upstream with the other CPU bugs);
  exception takes/returns must use live r13, never the stale bank, or
  nested IRQs unstack garbage as PC (fixed in both cores' cpu/mod.rs -
  reported upstream in `Documents/stm32 F4/cpu_bug.md` §11); DCPCTR.CCPL
  is a strobe (every write latches BEMP, retained bits wedge status);
  CFIFO OUT drains are byte reads (popping pairs drops odd bytes);
  predicated T1 ADD/SUB-imm preserves flags like MOVS/ADD-reg/SUB-reg
  already did (`ite le; addle; addgt` ran both arms and broke every
  integer print - fixed in both cores' thumb.rs, §12 upstream);
  RIIC ST auto-sets MST/TRS (FSP writes ST alone, then polls MST);
  RIIC TDRE+TXI fire on START and latched flags fire on ICIER enable
  (the interrupt-driven FSP flow waits on both, never on flag edges);
  FSP RXI discards the first ICDRR read, so SLA+R must NOT preload
  byte0 (stream from the next tick instead); restart-write yields no
  completion event (no STOP by design - tolerated, real sketches ignore
  its return too); RSPI D11-13 probe to SPI1 (0x40072100), polled SPRF,
  SPDR byte lane is +0x04;
  SCB VTOR keeps bits[31:7] (128B alignment, ARM TBLOFF): Arduino parks
  its RAM vector table at 0x20007F00 and a 1KB mask mangled it to
  0x20007C00, so every IRQ vectored into stack garbage (Blink survived
  by luck, FAT died) - fixed in both cores' scb.rs;
- NEXT: WiFi un-park assessment, SDHI for FAT-over-SD. LED matrix
  (WiFi board) renders from RA GPIO pins, not as a peripheral.
- Firmware order: bare-metal blinky -> UART echo -> ArduinoCore-renesas
  `Blink.ino` (wraps FSP, runs on the core, only needs register models).

## 6. R4 proofs (`src/ra4m1.rs`)

`ra4m1_boots_from_zero`, `clock_stub_accepts_boot_writes`,
`ra4m1_map_sci_tx_reaches_console`, `ra4m1_map_gpt_counts_and_matches`,
`ra4m1_map_port_output_retained`, `ra4m1_firmware_blinky_via_mmio`,
`ra4m1_map_adc_converts_channel`, `ra4m1_map_dac_output_retained`,
`ra4m1_map_rtc_ticks_seconds`, `ra4m1_map_dmac_mem_to_mem`,
`ra4m1_map_elc_routes_software_event`, `ra4m1_map_agt_counts`,
`ra4m1_map_crc_and_doc`, `ra4m1_map_sci_echo_path`,
`ra4m1_map_opamp_follower_and_acmp`, `ra4m1_arduino_blink_boots`,
`ra4m1_arduino_blink_toggles_led`, `ra4m1_map_usb_tx_reaches_capture`,
`ra4m1_usb_enumerates_cdc`, `ra4m1_usb_serial_hello`,
`ra4m1_usb_cdc_echo`, `ra4m1_map_ctsu_measures`,
`ra4m1_map_can_loopback`, `ra4m1_map_i2c_eeprom`,
`ra4m1_map_sci_spi_loopback`, `ra4m1_wire_ok`,
`ra4m1_ite_add_imm_preserves_flags`, `ra4m1_map_spi_loopback`,
`ra4m1_spi_ok`, `ra4m1_can_ok`, `ra4m1_map_icu_pin_irq`,
`ra4m1_attach_interrupt`, `ra4m1_serial1_echo`,
`ra4m1_map_extra_channels`, `ra4m1_rtc_firmware`, `ra4m1_map_rtc_alarm`,
`ra4m1_wdt_refresh`, `ra4m1_wdt_expire`, `ra4m1_opamp_firmware`,
`ra4m1_map_dataflash_program_erase`, `ra4m1_eeprom_ok`,
`ra4m1_map_i2c_slave`, `ra4m1_wire_slave_ok`, `ra4m1_map_spi_slave`,
`ra4m1_spi_slave_ok`, `ra4m1_map_can_fifo`, `ra4m1_can_fifo_ok`,
`ra4m1_map_ctsu_mutual`, `ra4m1_ctsu_mutual_ok`,
`ra4m1_map_usb_suspend_resume`, `ra4m1_usb_hid_keyboard`,
`ra4m1_usb_suspend_resume_ok`, `ra4m1_map_sd_card`, `ra4m1_sd_ok`,
`ra4m1_map_can_errors`, `ra4m1_can_error_ok`, `ra4m1_map_dtc_repeat`,
`ra4m1_dtc_ok`.
Keep all green and add one per peripheral using the same shape:
new_ra4m1 system -> MMIO writes -> tick -> assert state/marker.

## 7. Arduino firmware (arduino-cli, Renesas core 1.6.0)

```bash
arduino-cli compile --fqbn arduino:renesas_uno:minima --output-dir /tmp/r4build /tmp/r4blink
```

The Minima bootloader occupies `0x0000-0x3FFF`; the app links at `0x4000`
(see `.hex` record addresses). The raw `.bin` is the app image, so the
emulator loads it at `APP_BASE=0x4000` and boots from that table - loading
at zero executes shifted garbage (looks plausible, faults in an epilogue).
`core/blinky/r4blink.bin` is the vendored Blink build the boot test runs
(500k instructions, no fault, PC in app region). `core/blinky/r4serial.bin`
(Serial hello), `core/blinky/r4echo.bin` (bulk echo),
`core/blinky/r4wire.bin` (Wire EEPROM round-trip),
`core/blinky/r4spi.bin` (SPI loopback), `core/blinky/r4eep.bin`
(EEPROM), `core/blinky/r4wire1.bin` (Wire master + bare slave),
`core/blinky/r4spislv.bin` (SPI0 master + SPI1 slave),
`core/blinky/r4canfifo.bin` (CAN FIFO), `core/blinky/r4ctsu.bin`
(CTSU mutual), `core/blinky/r4hid.bin` (HID keyboard),
`core/blinky/r4susp.bin` (suspend/resume) and
`core/blinky/r4sd.bin` (SD card), `core/blinky/r4canerr.bin`
(CAN errors) and `core/blinky/r4dtc.bin` (GPT->DAC via DTC) are the
vendored USB/I2C/SPI/CAN/touch/HID/SD/DTC proof builds, compiled the
same way from their sketches.

## 8. Browser demo (`demo/`)

`./demo/build.sh` then `python3 -m http.server -d demo 8901`: dark
single page driving the 228KB Minima WASM (`WasmCpu` + the `usb_*` /
`periph_*` free functions + `spi_set_sd_card`/`sd_read_block` for the
SD tab). Eight tabs run the vendored firmware live: Blink (LED + full
12x16 GPIO grid from PORT), Serial (in-page virtual-host enumeration
with step checklist, hello in the terminal), Echo (bulk-pipe discovery
via PIPECFG + typed round-trip), Wire (IIC1 master vs bare-metal IIC0
slave flag trace), SPI (SPI0 master vs SPI1 slave flag trace), SD (init
+ MBR dump + block-1 recheck via export), CAN (RX-FIFO MB24 trace),
EEPROM (live dataflash byte trace). Flag traces poll MMIO only - data
registers (ICDRR/SPDR) are never read (a read would eat the firmware's
byte); sub-word reads return unmasked packs, so JS masks (`% 256`).
D13 = P111 (PORT1 bit 11), not bit 13. `demo/pkg/` + `demo/fw/` are
generated (git-ignored); only `index.html`/`app.js`/`styles.css`/
`build.sh` are tracked. Verified with Playwright screenshots (every
tab to green verdict), console clean. Deploys to GitHub Pages via
`.github/workflows/pages.yml` (rebuilds pkg+fw, publishes `demo/`).
