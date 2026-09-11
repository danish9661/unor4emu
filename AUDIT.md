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
suites are green (157 + 25, single-threaded), the demo runs three real
firmware images live, and every gap below is enumerated with its
Arduino relevance. The pattern is consistent: everything the Arduino
core drives on real Minima sketches is modeled and proven; the missing
pieces are either unused by Arduino, untestable without extra hardware
(slaves), or parked platform work (WiFi).

## Test inventory (all green, `--test-threads=1`)

- Snapshot `core/ra4m1-periph-wasm`: **157** = 128 legacy CPU decoder
  tests + 29 R4 proofs in `src/ra4m1.rs` (boot, clock stub, SCI TX,
  GPT, PORT, MMIO blinky, ADC, DAC, RTC, DMAC, ELC, AGT, CRC/DOC,
  SCI echo, OPAMP/ACMP, CTSU, CAN loopback, I2C EEPROM, SCI-SPI
  loopback, USB TX capture, CDC enumerate, Serial hello, CDC echo,
  Wire ok, ITE flags, SPI loopback, SPI ok, Blink boots, Blink
  toggles).
- Small core `core/ra4m1-core`: **25** register-level proofs (mirror,
  incl. ITE flags). No firmware-USB tests there by design.
- WASM: 228KB release (`uno_r4_minima_wasm_bg.wasm`), no STM32/ESP32.
- Demo `demo/`: Blink LED + GPIO grid, Serial enumerate + hello,
  Echo round-trip — all screenshot-verified, console clean.
- Core parity: snapshot vs small-core models differ ONLY in env-gated
  debug logs (`ICULOG`/`SELLOG`/`EXCLOG` in snapshot) plus snapshot's
  legacy CPU tests. No behavioral drift.
- Upstream CPU reports in `Documents/stm32 F4/cpu_bug.md`: §11
  (exception live-r13, both cores fixed), §12 (predicated ADD/SUB-imm
  flags, both cores fixed). Plus the earlier MRS-PSR IPSR fix.

## Peripheral coverage vs the real chip

Bases from `R7FA4M1AB.h`. "Arduino use" = what ArduinoCore-renesas
1.6.0 actually drives on Minima.

### Done (modeled + proven, firmware where it matters)

| Block | Base | Model | Proof |
|---|---|---|---|
| PORT0-14 | `0x4004_0000`+n*`0x20` | `ra_port.rs` real PCNTR layout, input-inject hook | port retained, blink toggles, demo grid |
| PFS | `0x4004_0800` | retain map | via PORT proofs |
| SYSC/MSTP | `0x4001_E000`/`0x4004_6FFC` | accept-and-retain stubs | boot proofs |
| ICU | `0x4000_6000` | IELSR routing mirror | ELC/USB/CAN/CTSU IRQ proofs |
| ELC | `0x4004_1000` | link table + soft trigger (GPT/ADC dispatch) | elc routes event |
| SCI0,1,2,9 | `0x4007_0000`+ch*`0x20` | `ra_sci.rs` UART byte-exact + simple-SPI mode | TX console, echo path, SPI loopback |
| GPT0-7 | `0x4007_8000`+ch*`0x100` | `ra_gpt.rs` instruction-count driven | counts + matches |
| AGT0-1 | `0x4008_4000`+ch*`0x100` | `ra_misc.rs` down-counter, latched reload, TUNDF | counts, millis IRQs |
| ADC0/1 | `0x4005_C000`/`0x4005_C200` | `ra_analog.rs` ADST=bit15, overrides | converts channel |
| DAC12 | `0x4005_E000` | retained output | dac retained |
| RTC | `0x4004_4000` | time + alarm IRQ (event 38) | ticks seconds |
| DMAC0-3/DTC | `0x4000_5000`/`0x4000_5400` | memcopy path | mem-to-mem |
| WDT/IWDT | `0x4004_4200`/`0x4004_4400` | countdown + reset flags | (no dedicated proof) |
| CRC/DOC | `0x4007_4000`/`0x4005_4100` | IEEE-802.3 / compare | crc_and_doc |
| OPAMP/ACMP | `0x4008_6000`/`0x4008_5E00` | follower loopback + compare | opamp proof |
| USBFS device | `0x4009_0000` | `ra_usb.rs` FIFOs, BEMP/BRDY/CTRT/DVST, CCPL strobe, byte FIFO reads | enumerate, hello, echo (100B), TX capture |
| CTSU | `0x4008_1000` | STRT->tick counters + END event | measures + overflow |
| CAN0 | `0x4005_0000` | mailboxes + self-test loopback | loopback proof |
| IIC0/1 | `0x4005_3000`/`0x4005_3100` | master + virtual EEPROM @0x50 | eeprom proof + Wire firmware proof |
| SPI0/1 | `0x4007_2000`/`0x4007_2100` | RSPI master, polled SPRF + loopback | loopback proof + SPI firmware proof (ch1) |
| ARM | `0xE000_xxxx` | NVIC/SysTick/SCB/MPU/FPU/DWT/STIR/ITM reused | legacy CPU suite |

