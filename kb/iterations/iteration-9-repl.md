---
type: iteration
title: "Iteration 9: hptx repl"
date: 2026-10-05
status: completed
tags:
  - iteration
  - cli
branch: iter-9/repl
---

# Iteration 9: hptx repl

From `backlog/repl.md` (2026-10-05): typing `hptx run` per line is clumsy and
every command reconnects. Read first: `docs/cli-conventions.md`,
`decision-log.md` (iteration 4: host-command replies are display text, the
77-byte `C` packet split, reals with a trailing dot), `docs/calculator-quirks.md`.

## Tasks

- [x] `hptx repl`: one `Calculator` kept open for the session; each input
  line is sent as a host command (the `run` path, split included) and the
  reply, the calculator's stack display, is printed line by line, deepest
  first, exactly as the calculator reports it. An empty stack prints
  nothing. A calculator error prints its message and the stack it left.
- [x] Plain prompt (`> `); no header, no softkey row, no screen mimicry.
- [x] Colon meta-commands over existing `Calculator` methods: `:ls [PATH]`,
  `:cd PATH`, `:get NAME [FILE]`, `:put FILE [NAME]`, `:rm NAME`,
  `:screenshot [FILE]`, `:info`, `:help`, `:quit` (also Ctrl-D). Unknown
  colon commands get a hint; a line starting with `::` is sent to the
  calculator verbatim.
- [x] Line editing and history (`rustyline` or `reedline`), history file
  under the user's data dir, Ctrl-C clears the current line and does not
  exit. Reading from a pipe (no TTY) runs each line without editing and
  exits at EOF, so scripts and agents can drive it.
- [x] Link failures end the session with the usual `{error,hint}` message;
  the `--port`, `--timeout`, `--retries` global options apply.
- [x] Tests: unit tests for the line classifier (colon vs RPL vs `::`),
  meta-command parsing, and the reply printer; `scripts/e2e-cli.sh` gets a
  piped-stdin REPL scenario (`42 'HPTXR' STO`, `HPTXR`, `:ls`, `:rm HPTXR`)
  on every model.
- [x] README and `hptx --help` mention the REPL.
- [x] `screenshot` becomes `pict` (CLI `hptx pict [-o FILE.png]`, REPL
  `:pict [FILE]`): it fetches the graphics screen with `PICT RCL`. The
  `LCD→` variant is dropped: run through the server it can only ever
  capture the "Awaiting Server Cmd." banner (seen 2026-10-05). The help
  explains that the display itself is captured by hand (`LCD→ 'S' STO`
  before SERVER, then `hptx get S` and `grob to-png`). e2e checks `pict`
  after drawing something into PICT via `run`.

## Acceptance criteria

e2e-cli green on the 48SX, 48GX and 49G; an interactive session on the
emulated 48SX shows `1: 42` after `42 'X' STO X`; gates and `hyalo lint`
clean.
