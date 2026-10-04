---
title: "Iteration 3: hptx-core"
type: iteration
date: 2026-10-04
status: completed
branch: iter-3/hptx-core
tags:
  - iteration
  - hptx
---

# Iteration 3: hptx-core

Read first: wiki `protocols/hp-object-format`, `protocols/iopar`,
`protocols/server-commands`; `~/devel/hpcomm/hpcomm/Prot.cpp`, `Filer*.cpp`.

## Tasks

- [x] Transports: `serialport` (9600 8N1, no flow control, assert DTR and RTS
  for early 49G units), TCP (`tcp://host:port`), in-memory for tests. Each
  drives a `kermit-proto` machine; discard pending input for 0.5 s on
  connect. Send each packet as one write (HP overruns on inter-byte gaps of
  ~4 frame times; wiki `hardware/uart`).
- [x] Directory listing: `G D` reply parsing. The 48SX sends variable lines
  only; 48G/49G prepend `{ HOME } 127828` (dir and free memory). The 49G
  prints reals with a trailing dot. Parse both.
- [x] Host commands via `C`: `PATH`, `{ dir } EVAL` to change directory, `VARS`,
  `PGDIR`, `PURGE`, `CRDIR`, rename, `MEM`, `VERSION`, `IOPAR` get/set.
  Returns the stack as text.
- [x] Object format: `HPHP48-x` / `HPHP49-x` header, prolog table (RPLMAN in
  `raw/saturn-hardware/hp-tools-1991/`), object length walk to strip trailing
  padding, type names for `ls`. ASCII transfer format header
  (`%%HP: T(3)A(R)F(.);`, spelling: wiki `questions/ascii-header-spelling`).
- [x] Screenshot: host command producing a GROB (e.g. `PICT RCL` or `LCD->`),
  GROB decode (131x64, rows padded to bytes, LSB = leftmost pixel) to PNG.
- [x] Backup/restore: `ARCHIVE :IO:name` / `RESTORE` over Kermit, binary.
- [x] Tests: unit tests on parsers with recorded replies from all three models;
  `tests/e2e.rs` (one binary; replace the iteration-1 raw I-packet smoke
  test with kermit-proto-based scenarios) with ~6 scenarios: ls, get, put round trip,
  run, screenshot, backup; each runs against whatever model the container
  is (CI runs the 48SX; developers run others locally).

### From iteration 2

- [x] Transfer mode: the fresh 48SX transfers in ASCII mode (bytes 0-26 of a
  binary file did not survive the iteration-2 `48sx-send` round trip).
  hptx-core sets or checks the mode (IOPAR / `TRANSIO`) before put/get, and
  the e2e put round trip proves binary survives byte for byte.
- [x] HP charset translation: `kermit-proto` passes names, data and server
  text as raw bytes (decision log 2026-10-04); hptx-core translates the HP
  charset to and from UTF-8 for names and text.
- [x] Command length: `Client::start` rejects R/C data over 77 encoded bytes
  (`StartError::TooLong`). Host commands built here (rename, `IOPAR` set,
  long paths) stay under that or are split across several `C` packets, and the
  error is surfaced to the CLI with a clear message.
- [x] Record `kermit-proto` traces from a 48GX (iteration 2 only has 48SX
  and 49G) and add them to the replay tests.
- [x] Loss recovery through a transport: an in-memory transport test that
  drops a D packet during GET and checks recovery through the NAK-on-timeout
  path (unit-tested only in iteration 2).

## Acceptance criteria

e2e green on 48SX in CI, and passes locally on 48GX and 49G.

## Outcome

Done 2026-10-05. `hptx-core` has `transport` (serial, TCP, in-memory with
trace replay), `session` (the Kermit driver loop), `charset`, `reply`,
`object`, `grob` and `calc` (`Calculator`). e2e: six scenarios (ls, get,
put round trip of all 256 byte values in binary mode, run, screenshot,
backup) pass on the emulated 48SX, 48GX and 49G in about 62 s each.
Restore is not in e2e (it ends server mode) and was checked by hand on the
48GX and 49G. Parser tests run on replies and objects recorded from all
three models (`crates/hptx-core/fixtures/`); seven 48GX traces were added
to the `kermit-proto` replay tests.

Findings (decision log, iteration 3; `kb/docs/calculator-quirks.md`; wiki
`protocols/server-commands`, `protocols/hp-object-format`): `C` replies are
stack display text and stay on the user's stack; errors come as an
`Error:` line; `ARCHIVE :IO:` fails in server mode, so backup and restore
go through port 0; a command sent right after the previous final ACK is
lost and costs the HP's 6 s timeout (200 ms turnaround fixed e2e from
425 s to 63 s); the directory object layout; `%%HP:` is the header
spelling.

Deviations: `VARS` is not used, because the 49G truncates it; names come
from `G D` listings. The CLI side of the 77-byte limit (a clear message)
moves to iteration 4 with the other items listed there under "From
iteration 3".
