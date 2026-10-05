---
title: Decision log
type: decisions
date: 2026-10-04
status: reference
tags:
  - decisions
  - hptx
---

# Decision log

Decisions already made. Do not re-litigate; add a dated entry to change one.

## 2026-10-04

- **Name** `hptx`. Rejected: `hpcomm` (unrelated code), `ngcomm`, `hpack`
  (HTTP/2 standard), `hpwire`.
- **License** MIT with `AI_NOTICE`; public repo; GitHub user `ractive`.
- **Language and layout** Rust, edition 2024, stable. Crates `kermit-proto`,
  `xmodem-proto` (generic, sans-IO, publishable), `hptx-core` (HP layer and
  transports), `hptx-cli` (binary `hptx`). The `-proto` suffix follows
  quinn-proto; `kermit` and `xmodem` are taken on crates.io.
- **Sans-IO protocol crates**: bytes in, bytes and events out; the caller owns
  I/O, time and files. The only link property a protocol knows is whether the
  link is 8-bit clean.
- **Target order** HP48SX first, then HP49G (both exist as real hardware here),
  HP48GX in CI only, HP38G/39G/40G last (only Emu48 on Windows emulates them).
- **GUI** Tauri 2 later; no front-end framework chosen; not before the CLI is
  used daily.
- **Existing crates** (`xmodem`, `ymodem`, `rmodem`) are not used: all drive
  blocking I/O themselves, which rules out WASM and in-process emulation.
- **Emulator image** stays build-time with ROMs inside, used locally and in
  CI only, never published.

## 2026-10-04 (iteration 2)

- **Seam takes `now`.** `start`, `handle_input`, `handle_timeout` and
  `poll_output` take an `Instant`, so the protocol owns retransmit deadlines
  and the inter-packet pause (the HP slows down as a file grows) without a
  clock of its own. Same shape planned for `xmodem-proto`.
- **Bytes, not strings.** File names, data, server text and E-packet text
  cross the `kermit-proto` boundary as `Vec<u8>`; HP charset translation is
  `hptx-core`'s job.
- **Command packets always use block check 1.** Verified on the emulator:
  after an I exchange agreed on type 3, a C packet with a type-3 check is
  NAKed. Every transaction starts with type 1; the agreed type applies after
  the S and its ACK.
- **Stale NAK.** A NAK while waiting for the reply to a command packet does
  not trigger an immediate resend; the client waits `nak_grace` (1 s) for the
  real reply first. Resending at once makes the server answer twice and
  desynchronises it.
- **Whole files in memory.** `Command::Send` takes the file contents as
  bytes; calculator objects are small (a full 48GX backup is under 4 MB).
- **Traces.** Emulator traces are recorded with
  `cargo run -p kermit-proto --example record` and replayed as unit tests;
  the format is in `kermit_proto::trace`.

## 2026-10-05 (iteration 3)

- **Host command replies are display text.** A `C` reply is the calculator's
  stack display (the 49G truncates long values and shows lists with commas),
  and results stay on the user's stack. Internal queries (`PATH`, `MEM`,
  `VERSION`, `IOPAR`, `-35 FS?`) read level 1 and then send `DROP`/`DROP2`.
  `hptx run` returns the display text as is.
- **Never evaluate an unchecked name.** Evaluating an undefined name pushes
  it, evaluating a variable runs it. `cd`, `remove` and `rename` look the
  name up in a `G D` listing first; names for directories come from
  listings, not `VARS` (truncated on the 49G).
- **hptx sets the transfer mode.** Every get/put sets flag -35 (binary) or
  clears it (ASCII) first, cached per `Calculator`. A fresh calculator is in
  ASCII mode.
- **Host command text** may use Unicode (`→`) or the calculator's ASCII
  trigraphs (`\->`); hptx translates both to HP bytes, because the
  calculator does not read trigraphs in a `C` packet. Commands hptx builds
  are lists of parts: joined when they fit in one 77-byte packet, sent one
  `C` packet per part otherwise; a part that alone is too long is
  `Error::CommandTooLong`.
