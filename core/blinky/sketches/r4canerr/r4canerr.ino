// CAN error proof: LED on isError() (see ra4m1_can_error_ok).
#include <Arduino_CAN.h>
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
  if (!CAN.begin(CanBitRate::BR_500k)) for (;;) {}
}
void loop() {
  uint8_t d[] = {0xCA, 0xFE};
  CanMsg m(CanStandardId(0x123), sizeof(d), d);
  if (CAN.write(m) < 0) for (;;) {}
  int code = 0;
  if (CAN.isError(code)) digitalWrite(LED_BUILTIN, HIGH);
  delay(50);
}
