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
   core/blinky/r4wire.bin core/blinky/r4spi.bin demo/fw/
ls -la demo/pkg demo/fw | head -n 20
