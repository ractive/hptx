---
type: backlog
title: Resync after a late reply from an aborted host command
date: 2026-10-05
status: planned
priority: medium
---

# Resync after a late reply from an aborted host command

Found during iteration 9 (2026-10-05): when hptx is killed (Ctrl-C in the
REPL) while a host command runs, the calculator finishes the command and
sends its reply later. The next connection's 0.5 s drain in `Session::new`
comes too early, so the first command gets that stale reply: `hptx ls`
failed with `unexpected reply: bad directory line: "Empty Stack"`; the
command after it worked.

## Options

- The parsers (`G D` listing, stack reply) detect a reply of the wrong kind
  and resend the command once.
- `Session` tracks the packet sequence and discards a reply whose sequence
  does not match the outstanding command.
- The REPL catches SIGINT during a command and waits for the reply before
  exiting.
