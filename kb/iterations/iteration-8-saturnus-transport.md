---
type: iteration
title: "Iteration 8: saturnus in-process transport"
date: 2026-10-05
status: in-progress
tags:
  - iteration
  - hptx
branch: iter-8/saturnus-transport
---

# Iteration 8: saturnus in-process transport

Read first: `crates/hptx-core/src/transport.rs`, `crates/hptx-core/tests/e2e.rs`,
`emulator/README.md`; in the sibling saturnus repo
(`~/devel/saturnus`, MIT, a clean-room HP 48SX emulator) its iteration 4
plan, which adds the serial API this transport drives:
`Machine::serial_push`, `serial_drain`, `serial_pending`.

The e2e suite needs a running saturnng container today. saturnus can run
the same 48SX ROM in-process, so the suite (and later unit-speed
calculator tests) can run without Docker.

## Tasks

- [x] Optional dependency `saturnus` behind a `saturnus` feature in
  `hptx-core`: a git dependency on `github.com/ractive/saturnus` pinned to
  the merge commit of saturnus iteration 4 (which adds the serial API). Not
  a path dependency: cargo loads an optional path dependency's manifest
  even with the feature off, so a lone hptx checkout would not build.
- [x] `SaturnusTransport` implementing `Transport`: owns a `Machine` built
  from a ROM path, boots it, answers "Try To Recover Memory?" with NO and
  types `SERVER` (the container's `AUTOSTART`). `write_packet` replays the
  host's wall time outside the transport as emulated time (bounded), then
  queues the packet; `read` runs emulated time until output has gone quiet
  or the timeout's worth of emulated time has passed.
- [x] `transport::open("saturnus://ROM-PATH")` and `open_saturnus(path)`;
  without the feature both return `Error::Emulator`.
- [x] The e2e suite runs in-process unchanged:
  `HPTX_E2E_ADDR=saturnus:///abs/path/sxrom-j cargo test -p hptx-core
  --features saturnus --test e2e`, `just e2e-saturnus`. All six scenarios
  pass.
- [x] Decision-log entry on the time model.

## Acceptance criteria

All six e2e scenarios pass in-process against saturnus with the 48SX ROM J;
the default build, `just lint` and `just test` are unchanged without the
feature.

## Outcome

Done 2026-10-05. `hptx-core` has the `saturnus` feature (git dependency on
`ractive/saturnus`, rev `4fc7573`, the iteration 4 merge),
`transport::SaturnusTransport`, `open_saturnus` and the `saturnus://ROM`
address; `Error::Emulator` for a missing ROM or a build without the
feature. `just e2e-saturnus` runs the suite in-process.

- In-process e2e (48SX ROM J): all six scenarios pass, 17.7 s for the
  suite. One `cargo test` per scenario: ls 4.8 s, get 2.5 s, put round
  trip 2.4 s, run 3.5 s, screenshot 2.5 s, backup 2.6 s. Each opens a
  freshly booted calculator; boot plus `SERVER` takes well under a second
  of wall time.
- The same suite against `saturnus run --serial tcp:4850 --autostart`
  (wall-clock paced) passes in 91 s; against the saturnng container in
  about 65 s. saturnus's ROM code runs slower than saturnng's in emulated
  time (an empty 2000-pass loop takes 48880 ticks there, 145 on
  saturnng); that is a saturnus finding, tracked there.
- Default build, `just lint` and `just test` are unchanged without the
  feature; clippy and tests also pass with it.
