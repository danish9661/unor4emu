// attachInterrupt button proof, all 16 lines (see ra4m1_attach_interrupt).
void setup() { pinMode(LED_BUILTIN, OUTPUT); attachInterrupt(0, [](){ digitalWrite(LED_BUILTIN, HIGH); }, CHANGE); }
void loop() {}
