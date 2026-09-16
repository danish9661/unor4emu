# UNO R4 Minima emulator — audit

Date: 2026-09-11. Scope: `uno r4/` only (Minima board, RA4M1 `R7FA4M1AB3CFM`).
Method: full peripheral census from `R7FA4M1AB.h` (arduino:renesas_uno
1.6.0) vs `Peripherals::new_ra4m1()`; Arduino-core usage check (which
channels the core actually drives: pinmux.inc, pins_arduino.h, library
sources); proof inventory (`cargo test -- --test-threads=1` in both
cores); snapshot-vs-small-core diff; live demo verification via
Playwright screenshots with a clean console.

## Verdict

No — not everything is implemented. But nothing known is broken: both
suites are green (120 + 83, single-threaded), the demo runs 26 tabs live, and every gap below is enumerated with its
Arduino relevance. The pattern is consistent: everything the Arduino
core drives on real Minima sketches is modeled and proven; the missing
pieces are either unused by Arduino or parked platform work (WiFi).

## Test inventory (all green, `--test-threads=1`)

- Snapshot `core/ra4m1-periph-wasm`: **120** = 37 legacy CPU/system
  tests + 83 R4 proofs in `src/ra4m1.rs` (boot, EK zero-boot/P106, clock stub, SCI TX,
  GPT, PORT, MMIO blinky, ADC, DAC, RTC, RTC alarm, DMAC, DTC repeat
  + GPT->DAC firmware + AnalogWave sine firmware, ELC, AGT, CRC/DOC, SCI echo, OPAMP/ACMP,
  dataflash program/erase, OPAMP firmware, CTSU, CTSU mutual +
  firmware, CAN loopback, CAN errors + isError firmware, I2C EEPROM,
  I2C slave, SPI slave + firmware, Wire slave firmware, CAN FIFO +
  firmware, SD card + firmware, PWM + firmware, analogWrite + tone +
  SoftwareSerial firmware, SCI-SPI loopback, SPI loopback, USB
  TX capture, CDC enumerate, Serial hello, USB suspend/resume, HID
  keyboard, USB suspend/resume firmware, MSC mock chip, CDC echo, Wire ok, SPI ok,
  CAN ok, ITE flags, ICU pin-IRQ, EEPROM firmware, attachInterrupt,
  Serial1 echo, extra channels, RTC firmware, WDT refresh/expire,
  CAN1 + DAC8 + TSN + SLCDC + KINT + SSI + matrix proofs, analog + matrix-heart
  firmware proofs, RTC alarm +
  bus-off recovery firmware, Blink boots, Blink toggles).
- Small core `core/ra4m1-core`: **83** proofs (exact one-for-one mirror
  of the snapshot's R4 proofs, incl. ITE flags, USB host-flow helpers,
  HID/suspend firmware proofs, MSC mock-chip proof, DTC repeat, AnalogWave sine firmware,
  CAN errors + bus-off recovery, analog + matrix-heart firmware proofs,
  RTC alarm firmware, EK-RA4M1 zero-boot; runs 3-4x faster
  than the snapshot suite).
- WASM: 288KB release (`uno_r4_minima_wasm_bg.wasm`), no STM32/ESP32.
- Demo `demo/`: Blink LED + GPIO grid, Serial enumerate + hello,
  Echo round-trip, Wire master/slave trace, SPI master/slave trace,
  SD init + MBR dump, CAN FIFO trace, EEPROM byte trace, PWM duty,
  analogWrite regs, tone toggle, SoftSerial 0xA5 loopback, RTC alarm,
  CAN bus-off recovery, CAN1/DAC8/KINT/SSI quartet, Signal (IRQ+Serial1+OPAMP+ADC),
  Engine (CAN/CANerr/DTC/Wire/SPI/susp), EK-RA4M1 zero-boot, Analog slider,
  Matrix heart — all screenshot-verified, console clean. Deploys to Pages via workflow.
