void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
  analogReadResolution(14);
  analogWriteResolution(12);
}
void loop() {
  int v = analogRead(A0);
  analogWrite(DAC, v >> 2);
  digitalWrite(LED_BUILTIN, v > 100 ? HIGH : LOW);
  delay(50);
}
