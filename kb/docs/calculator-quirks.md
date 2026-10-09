---
title: Calculator quirks checklist
type: docs
date: 2026-10-04
status: active
tags:
  - quirks
  - kermit
  - hptx
---

# Calculator quirks checklist

Each item must be covered by a test. Source: the calculator-knowledgebase wiki (`~/devel/calculator-knowledgebase/`) and the saturnng container experiments of 2026-10-04 and 2026-10-05.

- Server answers only R, S, C, G D, G F, G L, I. No REMOTE CD: hptx
  changes directory with `C HOME`, then `C NAME` per level, each name
  checked in a `G D` listing first. Covered: `calc` test `cd_checks_each_component`, e2e `ls`.
- Every control character must be prefixed; block check 3 default; 9600 max.
  Covered (prefixing): `kermit-proto` prefix tests.
- Stale NAK waits in the buffer when connecting to an idle server.
  Covered: `kermit-proto` tests `stale_nak_*`; `hptx-core` drains 0.5 s on
  connect (`session` test `stale_nak_drained`).
- A command packet sent right after the final ACK of the previous
  transaction is lost; the HP answers only after its own timeout (about
  6 s) NAKs it. `Session` waits 200 ms between transactions (100 ms was
  enough on the emulator). Covered: `session` test
  `turnaround_pause_between_transactions`; e2e runtime (425 s -> 63 s).
- A `C` packet resent after a lost or late ACK (or after a NAK) runs
  again: the server executes a command when it receives it and cannot tell
  a retransmission from a new command (sequence numbers restart at zero for
  every command). Seen once on the emulated 49G under load (`4711` twice,
  2026-10-05). hptx sends every `C` once (`first_packet_retries =
  Some(0)`, NAK grace = timeout) and reports a missing answer as
  `NoReply`; the reply packets after the `S` keep their retries. Covered:
  `calc` tests `host_command_is_never_resent`,
  `host_reply_packets_keep_their_retries`.
- The server NAKs a `C` packet it rejects right after it arrives and never
  answers it (in-process 48SX, saturnus iteration 18: NAK seq 0 11 ms after
  the `C`); the command did not run. The idle server's periodic NAK seq 0
  is byte-identical, and one that crosses a `C` the server took is followed
  by the `S` once the command is done; idle NAKs also pile up in the input
  between commands (a REPL left idle). hptx discards waiting input before
  each host command and takes a NAK within 250 ms of the `C` going out
  (`first_packet_nak_window`; a `C` of up to ~90 bytes takes ~94 ms at
  9600 baud), but not sooner than the `C`'s wire time
  (`first_packet_nak_byte_time`), as a rejection: it resends the `C` once
  if no answer (`S`, short reply or `E`) comes within 3 s; any other NAK
  never resends. Residual risk: an idle NAK sent in the narrow interval
  after our `C`'s wire time and within the window, for a command slower
  than 3 s, runs it twice. Covered: `calc` tests
  `host_command_rejected_at_once_is_resent`,
  `host_command_is_never_resent`, `host_command_late_nak_is_not_resent`,
  `session` test `input_between_host_commands_is_discarded`, kermit-proto
  tests `immediate_nak_*`, `nak_after_the_window_is_not_resent`,
  `nak_before_the_wire_time_is_stale`, `nak_after_the_grant_keeps_the_grace`,
  `two_immediate_naks_allow_only_one_resend`.
- Server command packets (I, S, R, C, G) must use block check 1 even after
  an I exchange agreed on 3; a type-3 C packet is NAKed. The HP sends no
  REPT field, so repeat prefixing is never used with it. Covered:
  `kermit-proto` trace tests.
- `G D` header line present on G/49G, absent on SX. Trailing dot on 49G reals.
  Covered: `reply` fixture tests (all three models).
- A `C` reply is the stack display: results stay on the user's stack
  (hptx drops what its queries push), the 49G truncates long values and
  shows lists with commas in algebraic mode, names without quotes; `VARS`
  is unusable on the 49G. Errors come as an `Error: X` first line, not an E
  packet, and leave the arguments on the stack. Covered: `reply` fixture
  tests, `calc` tests.
- Evaluating an undefined name pushes it; evaluating a variable runs it.
  The 48SX has no VERSION (returns `'VERSION'`); `1 0 /` gives `∞` on the
  49G, `Infinite Result` on the 48s. Covered: `reply` and `calc` tests.
- The calculator does not read ASCII trigraphs (`\->`) in a `C` packet;
  hptx translates them. C data over 77 encoded bytes does not fit one
  packet; hptx splits its own commands into parts. Covered: `charset`
  tests, `calc` rename tests, e2e `run`.
- ACK of the F packet carries the stored name; illegal names abort; a name
  clash gets a `.1` suffix unless flag -36.
- The fresh 48SX transfers in ASCII mode: GET returns a `%%HP: T(1)A(D)F(.);`
  header, and raw bytes sent to it are translated (bytes 0-26 of a 256-byte
  test file did not survive the round trip). Seen in the
  `kermit-proto` traces `48sx-send`/`48sx-get` (and `48gx-*`); hptx-core
  sets flag -35 before every get/put. Covered: e2e `put_round_trip` (all
  256 byte values survive in binary mode).
- Binary receive on the HP keeps a string until the end; ASCII mode compiles
  each packet and aborts on syntax error.