- Core parity: snapshot vs small-core models differ ONLY in env-gated
  debug logs (`ICULOG`/`EVLOG`/`DMAEVLOG` in both, gated off by default)
  plus snapshot's legacy CPU tests. R4 proofs are one-for-one identical
  (82/82 + EK boot). No behavioral drift.
- Upstream CPU reports in `Documents/stm32 F4/cpu_bug.md`: §11
  (exception live-r13, both cores fixed), §12 (predicated ADD/SUB-imm
  flags, both cores fixed), §14 (MOV-reg T1 flagless, both cores fixed),
  §15 (predicated T1 shifts/ALU + always-set test ops, both cores fixed).
  Plus the earlier MRS-PSR IPSR fix.

## Peripheral coverage vs the real chip

Bases from `R7FA4M1AB.h`. "Arduino use" = what ArduinoCore-renesas
1.6.0 actually drives on Minima.

### Done (modeled + proven, firmware where it matters)

| Block | Base | Model | Proof |
|---|---|---|---|
| PORT0-14 | `0x4004_0000`+n*`0x20` | `ra_port.rs` real PCNTR layout (PDR low + PODR high, settled vs FSP disasm), PFS-synced PODR/PDR, soft_wire TX->RX jig + input-inject hook + matrix-trace jig | port retained, blink toggles, softserial loopback, demo grid, matrix-heart trace |
| PFS | `0x4004_0800` | retain map (separate instance, explicit flag - slot offsets never reach 0x800) | via PORT proofs + PWM routing |
| SYSC/MSTP | `0x4001_E000`/`0x4004_6FFC` | accept-and-retain stubs | boot proofs |
| ICU | `0x4000_6000` | IELSR routing mirror | ELC/USB/CAN/CTSU IRQ proofs |
| ELC | `0x4004_1000` | link table + soft trigger (GPT/ADC dispatch) | elc routes event |
| SCI0-9 | `0x4007_0000`+ch*`0x20` | `ra_sci.rs` UART byte-exact + simple-SPI mode (ch4-9 eventless polled) | TX console, echo path, SPI loopback, Serial1 proof (SCI2) |
| GPT0-13 | `0x4007_8000`+ch*`0x100` | `ra_gpt.rs` real offsets (GTCR+0x2C GTIOR+0x34 GTCNT+0x48 GTCCR+0x4C GTPR+0x64), wrap/compare/overflow events, exact-time multi-wrap walk with due-stamped DMAC units, GTIOA/B function-0 PWM latches routed to PORT via PFS pinmux table, GTBER C/D->A/B buffering | counts + matches, PWM + firmware (25% duty on D6), analogWrite/tone/SoftSerial firmware |
| AGT0-5 | `0x4008_4000`+ch*`0x100` | `ra_misc.rs` down-counter, latched reload, TUNDF (ch2-5 eventless) | counts, millis IRQs |
| ADC0/1 | `0x4005_C000`/`0x4005_C200` | `ra_analog.rs` ADST=bit15, overrides | converts channel + Arduino analog firmware proof (A0=ch9, Analog tab slider) |
| DAC12 | `0x4005_E000` | retained output | dac retained + Arduino analog firmware proof (v>>2 into DADR0) |
| RTC | `0x4004_4000` | time (BCD) + alarm IRQ (event 38) | ticks, alarm event, RTC lib firmware proof (minute rollover) |
| DMAC0-7/DTC | `0x4000_5000`/`0x4000_5400` | memcopy path (ch4-7 eventless) + event-driven per-unit engine (DMSAR/DMDAR/DMCRA/DMTMD SZ/DMAMD/DMINT/DMCNT/DTE, DELSR links, due-bucketed FIFO, outstanding-tracked completion) + DTC repeat engine (IELSR.DTCE, SRAM vector table, serviced in mem path; repeat/block length = CRAL low byte since R_DTC doubles length into CRAL+CRAH) | mem-to-mem, SoftSerial PCNTR sampling firmware, DTC repeat + GPT->DAC firmware proof + AnalogWave sine firmware proof |
| WDT/IWDT | `0x4004_4200`/`0x4004_4400` | countdown + reset flags, TOPS period | WDT lib refresh + expiry proofs |
| CRC/DOC | `0x4007_4000`/`0x4005_4100` | IEEE-802.3 / compare | crc_and_doc |
| OPAMP/ACMP | `0x4008_6000`/`0x4008_5E00` | follower loopback + compare | opamp proof + OPAMP lib firmware proof (AMPMON0) |
| USBFS device | `0x4009_0000` | `ra_usb.rs` FIFOs, BEMP/BRDY/CTRT/DVST, CCPL strobe, byte FIFO reads, DVSQ SUSPx, RESM, WKUP | enumerate, hello, echo (100B), HID keyboard, suspend/resume, MSC mock chip, TX capture |
| CTSU | `0x4008_1000` | STRT->tick counters + END event, self + mutual (MD=2) pair defaults + overrides | measures + overflow, mutual + firmware |
| CAN0 | `0x4005_0000` | mailboxes + self-test loopback (MSSR search + TSRC strobe), RX/TX FIFO depth-4 via MB24, error counters (RECR/TECR/EIFR/ERI event 74) | loopback proof + Arduino_CAN firmware proof, FIFO + firmware, errors + isError firmware proof |
| IIC0-2 | `0x4005_3000`+ch*`0x100` | master + virtual EEPROM @0x50 + shared-bus slave (SAR/AAS/RXI/TXI/STOP, IIC2 eventless) | eeprom proof + Wire firmware proof, slave + Wire-master/bare-slave firmware proof |
| SPI0/1 | `0x4007_2000`/`0x4007_2100` | RSPI master (polled SPRF + loopback) + slave (MSTR=0 staging + exchange) + virtual SD card | loopback proof + SPI firmware proof (ch1), slave + dual-channel firmware proof, SD + firmware proof |
| Dataflash | `0x4010_0000` | 8KB window (erased 0xFF, bit-clear writes) | program/erase proof + EEPROM firmware proof |
| FACI_LP | `0x407E_C000` | program/erase/blankcheck engine (FSAR bias, FRDY busy latch, BCERR0) + FENTRYR | R_FLASH_LP driver proven via EEPROM |
| ICU ext-IRQ | `0x4000_6000` | IRQCR sense + `icu_pin_edge` injection, IELSR routing | pin-IRQ + attachInterrupt proofs (all 16 lines) |
| KINT | `0x4008_0000` | KRCTL/KRF/KRM + `kint_key_press` jig into KEY_INT event 69 | key flag + firmware proof |
| SLCDC | `0x4008_2000` | mode regs + 64B display RAM retain (no panel) | regs + display RAM proof |
| SSI0/SSI1 | `0x4004_E000`/`0x4004_E100` | TX drain + RX pattern FIFO + TXI/RXI edges (SSI1: same type, own slot/FIFOs; FSP mask = SSI0 only) | SSI0 flags + bare-metal firmware proof (Arduino I2S lib broken), SSI1 register proof |
| GPT OPS / POEG0-3 | `0x4007_8FF0` / `0x4004_2000`+n*`0x100` | safety-shutdown stubs (accept-and-retain, outputs never gated; no Arduino consumer) | register proof |
| R_DMA controller | `0x4000_5200` | module-activation stub (DMAST/DMECHR retain; engine stays in DMAC/DTC) | register proof |
| CAN1 | `0x4005_1000` | same mailboxes as CAN0, eventless (polled SENTDATA/NEWDATA) | self-test loopback + bare-metal firmware proof |
| DAC8 | `0x4009_E000` | DACS retain + DAM enable gate | retain + bare-metal firmware proof |
| TSN cal | `0x407E_C228` | fixed factory-trim constants (synthetic) | calibration read proof |
| LED matrix | GPIO P0/P2 | 12x8 charlieplex render from the real 11-pin set | smiley proof + Arduino-lib heart proof (trace reconstruction, WiFi-fqbn bin on same silicon) + demo tab |
| ARM | `0xE000_xxxx` | NVIC/SysTick/SCB/MPU/FPU/DWT/STIR/ITM reused | legacy CPU suite |

