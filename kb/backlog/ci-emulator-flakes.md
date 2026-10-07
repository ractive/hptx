---
type: backlog
title: "CI emulator flakes: the silent 48SX and the 49G XModem start"
date: 2026-10-06
status: planned
priority: medium
---

# CI emulator flakes: the silent 48SX and the 49G XModem start

Two failure shapes in the CI `e2e` job, both on code that passed the same
suite minutes earlier:

1. **The 48SX container never answers.** Every one of the seven
   `hptx-core` end-to-end tests fails with `Kermit(Timeout)` after its
   full retry budget (about 144 s each, 1006 s for the suite). Seen three
   times on 2026-10-06 (PR #21 run 37394209709, PR #22 run 37420562859,
   and once more on PR #22); each time `gh run rerun <id> --failed`
   passed, never twice in a row. The "Start HP 48SX and
   HP 49G" step passed each time (the serial bridge reported `bridged`),
   so the emulator booted but the Kermit server was not listening: either
   the typed `SERVER` did not land or the calculator was still busy.
2. **The 49G does not start XSEND in time.** `scripts/e2e-cli.sh` fails at
   the XModem step with "the calculator did not start XSEND within 60 s"
   (PR #23 run 37421578335, not rerun: the PR was docs-only and was merged
   with it red). Related to `flaky-xmodem-e2e-under-load.md` (the same
   typed-keys start window), but on a freshly restarted 49G, i.e. after
   that item's fresh-container mitigation.

## Options

- After the start step, probe each container with
  `hptx --timeout 5 --retries 1 info`; if it does not answer, type `SERVER`
  once (as `scripts/e2e-cli.sh` already does for its first command) and,
  if still silent, recreate the container before the suite runs. The
  probe costs seconds; the current failure costs 17 minutes and a rerun.
- Run the two core suites with a shorter per-test timeout in CI so a
  silent calculator fails fast.
- For the XSEND start, raise `--start-timeout` on the 49G step or retry
  the typed keys once (see the other backlog item).

Not a product bug: the same code passes locally and, for the 48SX shape,
on rerun.
