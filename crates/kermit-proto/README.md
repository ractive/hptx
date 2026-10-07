# kermit-proto

A sans-IO Kermit client (user Kermit) for talking to the Kermit server of an
HP 48 or 49 series calculator: send and get files, run host commands, list
the directory, end server mode.

The crate never touches a port, a clock or a file. The caller owns all three:
it reads and writes the link (a serial port, a TCP bridge to an emulator),
supplies the current `Instant` (see below) and loads or stores file contents.
`Client` is a pure state machine in between, so it runs unchanged on any
transport and is tested by replaying byte traces recorded against real
calculator ROMs.

Supported: short packets (up to 94 bytes), block checks 1, 2 and 3, control
prefixing, 8th-bit prefixing and repeat compression as negotiated in the
Send-Init exchange, and the server commands `R` (get), `S` (send), `C` (host
command), `G D` (directory), `G F` (finish), `G L` (logout) and `I` (info).
Not supported: server mode, long packets and sliding windows (the HP
calculators use neither); attribute packets are acknowledged and ignored.

## The seam

- `Client::start(now, command)` begins a transaction; the first packet is
  queued at once.
- `Client::handle_input(now, bytes)` feeds bytes read from the link, in any
  chunking.
- `Client::handle_timeout(now)` tells the client time has passed; call it when
  `Client::next_timeout()` is reached (it does nothing early).
- `Client::poll_output(now)` hands out the next packet to write: exactly one
  packet including padding and EOL.
  The transport must put it on the wire as one unit, without inter-byte
  gaps (see the HP notes below).
- `Client::poll_event()` yields events: `FileStart`, `Data`, `Progress`,
  `FileEnd`, `ServerText` and finally exactly one `Done` or `Error`.
- `Client::next_timeout()` is when the client wants to be called again; use it
  as the read timeout.

After `Done` or `Error` keep calling `poll_output` until it returns `None`: the
final ACK or an E packet may still be queued. After a receive the client
lingers briefly (`Config::linger`) to re-ACK a retransmitted `B` packet, in
case its final ACK was lost; keep driving it until `next_timeout()` is `None`,
or start the next command.

## Time and WebAssembly

The state machine never reads a clock; every call that needs the time takes
`now` from the caller. The type is `kermit_proto::time::Instant`: `std::time::Instant`
on every target except `wasm32`, where it is `web_time::Instant` from the
[`web-time`](https://crates.io/crates/web-time) crate, because
`std::time::Instant::now()` panics on `wasm32-unknown-unknown`. A wasm caller
creates it with `Instant::now()` from `web-time` (or `kermit_proto::time::Instant::now()`),
which reads `performance.now()` in the browser. `kermit_proto::time::Duration` is
`core::time::Duration` everywhere. Native callers use `std::time` as before.

## Driver loop

```rust,no_run
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};
use kermit_proto::{Client, Command, Config, Event};

fn main() -> std::io::Result<()> {
    let mut link = TcpStream::connect("localhost:4848")?;
    let mut client = Client::new(Config::default());
    client
        .start(Instant::now(), Command::Host(b"6 7 *".to_vec()))
        .map_err(std::io::Error::other)?;
    let mut finished = false;
    let mut buf = [0u8; 512];
    loop {
        let now = Instant::now();
        while let Some(packet) = client.poll_output(now) {
            // A whole packet per call; the link must not split it.
            link.write_all(&packet)?;
        }
        while let Some(event) = client.poll_event() {
            match event {
                Event::ServerText(text) => println!("{}", String::from_utf8_lossy(&text)),
                Event::Done | Event::Error(_) => finished = true,
                _ => {}
            }
        }
        let wait = match client.next_timeout() {
            Some(t) => t.saturating_duration_since(now),
            None if finished => break, // idle and nothing left to write
            None => Duration::from_millis(100),
        };
        link.set_read_timeout(Some(wait.max(Duration::from_millis(10))))?;
        match link.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => client.handle_input(Instant::now(), &buf[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                client.handle_timeout(Instant::now())
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
```

## HP calculator notes

- Each packet must reach the wire as one unit: inter-byte gaps overrun the
  HP's receiver. This is a requirement on the transport, not something
  `write_all` guarantees (it may split the buffer into several writes). On a
  TCP socket to an emulator, or a serial port whose driver buffers a whole
  packet, one `write_all` per packet is enough; otherwise buffer the packet
  and hand it to the port in one call.
- In server mode the HP periodically NAKs packet 0 while idle, so a stale NAK
  can be waiting right after connecting. Discard input for about 0.5 s after
  opening the link; the client also tolerates a stale NAK that slips through
  (`Config::nak_grace`).
- The HP drops a command packet that arrives right after the final ACK of the
  previous transaction; leave a short pause (about 200 ms) between
  transactions.
- Commands go out before any parameter exchange, so their data must fit in one
  packet under the default MAXL of 80 with block check 1; `Client::start`
  refuses longer ones.
- The HP runs a host command (`C`) when it receives it and cannot tell a
  resend from a new command: `Config::first_packet_retries = Some(0)` sends
  it once. The HP also NAKs a `C` it rejects, right after it arrives (11 ms
  on a 48SX), and that NAK looks exactly like the periodic idle one. Since
  0.1.1, `Config::first_packet_nak_window` (the packet's time on the wire
  plus slack), `Config::first_packet_nak_grace` and
  `Config::first_packet_nak_byte_time` (lower bound: a NAK sooner than the
  packet's wire time is stale) let a NAK inside the window allow one resend
  if no answer follows within the grace.
- A host command's reply arrives either in the ACK (short reply) or as a
  text transfer (`X`, `D`.., `Z`, `B`); both end up as `Event::ServerText`.

## Lower layers

`codec` (framing, block checks, the deframer), `prefix` (control, 8th-bit and
repeat prefixing), `params` (Send-Init negotiation) and `trace` (a text format
for byte traces) are public for tools and tests.

## License

MIT
