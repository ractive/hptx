# saturnng HP 49G / 48GX / 48SX emulator in Docker, serial port on TCP

This image runs the [saturnng](https://codeberg.org/gwh/saturnng) emulator
(commit `3f467d2`) headless. It uses the ncurses TUI inside tmux. The emulated
calculator's serial "wire" port is bridged to TCP port 4848 with socat. A host
program can then talk Kermit to the calculator over `tcp://localhost:4848`.

Everything below was verified on macOS (arm64) with Rancher Desktop, unless it
is marked **unverified**.

## Build

```sh
docker build -t hp49g-emu .
```

The build clones saturnng, builds it with `WITH_SDL=no LUA_VERSION=lua5.4` and
downloads three ROMs from hpcalc.org: HP 49G ROM 2.15 (`hp4950emurom.zip`),
48GX ROM R and 48SX ROM J, the last SX revision. The build checks each ROM's
size: 2 MB, 512 KB and 256 KB. Keep curl's default user agent, because
hpcalc.org serves junk to fake browser user agents. The image is about 217 MB, mostly because the upstream Makefile
always links GTK4.

## Run

```sh
docker run --rm -d -p 4848:4848 --name calc hp49g-emu           # HP 49G
docker run --rm -d -p 4849:4848 -e MODEL=48gx --name calc48 hp49g-emu  # HP 48GX
docker run --rm -d -p 4852:4848 -e MODEL=48sx --name calcsx hp49g-emu  # HP 48SX
```

Wait until the log says `bridged`. That takes about 8 to 12 s for every model,
with or without saved state. The TCP port is not accepted before that.

```sh
until docker logs calc 2>&1 | grep -q bridged; do sleep 1; done
```

```
saturn (49g) wire port: /dev/pts/1
Kermit server started (SERVER)
bridged /dev/pts/1 to tcp:4848
```

With the default `AUTOSTART=1` the entrypoint does the following:

1. It answers the first-boot prompts. All three ROMs ask "Try To Recover
   Memory?", answered NO with softkey F. The 49G then shows "Memory Clear" with
   an OK softkey, also answered with F. The 48GX and 48SX go straight to the
   stack with "Memory Clear" in the status area.
2. It types `SERVER` and ENTER (ALPHA ALPHA S E R V E R ENTER). The screen then
   shows "Awaiting Server Cmd."

Between key presses it waits until the LCD rendering has stopped changing.
Set `-e AUTOSTART=0` to do all of this by hand.

Environment variables:

| Variable    | Default | Meaning |
|-------------|---------|---------|
| `MODEL`     | `49g`   | `49g`, `48gx` or `48sx` |
| `AUTOSTART` | `1`     | answer first-boot prompts and start the Kermit server |
| `CARDS`     | `1`     | create empty RAM card files for the 48 models (see below) |
| `TUI`       | `tui`   | `tui` (1 char per pixel), `tui-small` or `tui-tiny` |

Extra arguments after the image name are passed to `saturn`, for example
`--debug-serial`. Saturn's stderr goes to `/tmp/saturn.log` in the container.

## Press keys and look at the screen

```sh
docker exec calc calc-screen                 # LCD as text (█ = pixel on)
docker exec calc calc-screen -a              # raw tmux pane
docker exec calc calc-keys '\'               # ON, which also cancels SERVER
docker exec calc calc-keys ';' ';' s e r v e r Enter
```

`calc-keys` sends one tmux key name per argument, with `KEY_DELAY` seconds in
between (default 0.2). The saturnng TUI key map is as follows:

| Key            | Calculator key |
|----------------|----------------|
| `a` to `z`     | the key whose alpha label is that letter; `a` to `f` are the softkeys |
| `0` to `9`, `.`, `+`, `-`, `*`, `/` | same key |
| `Enter`        | ENTER |
| `BSpace`       | backspace |
| `[` or `F2`    | left shift |
| `]` or `F3`    | right shift |
| `;` or `F4`    | ALPHA |
| `\`, `Escape` or `F5` | ON |
| `F7`, `\|` or `F10` | **quits saturn**, saving its state |

A bare `;` is tmux's command separator. `calc-keys` escapes it, so use
`calc-keys` rather than plain `tmux send-keys`.

The 49G boots in algebraic mode ("ALG" in the header). Typing `SERVER` ENTER
works in that mode.

## Talk to the calculator from the host

### Python smoke test (verified)

`kermit-probe.py` needs only python3. It is a minimal Kermit client with
type-1 checks and no 8-bit prefixing.

```sh
python3 kermit-probe.py localhost 4848 init            # I-packet exchange
python3 kermit-probe.py localhost 4848 dir             # REMOTE DIRECTORY
python3 kermit-probe.py localhost 4848 host "6 7 *"    # REMOTE HOST
python3 kermit-probe.py localhost 4848 get IOPAR       # GET a variable
python3 kermit-probe.py localhost 4848 finish          # FINISH, leaves server mode
```

A 49G exchange looks like this:

```
>> b'\x01+ I~* @-#N1L\r'
<< b'\x01+ Y~& @-# 1*'
>> b'\x01( C6 7 *C\r'
<< b'\x01+ S~* @-#Y1"'
>> b'\x01+ Y~* @-#N1\\\r'
<< b'\x01#!X>'
>> b'\x01#!Y?\r'
<< b'\x01?"D1:                    42#M#J6'
...
---- received 26 bytes ----
1:                    42
```

Verified on all three models: `init`, `dir`, `host`, `get IOPAR` and
`finish`. All three ACK the I-packet with the same parameters, `~& @-# 1`.
The 49G returns IOPAR as `{ 9600. 0. 0. 0. 3. 1. }`, the 48 models as
`{ 9600 0 0 0 3 1 }`: wire, 9600 baud, no parity, checksum type 3. Those are
the defaults, so no I/O setup is needed.

The replies differ between models in two places:

- **REMOTE DIRECTORY.** The 49G and 48GX start the listing with a header line
  holding the current directory and free memory, such as `{ HOME } 127828`.
  The 48SX sends only the variable lines, such as `IOPAR 29.5 List 8861`.
- **Number formatting.** The 49G prints reals with a trailing dot, for
  example `29.5 List 10777.`, and the 48 models do not.

After `finish`, wait until the screen has settled, 2 s is enough, before
typing `SERVER` again with `calc-keys`. On the 48SX, keys typed straight after
the FINISH ACK were lost except ENTER, which gave "DUP Error: Too Few
Arguments". Press ON (`calc-keys '\'`) to clear such an error.

### C-Kermit via a host pty (unverified)

C-Kermit and socat are not installed on the test Mac, so this path was not run.

```sh
brew install c-kermit socat
socat -d -d pty,raw,echo=0,link=/tmp/hp49g TCP:localhost:4848 &
kermit
  set line /tmp/hp49g
  set speed 9600
  set carrier-watch off
  set flow none
  set parity none
  set block-check 3
  set control prefix all
  remote directory
```

## Gotchas

- **Stale NAK on connect.** In server mode the calculator sends a NAK when its
  idle timeout expires. If no TCP client is connected, the pty buffers it and
  the next client reads it first. Clients should discard pending input for
  about 0.5 s before sending the first packet. If they resend the I-packet in
  reply to that NAK, they get two ACKs. Treating the second ACK as the answer
  to the next command desynchronises the server, which then answers
  `E Invalid Server Cmd.` `kermit-probe.py` drains input first.
- **One client at a time.** socat forks per connection, but all children
  share one pty. Two simultaneous clients interleave bytes.
- **`--datadir=DIR`, not `--datadir DIR`.** saturn parses argv twice with
  `getopt_long`. In the first pass `--datadir` is unknown and its value is
  permuted away, so the next option, such as `--reset`, becomes the datadir.
- **Terminal size.** `--tui` draws one character per LCD pixel (131x64). tmux
  runs at 200x80 so nothing is clipped. `--mono` is needed because the colour
  TUI paints pixels as coloured spaces, which `capture-pane -p` loses. A
  UTF-8 locale (`LANG=C.UTF-8`) is needed for the `█` glyphs.
- **Wire pty discovery.** The TUI never prints the pty name. The entrypoint
  reads `/proc/<saturn pid>/fd`, skips the tmux pane tty and `/dev/pts/ptmx`,
  and writes the result to `/run/calc-pty`.
- **No `-t` needed.** tmux provides the terminal for the TUI.

## Persisting calculator state

```sh
docker run --rm -d -p 4848:4848 -v hp49g-data:/data --name calc hp49g-emu
```

State lives in `/data/saturn<MODEL>`. That directory holds `rom`, `ram`,
`cpu`, `hdw` and `mod`, plus the RAM card files for the 48 models. `docker stop` makes the entrypoint press
F7, which makes saturn save its state before exiting. A variable stored over
Kermit was still present after `docker stop` and a new `docker run` on the
same volume. That was verified on all three models. With saved state there is no
first-boot prompt, and AUTOSTART only types `SERVER`. Without a volume every
run starts from a fresh reset, which is what CI wants.

### RAM cards on the 48 models

saturnng's launcher scripts create empty RAM card files, and so does the
entrypoint unless `CARDS=0`:

| Model | `port1` | `port2` |
|-------|---------|---------|
| 48GX  | 128 KB  | 4 MB    |
| 48SX  | 128 KB  | none    |

The upstream `saturn48sx` launcher also creates a 128 KB `port2`. saturnng
compiles port 2 as 4 MB for every 48 model, so that file fails to load with
"Can't initialize Port 2 from disk". The entrypoint skips it. The same warning
still appears for the 48SX, and it only means slot 2 is empty.

## GitHub Actions sketch (workflow unverified)

The image builds and runs natively on arm64. It also builds and runs as
`linux/amd64` under QEMU emulation: `init` and `host` work there, and a fresh
49G is ready in about 18 s. The workflow itself has not been run on GitHub.

```yaml
jobs:
  e2e:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
      - name: Build emulator image
        run: docker build -t hp49g-emu emu/saturnng-docker
      - name: Start HP 49G
        run: |
          docker run -d -p 4848:4848 --name calc hp49g-emu
          timeout 60 sh -c 'until docker logs calc 2>&1 | grep -q bridged; do sleep 1; done'
          python3 emu/saturnng-docker/kermit-probe.py localhost 4848 init
      - name: End-to-end tests
        env:
          HPCOMM_TEST_PORT: tcp://localhost:4848
        run: cargo test --test e2e
      - name: Screen and logs on failure
        if: failure()
        run: |
          docker exec calc calc-screen || true
          docker exec calc cat /tmp/saturn.log || true
          docker logs calc
```

To avoid rebuilding saturnng on every run, push the image to ghcr.io or use
`docker/build-push-action` with the GitHub Actions cache. A `services:`
container also works, but steps cannot easily wait for the `bridged` log line
from there. Running the container from a step is simpler.
