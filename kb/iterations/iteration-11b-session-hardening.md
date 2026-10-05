---
type: iteration
title: "Iteration 11b: session hardening and threat model"
date: 2026-10-05
status: completed
tags:
  - iteration
  - core
branch: iter-11b/session-hardening
---

# Iteration 11b: session hardening and threat model

From `backlog/stale-reply-after-abort.md`, the second plan review of
2026-10-05 (the in-process transport boots the 48SX only and hand-types
SERVER) and the security discussion of the same day.

## Tasks

- [x] Stale reply after an aborted command: when a client dies mid host
  command, the calculator finishes it and its reply arrives seconds later
  as the answer to the next client's first command (`bad directory line:
  "Empty Stack"`). Packet sequences restart at zero for every command and
  both host commands and `G D` answer with text, so a late reply cannot be
  told apart by sequence or kind, and resending a command after discarding
  a reply would repeat mutations (`DROP`, arbitrary RPL). Rule: at session
  start, after the 0.5 s drain, push a session-unique marker string
  (`HPTX-` and six random hex digits); a reply showing it at level 1 is
  ours and only the marker copies on top are dropped; any other reply, an
  E packet or a first-attempt timeout (one timeout period, no retries)
  sends the marker once more with the normal budget. A `PATH` query was
  the first design, rejected in the PR #16 review (the 49G cuts long
  paths; a path-shaped late reply would be dropped). Never resend a `run`
  or any mutating command. Cover with an in-memory transport test that injects a
  stale stack reply before the first answer, and with an e2e scenario on
  the 48SX (Ctrl-C a piped REPL mid `1 1000000 START NEXT`, then `hptx ls`).
- [x] REPL JSON-lines mode for agents: `hptx repl --json` with piped stdin
  emits exactly one JSON object per input line on stdout, in input order:
  an RPL line gives `{"stack": [...]}` (empty list for an empty stack), a
  calculator error gives `{"error", "hint", "stack"}` on stdout as well
  (deliberately not stderr, so a reader sees results and errors in order;
  this supersedes the CLI rule for this mode), a meta-command gives the
  object the CLI command emits, a blank line gives `{}`, `:quit` ends the
  stream after `{"quit": true}`. No `{results,total,hints}` envelope per
  line; `--jq` is refused in the REPL. Interactive (TTY) `--json` stays
  refused. This supersedes the iteration 9 entry "`--json`/`--jq` are
  refused in the REPL". Document it in `repl --help` and README as the agent
  pattern on real hardware (one process, no per-call reconnect, no
  stale-reply race). Unit tests and an e2e-cli scenario.
- [x] In-process saturnus transport on `saturnus-drive`: the address
  carries the model, `saturnus://<model>@<abs-rom-path>` with hptx's model
  names (`48sx`, `48gx`, `49g`; a bare path means `48sx` for compatibility),
  because a ROM path alone is ambiguous (the 38G and 48GX are both 512 KB)
  and `saturnus_drive::rom::load(model, path)` takes the model. Replace the
  hand-typed SERVER choreography in `hptx-core/src/transport/saturnus.rs`
  with `saturnus_drive::autostart::autostart_script(model, fresh_boot)`
  driven through the drive session, so the 48SX, 48GX and 49G boot; refuse
  the 42S and the 38G/39G/40G with `Error::Emulator` (no serial port / no
  Kermit server); read the LCD row count from the `Lcd` value; wildcard
  arms on saturnus enums; bump the saturnus git pin to a rev that has
  `saturnus-drive` with autostart and the 42S changes. `just e2e-saturnus`
  takes a `model` parameter and picks the ROM (`sxrom-j`, `gxrom-r`,
  `rom.49g` under `~/devel/saturnus/roms/`); it passes for all three. The
  decision-log entry replaces the iteration 8 entry "Address
  `saturnus://ROM-PATH` boots an HP 48SX".
- [x] `kb/docs/security.md`: the threat model. Untrusted inputs (bytes from
  the calculator over Kermit/XModem, files given to `put`, `object`, `grob`,
  `restore`, REPL and piped lines, `HPTX_PORT`, the ROM file for saturnus);
  invariants (no file written outside the explicit `-o` path or a
  hptx-chosen name in the current directory; calculator-provided names are
  never used as paths without validation; hptx spawns no process; no panic
  or unbounded allocation on crafted input; `--jq` has no I/O); supply chain
  (lockfile, cargo-deny). Verify each invariant against the code while
  writing it and list what is not yet true as tasks for the audit.
- [x] Decision-log entry: the stale-reply rule and the JSON-lines REPL.

## Acceptance criteria

Gates green; the new unit tests and e2e scenarios pass on the 48SX and
49G; `just e2e-saturnus` green for 48SX, 48GX, 49G; `hyalo lint` clean.
