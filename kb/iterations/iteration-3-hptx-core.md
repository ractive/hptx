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
  `tests/e2e.rs` (one binary) with ~6 scenarios: ls, get, put round trip,
  run, screenshot, backup; each runs against whatever model the container
  is (CI runs the 48SX; developers run others locally).

## Acceptance criteria

e2e green on 48SX in CI, and passes locally on 48GX and 49G.
