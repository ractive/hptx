---
title: Security and threat model
type: docs
date: 2026-10-05
status: active
tags:
  - security
---

# Security and threat model

hptx is a local tool: it talks to one calculator over a serial line (or an
emulator over TCP or in-process), reads files the user names and writes the
files the user asks for. It listens on no port and has no network service.
This page names what hptx must not trust, the invariants that follow, and
where the code does not meet them yet. Each invariant was checked against the
code on 2026-10-05 (iteration 11b); the gaps are the work list for the next
whole-crate audit (decision log, 2026-10-05, "Whole-crate audits").

## Untrusted inputs

- **Bytes from the calculator**: Kermit packets (file names in F packets,
  file data, server text: stack displays and `G D` listings), XModem blocks
  and XSERV frames. A calculator, an emulator or anything on the serial line
  can send arbitrary bytes, including a late reply that belongs to another
  client's command (`backlog/stale-reply-after-abort.md`).
- **Files given to hptx**: `put`, `xserv put`, `object inspect`, `object
  convert`, `grob to-png`, `restore`. They may be crafted HP objects
  (prologs, lengths, GROB sizes, `%%HP:` headers, BCD reals).
- **REPL and piped lines**: RPL text goes to the calculator as a host
  command; colon commands name files and variables.
- **`--port` / `HPTX_PORT`**: a serial device path, `tcp://host:port` or
  `saturnus://[MODEL@]ROM`.
- **The ROM file for saturnus** (`saturnus://`), read and executed by the
  emulator in-process.
- **`--jq` filters**: evaluated by jaq over hptx's own JSON output.

Out of scope: an attacker who controls the user's environment, shell or
binary; the calculator's own memory (hptx runs what the user tells it to).

## Invariants

1. **No file is written outside the explicit `-o` path or a hptx-chosen name
   in the current directory.** Downloads (`get`, `xmodem get`, `xserv get`,
   `:get`) write to `-o FILE` or to the variable name the user typed;
   `backup` and `pict` default to `hptx-backup-<time>.hp` and
   `hptx-pict-<time>.png`. All go through `write_file`, which uses
   `create_new` unless `--force` (no silent overwrite, no race between the
   existence check and the write). Known exceptions by design: the REPL
   history under the platform data directory and the restore marker in the
   temp directory (see "Not yet true" for both), and the offline converters
   (below).
2. **Calculator-provided names are never used as paths.** The name in a
   received F packet is decoded into `ReceivedFile::name` and only shown;
   `Calculator::get` returns the data and the CLI picks the path from the
   user's arguments. Listing names appear only in output and in hints, where
   they are shell-quoted (`output::shell_quote`). User-typed variable names
   pass `validate_name` (no `/`, no leading `.` or digit, no whitespace or
   control characters) before they become a default file name.
3. **hptx spawns no process.** No `std::process::Command` outside the e2e
   test (which drives `docker exec`); rustyline is built without default
   features. The REPL sends RPL to the calculator, never to a shell.
4. **No panic and no unbounded allocation on crafted input.** Kermit and
   XModem receives are capped by `Config::max_size` (4 MiB, `TooLarge`, E
   packet or CAN CAN CAN) per transaction, server text included; GROB
   decoding checks the length field against height and width with checked
   arithmetic before allocating (at most about 4 M pixels); object parsing
   reads nibbles through `get`-style accessors and returns
   `Error::Object`/`None` on truncation; `.unwrap()`/`.expect()` are linted
   (`clippy::unwrap_used`, `expect_used`) outside tests. The saturnus ROM is
   size-checked per model by `saturnus_drive::rom::load`.
5. **`--jq` has no file or network I/O.** jaq gets the envelope as its only
   input; no `input`/`inputs` source is wired and jaq has no file or network
   functions.
6. **A late reply cannot be mistaken for the answer to the user's command.**
   Every connection starts with `Calculator::sync`: a sacrificial command
   that pushes a marker string unique to the session (`HPTX-` and six
   random hex digits), sent once more if the first reply does not show the
   marker at level 1. Only copies of the marker on top of the stack are
   dropped. hptx never resends a `run` or any other mutating command
   (decision log, iteration 11b).

## Supply chain

- `Cargo.lock` is committed; release binaries and CI build from it.
- `cargo-deny` (`deny.toml`: advisories, licenses, bans, sources) runs in
  CI on every PR and push to main (iteration 11a) and locally with
  `cargo deny --locked check`.
- Git dependencies (`saturnus`, `saturnus-drive`, feature `saturnus` only)
  are pinned by commit rev.
- Default features are trimmed where they pull system libraries or I/O:
  `serialport` without libudev, `rustyline` with file history only.
- `kermit-proto` and `xmodem-proto` are the only crates published; they have
  no dependencies at all (`kermit-proto` is a dev-dependency of
  `xmodem-proto` for the trace format).

## Not yet true

- **Restore marker symlink**: `restore` writes
  `$TMPDIR/hptx-restore-pending-<port>` with `std::fs::write`, which follows
  a symlink. On Linux, where `/tmp` is shared, another local user can plant
  that name as a symlink and have hptx truncate a file the victim owns (the
  content is a fixed line). Use `create_new` or a per-user directory.
- **Offline converters write next to the input file**: `object convert` and
  `grob to-png` without `-o` derive the output from the input path
  (`with_extension`), so the output lands in the input's directory, not the
  current directory. Either document this as the rule or change it.
- **Windows file names**: `validate_name` allows `\` and device names
  (`CON`, `NUL`, `COM1`); `hptx get 'A\B'` without `-o` writes into a
  subdirectory `A` on Windows, and `get CON` writes to the console device.
  User-typed, so not an escalation, but the "current directory" invariant
  does not hold there. Hints are POSIX-quoted, wrong for `cmd.exe`/PowerShell.
- **Terminal escapes**: text output prints stack displays, listing names and
  object text as decoded, control characters (ESC included) unchanged. A
  calculator or a crafted file can send ANSI escape sequences to the user's
  terminal. Sanitize control characters in text mode (JSON escapes them).
- **`--jq` reads the environment**: jaq-std is built with default features,
  so `env` returns every environment variable (tokens included) and
  `debug`/`stderr` write to stderr, `halt` exits. No file or network I/O, but
  the "no I/O" invariant is stronger than what is built. Build jaq-std
  without default features (`std`, `log`) or document it.
- **Local input files are read whole**: `put`, `xserv put`, `object`,
  `grob` and `restore` use `std::fs::read` without a size limit, and the
  REPL reads a piped line of any length before it is refused as too long.
  User-chosen input, but invariant 4 does not hold for it.
- **No fuzzing of the parsers**: invariant 4 rests on review and fixture
  tests (`object`, `reply`, `grob`, `convert` with BCD reals and `%%HP:`
  headers); the decision log rules out fuzz suites, so the audit must read
  every index and slice in these modules.
- **REPL history symlink**: the history file
  (`$XDG_DATA_HOME/hptx/history` and the macOS/Windows equivalents) is
  created and appended with rustyline's `append_history`, which follows a
  symlink planted at that path. The data directory is the user's own, so
  this needs write access to it already; still, refuse a symlink or open
  with `O_NOFOLLOW` where the platform has it.
- **Sync limits**: a calculator still busy longer than the Kermit retry
  budget (20 s × 5) fails the connect with a timeout instead of syncing;
  if neither reply shows the marker (two late replies in a row) nothing is
  dropped, so a marker command that ran without its reply arriving leaves
  its string on the stack.
