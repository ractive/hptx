---
type: iteration
title: "Iteration 12: audit fixes"
date: 2026-10-06
status: planned
tags:
  - iteration
  - core
  - cli
  - security
branch: iter-12/audit-fixes
---

# Iteration 12: audit fixes

The first whole-hptx milestone audit (PR #18, review-only vehicle, 2026-10-06:
claude 6 findings, GLM 3, Greptile 9, security-review skill 2; 17 distinct)
plus the "Not yet true" list of `docs/security.md`. One regression test per
finding (`docs/test-policy.md`); no fuzz suites (decision log 2026-10-05).
Numbers refer to the table in the PR #18 review comment.

## Tasks: calculator-side correctness

- [ ] #1 (high; claude, glm, greptile) A Kermit `C` packet is retransmitted on
  timeout and 1 s after a stale NAK (`Session::run` -> `handle_timeout` ->
  kermit-proto `retry`; `nak_grace`), so every host command executes twice
  when its ACK is late or lost: `run`, REPL lines, `rm`, `mv`, `mkdir`,
  `put --overwrite`, `settings`, and every internal `DROP`/`DROP2` after a
  query. Fix in `Calculator::host`: send `Command::Host` with a per-call
  config of `retries = 0` and `nak_grace = timeout` (the pattern `sync_with`
  and `restore` already use) so a `C` is never resent; a timeout becomes an
  error that says the command may still be running or may have run, and
  that the next connection resyncs. `G D`, `R` and `S` keep their retries.
  Regression test with the in-memory transport: delay the reply past the
  timeout and past a crossing NAK, assert exactly one `C` on the wire.
  Supersedes `backlog/host-command-double-execution.md` (mark completed).
- [ ] #2 (high; greptile) `Calculator::sync` returns `Ok` when neither
  attempt shows the marker. Return `Error::Protocol`-class failure with a
  hint (press ON/ATTN, retry) instead; test with two odd replies.
- [ ] #3 (high; greptile) `Calculator::restore` checks only the Directory
  prolog of the backup, not that the object-size walk succeeded; a truncated
  file passes `--dry-run` and reaches RESTORE. Require a complete walk
  (`object::walk` result) before upload; test with a truncated fixture.
- [ ] #9 (medium; claude) `restore` maps a Kermit Timeout on the final
  `:0:HPTXRS RESTORE` to success. After the timeout, probe once with
  `retries = 0` (the sync marker or `DEPTH`): a reply means RESTORE did not
  run -> error, keep `:0:HPTXRS`, hint; no reply -> success and marker.
  In-memory test for both branches.
- [ ] #6 (high; greptile) `put --overwrite` purges the destination before
  validating the temp name the calculator reported in its ACK. Validate
  `temp == PUT_TEMP` (and `validate_name`) first; test with a transport
  that acknowledges with the wrong name.
- [ ] #15 (low; claude) `xserv --dir` evaluates path components as raw RPL;
  run `validate_name` per component like `Calculator::cd`; test.

## Tasks: crafted input and output safety

- [ ] #4 (high; greptile) `convert.rs` `C$ <count>` reserves the declared
  count before checking the remaining input: cap the count by the remaining
  bytes (or refuse > remaining) before any allocation; test with
  `C$ 18446744073709551615`.
- [ ] #5 (high; greptile; security.md "Local input files are read whole")
  Piped REPL lines: read with a bound (`take(limit)` on the line reader, the
  Kermit max packet payload plus a margin) and refuse an over-long line
  without buffering it; test with a 10 MiB line.
- [ ] security.md: local input files (`put`, `xserv put`, `object`, `grob`,
  `restore`) are read whole with `std::fs::read`; check `metadata().len()`
  against a limit (`Config::max_size`, 4 MiB, or the backup limit) before
  reading; test with an oversized temp file.
- [ ] #10 (medium; glm, greptile) `--jq` collects every output into a `Vec`
  without a cap: cap the count (e.g. 10 000) and total bytes (e.g. 64 MiB)
  and fail with `jq filter produced too much output`; test with
  `range(0; 1e9)`.
