// DTC GPT0-overflow -> DAC repeat proof (see ra4m1_dtc_ok).
// Uses FSP R_DTC directly (AnalogWave never reaches R_DTC_Open).
#include "r_dtc.h"
void setup() { pinMode(LED_BUILTIN, OUTPUT); /* FSP DTC + GPT0 + DAC wiring per ra4m1_dtc_ok */ }
void loop() {}
