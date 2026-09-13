  .syntax unified
  .thumb
  .text
  .global f
f:
  ldrd r0, r1, [r2]
  ldrd r4, r5, [r6]
  strd r8, r9, [r4]
