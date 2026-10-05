---
type: backlog
title: XModem e2e on the 49G is flaky under load
date: 2026-10-05
status: planned
priority: medium
---

# XModem e2e on the 49G is flaky under load

Seen 2026-10-05 in CI on a kb-only PR (#14, code identical to main): the
hptx-core `xmodem_round_trip` scenario on the 49G failed with `timed out:
too many retries` after 286 s, while the 48SX run and the six Kermit
scenarios passed. The implementation agents saw the same failure locally
only when the machine's load average was above about 8, and never on a
rerun alone. The scenario types `XRECV`/`XSEND` on the emulated keyboard
through `docker exec calc-keys` after a fixed delay; under load the
calculator is not ready when the keys arrive, or the keys arrive after the
sender's start window.

## Options

- Make the typing robust: wait for the LCD to settle (`calc-screen` shows
  the stack) before typing, and retry the keyboard command once if no start
  character arrives within a few seconds.
- Run the two CI emulators one after the other instead of side by side.
- Mark the scenario as retried once in CI only.

## 2026-10-05, later

In CI the emulated 49G was dead after the hptx-core suite in three runs in
a row (PRs #14 and #16): the CLI script's first `info` got no answer, and
retyping SERVER via `calc-keys` did not revive it. That is the emulator
wedge, not a missed SERVER. CI now gives the CLI script a fresh 49G
container after the core suite; the local scripts keep the SERVER
restart-and-retry on the first command. Root cause still unknown; the
core suite's last scenario is the XModem round trip with its keyboard
SERVER restart, so the ON press during a settling exchange ("Port Not
Available" state, see the quirks checklist) is the leading suspect.
