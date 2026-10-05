---
title: "Iteration 6: Real hardware"
type: iteration
date: 2026-10-04
status: planned
branch: iter-6/real-hardware
tags:
  - iteration
  - hptx
---

# Iteration 6: Real hardware

Manual checklist, run when the USB-serial adapter arrives: 48SX and 49G,
every CLI command, both protocols on the 49G, timing on long transfers,
early-49G DTR/RTS behaviour. File every surprise into the wiki.

## Tasks

- [ ] Carried over from iteration 5: verify which prolog a real 39G/40G
  aplet uses (#026D5 is the Conn4x guess) and that the object walk matches
  it, and whether the 39G/40G wire protocol is Kermit or XModem; record both
  in the wiki.
- [ ] Carried over from iteration 5: XSERV on a 49g+/50g (hptx `xserv` is
  implemented from the wiki only and unverified).
