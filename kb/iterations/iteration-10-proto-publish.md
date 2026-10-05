---
type: iteration
title: "Iteration 10: kermit-proto and xmodem-proto audit fixes and publishing"
date: 2026-10-05
status: completed
tags:
  - iteration
  - protocol
branch: iter-10/proto-publish
---

# Iteration 10: kermit-proto and xmodem-proto audit fixes and publishing

A whole-crate review of both sans-IO crates (PR #12, a review-only vehicle:
claude 9, glm 6, greptile 7 findings, 18 after merging) before they go to
crates.io. Fix every finding, then prepare the crates for publishing.

## Tasks

- [x] Final-ACK linger (greptile, high): after the client sends the ACK to
  `B` (Kermit) or to EOT (XModem receiver), stay in a short linger state and
  re-ACK a retransmitted `B`/EOT instead of ignoring it; `Done` is still
  emitted once. Tests with a lost final ACK on both sides.
- [x] Kermit receive completion (greptile, high): a `B` that arrives while a
  file is open (no `Z` yet) or while an `X` reply is buffered is an error,
  not `Done`. Test.
- [x] PADC decoding (greptile, high): finding rejected. The Kermit manual
  says a blank Send-Init field means "use the default", and 0x60 is not a
  control character; blank stays NUL, and a PADC outside 0-31/127 falls
  back to NUL. Rule documented on `InitParams::decode` and pinned by a test.
- [x] XModem receiver start deadline (claude, medium): clear the
  start-character deadline once a frame header is buffered so the byte
  timeout governs a frame in progress; never flip the check mode while a
  block is arriving. Test with a block that straddles `crc_interval`.
- [x] Kermit obsolete retry (greptile, medium): drop a queued
  retransmission when the ACK for that packet arrives. Test.
- [x] Recorder (greptile, medium): `record-xmodem` must not discard a live
  start character during its quiet wait, and `--host` must refuse text that
  does not fit one packet.
- [x] XModem sender start characters (claude, low): before the first ACK, a
  NAK/`C`/`D` re-selects the check (and block size) and re-encodes block 1.
  Test.
- [x] Kermit bad-check NAKs count against `retries` (claude, low). Test.
- [x] Receive size cap (claude, glm, low): `Config::max_size` on the XModem
  receiver (default a few MB) failing with CAN and a dedicated error; the
  Kermit receiver gets the same cap. Test.
- [x] `Client::start` clears the previous transaction's queue, events and
  peer params (claude, glm, low); document. Test.
- [x] Deframer resync is linear (glm, low): drop to the next SOH in one
  `drain`. Test with a 1 MiB run of SOH bytes finishing promptly.
- [x] `Packet::encode` returns an error for oversized data instead of a
  `debug_assert` (glm, low); callers adapted.
- [x] API hygiene before publish (claude, low): `#[non_exhaustive]` on the
  public `Event`, `Error`, `Command`, `Config`, `StartError` of both crates;
  docs reference no unpublished crate (describe the HP padding rule in
  `FileEnd` docs instead of `hptx_core::object::strip_padding`).
- [x] Publishing metadata (claude, glm, medium): `description`, `readme`
  (per-crate README with the seam and a driver loop), `keywords`,
  `categories`, `documentation`, `repository` in both manifests;
  `kermit-proto = { path, version }` as xmodem-proto's dev-dependency;
  `cargo publish --dry-run -p kermit-proto` and `-p xmodem-proto` clean
  (`cargo publish --dry-run -p kermit-proto -p xmodem-proto` packages and
  verifies both together; a single-crate dry run of xmodem-proto cannot
  resolve the unpublished kermit-proto dev-dependency).
- [x] No actual publish in this iteration: the user runs `cargo publish`.

## Acceptance criteria

All 18 findings fixed with tests; gates, `cargo doc -D warnings` for both
crates and `cargo publish --dry-run` clean; the hptx-core e2e suite and
`scripts/e2e-cli.sh` still green on the 48SX, 48GX and 49G (the protocol
changes must not regress the emulator runs); PR #12 closed after the merge.
