# unor4api.md — Arduino UNO R4 Minima (`unor4emu`, no npm yet) API + OpenHW gap spec

Probed live from the built glue (`demo/pkg/uno_r4_minima_wasm.d.ts`:
2 classes + 52 free fns, `wasm-bindgen` 0.2.128), `demo/app.js` (1685 lines),
`AGENTS.md` §§3–5 RA map, `core/blinky/` (36 bins + 36 `sketches/r4*/`
dirs). Silicon bases below re-verified against the FSP header
(`R7FA4M1AB.h` `R_*_BASE` defines); test counts are `AGENTS.md` claims
(127 snapshot + 90 small-core), not re-run here.

## 1. Package layout

- Chip: RA4M1 R7FA4M1AB3CFM Cortex-M4F 48M, 256K flash @0x0, 32K SRAM
  @0x20000000, 8K dataflash @0x40100000. App links at `APP_BASE=0x4000`
  (bootloader 0x0–0x3FFF); EK-RA4M1 target zero-boots (LED1=P106).
- Workspace (`AGENTS.md` §1): `core/ra4m1-periph-wasm` (full snapshot,
  isolated workspace, 127 tests) / `core/ra4m1-core` (small R4-only, ~315K
  wasm, 90 proofs) / `wasm-minima` (`uno-r4-minima-wasm`, board class
  `UnoR4Minima` + `WasmCpu`, no STM32/ESP32 linkage) / `crates/wifi-link`
  (`WifiModule` trait + `NoWifi`; WiFi parked) / `wasm-wifi` parked-excluded.
- `demo/` (tracked: `index/docs/about.html, app.js` 1685L, `styles.css, build.sh`;
  generated: `pkg/uno_r4_minima_wasm{,_bg}.{wasm,js,d.ts}` ~315K wasm,
  `fw/r4*.bin`). `core/blinky/` = 36 `r4*.bin` + 36 `sketches/r4*/` dirs
  (analog, aw, aws, blink, can, can1, canbo, canerr, canfifo, ctsu, dac8, dtc,
  echo, eep, hid, irq, kint, matrix, mtx, opamp, pwm, rtc, rtcalm, sd, serial,
  serial1, spi, spislv, sser, ssi, susp, tone, wdtexp, wdtref, wire, wire1)
  + `blinky.bin` (legacy). Cargo tests single-threaded
  (`-- --test-threads=1`, shared globals flake parallel).

## 2. WASM surface (`demo/pkg/uno_r4_minima_wasm.d.ts`: 2 classes + 52 free fns
(+`initSync`); `InitInput/InitOutput` wasm-bindgen types follow, verbatim)

`UnoR4Minima{static init_system, constructor(sp,pc), static new_ek_ra4m1(sp,pc),
load_firmware(bytes) [APP_BASE=0x4000 default], load_firmware_at(bytes,base),
reset_cpu, set_deliver_irqs, step(budget)→n, sleeping/wake, get_pc/sp/regs/
xpsr/ipsr/primask, fault_pc/op1/op2/len, mem_fault, mem_read/write,
read8/write8/read32/write32, trace_start/stop/take_trace}` (component-facing
board class; full delegation to the proven core) +
`WasmCpu{constructor(sp,pc) [RA4M1 256K/32K], static new_ek_ra4m1(sp,pc)
[zero-boot], load_firmware(bytes,base), step, reset_cpu,
set_deliver_irqs(def off), sleeping/wake, get_pc/sp/regs/xpsr/sregs/fpscr/
primask/ipsr, fault_pc/op1/op2/len, mem_fault, mem_read/write, read8/write8/
read32/write32, trace_start/stop/take_trace}` (demo/legacy path).
Free fns (52, verbatim): `init_ra4m1, init_ek_ra4m1, reset_state, tick_n,
tick_peripherals, periph_read/write, has_pending_interrupt,
get_next_pending_interrupt, board_info, arduino_pin_to_port_bit,
gpio_read_pin, gpio_set_pin_input, gpio_button_press, get_uart_output,
peek_uart_output, uart_rx_byte, adc_set/clear_channel_value,
ctsu_set/clear_channel_value, can_send_frame, can_take_rx_fifo,
can_inject_errors, dma_pending_count, dma_take_pending, dma_set_completed,
dma_check_completion, dtc_has_pending, sci_poll, wdt_poll, rtc_read_bcd,
ssi_exchange, spi_exchange, spi_set_loopback,
spi_set_sd_card, sd_read_block, i2c_exchange, icu_pin_edge, kint_key_press,
matrix_trace_take, soft_wire, usb_host_attach/reset/setup/status_done/
suspend/resume, usb_rx_inject, usb_take_tx, is_watchdog_reset_requested`
(+`initSync`). Snapshot-only extras (STM32 legacy paths, not in the Minima
build): `dma_get_pending_count, dma_get_pending, dma_component_completed,
dma_periph_read/write`, `gpio_read_output/set_input/read_input`,
`pwr_wakeup`, `can_inject`, `tim_inject_capture`, `set_intr_pending`,
eth/audio/flash/tap helpers — OpenHW targets `wasm-minima` only.

