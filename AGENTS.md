# AGENTS.md — Arduino UNO R4 (RA4M1) emulator

## 0. Working agreement

- Scope is `uno r4/` ONLY. Never touch `microbit-v2/`, `stm32 F4/`, background
  processes, or `pkill` anything.
- Keep going non-stop toward a working end result. Do not stall on questions;
  decide and build. No "limitations" - fix the bus/model until hardware-exact.
- Every change must keep `cargo test -- --test-threads=1` green in
  `core/ra4m1-periph-wasm` (currently 120: 37 legacy CPU/system + 83 R4 proofs)
  and in `core/ra4m1-core` (83 R4 proofs: 82 mirror + EK zero-boot) and
  `cargo build` green in the top workspace (Minima only; WiFi is parked).
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
  120 tests must stay green (37 legacy CPU/system + 83 R4 proofs).
- `core/ra4m1-core/` - the SMALL R4-only core (top-workspace member): same
  CPU + ARM + RA peripherals, NO STM32 code, deps are only
  `wasm-bindgen`+`console_error_panic_hook` (no aes/sha/des/svd/regex/serde).
  Release WASM is ~288KB vs ~2.1MB for the snapshot core (~7.3x smaller).
  Its 83 `ra4m1.rs` proofs (82 mirror + EK zero-boot) track the snapshot and must stay green.
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
# snapshot core (120 tests, always single-threaded)
cargo test --manifest-path core/ra4m1-periph-wasm/Cargo.toml --lib -- --test-threads=1
# small core (83 R4 proofs)
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
| GPT0-13 | `0x4007_8000`+ch*`0x100` | `ra_gpt.rs` real offsets (GTCR+0x2C GTIOR+0x34 GTCNT+0x48 GTCCR+0x4C GTPR+0x64), wrap/compare/overflow events, GTIOA/B function-0 PWM latches routed to PORT via PFS (0-1: 32-bit; 8-13 eventless polled) |
| AGT0-5 | `0x4008_4000`+ch*`0x100` | `ra_misc.rs` 16-bit count (2-5 eventless polled) |
| ACMPLP/OPAMP | `0x4008_5E00`/`0x4008_6000` | `ra_opamp.rs` loopback + compare |
| USBFS | `0x4009_0000` | `ra_usb.rs` endpoint FIFOs + TX capture + IRQs (CDC + HID, suspend/resume) + MSC mock chip (BOT+SCSI INQUIRY/READ_CAPACITY/READ10/WRITE10 over bulk pipes) |
| CTSU | `0x4008_1000` | `ra_ctsu.rs` STRT->tick counters + END event (self + mutual MD=2) |
| KINT | `0x4008_0000` | `ra_icu.rs` KRCTL/KRF/KRM + `kint_key_press` jig -> KEY_INT event 69 |
| SLCDC | `0x4008_2000` | `ra_misc.rs` mode regs + 64B display RAM retain (no panel) |
| SSI0 | `0x4004_E000` | `ra_ssi.rs` TX drain + RX pattern FIFO + TXI/RXI edges (Arduino I2S lib broken, bare-metal proven) |
| SSI1 | `0x4004_E100` | same model, own slot/FIFOs (FSP mask = SSI0 only, register proof) |
| GPT OPS/POEG | `0x4007_8FF0`/`0x4004_2000`+n*`0x100` | `ra_gpt.rs` safety stubs (accept-and-retain, register proof) |
| R_DMA | `0x4000_5200` | `ra_gpt.rs` module-activation stub (register proof) |
| CAN0 | `0x4005_0000` | `ra_can.rs` mailbox TX/RX + self-test loopback + RX/TX FIFO via MB24 (CAN1: polled-only, same mailboxes, no events) |
| CAN1 | `0x4005_1000` | same model, eventless (polled SENTDATA/NEWDATA) |
| DATAFLASH | `0x4010_0000` | `ra_flash.rs` 8KB window, erased `0xFF`, bit-clear writes |
| FACI_LP | `0x407E_C000` | `ra_flash.rs` program/erase/blankcheck engine + FENTRYR |
| DAC8 | `0x4009_E000` | `ra_analog.rs` DACS retain + DAM enable gate (no Arduino consumer) |
| ARM | `0xE000_xxxx` | reuse NVIC/SysTick/SCB/MPU/FPU/DWT/STIR/ITM |

