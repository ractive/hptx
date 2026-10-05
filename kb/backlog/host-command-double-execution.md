---
type: backlog
title: A retransmitted host command can run twice
date: 2026-10-05
status: completed
priority: high
---

# A retransmitted host command can run twice

Seen once on the emulated 49G under heavy load during the iteration 11b
e2e run (2026-10-05): the abort scenario ended with `4711` twice on the
stack, i.e. the host command `1 1000000 START NEXT 4711` had executed
twice. The likely path: the client's `C` packet was executed, its ACK was
lost or delayed past the client's timeout, the client retransmitted the
`C` packet, and the server executed it again. Kermit's retransmit rule is
correct for data packets but a host command is not idempotent.

Not reproduced on demand (three reruns alone passed at load 43). The sync
marker in `Calculator::sync` is immune (only markers are ever dropped), but
`run`, `put --overwrite`'s `PURGE`, `rm`, `mv` and every REPL line go
through the same path.

## Options

- Never retransmit a `C` packet after its first transmission: on timeout,
  wait for the reply with the full budget instead (the server resends its
  S packet every 2 s when unanswered, so the reply is not lost), and treat
  a NAK as "resend only if the server never received it", which the
  sequence number cannot tell today; needs the wire behaviour recorded.
- Make the dangerous commands idempotent on the calculator side where
  possible (e.g. `IFERR ... END` guards), which does not help arbitrary
  `run` text.
- Leave `run` as is and document that a retransmitted command may run
  twice on a lossy link.

Input for the whole-hptx audit.

## Resolution (iteration 12, 2026-10-06)

The first option, without a protocol change: `Calculator::host` sends
every `C` packet once (`retries = 0`, `nak_grace = timeout`), and a missing
reply is `Error::NoReply`, telling the user the command may still be
running or may have run and that the next connection resyncs. Decision log
2026-10-06; security.md invariant 6; test `calc::host_command_is_never_resent`.
