// CAN self-test loopback proof (see ra4m1_can_ok).
#include <Arduino_CAN.h>
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
  if (!CAN.begin(CanBitRate::BR_500k)) for (;;) {}
  uint8_t d[] = {0xDE, 0xAD};
  CanMsg m(CanStandardId(0x321), sizeof(d), d);
  if (CAN.write(m) < 0) for (;;) {}
}
void loop() {
  if (CAN.available()) {
    CanMsg m = CAN.read();
    if (m.id == 0x321 && m.data[0] == 0xDE) digitalWrite(LED_BUILTIN, HIGH);
  }
}