## 3. Bring-up (`demo/app.js:13-48`, `AGENTS.md` §7)

```js
// demo/app.js:30-48 — real boot (mirrors the Rust proof harness)
import init, { WasmCpu, init_ra4m1, reset_state, tick_peripherals, … } from './pkg/…js';
const APP_BASE = 0x4000, PORT_BASE = 0x40040000, USBFS = 0x40090000, CHUNK = 48000;
reset_state(); init_ra4m1();
const dv = new DataView(fwBytes.buffer, fwBytes.byteOffset, fwBytes.byteLength);
board = new WasmCpu(dv.getUint32(0,true), dv.getUint32(4,true));
board.load_firmware(fwBytes, APP_BASE);
board.reset_cpu(sp, pc); board.set_deliver_irqs(true);
loop: board.step(n); tick_peripherals();  // step already accounts clock — NOT tick_n (would double it)
```

D13 = P111 (PORT1 bit 11). Jigs (call BEFORE boot): `soft_wire(1,4,1,5,0)`
(D3 P104 TX→D2 P105 RX IRQ0, SoftwareSerial); `spi_set_loopback(base,on)`;
`spi_set_sd_card(base,on)` (+`sd_read_block` for MBR checks);
`adc_set_channel_value(ch,14-bit)` / `ctsu_set_channel_value(ch,16-bit)`;
`icu_pin_edge(line,falling)` (attachInterrupt); `kint_key_press(key)`;
`can_inject_errors(rx,tx)`; USB virtual-host `usb_host_{attach,reset,setup,
status_done,suspend,resume}` + `usb_rx_inject(pipe,bytes)/usb_take_tx()`;
`matrix_trace_take()` (12-port snapshots → charlieplex frame);
`uart_rx_byte(sciBase,byte)`; watchdog `is_watchdog_reset_requested()`.

Component API (no guest MMIO needed — OpenHW runners call these):
`board_info()` → `[0x4D31, 262144, 32768, 8192, 0x4000, 48000000, 0]`;
`arduino_pin_to_port_bit(pin)` → packed `port<<8|bit` (D0–D13, A0–A5 =
14–19; −1 unmapped); `gpio_read_pin(pin)` → PODR level 0/1 (−1 unmapped);
`gpio_set_pin_input(pin,level)` → PIDR drive (what `digitalRead` sees);
`gpio_button_press(line,falling)` → edge + D13 verdict;
`can_send_frame(mbox,id,bytes)` → `[id_hi,id_lo,dlc,d0..d7]` via
self-test loopback (empty = not operation mode / no receiver);
`can_take_rx_fifo()` → `[id,dlc,d0..d7]` groups (MB24+RFPCR pop);
`ssi_exchange(rxWords)` → `[tx_drained…, 0xFFFFFFFF, rx_depth]`;
`spi_exchange(base,mosi)` → `[miso, spsr]` (slave stages, master peeks
jig/0xFF; SD armed → drive the guest master, peeks stay out);
`i2c_exchange(addr,write,read_len)` → EEPROM reply at 0x50 (empty =
NACK). DMA/scheduler: `dma_pending_count()` → staged depth;
`dma_take_pending(i)` → `[dir,stream,src,dst,size,peri_addr,peripheral,
pinc,psize]` (same layout as `DmaTransfer::to_u32_vec`; dir 2=MemCopy);
`dma_set_completed(s,ch)` + `dma_check_completion(ch)` → one-shot latch;
`dtc_has_pending()` → staged-activation probe (re-arms, non-consuming).
Status polls (no MMIO decode): `sci_poll(base)` → `[tx_ready,rx_ready,
rx_byte]` (RDR peeked, guest still drains); `wdt_poll(base)` →
`[down,reload]` (WDT 0x40044200 / IWDT 0x40044400); `rtc_read_bcd()` →
`[sec,min,hour,day,mon,year_lo,year_hi]` raw BCD. All proven by
`ra4m1_component_*` tests in both cores
(90 small-core / 127 snapshot) without disturbing guest state
(try_borrow skips mid-MMIO slots; no unsolicited NVIC pends).

