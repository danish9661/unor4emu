#!/bin/bash
# Build the UNO R4 Minima browser demo: release WASM + JS glue + firmware.
# Requires: cargo, wasm32-unknown-unknown target, wasm-bindgen-cli
# (matching the workspace's wasm-bindgen version), python3 for serving.
# Run from the repo root:  ./demo/build.sh
# Serve:                   python3 -m http.server -d demo 8901
set -e
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
cargo build -p uno-r4-minima-wasm --target wasm32-unknown-unknown --release
mkdir -p demo/pkg demo/fw
"${WASM_BINDGEN:-wasm-bindgen}" \
  target/wasm32-unknown-unknown/release/uno_r4_minima_wasm.wasm \
  --out-dir demo/pkg --target web
cp core/blinky/r4blink.bin core/blinky/r4serial.bin core/blinky/r4echo.bin \
   core/blinky/r4wire.bin core/blinky/r4spi.bin core/blinky/r4wire1.bin \
   core/blinky/r4spislv.bin core/blinky/r4sd.bin core/blinky/r4canfifo.bin \
   core/blinky/r4eep.bin core/blinky/r4pwm.bin core/blinky/r4matrix.bin \
   core/blinky/r4rtc.bin core/blinky/r4ctsu.bin core/blinky/r4hid.bin \
   core/blinky/r4aw.bin core/blinky/r4tone.bin core/blinky/r4sser.bin \
   core/blinky/r4rtcalm.bin core/blinky/r4canbo.bin core/blinky/r4can1.bin \
   core/blinky/r4dac8.bin core/blinky/r4kint.bin core/blinky/r4ssi.bin demo/fw/
ls -la demo/pkg demo/fw | head -n 20
