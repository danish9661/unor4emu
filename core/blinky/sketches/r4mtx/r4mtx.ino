// Heart on the 12x8 LED matrix via the real Arduino_LED_Matrix library,
// driven polled from loop() (no FspTimer ISR): matrix.on/off hit the same
// turnLed/PFS path the ISR uses. Built for unor4wifi (the matrix pins
// D28-D38 only exist in the WiFi variant's g_pin_cfg); runs on the same
// RA4M1 silicon the Minima core emulates. See ra4m1_mtx_ok.
//
// Frame layout (matches the proof): HEART[w] bit b = LED index 32*w+b,
// i.e. the sketch's bit-reversed LEDMATRIX_HEART_BIG words in order.
// NOTE: the on/off pair must be adjacent in time (no delay between them);
// a delay(2) AFTER off() sets the frame rate. A delay between on and off
// leaves one anode + many cathodes asserted and ghosts extra LEDs.
#include "Arduino_LED_Matrix.h"
ArduinoLEDMatrix matrix;

// LEDMATRIX_HEART_BIG, bit-reversed per word (the ISR's reverse()).
static const uint32_t HEART[3] = { 0x2225218Cu, 0x81042022u, 0x02005008u };

void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
  digitalWrite(LED_BUILTIN, HIGH);
}

void loop() {
  for (int i = 0; i < 96; i++) {
    if (HEART[i / 32] & (1UL << (i % 32))) {
      matrix.on(i);
      matrix.off(i);
    }
  }
  delay(2);
}
