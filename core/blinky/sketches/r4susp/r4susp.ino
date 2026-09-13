// USB suspend/resume proof (see ra4m1_usb_suspend_resume_ok).
// NOTE: reconstructed shape — the vendored r4susp.bin is the proof binary.
// The original overrode TinyUSB's weak tud_suspend_cb/tud_resume_cb; at
// sketch level those weak symbols collide with the core archive, so a
// from-source rebuild needs the original linkage.
void setup() { Serial.begin(9600); while (!Serial) {} Serial.println("susp"); }
void loop() {}