`Peripherals::new_ra4m1()` builds this map. `WasmSystem::new_ra4m1()` +
`init_ra4m1()` install it. Legacy `new()`/`init()` (STM32 map) stay for the
37 legacy CPU/system tests - do not delete until replacements are proven.

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
  firmware proof; repeat length reads CRAL, the CRA low byte, since
  R_DTC doubles length into CRAL+CRAH), AnalogWave sine (real
  `analogWave.sine(10)` GPT->DTC->DAC12 firmware proof, `r4aws.bin`),
  GPT PWM output (GTIOA/B function 0 + OAE/OBE, PFS
  PSEL routing per the Minima pinmux table, 25% duty measured on D6),
  CAN1 (eventless polled self-test proof), DAC8 (retain + DAM gate),
  TSN calibration constants, SLCDC regs + display RAM, KINT key
  interrupt (`kint_key_press` jig + firmware proof), SSI0 audio FIFO
  (TX drain + RX pattern + TXI/RXI, bare-metal proof - Arduino I2S
  lib does not compile), LED matrix (12x8 charlieplex GPIO render,
  smiley reconstruction proof + demo tab).
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
  predicated T1 shifts (LSL/LSR/ASR-imm) and the 0x4000 ALU block
  preserve flags the same way, with the TST/CMP/CMN test ops always
  setting flags (their only output - fixed in both cores' thumb.rs,
  §15 upstream); MOV-reg T1 (0x0000 class, imm==0) never sets flags -
  GAS emits even `lsls r0,r5,#0` as the flagless alias 0028 (fixed in
  both cores' thumb.rs, §14 upstream);
  DTC repeat/block length is the CRA low byte (CRAL): R_DTC doubles the
  programmed length into CRAL+CRAH, so 24 reads back 0x1818 (fixed in
  both cores' mem.rs - the AnalogWave sine proof needed it);
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
  GPT offsets are the RA map (GTCR+0x2C GTPR+0x64 GTIOR+0x34), NOT the
  old fiction (GTCR+0x00 GTPR+0x08) - FSP writes real offsets and the
  old model ignored them (FSP GPT flows never ran); unprogrammed
  compares (all-ones) must be bounded by the period or they false-fire
  on every wrap; PORT/PFS are separate instances keyed by an explicit
  flag (slot offsets never reach 0x800, the old test routed every PFS
  access into the PORT arms - writes dropped, reads aliased, and a
  PORT read re-borrowed its own cell);
- NEXT: WiFi un-park assessment (waiting on S3 code). LED matrix
  (WiFi board) renders from RA GPIO pins, not as a peripheral.
- analogWrite (`analogWrite(6,64)` GPT0 GTPR97958 GTCCRB~24.4k via GTCCRD
  buffer + GTBER transfer, OBE) + tone (`tone(LED,440)` GPT4 PERIODIC
  toggle) + SoftwareSerial (9600 baud 0xA5 loopback D3 P104 TX -> D2
  P105 RX IRQ0 via GPT4/GPT5 timers + DMAC0/DMAC1 PCNTR samples + ELC
  GPT_A link + soft_wire jig) are all proven end-to-end on real Arduino
  API with `r4aw`/`r4tone`/`r4sser` bins + `core/blinky/sketches/` sources
  + demo tabs (25 tabs total).
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
`ra4m1_usb_suspend_resume_ok`, `ra4m1_map_usb_msc_mock` (NEW: BOT+SCSI
INQUIRY/READ_CAPACITY/READ10/WRITE10 mock chip over bulk pipes),
`ra4m1_map_sd_card`, `ra4m1_sd_ok`,
`ra4m1_map_can_errors`, `ra4m1_can_error_ok`, `ra4m1_map_dtc_repeat`,
`ra4m1_dtc_ok`, `ra4m1_analogwave_ok` (NEW: real `analogWave.sine(10)`
GPT->DTC->DAC12 sine proof, `r4aws.bin`), `ra4m1_pwm_ok`, `ra4m1_analogwrite_ok` (NEW: analogWrite(6,64) GPT0 GTPR97958 GTCCRB~24.5k via BER, OBE), `ra4m1_tone_ok` (NEW: tone(LED,440) GPT4 PERIODIC toggle), `ra4m1_map_can1_loopback`,
`ra4m1_can1_ok`, `ra4m1_map_dac8`, `ra4m1_dac8_ok`, `ra4m1_map_tsn`,
`ra4m1_map_slcdc`, `ra4m1_map_kint`, `ra4m1_kint_ok`,
`ra4m1_map_ssi`, `ra4m1_ssi_ok`, `ra4m1_matrix_ok`, `ra4m1_analog_ok`,
`ra4m1_mtx_ok`, `ra4m1_softserial_ok`,
`ra4m1_rtc_alarm_ok`, `ra4m1_map_can_busoff_recovery`, `ra4m1_can_busoff_ok`,
`ra4m1_ek_ra4m1_zero_boot_p106_led` (EK target: same silicon, zero-boot, P106).
`ra4m1_map_ssi1`, `ra4m1_map_gpt_protect_dma`.
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
(CAN errors), `core/blinky/r4dtc.bin` (GPT->DAC via DTC), `core/blinky/r4aws.bin`
(AnalogWave sine via the real Arduino `analogWave` library),
`core/blinky/r4pwm.bin` (GPT0 25% PWM on D6), `core/blinky/r4can1.bin`
(CAN1 self-test), `core/blinky/r4dac8.bin` (DAC8),
`core/blinky/r4kint.bin` (KINT key), `core/blinky/r4ssi.bin`
(SSI audio FIFO), `core/blinky/r4matrix.bin` (LED matrix smiley),
`core/blinky/r4analog.bin` (analog R/W), `core/blinky/r4mtx.bin`
(matrix heart, WiFi fqbn),
`core/blinky/r4aw.bin` (analogWrite), `core/blinky/r4tone.bin` (tone),
`core/blinky/r4sser.bin` (SoftwareSerial), `core/blinky/r4rtcalm.bin`
(RTC alarm), `core/blinky/r4canbo.bin` (CAN bus-off), `core/blinky/r4rtc.bin`
(RTC), `core/blinky/r4wdtref.bin`/`r4wdtexp.bin` (WDT refresh/expiry),
`core/blinky/r4can.bin` (CAN self-test), `core/blinky/r4opamp.bin` (OPAMP),
`core/blinky/r4irq.bin` (attachInterrupt), `core/blinky/r4serial1.bin`
(Serial1 echo) are the vendored
USB/I2C/SPI/CAN/touch/HID/SD/DTC/PWM/RTC/WDT proof builds, compiled the same
way from their sketches (`core/blinky/sketches/` holds the r4aw/r4tone/
r4sser sources).

## 8. Browser demo (`demo/`)

`./demo/build.sh` then `python3 -m http.server -d demo 8901`: three-page
site (`index.html` demo + `docs.html` + `about.html`) driving the 288KB Minima
WASM (`WasmCpu` + the `usb_*` / `periph_*` free functions + `spi_set_sd_card`/
`sd_read_block` for the SD tab + `matrix_trace_take` for the Matrix
heart runner + `adc_set_channel_value` for the Analog slider). Twenty-six tabs run the vendored firmware
live: Blink (LED + full
12x16 GPIO grid from PORT, plus a MIPS meter in the stats), Serial (in-page virtual-host enumeration
with step checklist, hello in the terminal, suspend/resume tail), Echo (bulk-pipe discovery
via PIPECFG + typed round-trip), Wire (IIC1 master vs bare-metal IIC0
slave flag trace), SPI (SPI0 master vs SPI1 slave flag trace), SD (init
+ MBR dump + block-1 recheck via export), CAN (RX-FIFO MB24 trace),
  EEPROM (live dataflash byte trace), PWM (live duty readout on D6),
  RTC (live BCD clock + rollover), CTSU (live SC/RC counters),
  HID (report descriptor + INT-IN 'a' report), analogWrite (live
  GTPR/GTCCRB/GTIOR), AnalogWave (live DAC DADR sine sample),
  tone (live D13 toggle), SoftSerial (0xA5
  loopback via soft wire + UART peek), WDT (refresh-holds + expiry-latches),
  Signal (IRQ + Serial1 + OPAMP + ADC), Engine (CAN/CANerr/DTC/Wire/SPI/susp),
  Matrix (12x8 charlieplex GPIO render), Docs (short pointer to `docs.html`).
  Flag traces poll MMIO only - data
registers (ICDRR/SPDR) are never read (a read would eat the firmware's
byte); sub-word reads return unmasked packs, so JS masks (`% 256`).
D13 = P111 (PORT1 bit 11), not bit 13. `demo/pkg/` + `demo/fw/` are
generated (git-ignored); only `index.html`/`docs.html`/`about.html`/
`app.js`/`styles.css`/`build.sh` are tracked. Verified with Playwright
screenshots (every
tab to green verdict), console clean. Deploys to GitHub Pages via
`.github/workflows/pages.yml` (rebuilds pkg+fw, publishes `demo/`).
