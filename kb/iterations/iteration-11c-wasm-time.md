---
type: iteration
title: "Iteration 11c: web-time on wasm32 for the proto crates"
date: 2026-10-05
status: in-progress
tags: [iteration, protocol]
branch: iter-11c/wasm-time
---

# Iteration 11c: web-time on wasm32 for the proto crates

`kermit-proto` and `xmodem-proto` are sans-IO and never read a clock, but
their seam takes `std::time::Instant`, which a `wasm32-unknown-unknown`
caller cannot construct (`Instant::now()` panics there). The saturnus web UI
will use `kermit-proto` as a wasm dependency. Owner decision (2026-10-05):
`web-time` on wasm32 only, `std::time` everywhere else, so the API is
unchanged on native targets and `web_time::Instant` is the crates' `Instant`
on wasm32.

## Tasks

- [x] Both crates: a public `time` module re-exporting `Duration` and `Instant` from `std::time` (not wasm32) or `web_time` (wasm32), used throughout; `web-time = "1.1.0"` as a wasm32-only target dependency
- [x] CI: ubuntu leg of the check job installs `wasm32-unknown-unknown` and runs `cargo check -p kermit-proto -p xmodem-proto --target wasm32-unknown-unknown --locked`; actions stay SHA-pinned; `cargo deny --locked check` clean
- [x] Docs: "Time and WebAssembly" in both READMEs and `lib.rs` docs; `RUSTDOCFLAGS=-Dwarnings cargo doc -p kermit-proto -p xmodem-proto --no-deps` clean
- [x] `cargo publish --dry-run -p kermit-proto -p xmodem-proto` passes
- [x] Gates: fmt, clippy, tests (all trace replays), wasm32 check, deny, doc build, `hyalo lint`

## Acceptance

- hptx-core and hptx-cli compile unchanged.
- No new dependency on native targets; on wasm32 `web-time` brings
  `wasm-bindgen` and `js-sys` (MIT/Apache-2.0).