- [ ] security.md "`--jq` reads the environment": build `jaq-std` without
  the `env`/`debug`/`stderr`/`halt` definitions (feature flags) or filter
  them out of the definitions loaded; test that `env` is undefined.
- [ ] #11 (medium; security-review; security.md "Terminal escapes") Control
  characters (0x00..0x1F, 0x7F, C1) in calculator text reach the terminal
  in text mode: one `escape_control()` helper applied to every
  calculator-originated string rendered in `Format::Text` (listing names
  and kinds, stack levels, error texts, server banner, hint descriptions);
  JSON stays as is. Test with a listing name holding ESC.
- [ ] #13 (low; claude) Hint `cmd`s in `put_replacing` and `reply_hint`
  interpolate the calculator-reported temp name and user names without
  `shell_quote`; quote every name in every hint; unit test with a name
  holding a space and a semicolon.

## Tasks: file-system invariants

- [ ] #12 (medium; security-review; security.md "Restore marker symlink")
  The restore marker is written with `std::fs::write` to a predictable
  `$TMPDIR` path. Write with `create_new` (fail if present, never follow a
  symlink) into a per-user directory (`dirs::data_local_dir()/hptx/`),
  and treat only a regular file as a marker; test.
- [ ] security.md "REPL history symlink": open the history file with
  `create(true).append(true)` after refusing a symlink
  (`symlink_metadata`), or `O_NOFOLLOW` where available; test on Unix.
- [ ] security.md "Offline converters write next to the input file": decide
  and document. Rule: the default output goes to the current directory
  (`file_name` only, `with_extension`), not beside the input; `-o` as
  before. Update help text and README; test.
- [ ] #14 (low; claude) `grob to-png -` without `-o` writes `./-.png`;
  write to stdout like `object convert -`; test.
- [ ] security.md "Windows file names": `validate_name` refuses `\`, `:` and
  the reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`,
  `LPT1`-`LPT9`, case-insensitive) on every platform (the calculator does
  not allow them in a variable name either); test.

## Tasks: CLI behaviour

- [ ] #16 (low; claude) `repl::is_link_failure` treats every
  `Error::Kermit(_)` as a dead link; only `Io`, `Serial` and
  `Kermit(Timeout)` end the session, `TooLarge`/`Protocol`/`Cancelled`
  are per-line errors; test.
- [ ] #17 (low; glm) `object inspect` hardcodes the 48 family for `%%HP:`
  text; give it the same `--model` handling as `object convert`; test with
  a bare integer and `--model 49g`.

## Tasks: e2e script data safety

- [ ] #7 (high; greptile) `scripts/e2e-cli.sh` cleanup deletes `HPTXCLI`
  (and the other test names) even when the script did not create them.
  Refuse to run when any test-owned name already exists on the calculator
  (first `ls`), and purge only names this run created.
- [ ] #8 (high; greptile) The PICT scenario `ERASE`s an existing drawing:
  save PICT to a test variable first and restore it in cleanup, or skip the
  scenario when PICT is non-empty and say so.

## Tasks: kb

- [ ] `docs/security.md`: move each fixed item from "Not yet true" to the
  invariants (with the test that proves it); invariant 6 reworded (a `C`
  is sent once; a timeout is reported, never retried); note the two items
  deliberately left (sync limits, no fuzzing).
- [ ] Decision-log entry: single-shot host commands (why retries = 0 is the
  only fix without a protocol change; the cost: a lossy link surfaces a
  timeout the user retries by hand), restore probe, converter output rule,
  jaq without `env`.
- [ ] `backlog/host-command-double-execution.md` completed; `docs/
  calculator-quirks.md` gains the "a `C` resent after a lost ACK runs
  again; the server cannot tell it from a new command" quirk.

## Acceptance criteria

Gates green (`cargo fmt`, `clippy -D warnings`, `cargo test`, `cargo deny
--locked check`, `hyalo lint`); one new regression test per finding, each
named after its table number or security.md item in a comment; e2e suites
and `scripts/e2e-cli.sh` pass on the 48SX and the 49G, one model at a time;
the PR review (all three reviewers) has every finding fixed or answered.