- **Backup through port 0.** `ARCHIVE :IO:name` fails in server mode ("Port
  Not Available"). hptx archives HOME to `:0:HPTXBK`, recalls it into a
  temporary directory variable, purges the port object and GETs the
  variable in binary: the backup file is `HPHP4x-x` plus a directory object.
- **Restore through port 0.** hptx sends the backup as `HPTXRS`, stores it
  into `:0:HPTXRS` and runs `:0:HPTXRS RESTORE`. The calculator warm-starts
  and leaves server mode, so the last command gets no reply (a short
  timeout counts as success) and `:0:HPTXRS` stays in port 0 until hptx
  purges it after SERVER runs again. Not in e2e (needs key presses).
- **Turnaround pause.** `Session` waits 200 ms (`Options::turnaround`)
  between transactions: a command sent right after the previous final ACK
  is lost and costs the HP's 6 s timeout.
- **Temporary variable names** `HPTXTMP` (screenshot), `HPTXBK`, `HPTXRS`;
  hptx refuses to run if one already exists.
- **`serialport` without default features**: no libudev on Linux. USB
  details for port auto-pick are iteration 4's call.
- **Recorded fixtures.** Parser tests use replies and objects recorded from
  the three emulated models in `crates/hptx-core/fixtures/`; the object
  walk is checked against the size of every recorded file.

## 2026-10-05 (iteration 4)

- **Port auto-pick by name, no libudev.** `serialport` stays without default
  features. A port is a candidate if serialport reports it as USB or its
  name matches macOS `cu.usbserial*`/`cu.usbmodem*`/`cu.wchusbserial*`/
  `cu.SLAB_USBtoUART*`/`cu.PL2303*`, Linux `ttyUSB*`/`ttyACM*`, or the only
  `COM*` on Windows. Order: `--port`, `HPTX_PORT`, exactly one candidate;
  otherwise the error lists the candidates with a ready `--port` command.
- **CLI output follows hyalo.** Envelope `{dir?, results, total?, hints}`
  with `hints: [{description, cmd}]`; text on a TTY, JSON when piped,
  `--format`/`--json` override, `--jq` (jaq) filters the envelope. Errors go
  to stderr, as `{error, hint}` in JSON mode (`stack` added for a failed
  host command); exit 1, or 2 for usage errors.
- **Directory navigation is stateless.** `--dir PATH` and `ls PATH` take a
  path from HOME (`HOME/A/B`, `A/B`, `{ HOME A B }`) and call core `cd`; the
  calculator stays there afterwards and the help says so.
- **Model detection.** The 49G's `VERSION` says `HP48-C ... Copyright HP
  2009`, so `HP49` or a copyright year of 1999 or later means 49G; `HP48`
  otherwise means 48G/GX; no `VERSION` means 48S/SX.
- **Never silently overwrite.** `put` refuses an existing name (`--overwrite`
  deletes the old variable first, never a directory; `--dry-run` shows the
  plan); `get`, `screenshot`, `backup` refuse to overwrite a local file
  without `--force`; `-o -` writes to stdout.
- **Restore cleanup.** `restore` prompts on a TTY (or needs `--yes`), checks
  offline that the file is a Directory backup, then writes a marker file in
  the temp directory keyed by port; the next hptx command on that port, or
  `restore --cleanup`, purges `:0:HPTXRS`. Because `PURGE` of a missing port
  object raises no error, the CLI checks `:0:HPTXRS VTYPE` (-1 = absent)
  first.
- **`run` arguments.** clap `allow_negative_numbers` (not
  `allow_hyphen_values`, which swallowed a trailing `--format` into the RPL);
  other words starting with `-` go after `--`. An `Error:` reply exits 1
  with the stack in the error object.
- **`--timeout` (20 s) and `--retries` (5)** are exposed; a timeout names
  the port, tries and seconds and hints at "Awaiting Server Cmd.".
- **`object convert` is a round trip through the calculator, not byte
  equality with `get --ascii`.** Supported both ways: Real, Complex, String,
  Binary Integer, 49G Integer, Graphic, and lists of these. Refused with the
  type named: programs, algebraics, names, directories, units, symbolic
  matrices, lists holding ROM pointers (the 48 stores `{ 1 2 }` as two ROM
  pointers), and strings holding CR. `--model 48|49` picks the header and
  whether `5` is a Real or an exact Integer.
- **`settings` skips the store when nothing changed**: storing IOPAR on the
  48SX grew the variable from 29.5 to 37.5 bytes with identical values.
- **Numbers in generated RPL are reals with a trailing dot.** The 49G runs
  in exact mode, so `{ 9600 0 0 0 3 3 } 'IOPAR' STO` stores exact integers;
  the server then stops with "Invalid IOPAR" and answers nothing. `Iopar::to_rpl`
  writes `{ 9600. 0. 0. 0. 3. 1. }` (the 48 reads that as reals too); the
  plain form stays for display.

## 2026-10-05 (iteration 8)

- **saturnus in-process, behind a feature.** `hptx-core` has an optional
  dependency on the saturnus emulator core (feature `saturnus`) as a git
  dependency pinned to a saturnus commit (`rev`). Not a sibling path: cargo
  loads an optional path dependency's manifest even with the feature off,
  so a lone hptx checkout would no longer build. Bump the `rev` to pick up
  saturnus changes.
- **Address `saturnus://ROM-PATH`.** `transport::open` boots an HP 48SX from
  the packed ROM image at that path (`saturnus:///abs/sxrom-j`), answers the
  boot prompt with NO and types `SERVER`, as the container's `AUTOSTART`
  does, then waits (at most 15 s of emulated time) for the idle server's
  first NAK as proof the server runs. A ROM that never shows the boot
  prompt, never settles after a key or never NAKs is `Error::Emulator` from
  `open`, not a link that only times out. Each open boots a fresh
  calculator, so every e2e scenario starts clean. Without the feature the
  address is `Error::Emulator`.
- **Time model of the in-process transport.** No threads and no pacing:
  `read` runs emulated time until the calculator's output has been quiet
  for 4 ms (a packet is sent back to back) or `timeout` worth of emulated
  time has passed; after a timeout it sleeps out the rest of `timeout` in
  wall time, so the host's wall-clock Kermit deadlines see one timeout, not
  many. `write_packet` first replays the wall time the host spent outside
  the transport (at most 2 s) as emulated time: the `Session` turnaround
  pause is time the calculator needs before the next command (a command
  right after the final ACK is lost). The saturnus core is built with
  `opt-level = 3` in the dev profile so the suite does not crawl.

## 2026-10-05 (iteration 5)

- **XModem transfers start on the keyboard, never from the Kermit server.**
  `'NAME' XRECV`/`XSEND` sent as a `C` host command answers "Port Not
  Available" on the 49G and 48GX and the server stays up. hptx ends server
  mode with FINISH (`Calculator::prepare_for_xmodem`), prints the exact
  command to type, waits up to a bounded start timeout (60 s default) and
  tells the user to type SERVER afterwards, as `restore` does.
- **`xmodem-proto` receiver asks with `D` (HP CRC) by default.** `D` is
  CRC-16/KERMIT (the Saturn CRC) sent high byte first. Neither calculator
  answers the standard `C`; the 48GX ignores `D` and the receiver falls back
  to NAK/checksum. The receiver keeps asking for the whole start window
  (`recv_start_timeout`) so a slow typist does not land in checksum mode.
- **1k blocks only when the receiver asked for a CRC** (`checksum_1k`
  default off): the 48GX rejects STX blocks, the 49G takes them. Per-model
  profiles come from `XmodemOptions::for_model`; the 48S/SX is
  `Unsupported`.
- **Received XModem data is stripped with the object walk** and an allowance
  equal to the block size; `XmodemReceived::stripped` is `None` when the
  walk failed and the bytes are returned unchanged. The 49G pads XSEND with
  memory garbage, the 48GX with zeros.
- **XSERV (49g+/50g) is framing and request builders only**, implemented
  from the wiki description and unit-tested; the emulated 49G ROM 2.15 has
  no XSERV, so it is unverified until hardware is available.
- **The driver has a stall watchdog**: no progress for the configured limit
  cancels the transfer, so noise during the block phase cannot hang hptx.
- **CLI XModem surface.** `get`/`put --protocol xmodem` (default stays
  kermit) with `--start-timeout` (1..=600 s); `--ascii`/`--binary` are
  refused with xmodem and `--start-timeout` with kermit. `put` refuses an
  existing name and `--overwrite` (XRECV never replaces: the 49G stores
  `NAME.1`, the 48G/GX errors). The keyboard instructions go to stderr in
  text mode; in JSON mode they are available through `--dry-run` (the JSON
  result carries `keys`, `instructions`, `check`, `model`, `stripped`,
  `server_mode: false`), because stderr in JSON mode is reserved for
  `{error,hint}` and a human has to type on the calculator anyway.
- **`hptx xserv ls|get|put|eval|mem`** is a thin client over
  `hptx_core::xserv`, marked UNVERIFIED ON HARDWARE in every help page.
- **Model detection lives in hptx-core** (`Model::from_version`); the CLI's
  own copy was removed in iteration 5.

## 2026-10-05 (iteration 9)

- **`hptx repl`.** One `Calculator` per session; every non-colon line goes
  through the `run` path (77-byte split, same error mapping) and the reply
  is printed as the calculator displays it, deepest level first, verbatim;
  an empty stack prints nothing; a calculator error prints its message and
  the stack it left, and the session continues. Only link failures end the
  session (exit 1). Plain `> ` prompt, no header or softkey mimicry.
- **Colon meta-commands** (`:ls`, `:cd`, `:get`, `:put`, `:rm`, `:pict`,
  `:info`, `:help`, `:quit`) reuse the CLI command code on the open link;
  `::` sends a line that starts with a colon verbatim. `--json`/`--jq` are
  refused in the REPL. Piped stdin runs without prompt or editing and exits
  at EOF, so scripts and agents can drive it.
- **`rustyline` without default features** (`with-file-history` only) for
  editing and history; history under the platform data dir
  (`$XDG_DATA_HOME/hptx/history`, `~/Library/Application Support/hptx/history`,
  `%APPDATA%\hptx\history.txt`).
- **`screenshot` is replaced by `pict`.** Through the Kermit server `LCD→`
  can only ever capture the server banner; `pict` fetches the graphics
  screen with `PICT RCL` (via `HPTXTMP`). The display itself is captured by
  hand (`LCD→ 'S' STO` before SERVER, then `get` and `grob to-png`).

## 2026-10-05 (publishing and the relation to saturnus)

- **Only `kermit-proto` and `xmodem-proto` are published to crates.io.**
  hptx-cli stays git-only (release binaries, `cargo install --git`):
  publishing it would chain every hptx release behind a saturnus release
  for the small gain of `cargo install hptx`. Dropped (user).
- **hptx is the cable tool.** CLI and REPL over Kermit and XModem, for real
  hardware and for emulators treated as hardware. Agents drive a real
  calculator through the CLI's JSON mode; there is no hptx MCP server.
- **Two repositories, not a monorepo.** Different products, cadences and
  knowledge bases. The crate graph is acyclic today: saturnus-mcp depends
  on hptx-core (with hptx-core's `saturnus` feature off), hptx-core's
  optional in-process transport depends on the saturnus core crate; the
  mutual git pins are a coordination cost, not a build cycle.
- **Open (two plan reviews on 2026-10-05):** whether saturnus-mcp stays as
  the agent's emulator session (depending on hptx-core, the Kermit layer
  being needed for execution and error text) or is dropped in favour of
  `saturnus run` plus the hptx CLI; and whether hptx-core's `object.rs`
  adopts the new `saturnus-objects` decoder (it covers the body decoder,
  not the file headers, padding, charset and reply parsers). Until decided:
  no adapter crate, no `hptx-transport` crate, no `:lcd`; the in-process
  transport stays in hptx-core and should use saturnus-drive's autostart
  so it boots the 48SX, 48GX and 49G.
- **Whole-crate audits at milestones** (user): before publishing a crate,
  before the hardware iteration and before a release, a review-only vehicle
  PR branched from the iteration 1 merge with the current sources copied on
  is reviewed by all three reviewers; one regression test per finding; no
  fuzz or adversarial suites. The hardware iteration precedes the first
  release.

## 2026-10-05 (iteration 10)

- **Linger after the final ACK.** Both protocol machines keep answering a
  retransmitted `B` (Kermit) or EOT (XModem) for `Config::linger` after the
  final ACK (default 1 s, `ZERO` off, deadline fixed at the start); `Done`
  is emitted once; `is_idle()` is false meanwhile; `start()` ends it. The
  HP retransmits a lost-ACK `B` only after the TIME it was sent (20 s), so
  the default catches an immediately damaged ACK, not a long silence.
  `Session` sets the Kermit linger to its turnaround (200 ms) and starts the
  turnaround at `Done`, so no transaction got slower.
- **Receive size caps.** `Config::max_size` (default 4 MiB) in both crates;
  `Error::TooLarge { limit }`, CAN CAN CAN or an E packet.
- **A `B` before the file's `Z`, or with `X` text buffered, is a protocol
  error**, not `Done`.
- **PADC**: a blank Send-Init field is the default (NUL), per the Kermit
  manual; a reviewer's claim that a space means pad byte 0x60 was rejected.
- **`Packet::encode` and `encode_block` return `Result`** with
  `codec::EncodeError`; `#[non_exhaustive]` on the public enums and configs
  of both crates, including `Check`, `BlockSize`, `BlockCheck` and
  `FrameError` after the PR #13 review (so `Config { x, ..Default::default() }`
  is no longer possible downstream; set fields on a default value instead);
  `start()` drops the previous transaction's queue, events and peer
  parameters.
