#include <SPI.h>
void setup() {
  Serial.begin(9600); while (!Serial) {}
  SPI.begin();
  delay(200);
  byte a = SPI.transfer(0xA5), b = SPI.transfer(0x5A), c = SPI.transfer(0x00);
  Serial.println((a == 0xA5 && b == 0x5A && c == 0x00) ? "spi-ok" : "spi-ng");
}
void loop() {}
