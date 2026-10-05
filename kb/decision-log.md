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
