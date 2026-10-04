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
