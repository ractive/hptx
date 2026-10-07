---
type: backlog
title: "CI emulator flakes: the silent 48SX and the 49G XModem start"
date: 2026-10-06
status: planned
priority: medium
---

# CI emulator flakes: the silent 48SX and the 49G XModem start

Two failure shapes in the CI `e2e` job on 2026-10-06, both on code that
had passed CI before (for the 48SX shape the same suite minutes earlier in
a run the concurrency group then cancelled; for the XSEND shape main's run
after the merge):

1. **The 48SX container never answers.** Every one of the seven
   `hptx-core` end-to-end tests fails with `Kermit(Timeout)` after its
   full retry budget (about 144 s each, 1007 s for the suite). Seen twice,
   each time on attempt 1 (PR #21 run 37394209709, PR #22 run
   37420562859); `gh run rerun <id> --failed` passed both times. The
   "Start HP 48SX and HP 49G" step passed each time and the container log
   ends with `Kermit server started (SERVER)` then `bridged` exactly like
   a healthy start, so the emulator booted but the Kermit server was not
   answering. The existing diagnostics gave no evidence: the
   "Screens and logs on failure" step's `calc-screen` dump of the 48SX is
   the same unreadable bitmap in healthy and failing runs (it works for
   the 49G only), and the boot log is identical to a healthy start.
2. **The 49G does not start XSEND in time.** `scripts/e2e-cli.sh` fails at
   the XModem step with "the calculator did not start XSEND within 60 s"
   (PR #23 run 37421578335, not rerun: the PR was docs-only and was merged
   with it red). Related to `flaky-xmodem-e2e-under-load.md` (the same
   typed-keys start window), but on a freshly restarted 49G, i.e. after
   that item's fresh-container mitigation. The failing step took 12
   minutes, of which the 60 s window was a small part: the rest was the
   script's cleanup (`restart_server`, then hptx commands against a
   calculator possibly out of server mode).

## Options

- After the start step, probe each container with
  `hptx --timeout 5 --retries 1 info`; if it does not answer, capture the
  `info` error, `docker logs` and a working screen dump (fix
  `emulator/calc-screen.sh` for the 48SX or dump the LCD another way) so
  the next occurrence yields a root cause, then type `SERVER` once (as
  `scripts/e2e-cli.sh` already does for its first command) and, if still
  silent, recreate the container before the suite runs. The probe costs
  seconds and tells a dead calculator from a slow one; the current failure
  costs 17 minutes and a rerun.
- For the XSEND start: make the start timeout an environment variable in
  `scripts/e2e-cli.sh` (it is a literal `--start-timeout 60` today) or
  retry the typed keys once (see the other item), and make the cleanup
  after a missed start bail out fast instead of running the full tail.
- A shorter Kermit budget for the core suite is not a CI knob: `tests/e2e.rs`
  uses `Options::default()`; it would need an `HPTX_E2E_TIMEOUT`-style
  variable, and a shorter budget worsens the load flake the other item
  records. Prefer the probe.

Not a product bug: the same code passes locally and, for the 48SX shape,
on rerun.
