# hptx

Transfer files between HP Saturn calculators (HP48 S/SX/G/GX, HP49G, and
later HP38G/39G/40G) and a modern computer over a serial cable, from macOS,
Windows and Linux. Kermit and XModem, a sans-IO Rust core, a CLI that works
for people and for AI agents, and a GUI later.

![hptx against the emulated HP 48SX: ls, get, object inspect, run and a REPL session](docs/hptx-demo.gif)

Status: early. The sans-IO Kermit client (`crates/kermit-proto`) and the
HP layer (`crates/hptx-core`: serial/TCP transports, directory listing,
get/put in binary or ASCII, host commands, the graphics screen PICT, backup
and restore) are done and tested against recorded replies and the emulated
48SX, 48GX and 49G; the `hptx` CLI drives all of it. Project knowledge (architecture,
decisions, iteration plans) lives in `kb/`, a markdown knowledge base read
with `hyalo`. The
`emulator/` directory holds a Docker container running the real calculator
ROMs in the saturnng emulator with the serial port on TCP, which is what the
end-to-end tests talk to.

## Usage

Install from source (Rust stable):

```sh
cargo install --path crates/hptx-cli    # puts `hptx` in ~/.cargo/bin
```

On the calculator, run `SERVER` (the display shows "Awaiting Server Cmd.").
hptx talks at 9600 baud; `hptx info` shows the calculator's IOPAR. Then:

```sh
hptx ports                              # find the cable
hptx --port /dev/cu.usbserial-1410 ls   # or set HPTX_PORT once
hptx --port tcp://localhost:4848 ls     # the emulator
hptx get PRG -o prg.hp                  # download a variable (binary)
hptx put prg.hp --as PRG2               # upload a file
hptx run '6 7 *'                        # run RPL, print the stack
hptx pict -o plot.png                   # the graphics screen PICT (plots)
hptx backup -o home.hp
hptx object inspect prg.hp              # offline: type and size of a file
hptx object convert dl/x.hp --to ascii  # offline: writes ./x.txt
```

Files are written only where you say (`-o FILE`) or under a name hptx picks
in the current directory: the variable's name for `get`, a timestamped name
for `backup` and `pict`, and the input's file name with a new extension for
`object convert` and `grob to-png` (not beside the input). An existing file
is never replaced without `--force`. A host command (`run`, a REPL line,
`rm`, `mv`, ...) is sent once and never resent: if its reply does not come
in time, hptx says so and you check on the calculator before running it
again.

With exactly one USB serial port, `--port` can be left out. Output is text
on a terminal and JSON when piped, in one envelope
`{"results": ..., "total": N, "hints": [...]}`; errors go to stderr as
`{"error", "hint"}` with a non-zero exit code. Hints are ready-to-run
commands, which makes hptx usable from scripts and AI agents:

```sh
hptx ls --json | jq -r '.results[] | select(.type == "Program") | .name'
hptx ls --jq '.total'                   # jq built in, no jq needed
hptx completions zsh > ~/.local/share/zsh/site-functions/_hptx
```

`hptx --help` lists every command, and `hptx <command> --help` has the
details and examples.

`hptx repl` keeps one link open and sends each line you type as RPL, then
prints the stack as the calculator displays it. Lines starting with a colon
are hptx commands (`:ls`, `:cd`, `:get`, `:put`, `:rm`, `:pict`, `:info`,
`:help`, `:quit`), and `::` sends RPL that starts with a colon. `:put`
replaces an existing variable only with `--overwrite`. Line editing and
history work on a terminal. Piped lines run without a prompt, for scripts
and agents:

```text
> 42 'X' STO
> X
1: 42
> :rm X
Deleted X (Real Number, 16 bytes)
```

For an agent on real hardware, `hptx repl --json` with piped input is the
way to go: one process and one link for many commands, no reconnect per
call, and no late reply from an aborted command landing on the next one.
Each input line gives exactly one JSON object on stdout, in order:
`{"stack": [...]}` (level 1 first), `{"error", "hint", "stack"}` for a
calculator error (on stdout too, unlike the other commands, so results and
errors stay in order), `{"results": ...}` for a colon command, `{}` for a
blank line and `{"quit": true}` for `:quit`:

```sh
$ printf "6 7 *\n1 0 /\nCLEAR\n:ls\n" | hptx repl --json
{"stack":["42"]}
{"error":"calculator error: Infinite Result","hint":"...","stack":["0","1","42"]}
{"stack":[]}
{"results":[{"checksum":8861,"directory":false,"name":"IOPAR","size":29.5,"type":"List"}]}
```

XModem is the alternative to Kermit on the 48G/GX and 49G (the 48S/SX has
none): `hptx put prg.hp --protocol xmodem` or `hptx get PRG --protocol
xmodem`. The Kermit server cannot start XRECV/XSEND, so hptx ends server
mode, prints what to type on the calculator (`'PRG' XRECV` or `'PRG'
XSEND`, then ENTER) and waits `--start-timeout` seconds (default 60). After
the transfer, type `SERVER` on the calculator again. Kermit stays the
default because it needs no typing on the calculator; XModem is for when the
Kermit server is not wanted, and on the 49G it moves 1k blocks. `--dry-run`
shows the plan without ending the server.

`hptx xserv ls|get|put|eval|mem` talks to the XSERV command server of the
49g+/50g. It is **unverified on hardware**: it follows HP's client code as
documented in the wiki, and the emulated 49G has no XSERV.

Developing hptx itself:

```sh
just test          # fast test suite
just lint          # rustfmt check, clippy and cargo-deny (cargo install cargo-deny --locked)
just emulator-up   # build and start the emulated HP 48SX on tcp://localhost:4848
just e2e           # end-to-end tests against the emulator
just record-trace host "6 7 *"   # record a Kermit trace from the emulator
```

Not affiliated with HP. HP, HP48 and HP49 are trademarks of HP Inc.
HP's calculator ROMs are not included; the emulator container downloads them
from hpcalc.org at build time for local use only.

License: MIT, see `LICENSE` and `AI_NOTICE`.
