---
title: "Iteration 3: hptx-core"
type: iteration
date: 2026-10-04
status: planned
branch: iter-3/hptx-core
tags:
  - iteration
  - hptx
---

# Iteration 3: hptx-core

Read first: wiki `protocols/hp-object-format`, `protocols/iopar`,
`protocols/server-commands`; `~/devel/hpcomm/hpcomm/Prot.cpp`, `Filer*.cpp`.

## Tasks

- [ ] Transports: `serialport` (9600 8N1, no flow control, assert DTR and RTS
  for early 49G units), TCP (`tcp://host:port`), in-memory for tests. Each
  drives a `kermit-proto` machine; discard pending input for 0.5 s on
  connect. Send each packet as one write (HP overruns on inter-byte gaps of
  ~4 frame times; wiki `hardware/uart`).
- [ ] Directory listing: `G D` reply parsing. The 48SX sends variable lines
  only; 48G/49G prepend `{ HOME } 127828` (dir and free memory). The 49G
  prints reals with a trailing dot. Parse both.
- [ ] Host commands via `C`: `PATH`, `{ dir } EVAL` to change directory, `VARS`,
  `PGDIR`, `PURGE`, `CRDIR`, rename, `MEM`, `VERSION`, `IOPAR` get/set.
  Returns the stack as text.
- [ ] Object format: `HPHP48-x` / `HPHP49-x` header, prolog table (RPLMAN in
  `raw/saturn-hardware/hp-tools-1991/`), object length walk to strip trailing
  padding, type names for `ls`. ASCII transfer format header
  (`%%HP: T(3)A(R)F(.);`, spelling: wiki `questions/ascii-header-spelling`).
- [ ] Screenshot: host command producing a GROB (e.g. `PICT RCL` or `LCD->`),
  GROB decode (131x64, rows padded to bytes, LSB = leftmost pixel) to PNG.
- [ ] Backup/restore: `ARCHIVE :IO:name` / `RESTORE` over Kermit, binary.
- [ ] Tests: unit tests on parsers with recorded replies from all three models;
  `tests/e2e.rs` (one binary; replace the iteration-1 raw I-packet smoke
  test with kermit-proto-based scenarios) with ~6 scenarios: ls, get, put round trip,
  run, screenshot, backup; each runs against whatever model the container
  is (CI runs the 48SX; developers run others locally).

### From iteration 2

- [ ] Transfer mode: the fresh 48SX transfers in ASCII mode (bytes 0-26 of a
  binary file did not survive the iteration-2 `48sx-send` round trip).
  hptx-core sets or checks the mode (IOPAR / `TRANSIO`) before put/get, and
  the e2e put round trip proves binary survives byte for byte.
- [ ] HP charset translation: `kermit-proto` passes names, data and server
  text as raw bytes (decision log 2026-10-04); hptx-core translates the HP
  charset to and from UTF-8 for names and text.
- [ ] Command length: `Client::start` rejects R/C data over 77 encoded bytes
  (`StartError::TooLong`). Host commands built here (rename, `IOPAR` set,
  long paths) stay under that or are split across several `C` packets, and the
  error is surfaced to the CLI with a clear message.
- [ ] Record `kermit-proto` traces from a 48GX (iteration 2 only has 48SX
  and 49G) and add them to the replay tests.
- [ ] Loss recovery through a transport: an in-memory transport test that
  drops a D packet during GET and checks recovery through the NAK-on-timeout
  path (unit-tested only in iteration 2).

## Acceptance criteria

e2e green on 48SX in CI, and passes locally on 48GX and 49G.
