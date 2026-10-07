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
code on 2026-10-05 (iteration 11b) and again after the first whole-hptx
audit (PR #18, fixed in iteration 12, 2026-10-06); the gaps are the work list
for the next audit (decision log, 2026-10-05, "Whole-crate audits").

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
   `hptx-pict-<time>.png`; `object convert` and `grob to-png` default to the
   input's file name with a new extension in the current directory, not
   beside the input, and write stdout for stdin (iteration 12; tests
   `offline::default_outputs_go_to_the_current_directory`,
   `grob_from_stdin_goes_to_stdout`). All go through `write_file`, which
   uses `create_new` unless `--force` (no silent overwrite, no race between
   the existence check and the write). Known exceptions by design, both in
   hptx's per-user data directory (`$XDG_DATA_HOME/hptx/` and the
   macOS/Windows equivalents, `repl::data_dir`): the REPL history, never
   read or written through a symbolic link (test
   `repl::history_refuses_a_symlink`), and the restore marker
   `restore-pending-<port>`, created with `create_new` and counted only as
   a regular file (test `commands::restore_marker_never_follows_a_symlink`).
2. **Calculator-provided names are never used as paths.** The name in a
   received F packet is decoded into `ReceivedFile::name` and only shown;
   `Calculator::get` returns the data and the CLI picks the path from the
   user's arguments. Listing names appear only in output and in hints, where
   they are shell-quoted (`output::shell_quote`; every name in every hint
   command, test `commands::hint_names_are_shell_quoted`). User-typed
   variable names pass `validate_name` (no `/`, `\`, leading `.` or digit,
   whitespace or control characters, and none of the Windows device names
   `CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9` in any case,
   alone or before a `.`, on every platform; test `calc::names`) before they
   become a default file name. `put --overwrite` deletes nothing unless the
   calculator stored the temporary under `HPTXPT` (test
   `commands::put_overwrite_checks_the_stored_name_first`), and `xserv
   --dir` components pass `validate_name` before the calculator evaluates
   them, and each must be listed as a directory first, as in
   `Calculator::cd` (tests `xserv::dir_components_are_names`,
   `xserv::cd_checks_each_component_is_a_directory`).
3. **hptx spawns no process.** No `std::process::Command` outside the e2e
   test (which drives `docker exec`); rustyline is built without default
   features. The REPL sends RPL to the calculator, never to a shell.
4. **No panic and no unbounded allocation on crafted input.** Kermit and
   XModem receives are capped by `Config::max_size` (4 MiB, `TooLarge`, E
   packet or CAN CAN CAN) per transaction, server text included; GROB
   decoding checks the length field against height and width with checked
   arithmetic before allocating (at most about 4 M pixels); object parsing
   reads nibbles through `get`-style accessors and returns
   `Error::Object`/`None` on truncation; the `%%HP:` parser checks a `C$`
   count against the remaining input before allocating (test
   `convert::counted_string_count_beyond_the_input`);
   `.unwrap()`/`.expect()` are linted (`clippy::unwrap_used`,
   `expect_used`) outside tests. Local input (`put`, `xserv put`, `object`,
   `grob`, `restore`, stdin for each) is read through `util::read_input`:
   the size is checked before reading and the read stops past 4 MiB (test
   `util::oversized_input_is_refused`); a piped REPL line longer than
   8 KiB is skipped without being buffered (test
   `repl::overlong_piped_line_is_skipped`). `--jq` stops after 10 000
   results or 64 MiB of output (test `output::jq_output_is_capped`). The
   saturnus ROM is size-checked per model by `saturnus_drive::rom::load`.
5. **`--jq` has no I/O.** jaq gets the envelope as its only input; no
   `input`/`inputs` source is wired and jaq has no file or network
   functions. `env`, `debug`, `stderr` and `halt` (`halt_error`,
   `debug_empty`, `stderr_empty`) are filtered out of jaq-std by name, so
   they are undefined (test `output::jq_has_no_env_stderr_or_halt`; jaq-std's
   features are too coarse to drop them, see the decision log 2026-10-06).
6. **A host command is sent once, and a late reply cannot be mistaken for
   the answer to the user's command.** `Calculator::host` sends every `C`
   packet with `first_packet_retries = Some(0)` and a NAK grace as long as
   the timeout: a calculator cannot tell a resent `C` from a new one and
   would run it again. No answer in time is `Error::NoReply` ("may still be
   running or may have run; the next connection resyncs"), never a retry
   (tests `calc::host_command_is_never_resent`, kermit-proto
   `first_packet_retries_only_cover_the_command`). One exception
   (iteration 13): a NAK within 250 ms of the `C` going out
   (`first_packet_nak_window`; a `C` takes at most ~94 ms at 9600 baud),
   but not sooner than the `C`'s wire time (`first_packet_nak_byte_time`,
   10 bits per byte at 9600 baud), is the calculator rejecting it, and the
   `C` is resent once if no answer (`S`, short reply or `E`) comes within
   3 s (tests `calc::host_command_rejected_at_once_is_resent`,
   kermit-proto `immediate_nak_allows_one_resend`,
   `nak_after_the_window_is_not_resent`, `nak_before_the_wire_time_is_stale`,
   `two_immediate_naks_allow_only_one_resend`). Input waiting before a host
   command is discarded first, so a stale NAK buffered between commands
   (an idle REPL) is not read as a rejection (test
   `session::input_between_host_commands_is_discarded`), and a NAK read
   within the `C`'s wire time never counts (test
   `calc::host_command_is_never_resent`). Residual risk: the idle server's
   periodic NAK (every few seconds) is byte-identical; one the server sends
   in the narrow interval after our `C`'s wire time and within the window,
   while it took the `C`, runs that command twice if its answer takes
   longer than 3 s. Once the calculator's
   `S` is in, the reply packets keep the normal retries (test
   `calc::host_reply_packets_keep_their_retries`). The REPL prints a
   `NoReply`, resyncs and goes on (test
   `repl::line_without_reply_resyncs_and_goes_on`). Every
   connection starts with `Calculator::sync`: a sacrificial command that
   pushes a marker string unique to the session (`HPTX-` and six random hex
   digits), sent once more if the first reply does not show the marker at
   level 1. Only copies of the marker on top of the stack are dropped, and
   two odd replies in a row fail the connect (test
   `calc::sync_never_drops_what_it_did_not_push`). Apart from a `C`
   the calculator rejected at once, the marker is the only command ever
   resent: its second attempt has the normal retries.
   `restore` uploads only a backup whose directory walk succeeds, an
   attached library id in any directory included (tests
   `calc::restore_refuses_a_truncated_backup`,
   `calc::check_backup_accepts_an_attached_library`,
   `commands::restore_checks_the_whole_backup`) and counts the silent
   `RESTORE` as done only when a probe after it gets no reply either; an
   answered probe is reported as an unknown result, never as success (test
   `calc::restore_probe_after_the_silent_restore`).
7. **Text from the calculator or a file cannot drive the terminal.** Text
   mode (`output::render`, `Failure::render`, the REPL's output and `--jq`
   raw strings) passes through `output::escape_control`: every control
   character but newline and tab (C0, DEL, C1, a CR not before LF) is shown
   as `\xHH`. JSON output (`--json`, `--jq` objects and arrays, failures,
   REPL JSON lines) keeps the values: serde_json escapes C0 itself, and
   `output::escape_json` writes DEL and C1 (U+007F-U+009F), which serde_json
   leaves raw, as `\u00XX` (test
   `output::control_characters_are_escaped_in_text_and_json`).

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

- **No fuzzing of the parsers**: invariant 4 rests on review and fixture
  tests (`object`, `reply`, `grob`, `convert` with BCD reals and `%%HP:`
  headers); the decision log rules out fuzz suites, so the audit must read
  every index and slice in these modules. Left deliberately.
- **Sync limits**: the first marker attempt has one timeout period and no
  retries, so a crossed exchange costs one `--timeout` (20 s by default)
  before the second attempt, which has the normal budget (20 s × 6 tries);
  a calculator busy for longer than that fails the connect with a timeout.
  A marker command that ran without its reply arriving leaves its string
  on the stack when neither reply shows it (the connect then fails). Left
  deliberately (iteration 12).
- **A lost command is not resent**: a `C` (or the calculator's first
  answer to it) lost on the line ends the command with `NoReply`, and the
  user runs it again after checking. The price of invariant 6 without a
  protocol change.
- **`--jq` can still build a large value**: the output caps count results
  and bytes as they come; a filter that builds one huge value
  (`[range(1e9)]`) allocates inside jaq before anything is output. jaq has
  no evaluation limit to set.
- **REPL history check-then-open**: the history path is checked with
  `symlink_metadata` and then opened by rustyline, which follows a link
  planted in between. It needs write access to the user's own data
  directory, which already means control of the user's account.
- **Hints are POSIX-quoted**: wrong for `cmd.exe`/PowerShell on Windows.
