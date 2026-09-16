// 12x8 charlieplex smiley on GPIO P0/P2 (see ra4m1_matrix_ok).
// Bare-metal multiplex: for each of the 96 charlieplex slots, look up
// the anode/cathode rails in MP, drive anode OUT+HIGH + cathode
// OUT+LOW for lit LEDs (PCNTR1 word: PDR low half, PODR high half),
// tristate the rails otherwise, delay, next slot. No panel on Minima
// - the pattern IS the verdict.
//
// Ordering (glitch-free, like the RA quick-design guide): program the
// LEVEL first while the rails are still tristated inputs, THEN set
// direction. Every intermediate state has at most one rail as output,
// so no phantom LED ever conducts - and no sampler/trace ever sees one.
static const uint16_t MP[11][2] = {
  {0,3},{0,4},{0,11},{0,12},{0,13},{0,15},
  {2,4},{2,5},{2,6},{2,12},{2,13},
};
static const uint8_t SMILE[12] = {
  0x3C,0x42,0xA5,0x81,0xA5,0x99,0xA5,0x81,0xA5,0x42,0x3C,0x00,
};
#define P0_PCNTR1 (*(volatile uint32_t *)0x40040000)
#define P2_PCNTR1 (*(volatile uint32_t *)0x40040040)
static void drive(int a, int c, int on) {
  uint32_t ap = MP[a][0], ab = MP[a][1], cp = MP[c][0], cb = MP[c][1];
  if (!on) return;
  // 1. Levels while tristated: anode HIGH, cathode LOW, rest LOW.
  uint32_t l0 = 0, l2 = 0;
  if (ap == 0) l0 |= 1u << ab;
  else l2 |= 1u << ab;
  P0_PCNTR1 = l0 << 16; // LOW half 0 keeps PDR tristated
  P2_PCNTR1 = l2 << 16;
  // 2. Direction: exactly the two rails output.
  uint32_t m0 = 0, m2 = 0;
  if (ap == 0) m0 |= 1u << ab; else m2 |= 1u << ab;
  if (cp == 0) m0 |= 1u << cb; else m2 |= 1u << cb;
  P0_PCNTR1 = (l0 << 16) | m0;
  P2_PCNTR1 = (l2 << 16) | m2;
}
static void idle_all(void) {
  P0_PCNTR1 &= 0xFFFF0000; // PDR=0: tristate P0 rails (levels kept)
  P2_PCNTR1 &= 0xFFFF0000; // PDR=0: tristate P2 rails
}
void setup() { idle_all(); }
void loop() {
  for (int k = 0; k < 96; k++) {
    int a = k / 9, t = k % 9, c = t + (t >= a ? 1 : 0);
    int on = (SMILE[k / 8] >> (k % 8)) & 1;
    idle_all();
    drive(a, c, on);
    delayMicroseconds(300);
  }
}
