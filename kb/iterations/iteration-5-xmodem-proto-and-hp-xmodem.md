---
title: "Iteration 5: xmodem-proto and HP XModem"
type: iteration
date: 2026-10-04
status: planned
branch: iter-5/xmodem-proto-and-hp-xmodem
tags:
  - iteration
  - hptx
---

# Iteration 5: xmodem-proto and HP XModem

Read first: wiki `protocols/xmodem`, `xmodem-hp`, `xserv`,
`questions/xmodem-hp-crc-mode`; `~/devel/hpcomm/hpgcomm/XModem.cpp`.

## Tasks

- [ ] 128-byte and 1k blocks, checksum and CRC-16 (MSB-first #1021), receiver
  start characters NAK / `C`, fallback to checksum after failed CRC attempts
  (the 48G has no CRC at all).
- [ ] HP padding: strip trailing bytes using the object-length walk; the 49G
  rejects objects with more than ~255 bytes of padding.
- [ ] XSERV (49g+/50g) framing: 2-byte big-endian length, data, 1-byte sum;
  commands P, G, E, M, L. Facts came from HP-written Conn4x code under a
  non-commercial license: implement from the wiki description only.
- [ ] CLI: `hptx get/put --protocol xmodem`, `hptx xserv ...`.

## Acceptance criteria

e2e on the 49G container (XRECV/XSEND); 39G/40G aplet upload
  verified on real hardware or Emu48 on Windows when available.
