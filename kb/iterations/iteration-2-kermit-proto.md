---
title: "Iteration 2: kermit-proto"
type: iteration
date: 2026-10-04
status: planned
branch: iter-2/kermit-proto
tags:
  - iteration
  - hptx
---

# Iteration 2: kermit-proto

Read first: wiki `protocols/kermit`, `protocols/kermit-hp`,
`protocols/server-commands`; `~/devel/hpcomm/hpcomm/Kermit.cpp` for behaviour.

## Tasks

- [ ] Packet codec: SOH, LEN, SEQ, TYPE, DATA, CHECK, EOL; block check 1, 2, 3
  (type 3 is CRC-CCITT LSB-first, poly #1081, same as the calculator's CRC
  register; wiki `hardware/crc`).
- [ ] Prefixing: control (`#`), 8th bit (`&`), repeat (`~`); `tochar/unchar/ctl`.
- [ ] Send-init negotiation from the HP's reply `~& @-# 1` (MAXL, TIME, NPAD,
  PADC, EOL, QCTL, QBIN, CHKT, REPT); no long packets, no windows.
- [ ] Client side only (we are the host): send file(s), receive file(s), generic
  commands `G D`, `G F`, `G L`, host command `C`, and the `I` exchange.
- [ ] Events: FileStart{name}, Data, FileEnd, ServerText (reply to C / G D),
  Error{packet text}, Done. The ACK to the F packet carries the name the
  calculator actually used; surface it.
- [ ] Timeouts and retries per the manual, but the HP slows down as a received
  file grows (System RPL copies on every packet): default timeout 20 s and a
  configurable inter-packet pause.
- [ ] Tests: hand-written byte traces for each packet type and block check;
  traces recorded from the emulator (M1's container) for a full `C "6 7 *"`,
  `G D`, GET and SEND; the stale-NAK-on-connect case (discard pending input
  for ~0.5 s after connect is a transport concern, but the state machine must
  survive an unsolicited NAK before S).

## Acceptance criteria

all traces pass; crate has no I/O dependencies; `cargo doc`
  explains the seam.
