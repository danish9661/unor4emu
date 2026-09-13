#include "analogWave.h"
analogWave wave(DAC);
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
  wave.sine(10);
  digitalWrite(LED_BUILTIN, HIGH);
}
void loop() {}
