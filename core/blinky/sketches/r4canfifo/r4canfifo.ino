#include <Arduino_CAN.h>
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
  if (!CAN.begin(CanBitRate::BR_500k)) for (;;) {}
  uint8_t d[] = {0xCA, 0xFE};
  CanMsg m(CanStandardId(0x123), sizeof(d), d);
  if (CAN.write(m) < 0) for (;;) {}
}
void loop() {
  if (CAN.available()) {
    CanMsg m = CAN.read();
    if (m.id == 0x123 && m.data[0] == 0xCA && m.data[1] == 0xFE)
      digitalWrite(LED_BUILTIN, HIGH);
  }
}
