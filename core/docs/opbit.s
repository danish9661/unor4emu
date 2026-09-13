  .syntax unified
  .thumb
  .text
  orr.w r1, r1, r4, lsl #20
  orrs.w r1, r1, r4, lsl #20
  eor.w r2, r0, r2
  eors.w r2, r0, r2
  add.w r2, r0, r2
  adds.w r2, r0, r2
  rsbs r5, r4, r5, lsr #21
  rsb.w r5, r4, r5, lsr #21
