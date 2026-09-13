// Wire master (IIC1) vs bare-metal IIC0 slave @ 0x42 (see ra4m1_wire_slave_ok).
#include <Wire.h>
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
  Wire.begin();
  Wire.beginTransmission(0x42); Wire.write(0xBE); Wire.endTransmission();
  Wire.requestFrom(0x42, 1);
  digitalWrite(LED_BUILTIN, Wire.available() && Wire.read() == 0xEF);
}
void loop() {}
