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

- [ ] Same seam as `kermit-proto` (decision log 2026-10-04): `start`,
  `handle_input`, `handle_timeout` and `poll_output` take `now`, one packet
  per `poll_output`, events out, whole file in memory, no I/O dependencies.
  Reuse the `kermit_proto::trace` format and the `record` example pattern
  (`just record-trace`) for XModem traces replayed as unit tests.
- [ ] 128-byte and 1k blocks, checksum and CRC-16 (MSB-first #1021), receiver
  start characters NAK / `C`, fallback to checksum after failed CRC attempts
  (the 48G has no CRC at all).
- [ ] HP padding: strip trailing bytes with
  `hptx_core::object::strip_padding(data, allowance)` (the object-length
  walk, iteration 3). The allowance bounds how much may be cut: Kermit uses
  `KERMIT_PADDING_ALLOWANCE` (4 bytes); XModem passes the block size (128
  or 1024) because the sender pads whole blocks. The 49G rejects objects
  with more than ~255 bytes of padding.
- [ ] Walk rules for the 49G aplet (#026D5) and minifont (#026FE) prologs
  come from the Conn4x table only and are unverified (iteration 3 review).
  39G/40G transfers carry aplets, but their wire protocol is unverified
  (Kermit or XModem; wiki `hardware/hp39g-40g`), and so is the prolog a
  real aplet uses. Verify both on Emu48 or hardware, check the walk against
  the object, and record the results in the wiki.
- [ ] Driver: an XModem session in `hptx-core` next to `session::Session`,
  reusing `transport::Transport` (one packet per write, read with timeout,
  `drain`) and `MemoryTransport` for tests. Starting XRECV/XSEND on the
  calculator goes through a Kermit `C` host command if the calculator is
  in Kermit server mode; results stay on its stack (decision log,
  iteration 3).
- [ ] XSERV (49g+/50g) framing: 2-byte big-endian length, data, 1-byte sum;
  commands P, G, E, M, L. Facts came from HP-written Conn4x code under a
  non-commercial license: implement from the wiki description only.
- [ ] Server interplay, verify first on the emulated 49G and record it in the
  wiki: when the Kermit server runs `C "'NAME' XRECV"` (or `XSEND`), does
  the ACK arrive before or after the XModem transfer, does the calculator
  stay in server mode afterwards, and what is left on the stack? Design the
  driver from the answer (e.g. send the `C` packet, hand the same transport
  to the XModem state machine until Done, then resume Kermit or tell the
  user to run SERVER again, as `restore` does).
- [ ] RPL that hptx builds for the 49G writes numbers as reals with a
  trailing dot (decision 2026-10-05, iteration 4: exact integers broke
  IOPAR). Names and strings go through `hptx_core::charset` as in
  iteration 3.
- [ ] CLI: `hptx get/put --protocol xmodem`, `hptx xserv ...`, following the
  iteration 4 conventions (`{results,total,hints}` envelope, `{error,hint}`
  on stderr, `--dry-run` where destructive, bounded `--timeout`/`--retries`,
  never overwrite without `--force`/`--overwrite`). `scripts/e2e-cli.sh`
  gets XModem scenarios that run only when `HPTX_E2E_MODEL=49g` (the 48SX
  has no XModem, so neither the 48SX container nor the in-process saturnus
  transport of iteration 8 can cover them); CI already boots the 49G.

## Acceptance criteria

e2e on the 49G container (XRECV/XSEND); 39G/40G aplet upload
  verified on real hardware or Emu48 on Windows when available.