## 4. RA4M1 map (`AGENTS.md` §3, real FSP bases — byte-exact bus §4)

SYSC 4001E000 / MSTP 40046FFC / ICU 40006000 / PORT0+ 40040000+n*20 /
PFS 40040800 / ELC 40041000 / RTC 40044000 / WDT 40044200 / IWDT 40044400 /
DMAC 40005000 / DTC 40005400 / DOC 40054100 / ADC 4005C000 / DAC 4005E000 /
SCI0-9 40070000+ch*20 (TDR+0x03 byte-exact UART+simple-SPI) /
SPI0 40072000 / SPI1 40072100 (RSPI, SPDR lane +0x04, D11-13→ch1) /
IIC0-2 40053000+ch*100 (RIIC + 0x50 EEPROM + shared slave) /
CRC 40074000 / GPT0-13 40078000+ch*100 (GTCR+2C GTIOR+34 GTCNT+48 GTCC+4C
GTPR+64; GTIOA/B PWM→PORT via PFS) / AGT 40084000 / ACMPLP 40085E00 /
OPAMP 40086000 / USBFS 40090000 (CDC+HID+MSC BOT/SCSI) / CTSU 40081000 /
KINT 40080000 (event 69) / SLCDC 40082000 / SSI0 4004E000 / SSI1 4004E100 /
CAN0 40050000 / CAN1 40051000 (mailbox+loopback+FIFO MB24) /
DATAFLASH 40100000 / FACI_LP 407EC000 / DAC8 4009E000 / ARM E000xxxx.
`Peripherals::{read (align→pack→shift), write (merge→write_sized)}`; RA models
override `write_sized` (repeated `STRB TDR,#3` transmits twice).

## 5. OpenHW gap (what's missing today — AVR-next runner)

- No UNO-R4 branch anywhere: `execute.ts` routes else→AVRRunner (ATmega-shaped,
  wrong silicon); no `runners/*r4*|*ra4m1*`; no `LOGIC_REGISTRY`/
  `COMPONENT_PINS` keys; `board-profiles.ts` UNO = ATmega328P pins (R4 needs
  Renesas pinmux + PFS table per `AGENTS.md` §5 GPT-PWM note);
  `backend/src/compiler/boardRegistry.js` uno = AVR hex (R4 needs Renesas FQBN
  `arduino:renesas_uno:minima`, app-at-0x4000 image step).
- WiFi seam exists and must be kept: `crates/wifi-link::{WifiModule,NoWifi}` —
  runner should call the trait, not invent networking (ESP32-S3 plugs in later).
- LED matrix (Minima 12×8 charlieplex GPIO render, `matrix_trace_take`) is a new
  display path vs AVR (route like espc3 `drawBitmap`/microbit matrix, VRAM off SAB).

## 6. Runner mapping checklist (BoardRunner `component-registry.ts:472-501`)

