---
title: "Iteration 4: hptx CLI"
type: iteration
date: 2026-10-04
status: completed
branch: iter-4/hptx-cli
tags:
  - iteration
  - hptx
---

# Iteration 4: hptx CLI

## Tasks

- [x] Commands: `ports`, `info`, `ls`, `get`, `put`, `rm`, `mkdir`, `mv`,
  `run`, `screenshot`, `backup`, `restore`, `settings`, `finish`, and
  offline `object inspect`, `object convert` (binary <-> ASCII where the
  object is text-representable), `grob to-png`.
- [x] `--port` accepts a device path or `tcp://`; `HPTX_PORT` env var; auto-pick
  when exactly one USB serial port exists.
- [x] Output per the conventions in `CLAUDE.md`. `--json` is implied when piped.
- [x] Transfer mode: `get`/`put` default to binary and rely on hptx-core to
  set or check the HP's mode first (the fresh 48SX is in ASCII mode and
  mangles binary data, iteration 2); `--ascii` opts into ASCII transfer.
- [x] Host-command length: `run`, `mv`, `settings` and friends go through
  `C` packets limited to 77 encoded bytes (`kermit-proto`
  `StartError::TooLong`); surface hptx-core's split-or-error behaviour with
  a clear message, never a truncated command. Names and server text arrive
  as HP-charset bytes and are shown translated (hptx-core, iteration 3).

### From iteration 3

- [x] `restore` ends server mode (the calculator warm-starts and needs
  SERVER typed again), and leaves the backup object `:0:HPTXRS` in port 0;
  after the user restarts SERVER, `hptx restore --cleanup` (or the next
  connect) calls `Calculator::purge_restore_leftover`. Say both in the
  output and in `--help`; ask for confirmation (it replaces HOME).
- [x] `run` prints the calculator's stack display as returned: the 49G
  truncates long values and shows lists with commas. Document it; for
  exact values `get` the object instead.
- [x] `ports` / auto-pick: `hptx-core` builds `serialport` without default
  features (no libudev), so USB vendor/product info is not available on
  Linux. Either enable the `libudev` feature in `hptx-cli` (CI then needs
  `libudev-dev`) or auto-pick by name pattern; decide and log it.
- [x] Temporary variables `HPTXTMP`, `HPTXBK`, `HPTXRS`: surface the
  "already exists" error from hptx-core with a hint to rename or remove it.
- [x] `object inspect` shows the type name and walked size and reports an
  unknown prolog as an error, never as a ROM pointer (iteration 3 review:
  the walk used to truncate composites holding HP49 matrices). The 49G
  stores any matrix with exact integers as a symbolic matrix (#02686).
  The e2e script includes a byte-exact `get` of a list holding a symbolic
  matrix (49G only: the 48 has no symbolic matrices). CI currently runs
  the e2e job against the 48SX; start a second container from the same
  image as the 49G (another port) in that job so this regression runs in
  CI, not only locally.
- [x] Transport failures end in a message, never a hang: a Kermit timeout
  after the configured retries (noisy line, wrong speed, calculator not in
  SERVER) prints what was tried and a hint; `Session` fires the retransmit
  timeout even while garbage keeps arriving (iteration 3 review).

## Acceptance criteria

the M3 e2e scenarios re-expressed as a shell script driving the
  binary (one script, run in the same CI job); `hptx --help` reads well for a
  human; an agent can do `hptx ls --json | jq`.
