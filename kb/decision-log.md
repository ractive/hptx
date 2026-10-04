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
