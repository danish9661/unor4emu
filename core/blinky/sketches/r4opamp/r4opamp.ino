// OPAMP firmware proof (see ra4m1_opamp_firmware): begin -> AMPMON0.
#include <OPAMP.h>
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
  if (OPAMP.begin(OPAMP_SPEED_HIGHSPEED) && OPAMP.isRunning(0))
    digitalWrite(LED_BUILTIN, HIGH);
}
void loop() {}
