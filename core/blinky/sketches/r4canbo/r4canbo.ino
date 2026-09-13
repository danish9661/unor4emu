#include <Arduino_CAN.h>
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
  if (!CAN.begin(CanBitRate::BR_500k)) for (;;) {}
  // Self-test loopback so TX completes on the bench with no transceiver.
  // (The model honors TCR self-test like the r4can proof.)
}
void loop() {
  uint8_t d[] = {0xCA, 0xFE};
  CanMsg m(CanStandardId(0x123), sizeof(d), d);
  if (CAN.write(m) < 0) for (;;) {}
  int code = 0;
  if (CAN.isError(code)) {
    // Bus-off recovery: silicon auto-recovers after 11x11 recessive
    // bits; the driver reports BUS_RECOVERY when back. Light the LED
    // once any error episode surfaces (warning/passive/bus-off).
    digitalWrite(LED_BUILTIN, HIGH);
    delay(50);
  }
  delay(50);
}
