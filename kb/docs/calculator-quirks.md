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

Each item must be covered by a test. Source: the wiki at `~/devel/hp-literature/` and the saturnng container experiments of 2026-10-04 and 2026-10-05.

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
