# hptx plan

Status: skeleton only (2026-10-04). Nothing built yet except `emulator/`.

## Goal

`hptx` lets a person or an AI agent list, fetch, store, run and back up
objects on an HP48/49 (later 38/39/40) over a USB-serial adapter, from macOS,
Windows and Linux. A GUI follows once the CLI is solid.

## Architecture

```
kermit-proto   sans-IO Kermit state machine (generic, publishable)
xmodem-proto   sans-IO XModem (+1k, CRC, HP variants, XSERV framing)
hptx-core      HP layer: server commands, object format, IOPAR, GROB,
               transports (serial via `serialport`, TCP, in-memory)
hptx-cli       binary `hptx`
```

Sans-IO seam, same for both protocol crates:

```rust
fn handle_input(&mut self, bytes: &[u8]);
fn handle_timeout(&mut self, now: Instant);
fn poll_output(&mut self) -> Option<Vec<u8>>;
fn poll_event(&mut self) -> Option<Event>;   // FileStart, Data, FileEnd, Error, Done
fn next_timeout(&self) -> Option<Instant>;
```

Files cross the boundary as events and chunks. The only link property the
protocol must know is whether the link is 8-bit clean (Kermit prefixing);
baud, parity and port names belong to the transport.

## Milestones

### M1 Skeleton and CI

- Cargo workspace with the four crates (empty libs, `hptx --version`).
- `justfile`: `test`, `e2e`, `lint` (clippy + fmt), `emulator-up/down`.
- GitHub Actions: job 1 fmt+clippy+`cargo test`; job 2 builds the
  `emulator/` image for the 48SX, starts it, waits for the `bridged` log
  line, runs `cargo test -p hptx-core --test e2e` with `HPTX_E2E_ADDR`.
  Cache cargo, not the Docker image (it contains ROMs).
- README stub. Acceptance: CI green on both jobs.

### M2 kermit-proto

Read first: wiki `protocols/kermit`, `protocols/kermit-hp`,
`protocols/server-commands`; `~/devel/hpcomm/hpcomm/Kermit.cpp` for behaviour.

- Packet codec: SOH, LEN, SEQ, TYPE, DATA, CHECK, EOL; block check 1, 2, 3
  (type 3 is CRC-CCITT LSB-first, poly #1081, same as the calculator's CRC
  register; wiki `hardware/crc`).
- Prefixing: control (`#`), 8th bit (`&`), repeat (`~`); `tochar/unchar/ctl`.
- Send-init negotiation from the HP's reply `~& @-# 1` (MAXL, TIME, NPAD,
  PADC, EOL, QCTL, QBIN, CHKT, REPT); no long packets, no windows.
- Client side only (we are the host): send file(s), receive file(s), generic
  commands `G D`, `G F`, `G L`, host command `C`, and the `I` exchange.
- Events: FileStart{name}, Data, FileEnd, ServerText (reply to C / G D),
  Error{packet text}, Done. The ACK to the F packet carries the name the
  calculator actually used; surface it.
- Timeouts and retries per the manual, but the HP slows down as a received
  file grows (System RPL copies on every packet): default timeout 20 s and a
  configurable inter-packet pause.
