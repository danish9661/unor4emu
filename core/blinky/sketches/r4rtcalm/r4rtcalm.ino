#include "RTC.h"
volatile bool fired = false;
void alarmCallback() { fired = true; }
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
  RTC.begin();
  RTCTime t(1, Month::JANUARY, 2024, 11, 59, 55, DayOfWeek::MONDAY, SaveLight::SAVING_TIME_INACTIVE);
  RTC.setTime(t);
  RTCTime alarm;
  alarm.setSecond(0);
  AlarmMatch m;
  m.addMatchSecond();
  RTC.setAlarmCallback(alarmCallback, alarm, m);
}
void loop() {
  if (fired) digitalWrite(LED_BUILTIN, HIGH);
}
