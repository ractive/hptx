---
title: CLI conventions
type: docs
date: 2026-10-04
status: active
tags:
  - cli
  - hptx
---

# CLI conventions

Borrowed from `~/devel/hyalo`, and nothing more elaborate:

- Text output on a TTY, JSON when piped; `--format text|json` overrides.
- JSON envelope `{results, total, hints}`; `--jq` for reshaping.
- Hints suggest the next commands.
- `--dry-run` on destructive commands (`rm`, `restore`, `put` overwrite).
- Shell completions.
- One `--help` must read well for a human who loves their HP48 and for an
  agent. The surface is about fifteen commands, so no profiles or schemas.