- Tests: hand-written byte traces for each packet type and block check;
  traces recorded from the emulator (M1's container) for a full `C "6 7 *"`,
  `G D`, GET and SEND; the stale-NAK-on-connect case (discard pending input
  for ~0.5 s after connect is a transport concern, but the state machine must
  survive an unsolicited NAK before S).
- Acceptance: all traces pass; crate has no I/O dependencies; `cargo doc`
  explains the seam.

### M3 hptx-core

Read first: wiki `protocols/hp-object-format`, `protocols/iopar`,
`protocols/server-commands`; `~/devel/hpcomm/hpcomm/Prot.cpp`, `Filer*.cpp`.

- Transports: `serialport` (9600 8N1, no flow control, assert DTR and RTS
  for early 49G units), TCP (`tcp://host:port`), in-memory for tests. Each
  drives a `kermit-proto` machine; discard pending input for 0.5 s on
  connect. Send each packet as one write (HP overruns on inter-byte gaps of
  ~4 frame times; wiki `hardware/uart`).
- Directory listing: `G D` reply parsing. The 48SX sends variable lines
  only; 48G/49G prepend `{ HOME } 127828` (dir and free memory). The 49G
  prints reals with a trailing dot. Parse both.
- Host commands via `C`: `PATH`, `{ dir } EVAL` to change directory, `VARS`,
  `PGDIR`, `PURGE`, `CRDIR`, rename, `MEM`, `VERSION`, `IOPAR` get/set.
  Returns the stack as text.
- Object format: `HPHP48-x` / `HPHP49-x` header, prolog table (RPLMAN in
  `raw/saturn-hardware/hp-tools-1991/`), object length walk to strip trailing
  padding, type names for `ls`. ASCII transfer format header
  (`%%HP: T(3)A(R)F(.);`, spelling: wiki `questions/ascii-header-spelling`).
- Screenshot: host command producing a GROB (e.g. `PICT RCL` or `LCD->`),
  GROB decode (131x64, rows padded to bytes, LSB = leftmost pixel) to PNG.
- Backup/restore: `ARCHIVE :IO:name` / `RESTORE` over Kermit, binary.
- Tests: unit tests on parsers with recorded replies from all three models;
  `tests/e2e.rs` (one binary) with ~6 scenarios: ls, get, put round trip,
  run, screenshot, backup; each runs against whatever model the container
  is (CI runs the 48SX; developers run others locally).
- Acceptance: e2e green on 48SX in CI, and passes locally on 48GX and 49G.

### M4 hptx CLI

- Commands: `ports`, `info`, `ls`, `get`, `put`, `rm`, `mkdir`, `mv`,
  `run`, `screenshot`, `backup`, `restore`, `settings`, `finish`, and
  offline `object inspect`, `object convert` (binary <-> ASCII where the
  object is text-representable), `grob to-png`.
- `--port` accepts a device path or `tcp://`; `HPTX_PORT` env var; auto-pick
  when exactly one USB serial port exists.
- Output per the conventions in `CLAUDE.md`. `--json` is implied when piped.
- Acceptance: the M3 e2e scenarios re-expressed as a shell script driving the
  binary (one script, run in the same CI job); `hptx --help` reads well for a
  human; an agent can do `hptx ls --json | jq`.

### M5 xmodem-proto and HP XModem

Read first: wiki `protocols/xmodem`, `xmodem-hp`, `xserv`,
`questions/xmodem-hp-crc-mode`; `~/devel/hpcomm/hpgcomm/XModem.cpp`.

- 128-byte and 1k blocks, checksum and CRC-16 (MSB-first #1021), receiver
  start characters NAK / `C`, fallback to checksum after failed CRC attempts
  (the 48G has no CRC at all).
- HP padding: strip trailing bytes using the object-length walk; the 49G
  rejects objects with more than ~255 bytes of padding.
- XSERV (49g+/50g) framing: 2-byte big-endian length, data, 1-byte sum;
  commands P, G, E, M, L. Facts came from HP-written Conn4x code under a
  non-commercial license: implement from the wiki description only.
- CLI: `hptx get/put --protocol xmodem`, `hptx xserv ...`.
- Acceptance: e2e on the 49G container (XRECV/XSEND); 39G/40G aplet upload
  verified on real hardware or Emu48 on Windows when available.

### M6 Real hardware

Manual checklist, run when the USB-serial adapter arrives: 48SX and 49G,
every CLI command, both protocols on the 49G, timing on long transfers,
early-49G DTR/RTS behaviour. File every surprise into the wiki.

### M7 GUI (deferred)

Tauri 2, front-end undecided, two-pane filer on `hptx-core`. Not before M4
is used daily.

## Known quirks checklist (verify each in tests)

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
