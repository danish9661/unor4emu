// RTC firmware proof: set 11:59:58, BCD rollover to 12:00 (see ra4m1_rtc_firmware).
#include "RTC.h"
void setup() {
  RTC.begin();
  RTCTime t(1, Month::JANUARY, 2024, 11, 59, 58, DayOfWeek::MONDAY, SaveLight::SAVING_TIME_INACTIVE);
  RTC.setTime(t);
}
void loop() {}
