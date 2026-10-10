---
type: iteration
title: "Iteration 14: transport-agnostic, wasm-ready hptx-core"
date: 2026-10-10
status: completed
tags:
  - iteration
  - core
  - wasm
branch: iter-14/wasm-core
---

# Iteration 14: transport-agnostic, wasm-ready hptx-core

hptx-core becomes the protocol core of saturnus-tx, a web and desktop
transfer app in the saturnus repository (owner approval 2026-10-10). In a
browser the bytes arrive through JS callbacks (Web Serial) and nothing may
block, so the core cannot own the read loop. `kermit-proto` and
`xmodem-proto` are already sans-I/O (bytes and `now` in, packets and events
out, `web_time::Instant` on wasm32, iteration 11c); everything above them
(`Session`, `Calculator`, `XmodemSession`) reads a `Transport` with
`std::time` timeouts and sleeps.

Design (decision log 2026-10-10): the logic above the proto crates is
written once as `async` code against a crate-private mailbox (the link):
writes are queued, a read waits for fed bytes or a deadline of the
caller's clock, a pause waits for the clock. No executor and no waker:
whoever owns the I/O polls the future after each feed.

- `machine::Machine` is the sans-I/O face, shaped like
  `kermit_proto::Client`: `start(now, Op)`, `handle_input(now, bytes)`,
  `handle_timeout(now)`, `handle_link_error`, `poll_transmit`,
  `poll_event`, `next_timeout`, `poll_result`. One machine per link
  covers Kermit and XModem.
- `Session`, `Calculator` and `XmodemSession` keep their public API as a
  thin blocking loop over a `Transport` and `Instant::now()`.

## Tasks

- [x] `link` module: the mailbox (`now`, input, queued packets, events,
  wake-up deadline, link error), its read/pause futures and the blocking
  driver; the core never calls `Instant::now()`, sleeps or touches a
  `Transport`.
- [x] Kermit session core: `Session::run`, the drain, the turnaround and the
  discard before host commands as async code on the link; `Session` is a
  transport plus the core.
- [x] Calculator core: every `Calculator` operation as async code; the
  blocking methods drive it. The sync marker comes from a seeded generator
  (the caller's seed in the machine; OS entropy in the blocking API), not
  `SystemTime` (it panics on wasm32-unknown-unknown).
- [x] XModem core: `XmodemSession::run` as async code; the blocking API
  unchanged.
- [x] `machine::Machine`, `Op`, `Reply`, `Progress`; abort.
- [x] Features: `native` (default: serial port, TCP), `saturnus`
  (non-default: in-process emulator); `Error::Serial` behind `native`;
  hptx-cli enables both, so released binaries open `saturnus://`.
- [x] `cargo build -p hptx-core --no-default-features --target
  wasm32-unknown-unknown` passes; CI runs it next to the proto crates'
  check, and clippy on hptx-core without default features.
- [x] Unit tests through `Machine` with an in-memory peer and a fake clock,
  no threads and no sleeps: a Kermit GET that recovers a lost packet by
  timeout, calculator operations (connect, then a listing), an XModem receive,
  an XModem send, a link error and an abort.
- [x] All existing tests unchanged in intent; e2e against saturnus
  in-process on the 48SX, 48GX and 49G. The Docker e2e (TCP, CLI script)
  was not run locally (Docker down); CI runs it.
- [x] Docs: crate docs and README (the embedder API and a browser host
  loop), CHANGELOG, decision log.

## Acceptance criteria

Gates green (`cargo fmt`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace -q`, `cargo deny --locked check`, `hyalo
lint`, the wasm32 checks); `cargo tree -p hptx-core --no-default-features`
names neither serialport nor saturnus; the CLI behaves as before. No
publish and no tag.

## Found

- Bytes the blocking driver read into the link after an operation's last
  read are dropped with the `Session` on `into_transport`; before, they
  stayed in the OS buffer for the next reader. Only the end of a FINISH
  before an XModem transfer is affected, where nothing is expected; the
  `Machine` keeps one link for Kermit and XModem and loses nothing.
- `xmodem::run` (unchanged from before): once the transfer is over and
  the machine has no deadline, a packet `poll_output` still hands out is
  dropped instead of written (`if xfer.poll_output(now).is_none() {
  break; } continue;`). The tests and e2e pass, so likely nothing is queued at that point
  in practice (not verified further); worth a look in the next xmodem iteration.
