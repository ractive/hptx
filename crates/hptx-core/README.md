# hptx-core

The HP calculator layer of hptx on top of `kermit-proto` and
`xmodem-proto`: the Kermit and XModem driver loops, the calculator
operations (list, get, put, host commands, PICT, backup, restore, XModem
preparation), the HP object format, GROBs and the HP character set.

The protocol logic never reads a clock, sleeps or touches a port. Two faces
run it:

- **Blocking**: `Calculator`, `Session` and `XmodemSession` over a
  `transport::Transport` with the system clock. The `hptx` CLI uses these.
- **Sans-I/O**: `Machine` is fed bytes and the caller's time and hands out
  bytes to write, progress events and the result of each operation. It
  never blocks, so it runs where bytes arrive by callback, such as a
  browser with Web Serial on `wasm32-unknown-unknown`.

## Features

| Feature | Default | What it adds |
|---|---|---|
| `native` | yes | Serial ports (`serialport`) and TCP in `transport::open` |
| `saturnus` | no | The saturnus emulator in-process, `saturnus://[MODEL@]ROM` (git dependency) |

For the browser, or for any crate that brings its own I/O:

```toml
hptx-core = { version = "0.1", default-features = false }
```

That build has no OS I/O, pulls in neither `serialport` nor `saturnus`, and
builds with `cargo build -p hptx-core --no-default-features --target
wasm32-unknown-unknown`. On wasm32 the clock type
`hptx_core::time::Instant` is `web_time::Instant` (`performance.now()`);
elsewhere it is `std::time::Instant`.

## The sans-I/O API

`Machine` follows the calling convention of `kermit_proto::Client`, one
level up: a whole operation instead of one Kermit transaction.

| Call | What it does |
|---|---|
| `Machine::new(now, options, seed)` | A machine for a newly opened link. `seed` should be random (`crypto.getRandomValues`): it picks the sync markers. |
| `start(now, op) -> Result<(), Busy>` | Begin an `Op`; its first bytes are ready at once. One operation at a time. |
| `handle_input(now, bytes)` | Bytes read from the link, any chunking. |
| `handle_timeout(now)` | The time; call it at `next_timeout()` (early calls do nothing). |
| `handle_link_error(now, &io::Error)` | The port failed or closed: the running operation fails. |
| `poll_transmit() -> Option<Vec<u8>>` | The next chunk to write, as one write (the HP overruns on inter-byte gaps). |
| `poll_event() -> Option<Progress>` | Kermit or XModem progress (`set_events(false)` drops them). |
| `poll_result() -> Option<Result<Reply>>` | The result once the operation is over. |
| `next_timeout() -> Option<Instant>` | When to call `handle_timeout` at the latest. |
| `abort(now)` | Drop the running operation without telling the calculator. |

The operations (`Op`) are those of `Calculator` and `XmodemSession`:
`Connect` (drain stale input, then `Sync`), `Sync`, `Run`, `List`, `Path`,
`Cd`, `Updir`, `Mkdir`, `Remove`, `Rename`, `Mem`, `Version`, `Model`,
`Iopar`, `SetIopar`, `TransferMode`, `SetTransferMode`, `Get`, `Put`,
`Pict`, `Backup`, `Restore`, `PurgeRestoreLeftover`, `Finish`,
`PrepareXmodem`, `XmodemSend`, `XmodemReceive`, and `Transact` for one raw
Kermit transaction. Each names its `Reply` variant.

After every `start` and `handle_*` call the host writes everything
`poll_transmit` hands out, takes the events and the result, and arms one
timer for `next_timeout`.

### A browser host

A wasm-bindgen wrapper owns one `Machine` per opened Web Serial port. A
sketch of a GET (Rust in the wasm module, JS glue in comments):

```rust
use hptx_core::machine::{Machine, Op, Reply};
use hptx_core::time::Instant;
use hptx_core::{Options, TransferMode};

struct Link {
    machine: Machine,
}

impl Link {
    // JS: `port.open({ baudRate: 9600 })`, then `new Link(seed)` with a
    // seed from crypto.getRandomValues, then `link.start_connect()`.
    fn new(seed: u64) -> Self {
        Link { machine: Machine::new(Instant::now(), Options::default(), seed) }
    }

    fn start_connect(&mut self) {
        let _ = self.machine.start(Instant::now(), Op::Connect);
        self.flush();
    }

    fn start_get(&mut self, name: &str) {
        let op = Op::Get { name: name.into(), mode: TransferMode::Binary };
        let _ = self.machine.start(Instant::now(), op);
        self.flush();
    }

    // JS: the reader loop calls this with every chunk of `reader.read()`.
    fn on_bytes(&mut self, bytes: &[u8]) {
        self.machine.handle_input(Instant::now(), bytes);
        self.flush();
    }

    // JS: the timer armed in `flush` calls this.
    fn on_timer(&mut self) {
        self.machine.handle_timeout(Instant::now());
        self.flush();
    }

    fn flush(&mut self) {
        while let Some(chunk) = self.machine.poll_transmit() {
            // JS: `await writer.write(chunk)`, one write per chunk.
            let _ = chunk;
        }
        while let Some(_event) = self.machine.poll_event() {
            // Progress bar: Kermit FileStart / Progress, XModem Progress.
        }
        if let Some(result) = self.machine.poll_result() {
            match result {
                Ok(Reply::Data(_file)) => { /* hand the file to JS */ }
                Ok(_) => { /* Connect: ready for the next operation */ }
                Err(_error) => { /* show it */ }
            }
        }
        if let Some(deadline) = self.machine.next_timeout() {
            // JS: clearTimeout(t); t = setTimeout(on_timer, ms), where ms
            // is the time until `deadline`.
            let _ms = deadline.saturating_duration_since(Instant::now()).as_millis();
        }
    }
}
```

A PUT is the same with `Op::Put { name, data, mode }` and
`Reply::Stored(name)`. An XModem transfer is `Op::PrepareXmodem` (the
calculator leaves server mode and the reply says what to type), then
`Op::XmodemSend` or `Op::XmodemReceive` on the same machine; afterwards the
user types `SERVER` and the next operation starts with `Op::Connect`.

## The blocking API

```rust,no_run
use hptx_core::{Calculator, TransferMode};

let mut calc = Calculator::open("/dev/ttyUSB0")?; // or tcp://localhost:4848
let data = calc.get("ALLB", TransferMode::Binary)?;
calc.put("COPY", &data, TransferMode::Binary)?;
# Ok::<(), hptx_core::Error>(())
```

## Licence

MIT, see `AI_NOTICE` and `LICENSE` in the repository.