- **A NAK before the first ACK only switches to checksum from HP's `D` mode**
  (the 49G's fallback); a standard CRC-16 receiver that NAKs a damaged first
  block gets the same CRC block again (PR #13 review). Re-ACKs during the
  linger never pile up: at most one is queued. The XModem driver's linger is
  short like the Kermit one, and a transport error during the linger after a
  completed transfer does not discard the transfer.
- **XModem sender before the first ACK** re-selects the check and block size
  from any start character (49G falls back D -> NAK); the receiver holds the
  start-character deadline while a block is arriving.
- **Publishing**: `cargo publish --dry-run -p kermit-proto -p xmodem-proto`
  is the verification and the publish command (both at once); the user
  publishes. No `rust-version` until an MSRV is verified.

## 2026-10-05 (iteration 11b)

- **Sync on connect, never resend.** A host command whose client died is
  still finished by the calculator, and its reply (an S packet offered every
  5 s for about a minute) is taken as the answer to the next client's first
  command. Packet sequences restart at zero for every command and host
  commands and `G D` both answer with text, so a late reply cannot be told
  apart on the wire. Every connection (`Calculator::open`, every CLI
  command, the REPL) starts with `Calculator::sync`: a sacrificial `PATH`
  query whose first reply is accepted whatever it is, asked once more if
  that reply is not a path; only a path answer is dropped. hptx never
  resends `run` or any mutating command. Cost: two short transactions per
  connection. Closes `backlog/stale-reply-after-abort`.
- **REPL JSON lines.** `hptx repl --json` with piped stdin writes exactly one
  compact JSON object per input line on stdout, in order: `{"stack": [...]}`
  (level 1 first, `[]` when empty); `{"error","hint","stack"}` for a
  calculator error; `{"error","hint"}` for other errors; `{"results": ...}`
  for a colon command (the CLI command's results value, wrapped so a line is
  always an object; no `total` or `hints`, whose `hptx ...` wording is wrong
  inside the REPL); `{}` for a blank line; `{"quit": true}` for `:quit`.
  Errors go to stdout deliberately, unlike the CLI rule, so a reader sees
  results and errors in order; a link failure is the last object and hptx
  exits 1. `--jq` stays refused, and so does `--json` on a terminal. This is
  the agent pattern on real hardware: one process, no reconnect per call.
  Supersedes the iteration 9 entry "`--json`/`--jq` are refused in the REPL".
- **`saturnus://MODEL@ROM` boots the 48SX, 48GX or 49G.** The model is part
  of the address because a ROM path alone is ambiguous (the 38G and 48GX are
  both 512 KB); names are `48sx`, `48gx`, `49g`, and a bare path means
  `48sx`. hptx-core boots through `saturnus_drive::autostart` at saturnus
  rev `a6e7e95` and refuses the 42S (no serial port) and the 38G/39G/40G (no
  Kermit server) with `Error::Emulator`. `just e2e-saturnus MODEL` picks the
  ROM. Replaces the iteration 8 entry "Address `saturnus://ROM-PATH` boots an
  HP 48SX".
- **Threat model** lives in `docs/security.md`; its "Not yet true" list is
  the input for the next whole-hptx audit.
