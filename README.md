# hptx

Transfer files between HP Saturn calculators (HP48 S/SX/G/GX, HP49G, and
later HP38G/39G/40G) and a modern computer over a serial cable, from macOS,
Windows and Linux. Kermit and XModem, a sans-IO Rust core, a CLI that works
for people and for AI agents, and a GUI later.

Status: early. The sans-IO Kermit client (`crates/kermit-proto`) and the
HP layer (`crates/hptx-core`: serial/TCP transports, directory listing,
get/put in binary or ASCII, host commands, screenshots, backup and restore)
are done and tested against recorded replies and the emulated 48SX, 48GX
and 49G; the `hptx` CLI drives all of it. Project knowledge (architecture,
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
hptx screenshot -o screen.png
hptx backup -o home.hp
hptx object inspect prg.hp              # offline: type and size of a file
```

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

Developing hptx itself:

```sh
just test          # fast test suite
just lint          # rustfmt check and clippy
just emulator-up   # build and start the emulated HP 48SX on tcp://localhost:4848
just e2e           # end-to-end tests against the emulator
just record-trace host "6 7 *"   # record a Kermit trace from the emulator
```

Not affiliated with HP. HP, HP48 and HP49 are trademarks of HP Inc.
HP's calculator ROMs are not included; the emulator container downloads them
from hpcalc.org at build time for local use only.

License: MIT, see `LICENSE` and `AI_NOTICE`.
