---
title: Test policy
type: docs
date: 2026-10-04
status: active
tags:
  - testing
  - hptx
---

# Test policy

Tests must help without slowing the project down (lesson from ff-rdp and hyalo).

- Protocol crates: unit tests only, byte traces in, bytes/events out.
  Milliseconds. No mocks of serial ports.
- At most one integration test binary per crate (`tests/e2e.rs` with modules).
  Never one file per scenario: cargo links each file into its own binary.
- End-to-end tests against the emulator run only when `HPTX_E2E_ADDR`
  (e.g. `tcp://localhost:4848`) is set, and only in a separate CI job. A
  handful of scenarios per model, covering what unit tests cannot.
- No heavy dev-dependencies in the sans-IO crates. No fuzzing, property or
  snapshot tests until a bug justifies one.
- `just test` runs the fast suite; `just e2e` the emulator suite.
- Whole-crate audits at milestones instead of adversarial suites
  (2026-10-05): before publishing a crate, before the hardware iteration and
  before a release, a review-only PR branched from the iteration 1 merge
  (`26618c4`) with the current sources copied on is reviewed by the
  `review-pr` skill's three reviewers (claude, glm, greptile); findings are
  fixed in a normal iteration with one regression test each; the vehicle PR
  is closed. No fuzz, property or adversarial suites until a bug justifies
  one.
