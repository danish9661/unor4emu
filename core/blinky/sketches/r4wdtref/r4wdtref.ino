// WDT refresh-holds proof: refresh every 10ms forever (see ra4m1_wdt_refresh).
#include "WDT.h"
void setup() { WDT.begin(8000); }
void loop() { WDT.refresh(); delay(10); }
