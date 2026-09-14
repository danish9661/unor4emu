    .syntax unified
    .cpu cortex-m4
    .thumb
    .text
    // vector A: ite le / addle r3,#48 / addgt r3,#55
    ite le
    addle r3, #48
    addgt r3, #55
    // vector B: itt mi / addmi r0,r0,r1 / addmi r2,r2,#7
    itt mi
    addmi r0, r0, r1
    addmi r2, r2, #7
    // vector C: ite eq / cmpeq r0,#1 / addne r1,r1,#2
    ite eq
    cmpeq r0, #1
    addne r1, r1, #2
    // vector D: itt pl / lslpl r0,r0,#1 / lsrpl r1,r1,#1
    itt pl
    lslpl r0, r0, #1
    lsrpl r1, r1, #1
    // vector E: itt pl / andpl r0,r0,r1 / orrpl r2,r2,r3
    itt pl
    andpl r0, r0, r1
    orrpl r2, r2, r3
    // vector F: ite eq / tsteq r0,r1 / movne r2,#0
    ite eq
    tsteq r0, r1
    movne r2, #0
    bx lr
