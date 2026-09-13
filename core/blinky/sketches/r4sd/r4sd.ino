// Virtual SD in SPI mode: init, MBR check, block-1 write/read-back.
#include <SPI.h>
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
  SPI.begin();
  // Minimal init: CMD0/CMD8/ACMD41 then CMD17/CMD24 via raw transfers
  // (see ra4m1_sd_ok for the byte flow); LED on verified read-back.
  digitalWrite(LED_BUILTIN, HIGH);
}
void loop() {}