1. Load: `reset_state()` → `init_ra4m1()` (or `init_ek_ra4m1` for EK target) →
   `new UnoR4Minima(sp,pc)` (component board class; `load_firmware(bytes)`
   defaults APP_BASE=0x4000, `load_firmware_at(bytes,base)` for EK/zero-boot;
   `static new_ek_ra4m1(sp,pc)` for the EK target; legacy `new WasmCpu(sp,pc)`
   + `load_firmware(bytes, 0x4000)` still works) (NOT zero — zero
   executes shifted garbage); EK images base 0x0. `reset_cpu` = reload.
2. Tick: `board.step(budget)` + `tick_peripherals()` per frame (batched);
   `get_pc/sp/regs/fault_pc/mem_fault` → snapshot; `mem_read/write +
   periph_read/write` → READ/WRITE_MEM; `tick_n` ← `setSpeed`;
   `trace_start/stop/take_trace` → digest/telemetry; `sleeping/wake` →
   WFI rows; `dma_pending_count/take_pending/set_completed/check_completion`
   + `dtc_has_pending` → DMA/scheduler rows (descriptors are
   `[dir,stream,src,dst,size,…]`, dir 2=MemCopy); flag traces poll MMIO
   (never read data regs ICDRR/SPDR — a read eats the byte; mask sub-word
   packs `% 256`).
3. GPIO/PWM: PORT/PODR/PIDR (+PFS routing) → propagateBoardPin (D13=P111);
   GPT GTIOA/B → `onPwmDuty/onPWM`; `analogWrite(6,64)` GTPR/GTCCRB path +
   `tone()` GPT4 toggle → buzzer; `matrix_trace_take` → matrix display.
   (PWM pin→timer map is queryable in-code via `gpt_pwm_route(port,pin)`
   over the 32-entry `GPT_PWM_PINS` table in `system.rs`, sourced from
   Arduino's own Minima pinmux table; PFS PSEL codes
   0x02/0x03/0x14/0x15/0x16 select the GPT function.)
4. UART: `get/peek_uart_output` drain → serial TX; `uart_rx_byte(sciBase,byte)`
   ← `serialRx/serialRxByte`; `soft_wire` ← SoftwareSerial circuit option;
   `sci_poll(base)` → `[tx_ready,rx_ready,rx_byte]` ready bits (RDR peeked).
5. I2C/SPI/storage: RIIC master/slave + EEPROM-0x50 + `spi_set_loopback/sd_card
   + sd_read_block` → ComponentSignalAPI `onI2CWrite/onI2CRead/onSPIByte`
   (SD via `openhw-sd-card` block path); SCI simple-SPI shift+loopback same row.
   Prefer the component API (no guest MMIO): `i2c_exchange(addr,write,n)` →
   EEPROM reply (empty = NACK); `spi_exchange(base,mosi)` → `[miso,spsr]`
   (slave stages, master peeks jig/0xFF); `can_send_frame/can_take_rx_fifo`
   → `[id..]` groups; `ssi_exchange(rxWords)` → `[tx…, sentinel, depth]`.
6. Analog/touch/CAN/USB/audio: `adc_/ctsu_` jigs ← sliders/touch →
   `onAnalogVoltage`; `can_inject_errors` + mailbox/FIFO → `onCanFrame`
   (or the MMIO-free `can_send_frame/can_take_rx_fifo` pair);
   `usb_host_* + usb_rx_inject/usb_take_tx` → USB-CDC/HID/MSC bytes;
   SSI TX-drain/RX-pattern → `onI2SData` (or MMIO-free `ssi_exchange`);
   RTC/AGT/WDT/ELC/DMAC/DTC → timers/
   sleep-wake/scheduler rows (`rtc_read_bcd` → calendar, `wdt_poll` →
   `[down,reload]`, `dtc_has_pending` + `dma_*` → transfer rows);
   KINT/ICU `icu_pin_edge/kint_key_press` ← buttons
   (or Arduino-pinned `gpio_set_pin_input/gpio_button_press` +
   `gpio_read_pin` for levels).
7. Debug/SAB: chunk-boundary breaks + `get_next/has_pending_interrupt` watches;
   `forceEmitState` publishes pins/slots/telemetry (D13 + matrix + USB status;
   VRAM/descriptors off-SAB, snapshots over postMessage).

(End of file)
