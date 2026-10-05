---
type: iteration
title: Iteration 11b
date: 2026-10-05
status: planned
tags:
  - iteration
  -  session hardening and threat model:core
branch: iter-11b/session-hardening
---

# Iteration 11b: session hardening and threat model

From `backlog/stale-reply-after-abort.md`, the second plan review of
2026-10-05 (the in-process transport boots the 48SX only and hand-types
SERVER) and the security discussion of the same day.

## Tasks

- [ ] Stale reply after an aborted command: when a client dies mid host
  command, the calculator finishes it and its reply arrives seconds later
  as the answer to the next client's first command (`bad directory line:
  "Empty Stack"`). Make `Session` robust: track the outstanding command and
  discard a reply that does not match it (sequence or kind), resending the
  command once; cover with an in-memory transport test that injects a stale
  stack reply before the `G D` answer, and with an e2e scenario on the 48SX
  (Ctrl-C a piped REPL mid `1 1000000 START NEXT`, then `hptx ls`).
- [ ] REPL JSON-lines mode for agents: with `--json` and piped stdin, each
  input line produces one JSON object on stdout (`{stack}` or
  `{error,hint,stack}`), instead of refusing `--json`; document it as the
  agent pattern on real hardware (one process, no per-call reconnect, no
  stale-reply race). e2e-cli scenario.
- [ ] In-process saturnus transport on `saturnus-drive`: replace the
  hand-typed SERVER choreography in `hptx-core/src/transport/saturnus.rs`
  with `saturnus_drive::autostart` so the 48SX, 48GX and 49G boot (the 42S
  and 38G/39G/40G are refused: no serial port / no Kermit server), reading
  the LCD row count from the `Lcd` value and matching saturnus enums with a
  wildcard arm; bump the saturnus git pin to a rev that has `saturnus-drive`
  with autostart and the 42S changes. `just e2e-saturnus` passes for all
  three models (ROMs under `~/devel/saturnus/roms/`).
- [ ] `kb/docs/security.md`: the threat model. Untrusted inputs (bytes from
  the calculator over Kermit/XModem, files given to `put`, `object`, `grob`,
  `restore`, REPL and piped lines, `HPTX_PORT`, the ROM file for saturnus);
  invariants (no file written outside the explicit `-o` path or a
  hptx-chosen name in the current directory; calculator-provided names are
  never used as paths without validation; hptx spawns no process; no panic
  or unbounded allocation on crafted input; `--jq` has no I/O); supply chain
  (lockfile, cargo-deny). Verify each invariant against the code while
  writing it and list what is not yet true as tasks for the audit.
- [ ] Decision-log entry: the stale-reply rule and the JSON-lines REPL.

## Acceptance criteria

Gates green; the new unit tests and e2e scenarios pass on the 48SX and
49G; `just e2e-saturnus` green for 48SX, 48GX, 49G; `hyalo lint` clean.
