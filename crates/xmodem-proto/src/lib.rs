//! Sans-IO XModem for talking to HP calculators: 128-byte and 1k blocks,
//! checksum and CRC-16, both roles.
//!
//! The crate never touches a port, a clock or a file. The caller owns all
//! three: it reads and writes the link, supplies the current [`Instant`]
//! (`std::time::Instant`) and loads or stores file contents. [`Transfer`] is a
//! pure state machine in between, with the same seam as `kermit-proto`.
//!
//! - Sender ([`Command::Send`]): the calculator runs `XRECV`. We wait for its
//!   start character, NAK (checksum), `C` (CRC-16) or `D` (HP's CRC,
//!   [`Check::HpCrc`]), and use whichever it asks for. The 49G asks with `D`,
//!   the 48GX with NAK.
//! - Receiver ([`Command::Receive`]): the calculator runs `XSEND`. We send `D`
//!   (by default; see [`Config::check`]) [`Config::crc_attempts`] times, then
//!   fall back to NAK (checksum). The 49G answers `D`, the 48GX only NAK;
//!   neither answers `C`.
//!
//! HP specifics live in the caller: starting `XRECV`/`XSEND` on the
//! calculator, and stripping the padding of the last block (the receiver hands
//! back every byte it received; [`Event::FileEnd`] says how much padding there
//! can be).
//!
//! # Starting a transfer on the calculator
//!
//! `XRECV` and `XSEND` do not run inside the Kermit server: sent as a Kermit
//! `C` packet, both fail with "Port Not Available" on the 49G and the 48GX,
//! even after `CLOSEIO`. The server answers the `C` packet with its usual
//! reply (S, X, D packets carrying the error and the stack, which still holds
//! the name) and stays in server mode. The calculator must leave the server
//! (Kermit FINISH, or ON) and run the command itself, e.g. typed on the
//! keyboard. The machine tolerates a Kermit packet in the pipe before the
//! first start character or block. Verified on the emulated 49G and 48GX
//! (traces `49g-server-xrecv`, `48gx-server-xsend`).
//!
//! # The seam
//!
//! - [`Transfer::start`] begins a transfer. A receiver's first start character
//!   is queued at once; a sender waits for the receiver.
//! - [`Transfer::handle_input`] feeds bytes read from the link, in any
//!   chunking. Bytes of a Kermit packet (SOH .. CR) that arrive before the
//!   transfer starts are skipped.
//! - [`Transfer::handle_timeout`] tells the machine that time has passed; call
//!   it when [`Transfer::next_timeout`] is reached (it does nothing early).
//! - [`Transfer::poll_output`] hands out the next write: one whole block, one
//!   control byte, or the CAN sequence. The transport must put each on the
//!   wire as one unit, without inter-byte gaps, because gaps overrun the HP's
//!   receiver. `write_all` alone does not promise that (it may issue several
//!   writes); it is enough where one write of a buffer this size is not
//!   split, as on a TCP socket to an emulator or a serial port with a large
//!   enough output buffer.
//! - [`Transfer::poll_event`] yields [`Event`]s: `Started`, `Progress`, a
//!   receiver's `FileEnd`, and finally exactly one `Done` or `Error`.
//! - [`Transfer::next_timeout`] is the earliest instant at which the machine
//!   wants to be called again (reply deadline, start-character interval,
//!   inter-byte timeout, quiet line before a NAK, end of the linger). Use it
//!   as the read timeout.
//!
//! Every call that can queue output takes `now`, so retransmit deadlines are
//! computed from the caller's clock. After `Done` or `Error` keep calling
//! `poll_output` until it returns `None`: the final ACK or the CANs may still
//! be queued. A receiver then lingers for [`Config::linger`] to re-ACK a
//! retransmitted EOT in case the final ACK was lost; keep driving it until
//! [`Transfer::next_timeout`] is `None`, or [`Transfer::start`] the next
//! transfer, which ends the linger.
//!
//! # Driver loop
//!
//! ```no_run
//! use std::io::{ErrorKind, Read, Write};
//! use std::net::TcpStream;
//! use std::time::{Duration, Instant};
//! use xmodem_proto::{Command, Config, Event, Transfer};
//!
//! fn main() -> std::io::Result<()> {
//!     let mut link = TcpStream::connect("localhost:4848")?;
//!     let mut xfer = Transfer::new(Config::default());
//!     xfer.start(Instant::now(), Command::Receive)
//!         .map_err(std::io::Error::other)?;
//!     let mut finished = false;
//!     let mut buf = [0u8; 2048];
//!     loop {
//!         let now = Instant::now();
//!         while let Some(bytes) = xfer.poll_output(now) {
//!             // A whole block per call; the link must not split it.
//!             link.write_all(&bytes)?;
//!         }
//!         while let Some(event) = xfer.poll_event() {
//!             match event {
//!                 Event::FileEnd { data, .. } => std::fs::write("file.hp", data)?,
//!                 Event::Done | Event::Error(_) => finished = true,
//!                 _ => {}
//!             }
//!         }
//!         let wait = match xfer.next_timeout() {
//!             Some(t) => t.saturating_duration_since(now),
//!             None if finished => break, // idle and nothing left to write
//!             None => Duration::from_millis(100),
//!         };
//!         link.set_read_timeout(Some(wait.max(Duration::from_millis(10))))?;
//!         match link.read(&mut buf) {
//!             Ok(0) => break,
//!             Ok(n) => xfer.handle_input(Instant::now(), &buf[..n]),
//!             Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
//!                 xfer.handle_timeout(Instant::now())
//!             }
//!             Err(e) => return Err(e),
//!         }
//!     }
//!     Ok(())
//! }
//! ```
//!
//! The block layer is public for tools and tests: [`codec`] (control bytes,
//! framing, checksum and CRC-16).
//!
//! [`Instant`]: std::time::Instant

pub mod codec;
#[cfg(test)]
mod trace_tests;
mod transfer;

pub use codec::{BlockSize, Check};
pub use transfer::{Command, Config, Error, Event, StartError, Transfer};
