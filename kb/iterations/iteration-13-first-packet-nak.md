---
type: iteration
title: "Iteration 13: immediate NAK of the first packet"
date: 2026-10-08
status: completed
tags:
  - iteration
  - kermit
branch: iter-13/first-packet-nak
---

# Iteration 13: immediate NAK of the first packet

saturnus (its iteration 18, "Outcome") found that the in-process 48SX NAKs a
`C` packet 11 ms after it went out and never answers it: the calculator
rejected the packet, the command did not run. Trace, link clock in seconds:

```text
 0.000 > \x01& C2_m)\r          our C packet, seq 0
 0.011 < \x01# N3\r             NAK seq 0, 11 ms later
 1.011 > \x013 EToo many retries_\r
```

`first_packet_retries = Some(0)` (iteration 12) forbids the resend, so the
command fails; in hptx (`nak_grace = timeout`) it becomes `NoReply` after
the full `--timeout` (20 s) on a command that never ran. The idle server's
periodic NAK seq 0 is byte-identical, and one that crosses our `C` means the
server did take it, so a NAK alone must not allow a resend. The design
(decision log 2026-10-08) tells them apart by time: a NAK inside a window
of the packet's time on the wire plus slack is a rejection.

## Tasks

- [x] kermit-proto: `Config::first_packet_nak_window: Option<Duration>` and
  `Config::first_packet_nak_grace: Option<Duration>` (default `None`:
  behaviour unchanged; additive, `#[non_exhaustive]` and `Default` kept),
  documented on the fields, in the crate docs and in the README.
- [x] kermit-proto: in `Phase::Await`, when `first_packet_retries` forbids a
  resend, a NAK seq 0 within the window after `poll_output` handed the
  packet out allows exactly one resend, after the immediate-NAK grace and
  only if no answer arrived; later NAKs, a second NAK and timeouts keep the
  `Some(0)` behaviour.
- [x] kermit-proto tests replaying the saturnus trace:
  `immediate_nak_allows_one_resend`, `immediate_nak_then_s_is_not_resent`,
  `nak_after_the_window_is_not_resent`,
  `two_immediate_naks_allow_only_one_resend`,
  `nak_window_does_not_resend_after_a_timeout`.
- [x] hptx-core `host_once`: `first_packet_retries = Some(0)` and
  `nak_grace = timeout` as before, plus `HOST_NAK_WINDOW` (250 ms) and
  `HOST_NAK_GRACE` (3 s, capped by the timeout).
- [x] hptx-core tests: `host_command_rejected_at_once_is_resent` (new);
  `host_command_is_never_resent` now delivers its crossing NAK after the
  window (same intent: a periodic NAK never resends);
  `host_reply_packets_keep_their_retries` unchanged.
- [x] Workspace version 0.1.1 (path-dependency versions, `Cargo.lock`).
- [x] kb: decision log 2026-10-08, `docs/security.md` invariant 6,
  `docs/calculator-quirks.md` (the immediate-NAK case, the residual risk).
- [x] Gates, e2e on the 48SX container (4848), `just e2e-saturnus 48sx`.

## Tasks: PR #26 review

- [x] #1 (high) Stale idle NAKs buffered between host commands were read
  right after the `C`, inside the window: `Session` discards waiting input
  before each host command (`input_between_host_commands_is_discarded`),
  and `Config::first_packet_nak_byte_time` puts a lower bound (the
  packet's wire time) on the window (`nak_before_the_wire_time_is_stale`;
  hptx-core: 10 bits at 9600 baud per byte).
- [x] #2 (medium) `host_command_is_never_resent` back to its original shape
  (NAK in the same write as the `C`), passing through the lower bound; the
  300 ms variant is `host_command_late_nak_is_not_resent`.
- [x] #3 (low) A NAK while the granted resend waits keeps its deadline
  (`nak_after_the_grant_keeps_the_grace`).
- [x] #4 (low) `host_command_rejected_at_once_is_resent` uses a 300 ms
  session timeout (the grace is capped by it) and checks the wiring only.
- [x] #5 (low, GLM) Docs say the resend is cancelled by any answer (`S`,
  short reply or `E`), not only an `S`.
- [x] Decision log, security.md invariant 6 and the quirk updated for the
  discard, the lower bound and the narrower residual risk; gates and e2e
  rerun.

## Acceptance criteria

Gates green (`cargo fmt`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace -q`, `cargo deny --locked check`, `hyalo
lint`, `cargo check -p kermit-proto -p xmodem-proto --target
wasm32-unknown-unknown --locked`); one regression test per behaviour, each
commented with what it covers; the iteration 12 tests pass unchanged in
intent; `just e2e` and `just e2e-cli tcp://localhost:4848` pass on the 48SX
container and `just e2e-saturnus 48sx` in process.
