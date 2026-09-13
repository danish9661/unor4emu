#include <SoftwareSerial.h>
SoftwareSerial soft(2, 3);
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
  Serial1.begin(115200);
  soft.begin(9600);
  soft.write(0xA5);
  unsigned long t0 = millis();
  while (!soft.available() && millis() - t0 < 2000) {}
  int c = soft.read();
  Serial1.print("got=");
  Serial1.println(c, HEX);
  digitalWrite(LED_BUILTIN, c == 0xA5);
}
void loop() {}