- Inter-byte gaps cause overruns; write packets atomically.
- ARCHIVE with a ticking clock can corrupt the backup: stop the clock display.
- `ARCHIVE :IO:` fails in server mode ("Port Not Available"); backup goes
  through `:0:` and a temporary directory variable; `RESTORE` ends server
  mode and leaves `:0:HPTXRS` behind. Purging the port object while its
  recalled copy is on the stack gives "Object In Use". Covered: e2e
  `backup`; restore checked by hand on the 48GX and 49G (2026-10-05).
- `STO` strips tags; inside lists, tags and directories an element may be a
  5-nibble ROM pointer (the real 5). Covered: `object` fixture tests.
- 48G XModem checksum-only; 48S/SX no XModem at all.
- On the SX, SERVER typed within 2 s after FINISH loses keys (harness only).
- The 49G's `VERSION` string says `HP48-C ... Copyright HP 2009`; only the
  year tells it from a 48G. Covered: hptx-cli model-detection tests.
- Storing IOPAR back with the same values grows the 48SX variable from 29.5
  to 37.5 bytes. Covered: `settings` only stores on a change.
- ASCII transfer of strings has two layers: T(2)/T(3) translation doubles
  backslashes and applies trigraphs on top of the string syntax; the 49G
  syntax escapes `\"` and `\\`, so a 49G backslash becomes four in T(3)
  text; the 48 has no escapes and uses `C$ n` for strings holding a quote.
  Covered: e2e-cli `object convert` round trips on both models.
- `PURGE` of a missing `:0:` port object raises no error; use `VTYPE` (-1)
  to test existence. Covered: hptx-cli restore cleanup.
- 49G: an IOPAR of exact integers is invalid. The server stores it, answers
  nothing to that command and leaves server mode with "Invalid IOPAR"; the
  state persists until IOPAR is fixed. Build numbers for the 49G as reals
  with a trailing dot. Changing the checksum type with valid reals does not
  disturb the running session; it takes effect at the next SERVER. Covered:
  `calc::tests::set_iopar_stores_reals`; verified on the emulated 49G and
  48SX (2026-10-05).
- XRECV/XSEND cannot be started through the Kermit server: a `C` command
  answers "Port Not Available" on the 49G and 48GX and the server stays up
  with the name left on the stack. After a keyboard-started transfer the
  stack is empty and SERVER must be typed again. Covered: traces
  `49g-server-xrecv`, `48gx-server-xsend`; e2e `xmodem_round_trip`.
- 49G XRECV asks `D` every ~3 s (4 times), then NAK every 10 s (~10 times),
  then "XRECV Error: Receive Error" after ~108 s. `D` = CRC-16/KERMIT, high
  byte first; the 49G never answers `C`. It takes 1k and 128-byte blocks and
  sends one 1k block plus 128-byte tail blocks. XSEND pads with memory
  garbage. XRECV stores `NAME.1` instead of overwriting. Covered: traces
  `49g-xrecv*`, `49g-xsend*`.
- 48GX XModem is checksum only: XRECV starts with NAK, ignores `C`/`D` as a
  sender, NAKs every 1k block and sends CAN CAN CAN after 9 tries; XSEND pads
  with 0x00; a cancelled XRECV leaves an empty string in the variable.
  Covered: traces `48gx-*`.
- The 49G boots in ALG mode; `XRECV` without an argument there is "Invalid
  Syntax". `-95 CF` switches to RPN. Covered: `prepare_for_xmodem`.
- Emulator only (saturnng): leaving the server with ON while a Kermit
  exchange settles can leave the 49G answering "Port Not Available" even to
  SERVER; the state survives CLOSEIO and a container restart. Use a fresh
  container.
- 48GX XRECV onto an existing name stops with "XRECV Error: Name Conflict":
  no start character, the name stays on the stack, no `.1` fallback (flag
  -36 untested). hptx refuses such a put before FINISH. The 49G stores
  `NAME.1` instead. Covered: `prepare_for_xmodem` tests.
- 49G: `-95 CF` over Kermit, a keyboard XRECV/XSEND, SERVER, then `-95 SF`
  restores ALG mode. A 3 s reply timeout suffices for 48GX 128-byte
  checksum blocks at 9600 baud. Covered: e2e `xmodem_round_trip`.
- After the Kermit server ends, alpha mode can still be on: typing `xsend`
  on the emulated 49G came out as `SIN(X)!`. Press ON before typing.
  Covered: e2e typing helpers press ON first.
- 49G in RPN mode: typing SERVER leaves a tagged `SERVER` and `NOVAL` on
  the stack (ALG mode and the 48GX leave nothing). Covered: e2e-cli runs
  CLEAR after the XModem steps.
- In server mode `LCD→` returns the "Awaiting Server Cmd. / Processing
  Command" banner, never the stack: there is no screenshot of the display
  over the link. Covered: `pict` replaces `screenshot` (iteration 9).
- PICT: a fresh PICT is `Graphic 0 × 0` (the 49G shows `# 83h # 40h` after
  ERASE); `ERASE` makes it 131x64 and `# 0d # 0d BLANK PICT STO` does not
  shrink it back. `ERASE { # 10d # 10d } PIXON` works over the link on all
  three models; `PVIEW` over the link is "Bad Argument Type". Covered: e2e
  `pict`.
- Emulator only (saturnng): under parallel load (three containers running
  e2e at once) or after an aborted client, a calculator can stop answering
  with random pixels on the LCD and the busy annunciator lit; seen four
  times on 2026-10-05, never reproduced on demand. Restart the container.
- A host command whose client died mid-way is still finished by the
  calculator, and its reply arrives seconds later as the answer to the next
  client's first command (seen as `bad directory line: "Empty Stack"` after
  Ctrl-C in the REPL). Not covered; backlog `stale-reply-after-abort`.
