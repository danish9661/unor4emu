#include <Wire.h>
void setup() {
  Serial.begin(9600); while (!Serial) {}
  Wire.begin();
  byte w[] = {0x10, 0x55, 0x66};
  Wire.beginTransmission(0x50); Wire.write(w, 3); Wire.endTransmission();
  delay(10);
  Wire.beginTransmission(0x50); Wire.write(0x10); Wire.endTransmission(false);
  Wire.requestFrom(0x50, 2);
  byte a = Wire.read(), b = Wire.read();
  Serial.println((a == 0x55 && b == 0x66) ? "wire-ok" : "wire-ng");
}
void loop() {}
