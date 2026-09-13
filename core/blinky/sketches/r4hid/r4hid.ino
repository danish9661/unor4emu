// USB HID keyboard proof (see ra4m1_usb_hid_keyboard).
// NOTE: reconstructed shape — the vendored r4hid.bin is the proof binary.
// The original drove TinyUSB's HID device class directly (boot-keyboard
// report descriptor + INT-IN 'a' report); the public HID_::begin() symbol
// the sketch referenced is not exported by the 1.6.0 core archive, so a
// from-source rebuild needs the same internal linkage the original had.
void setup() { pinMode(LED_BUILTIN, OUTPUT); digitalWrite(LED_BUILTIN, HIGH); }
void loop() {}