Channel notes (verified, not assumed): Arduino PWM uses GPT0-7 only
(`GPT_HOWMANY=8`); Serial1 (D0/D1 = P302/P301) is SCI2; Wire is IIC1
(events 58-61 routed, proven live); SPI D11-13 probes to SPI1 ch1;
`MRS PSR` carries live IPSR.

### Partial (works for the proven path, documented limits)

- **DMAC**: ch0-7 mapped (ch4-7 eventless). Only memcopy proven.
- **DTC**: repeat engine proven (timer-overflow -> DAC via FSP R_DTC, GPT->DAC firmware proof + real-Arduino AnalogWave sine firmware proof `r4aws.bin`). NORMAL mode, BLOCK/chain, OFFSET addr mode unmodeled (no consumer). Repeat/block length reads CRAL (R_DTC doubles length into CRAL+CRAH, so live 24 reads 0x1818). Serviced in the CPU run loop while staged (atomic-guarded, free when idle); register tests drain via `mem.service_sync_dma()`.
- **CTSU**: self + mutual (MD=2) proven incl. firmware; DTC transfer requests unmodeled (polling works).
- **CAN0**: mailbox + FIFO modes proven incl. firmware; error counting (RECR/TECR saturating, EWF/EPF/BOEF, ERI event 74) proven incl. Arduino_CAN isError firmware proof. Search regs are retain-only.
- **IIC**: master + slave proven incl. firmware (shared-bus fabric); 10-bit/general-call unmodeled.
- **SCI-SPI / RSPI**: master + RSPI slave proven incl. firmware; RSPI assumes 8-bit frames.
- **USBFS**: device with CDC + HID keyboard + suspend/resume proven incl. firmware; MSC transport proven via mock chip (BOT+SCSI over bulk pipes, no audio classes).
- **CAN1**: DONE — polled self-test loopback proven (`ra4m1_map_can1_loopback` asserts routed CAN0 events stay quiet while CAN1's SENTDATA/NEWDATA still set — the mock consumer is the CAN0-routed IRQ table itself), plus the bare-metal firmware proof. No Arduino CAN1 on Minima: `CAN_HOWMANY=1`, and `bsp_elc.h` carries events 74-78 for CAN0 only.
- **DAC8** (`0x4009_E000`): DONE — DACS retain + DAM enable gate, bare-metal firmware proof (no Arduino consumer: `DAC8_HOWMANY=0`).
- **GPIO input**: works via injection hook (`set_input`, like `uart_rx_byte`); pin levels feed `icu_pin_edge` for external IRQs.
- **Clocks**: no tree modeling — fixed 48MHz/PCLKB assumptions baked into dividers (AGT /8). `SystemInit` writes are accepted, never interpreted.
- **SCB VTOR**: keeps bits[31:7] like silicon (was a 1KB mask). Arduino parks its RAM vector table at `0x20007F00`; the old mask mangled it to `0x20007C00` so every IRQ vectored into stack garbage (Blink survived by luck, FAT-format died at the first AGT tick). Fixed in both cores.

### Missing (deliberate or future)

- **External pin interrupts** (`attachInterrupt`, ICU IRQCR/NMI): DONE — `icu_pin_edge` injection, all 16 lines proven with a button sketch.
- **EEPROM / dataflash programming** (FACI): DONE — dataflash window + FACI_LP engine, Arduino EEPROM round-trip proven.
- **I2S audio / SSI** (`0x4004E000`): SSI layer DONE (bare-metal) — TX drain + RX pattern FIFO + TXI/RXI edges, firmware proof on exact 0,1,2,3 read-back. The audio CLASS on top is unbuildable and therefore closed, not missing: the in-tree Arduino I2S lib fails to compile on 1.6.0 with `r_i2s_api.h: No such file or directory` (reproduced with arduino-cli against the stock `I2S.h`), and the underlying `r_ssi.h` FSP driver is absent from the bundle too. No consumer can reach the SSI model until Arduino ships the headers — nothing further to model.
- **USB MSC**: NO Arduino consumer (`CFG_TUD_MSC=0`, no in-tree MSC library). Transport proven via mock chip instead (`ra4m1_map_usb_msc_mock` in both cores: BOT CBW/CSW + SCSI INQUIRY/READ_CAPACITY/READ10/WRITE10 over bulk pipes), so a future MSC stack has a proven path.
- **USB audio class**: NO stack support by config — Minima's `tusb_config.h` leaves `CFG_TUD_AUDIO` at its default 0 (only CDC/HID/DFU on), so an audio SET_INTERFACE is never claimed. Closed in the MSC mock tail: no CTRT latches, bulk pipes stay BOT-claimed. No model work possible until the TinyUSB config enables the class.
- **On-chip FAT, closed by silicon-truth** (no SDHI peripheral on RA4M1: zero `SDMMC`/`SDHI` symbols in `R7FA4M1AB.h`, so no 4-bit SD bus/ADMA exists to host it): (a) dataflash FAT — Arduino's `FATFileSystem.cpp` asserts `sector_count >= 64` and `pins_arduino.h` gives Minima `FLASH_TOTAL_SIZE 0x2000 / FLASH_BLOCK_SIZE 0x400` = only 8 blocks, so the assert fires by construction; (b) LittleFS — the same 8x1KB-block geometry leaves no room for metadata+lookahead (`LittleFileSystem.cpp` sizes `block_count` from `size()/block_size`), and its block-range assert fires the same way. The dataflash consumer that does exist (Arduino EEPROM via virtualEEPROM) is proven. Real FAT-over-SD would need an SDHI that the silicon does not have — closed, not parked.
- **POEG/OPS/R_DMA/R_SLCDC/R_TSN stubs, closed by driver-truth with mock consumers**: accept-and-retain registers (no Arduino consumer opens them — `FspTimer.cpp` only writes `poeg_link`/`output_disable` fields inside the GPT PWM cfg, `R_GPT_POEG_Open` is never called; `R_DMA`/`R_SLCDC`/`R_TSN` have zero refs in core + drivers; `r_dmac` never touches `DMAST`). `ra4m1_map_gpt_protect_dma` now proves an SSF-strobe halts a running GPT latch-freeze and a DMAC memcopy still ships under `DMST=1`; `ra4m1_map_slcdc` proves an LCDON + digit-5 mock-panel scan; `ra4m1_map_tsn` proves an ADC temp-path sample against the trim. No firmware flows are possible until a driver opens these blocks.
- **AnalogWave wrapper** (`analogWave.sine(10)`, real Arduino lib): DONE — GPT->DTC->DAC12 sine firmware proof (`r4aws.bin`, `ra4m1_analogwave_ok` in both cores, DADR shows a non-zero sample). Needed the MOV-reg / predicated-T1 / CRAL fixes below.
- **USB HID endpoints**: DONE — PluggableUSB keyboard (report descriptor + INT-IN report) proven.
- **USB suspend/resume**: DONE — DVSQ SUSPx + RESM + WKUP, weak-callback firmware proof.
- **SD storage**: DONE — virtual SD slave in SPI mode (CMD0/8/55/41/58/17/24, MBR + write/read-back) proven. FATFilesystem over it untested.
- **TSN / SLCDC / KINT**: DONE — TSN calibration constants (synthetic, documented) with an ADC temp-path mock-consumer scan (sample lands in ADDR, trim stays read-only); SLCDC mode regs + 64B display RAM retain with an LCDON + "5"-digit mock-panel scan; KINT KRCTL/KRF/KRM + `kint_key_press` jig into KEY_INT event 69 with firmware proof. No Arduino consumers for any of them (no `R_TSN`/`R_SLCDC` refs anywhere in the core or drivers outside `R7FA4M1AB.h` itself).
- **System/debug**: BUS, CAC, DEBUG, FCACHE, SRAM (ECC/parity cfg), PMISC. Unmapped on purpose; Arduino boot touches none of them (Blink boots clean). Any access faults precisely like unmapped silicon.
- **PCNTR1 halves, settled by FSP disassembly** (`R_IOPORT_PinWrite` writes the HIGH halfword for SET and `(mask<<16)` for CLEAR, both to PCNTR3+8; `turnLed` tristates with a 16-bit PCNTR1 mask): the word is PDR-low + PODR-high. Level readers use the high half; stale levels on tristated rails never decode (this ghosted the first heart attempt). On-windows of the Arduino-lib matrix are ~15 instructions, so the heart proof reconstructs from a GPIO write-trace jig, not chunk sampling.
- **WiFi** (`wasm-wifi/`, ESP32-S3): parked by design, folders stay on disk excluded from the workspace. **LED matrix**: DONE — 12x8 charlieplex GPIO render from the real 11-pin set (smiley bare-metal proof + Arduino-lib heart proof via the real `Arduino_LED_Matrix` lib, WiFi-fqbn `r4mtx.bin` on the same RA4M1 silicon + demo tab with both runners).
- **Analog path** (Arduino API): DONE — `analogRead(A0)` (ADC ch9, 14-bit) → `analogWrite(DAC, v>>2)` (DAC12, 12-bit) + P111 LED threshold, proven end-to-end (`r4analog.bin`, `ra4m1_analog_ok` in both cores) with a live-slider Analog demo tab driving the same ADC override jig.

## Known quirks (all covered by proofs, listed so nobody "fixes" them)

- Tests must run `--test-threads=1` (shared `SYS`, instruction count, UART buffer).
- `rx_inject` raises BRDY like HW; CCPL is a strobe; CFIFO OUT drains are single bytes.
- RIIC: ST auto-sets MST/TRS; TDRE+TXI fire on START; enables fire latched flags; first RXI read is a dummy (no byte0 preload); restart-write has no completion event (FSP quirk, tolerated).
- Virtual-time assumptions: AGT/RTC/WDT run on instruction count (WDT FSP default is TOPS=3 = 4096 ticks); ADC converts instantly; CAN loopback needs self-test mode; I2C slave jig lives at 0x50; SPI loopback is a test jig (open bus reads 0xFF); SD CRCs are accepted unchecked and CS is always-selected; the RTC alarm is evaluated against the current second on each tick, so tests step second-by-second; USB HID reports complete on FIFO flush (no IN-token modeling); DTC transfers complete in the CPU run loop while staged (peripheral ticks own no RAM).
- FSP passes raw EIFR bits as the CAN error event (`ERR_WARNING=2`, `ERR_PASSIVE=4`): inject one threshold per episode like silicon, or Arduino's exact-match switch sees nothing.
- SSI TDE means TX FIFO empty (like SCI TDRE); the RX pattern counter resets on RFRST; CAN1/DAC8/TSN/SLCDC/KINT/SSI/matrix have no Arduino consumers on Minima, so their proofs are register-level + bare-metal firmware with mock consumers driving the stub surface where one exists (no Arduino-lib flows).
- GPT compares are crossing-detected (coarse virtual ticks jump over exact equalities); unprogrammed all-ones compares are bounded by the period so they never false-fire on wrap.
- PORT and PFS are separate peripheral instances keyed by an explicit flag; PWM pins follow GPT latches only when PFS PSEL selects a GPT function.
- Vendored firmware (`core/blinky/r4*.bin`) is built with arduino-cli 1.6.0 and committed; `demo/pkg` + `demo/fw` are generated (git-ignored, `./demo/build.sh` rebuilds).

## What's next (priority order)

1. **Platform**: WiFi un-park (waiting on S3 code). Demo ships Blink/Serial/Echo/Wire/SPI/SD/CAN/EEPROM/PWM/analogWrite/AnalogWave/Analog/tone/SoftSerial/RTC/CTSU/HID/WDT/Signal/Engine/Matrix/Docs tabs + MIPS meter + a Pages deploy workflow.
2. **New models**: nothing with a buildable Arduino consumer remains. Every no-consumer stub is closed above with its driver-truth reason + mock-consumer proof (OPS/POEG/R_DMA, TSN, SLCDC, USB-audio config, I2S headers, on-chip FAT asserts, CAN1 events, SDHI absence).
