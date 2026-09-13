// Serial1 (SCI2) echo proof (see ra4m1_serial1_echo).
void setup() { Serial1.begin(115200); }
void loop() { if (Serial1.available()) Serial1.write(Serial1.read()); }
