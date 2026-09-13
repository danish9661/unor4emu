#include <EEPROM.h>
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
  EEPROM.write(0, 0xA5); EEPROM.write(1, 0x3C);
  digitalWrite(LED_BUILTIN, (EEPROM.read(0) == 0xA5 && EEPROM.read(1) == 0x3C));
}
void loop() {}
