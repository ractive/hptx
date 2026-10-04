# hptx

Transfer files between HP Saturn calculators (HP48 S/SX/G/GX, HP49G, and
later HP38G/39G/40G) and a modern computer over a serial cable, from macOS,
Windows and Linux. Kermit and XModem, a sans-IO Rust core, a CLI that works
for people and for AI agents, and a GUI later.

Status: skeleton. Project knowledge (architecture, decisions, iteration
plans) lives in `kb/`, a markdown knowledge base read with `hyalo`. The
`emulator/` directory holds a Docker container running the real calculator
ROMs in the saturnng emulator with the serial port on TCP, which is what the
end-to-end tests talk to.

```sh
just test          # fast test suite
just lint          # rustfmt check and clippy
just emulator-up   # build and start the emulated HP 48SX on tcp://localhost:4848
just e2e           # end-to-end tests against the emulator
```

Not affiliated with HP. HP, HP48 and HP49 are trademarks of HP Inc.
HP's calculator ROMs are not included; the emulator container downloads them
from hpcalc.org at build time for local use only.

License: MIT, see `LICENSE` and `AI_NOTICE`.