Channel notes (verified, not assumed): Arduino PWM uses GPT0-7 only
(`GPT_HOWMANY=8`); Serial1 (D0/D1 = P302/P301) is SCI2; Wire is IIC1
(events 58-61 routed, proven live); SPI D11-13 probes to SPI1 ch1;
`MRS PSR` carries live IPSR.

### Partial (works for the proven path, documented limits)

- **DMAC**: ch0-3 only (HW has ch0-7); DTC is a stub. Only memcopy proven.
- **CTSU**: self-capacitance proven; mutual mode untested (no Arduino consumer).
- **CAN0**: mailbox mode proven; FIFO mode, error counting, search regs are retain-only.
- **IIC**: master proven incl. firmware; slave mode not modeled (SARs retain-only).
- **SCI-SPI / RSPI**: master proven; slave mode not modeled; RSPI assumes 8-bit frames.
- **USBFS**: device only; no suspend/resume, no HID endpoints.
- **SCI3-8**: unmapped. No Arduino Minima consumer (Serial=USB, Serial1=SCI2).
- **GPT8-13 / AGT2-5**: unmapped. Same stride as modeled channels — trivial to extend, no Arduino consumer found.
- **IIC2**: unmapped (`TWOWIRE` uses IIC0-1 in practice; same stride, trivial).
- **CAN1**: unmapped — no routable mailbox events on this part.
- **DAC8** (`0x4009_E000`): unmapped. Arduino AnalogWave uses DAC12.
- **WDT/IWDT, RTC**: modeled, no dedicated firmware proof (RTC/WDT Arduino libs exist).
- **OPAMP/ACMP**: register proofs only (Arduino OPAMP lib exists, no firmware proof yet).
- **GPIO input**: works via injection hook (`set_input`, like `uart_rx_byte`); pin-change interrupts do NOT exist (see missing).
- **Clocks**: no tree modeling — fixed 48MHz/PCLKB assumptions baked into dividers (AGT /8). `SystemInit` writes are accepted, never interpreted.

### Missing (deliberate or future)

- **External pin interrupts** (`attachInterrupt`, ICU IRQCR/NMI): the biggest Arduino-visible gap. IELSR routes peripheral events only; no pin-edge injection exists. Fix shape is clear (`icu_raise_pin` + PFS config), needs a proof sketch with a button press.
- **EEPROM / dataflash programming** (FACI): Arduino EEPROM lib has no backing. Needs a dataflash model.
- **I2S audio** (SSI0/1 `0x4004_E000`): Arduino I2S lib exists; SSI unmodeled.
- **USB HID endpoints**: CDC-only USB. HID needs report endpoints + a proof sketch.
- **SD storage** (BlockDevices/FAT over SPI): needs a virtual SD slave (same jig pattern as the EEPROM).
- **System/debug**: BUS, CAC, DEBUG, FCACHE, SRAM (ECC/parity cfg), PMISC, TSN, SLCDC (no LCD on Minima), KINT. Unmapped on purpose; Arduino boot touches none of them (Blink boots clean). Any access faults precisely like unmapped silicon.
- **WiFi** (`wasm-wifi/`, ESP32-S3) and **LED matrix**: parked by design, folders stay on disk excluded from the workspace.

## Known quirks (all covered by proofs, listed so nobody "fixes" them)

- Tests must run `--test-threads=1` (shared `SYS`, instruction count, UART buffer).
- `rx_inject` raises BRDY like HW; CCPL is a strobe; CFIFO OUT drains are single bytes.
- RIIC: ST auto-sets MST/TRS; TDRE+TXI fire on START; enables fire latched flags; first RXI read is a dummy (no byte0 preload); restart-write has no completion event (FSP quirk, tolerated).
- Virtual-time assumptions: AGT/RTC run on instruction count; ADC converts instantly; CAN loopback needs self-test mode; I2C slave jig lives at 0x50; SPI loopback is a test jig (open bus reads 0xFF).
- Vendored firmware (`core/blinky/r4*.bin`) is built with arduino-cli 1.6.0 and committed; `demo/pkg` + `demo/fw` are generated (git-ignored, `./demo/build.sh` rebuilds).

## What's next (priority order)

1. **attachInterrupt** (pin IRQs) — biggest real-sketch gap; model + button-press proof.
2. **Serial1 proof** (SCI2 TX via console + RX inject) — small, closes UART.
3. **Firmware proofs for existing models**: CAN (Arduino_CAN loopback?), OPAMP lib, RTC lib, WDT lib — all register-proven already, each is one sketch + one test.
4. **Completeness loops** (all trivial, same-stride): AGT2-5, GPT8-13, SCI4-8, IIC2, DMAC4-7.
5. **New models**: EEPROM/dataflash (FACI), I2C slave mode, SPI slave mode, CAN FIFO mode, CTSU mutual mode, USB HID endpoints, virtual SD slave.
6. **Platform**: browser demo extensions (Wire/SPI tabs), WiFi un-park (ESP32-S3), LED-matrix-from-GPIO.
