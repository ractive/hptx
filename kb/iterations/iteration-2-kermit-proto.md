---
title: "Iteration 2: kermit-proto"
type: iteration
date: 2026-10-04
status: completed
branch: iter-2/kermit-proto
tags:
  - iteration
  - hptx
---

# Iteration 2: kermit-proto

Read first: wiki `protocols/kermit`, `protocols/kermit-hp`,
`protocols/server-commands`; `~/devel/hpcomm/hpcomm/Kermit.cpp` for behaviour.

## Tasks

- [x] Packet codec: SOH, LEN, SEQ, TYPE, DATA, CHECK, EOL; block check 1, 2, 3
  (type 3 is CRC-CCITT LSB-first, poly #1081, same as the calculator's CRC
  register; wiki `hardware/crc`).
- [x] Prefixing: control (`#`), 8th bit (`&`), repeat (`~`); `tochar/unchar/ctl`.
- [x] Send-init negotiation from the HP's reply `~& @-# 1` (MAXL, TIME, NPAD,
  PADC, EOL, QCTL, QBIN, CHKT, REPT); no long packets, no windows.
- [x] Client side only (we are the host): send file(s), receive file(s), generic
  commands `G D`, `G F`, `G L`, host command `C`, and the `I` exchange.
- [x] Events: FileStart{name}, Data, FileEnd, ServerText (reply to C / G D),
  Error{packet text}, Done. The ACK to the F packet carries the name the
  calculator actually used; surface it.
- [x] Timeouts and retries per the manual, but the HP slows down as a received
  file grows (System RPL copies on every packet): default timeout 20 s and a
  configurable inter-packet pause.
- [x] Tests: hand-written byte traces for each packet type and block check;
  traces recorded from the emulator (`just emulator-up`, see
  `emulator/README.md`) for a full `C "6 7 *"`,
  `G D`, GET and SEND; the stale-NAK-on-connect case (discard pending input
  for ~0.5 s after connect is a transport concern, but the state machine must
  survive an unsolicited NAK before S).

## Acceptance criteria

all traces pass; crate has no I/O dependencies; `cargo doc`
  explains the seam.

## Outcome

Done 2026-10-04. `kermit-proto` has no dependencies; the seam takes `now`
on every output-producing call (decision log, iteration 2). Nine traces
recorded from the emulator (48SX: info, host, dir, send, get, get of a
missing name; 49G: host, dir, finish) replay as unit tests, plus a stale-NAK
variant. Findings: command packets must use block check 1 even after an I
exchange agreed on 3; the fresh 48SX transfers in ASCII mode
(`kb/docs/calculator-quirks.md`).
