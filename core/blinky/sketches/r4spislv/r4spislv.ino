// SPI0 master vs SPI1 slave exchange (see ra4m1_spi_slave_ok).
#include <SPI.h>
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
  SPI.begin();
  SPI.beginTransaction(SPISettings(1000000, MSBFIRST, SPI_MODE0));
  digitalWrite(SS, LOW);
  byte a = SPI.transfer(0xA5);
  digitalWrite(SS, HIGH);
  SPI.endTransaction();
  digitalWrite(LED_BUILTIN, a == 0x5A);
}
void loop() {}
