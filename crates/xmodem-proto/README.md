# xmodem-proto

A sans-IO XModem sender and receiver for transferring objects to and from HP
48G/GX and 49G calculators (`XRECV`, `XSEND`): 128-byte and 1k blocks,
checksum, CRC-16 and HP's own CRC mode.

The crate never touches a port, a clock or a file. The caller owns all three:
it reads and writes the link (a serial port, a TCP bridge to an emulator),
supplies the current `std::time::Instant` and loads or stores file contents.
`Transfer` is a pure state machine in between, so it runs unchanged on any
transport and is tested by replaying byte traces recorded against real
calculator ROMs.

- Sender (`Command::Send`): the calculator runs `XRECV`. The machine waits for
  the receiver's start character, NAK (checksum), `C` (CRC-16) or `D` (HP's
  CRC), and uses whichever it asks for.
- Receiver (`Command::Receive`): the calculator runs `XSEND`. The machine asks
  with `D` (by default) a few times, then falls back to NAK (checksum).

## The seam

- `Transfer::start(now, command)` begins a transfer. A receiver's first start
  character is queued at once; a sender waits for the receiver.
- `Transfer::handle_input(now, bytes)` feeds bytes read from the link, in any
  chunking.
- `Transfer::handle_timeout(now)` tells the machine time has passed; call it
  when `Transfer::next_timeout()` is reached (it does nothing early).
- `Transfer::poll_output(now)` hands out the next write: one whole block, one
  control byte, or the CAN sequence.
  The transport must put it on the wire as one unit, without inter-byte
  gaps (see the HP notes below).
- `Transfer::poll_event()` yields events: `Started`, `Progress`, a receiver's
  `FileEnd` and finally exactly one `Done` or `Error`.
- `Transfer::next_timeout()` is when the machine wants to be called again; use
  it as the read timeout.

After `Done` or `Error` keep calling `poll_output` until it returns `None`: the
final ACK or the CANs may still be queued. A receiver lingers briefly after
the final ACK (`Config::linger`) to re-ACK a retransmitted EOT; keep driving
it until `next_timeout()` is `None`, or start the next transfer.

## Driver loop

```rust,no_run
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};
use xmodem_proto::{Command, Config, Event, Transfer};

fn main() -> std::io::Result<()> {
    let mut link = TcpStream::connect("localhost:4848")?;
    let mut xfer = Transfer::new(Config::default());
    xfer.start(Instant::now(), Command::Receive)
        .map_err(std::io::Error::other)?;
    let mut finished = false;
    let mut buf = [0u8; 2048];
    loop {
        let now = Instant::now();
        while let Some(bytes) = xfer.poll_output(now) {
            // A whole block per call; the link must not split it.
            link.write_all(&bytes)?;
        }
        while let Some(event) = xfer.poll_event() {
            match event {
                Event::FileEnd { data, .. } => std::fs::write("file.hp", data)?,
                Event::Done | Event::Error(_) => finished = true,
                _ => {}
            }
        }
        let wait = match xfer.next_timeout() {
            Some(t) => t.saturating_duration_since(now),
            None if finished => break, // idle and nothing left to write
            None => Duration::from_millis(100),
        };
        link.set_read_timeout(Some(wait.max(Duration::from_millis(10))))?;
        match link.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => xfer.handle_input(Instant::now(), &buf[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                xfer.handle_timeout(Instant::now())
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
```

## HP calculator notes

- No HP calculator answers `C`. The 49G answers `D` with HP-CRC blocks (1k
  blocks for bigger objects); the 48G/GX knows only checksum mode, ignores `C`
  and `D`, and rejects 1k blocks. Hence the defaults: the receiver asks with
  `D`, and the sender sends 1k blocks only to a receiver that asked for a CRC.
- The 49G's `XRECV` asks with `D` three times, then falls back to NAK; before
  the first ACK the sender follows such a change and re-sends block 1 in the
  check asked for.
- `XRECV` and `XSEND` do not run inside the Kermit server ("Port Not
  Available"): leave server mode and start them on the calculator. The machine
  skips a Kermit packet still in the pipe before the transfer starts.
- XModem has no length field: the receiver hands back every byte, padding of
  the last block included (`Event::FileEnd` says how much padding there can
  be). For an HP object the real end is found by walking the object from its
  prolog. The 49G pads with whatever follows the object in memory, so trailing
  bytes are not a reliable hint.
- The 49G does not convert a received object with more than about 255 bytes of
  padding: with 1k blocks the sender sends the tail in 128-byte blocks
  (`Config::short_tail`).
- Each block must reach the wire as one unit: inter-byte gaps overrun the
  HP's receiver. This is a requirement on the transport, not something
  `write_all` guarantees (it may split the buffer into several writes). On a
  TCP socket to an emulator, or a serial port whose driver buffers a whole
  1k block, one `write_all` per block is enough; otherwise buffer the block
  and hand it to the port in one call.

## License

MIT
