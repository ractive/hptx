# hptx

Rust tools for transferring files between HP Saturn calculators (HP48 S/SX/G/GX,
HP49G, HP38G/39G/40G) and a modern computer over serial: Kermit and XModem.
Read `PLAN.md` before doing anything; it holds the milestones, the decisions
already made and the known calculator quirks.

## Where knowledge lives

- `~/devel/hp-literature/` is an LLM wiki about these calculators. Query it
  with the `hyalo` CLI from that directory (`hyalo summary`, `hyalo read
  index.md`, `hyalo find "kermit server"`, `hyalo read protocols/kermit-hp.md`).
  Protocol pages: `protocols/kermit`, `kermit-hp`, `server-commands`, `iopar`,
  `hp-object-format`, `xmodem`, `xmodem-hp`, `xserv`. Cite wiki pages in code
  comments as `wiki: protocols/kermit-hp` when a quirk is implemented.
  If you learn something new about the calculators, add it to the wiki
  (follow its own `CLAUDE.md`), don't bury it in a code comment only.
- `~/devel/hpcomm/` is the 1999-2001 HPComm C++ source (GPL, co-owned by HP).
  Read `hpcomm/Kermit.cpp`, `Prot.cpp`, `Filer*.cpp` and `hpgcomm/XModem.cpp`
  to learn HP behaviour. Never copy or closely translate code from it: hptx
  is MIT. Facts, yes; expression, no.
- `emulator/` holds the saturnng Docker container (HP48SX, 48GX, 49G) with the
  calculator's serial port on TCP 4848. `emulator/README.md` explains it.
  The image contains HP ROMs: never push it to a registry.

## Decisions (do not re-litigate)

- License MIT with `AI_NOTICE`; public repo; GitHub user `ractive`.
- Rust, edition 2024, stable toolchain. Crates: `kermit-proto`, `xmodem-proto`
  (generic, sans-IO, publishable), `hptx-core` (HP layer + transports),
  `hptx-cli` (binary `hptx`). A Tauri app comes later, no front-end chosen.
- Protocol crates are sans-IO: bytes in, bytes and events out, caller owns
  I/O, time and files. No threads, sockets, filesystem or async inside them.
- First target HP48SX, then HP49G (both exist as real hardware here), HP48GX
  in CI only, HP38G/39G/40G last (only Emu48 on Windows emulates them).
- CLI conventions borrowed from `~/devel/hyalo`: text on a TTY, JSON when
  piped, `{results, total, hints}` envelope, `--jq`, hints suggesting next
  commands, `--dry-run` on destructive commands, shell completions. Nothing
  more elaborate; one `--help` must serve humans and agents.

## Test policy (important)

- Protocol crates: unit tests only, byte traces in, bytes/events out.
  Milliseconds. No mocks of serial ports.
- At most one integration test binary per crate (`tests/e2e.rs` with
  modules). Never one file per scenario.
- End-to-end tests against the emulator run only when `HPTX_E2E_ADDR`
  (e.g. `tcp://localhost:4848`) is set, and only in a separate CI job. Keep
  them to a handful of scenarios per model.
- No heavy dev-dependencies in the sans-IO crates. No fuzzing, property or
  snapshot tests until a bug justifies one.
- `just test` runs the fast suite; `just e2e` the emulator suite.

## Working style

- Implementation milestones are done by Opus agents (`model: opus`) with a
  brief that names the PLAN.md milestone, the wiki pages to read and the
  acceptance criteria. The main session reviews.
- Work on a branch per milestone, open a PR, the user merges.
- Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`
  (or the model that wrote it). Never commit secrets or ROM images.
