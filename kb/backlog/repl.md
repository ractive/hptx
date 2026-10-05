---
type: backlog
title: "hptx repl: interactive RPL session"
date: 2026-10-05
status: completed
priority: medium
---

# hptx repl: interactive RPL session

Asked for on 2026-10-05 after the first CLI demo: typing `hptx run` per line
is clumsy, and every command reconnects (0.5 s drain, turnaround pause).

## Shape

- `hptx repl`: one `Calculator` kept open; each line goes out as a host
  command (the `run` path, 77-byte split included) and the reply, the
  calculator's stack display, is printed line by line, deepest first, as the
  calculator reports it (49G truncation and comma lists included). Empty
  stack prints nothing. Errors print the calculator's message and the stack
  it left behind.
- Plain prompt (`> `), no screen mimicry: no header, no softkey row. The path
  is available via `:info`.
- Colon meta-commands mapping onto existing `Calculator` methods: `:ls`,
  `:get NAME [FILE]`, `:put FILE [NAME]`, `:cd PATH`, `:screenshot`,
  `:info`, `:quit`. Optional later: `:screen` rendering a real screenshot
  in block characters (one transfer per call, opt-in only).
- Line editing and history from `rustyline` (or `reedline`); the only
  terminal work, portable to Windows. Reading from a pipe behaves like
  batch `run`, so agents are unaffected.
- No change to hptx-core expected; roughly 200-400 lines in hptx-cli.

## Open

- Whether to show the path in the prompt (`{ HOME } > `) was considered and
  dropped for now as confusing.
