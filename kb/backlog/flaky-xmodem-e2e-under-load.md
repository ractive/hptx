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
