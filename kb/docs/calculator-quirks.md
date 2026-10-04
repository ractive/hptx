---
title: Calculator quirks checklist
type: docs
date: 2026-10-04
status: active
tags:
  - quirks
  - kermit
  - hptx
---

# Calculator quirks checklist

Each item must be covered by a test. Source: the wiki at `~/devel/hp-literature/` and the saturnng container experiments of 2026-10-04.

- Server answers only R, S, C, G D, G F, G L, I. No REMOTE CD: use
  `C "{ dir } EVAL"`.
- Every control character must be prefixed; block check 3 default; 9600 max.
- Stale NAK waits in the buffer when connecting to an idle server.
- `G D` header line present on G/49G, absent on SX. Trailing dot on 49G reals.
- ACK of the F packet carries the stored name; illegal names abort; a name
  clash gets a `.1` suffix unless flag -36.
- Binary receive on the HP keeps a string until the end; ASCII mode compiles
  each packet and aborts on syntax error.
- Inter-byte gaps cause overruns; write packets atomically.
- ARCHIVE with a ticking clock can corrupt the backup: stop the clock display.
- 48G XModem checksum-only; 48S/SX no XModem at all.
- On the SX, SERVER typed within 2 s after FINISH loses keys (harness only).
