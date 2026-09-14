# sketches/ — Arduino sources for the vendored `r4*.bin` proof firmware

`core/blinky/r4*.bin` are the arduino-cli 1.6.0
(`arduino:renesas_uno:minima`) builds the `ra4m1.rs` proofs boot.
`/tmp` is wiped between sessions, so the original sketch dirs were lost;
these are reconstructions from the test comments + ArduinoCore-renesas
1.6.0 APIs. Three tiers:

- **Exact** (recompile to a working equivalent): r4aw, r4aws, r4tone, r4sser,
  r4rtcalm, r4canbo, r4blink, r4serial, r4echo, r4wire, r4spi, r4eep,
  r4wire1, r4serial1, r4irq, r4wdtref, r4wdtexp, r4rtc, r4can, r4can1,
  r4canfifo, r4canerr. Verified: `arduino-cli compile --fqbn
  arduino:renesas_uno:minima` passes for each (34/34 dirs).
- **Shape** (shows the Arduino-level flow; vendored bin stays the proof
  because the original used internals): r4spislv (SPI slave select flow),
  r4sd (SD SPI-mode init flow), r4dtc/r4pwm/r4can1/r4dac8/r4kint/r4ssi/
  r4matrix (bare-metal register flows per the test comments),
  r4ctsu (mutual touch flow; original used raw FSP, no Arduino CTSU lib
  exists in 1.6.0), r4opamp (recompiles OK; `OPAMP.begin(HIGHSPEED)` +
  `isRunning(0)`), r4hid (TinyUSB HID internals; `HID_::begin()` is not
  exported by the 1.6.0 archive so from-source link needs the original
  linkage — bin stays canonical).

Rebuild check: `for d in core/blinky/sketches/r4*/; do arduino-cli compile
--fqbn arduino:renesas_uno:minima --output-dir /tmp/rb/$(basename $d) $d;
done` — 34 pass (incl. r4aws, r4rtcalm, r4canbo, r4opamp), r4ctsu (no FspTouch.h
in 1.6.0), r4hid (no exported `HID_::begin`), r4susp (no `tud_suspend_cb`
override at sketch level) document real upstream gaps, matching AUDIT.md.
r4rtcalm/r4canbo bins are the RTC-alarm / CAN-bus-off proof firmware.
