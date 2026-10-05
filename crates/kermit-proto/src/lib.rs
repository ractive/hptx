//! Sans-IO Kermit protocol for talking to HP calculators.
//!
//! The crate never touches a port, a clock or a file. The caller owns all
//! three: it reads and writes the link, supplies the current [`Instant`]
//! ([`time::Instant`]) and loads or stores file contents. [`Client`] is a
//! pure state machine in between.
//!
//! Only client mode is implemented: we are the host (user Kermit), the
//! calculator is the server.
//!
//! # The seam
//!
//! - [`Client::start`] begins a transaction ([`Command`]); the first packet is
//!   queued at once. It returns a [`StartError`] while a transaction is
//!   running or if the command does not fit in one packet.
//! - [`Client::handle_input`] feeds bytes read from the link, in any chunking.
//! - [`Client::handle_timeout`] tells the client that time has passed; call it
//!   when [`Client::next_timeout`] is reached (or whenever a read times out;
//!   it does nothing early).
//! - [`Client::poll_output`] hands out the next packet to write. Each result is
//!   exactly one packet including padding and EOL. The transport must put it
//!   on the wire as one unit, without inter-byte gaps, because gaps overrun
//!   the HP's receiver. `write_all` alone does not promise that (it may
//!   issue several writes); it is enough where one write of a short buffer
//!   is not split, as on a TCP socket to an emulator or a serial port with
//!   a large enough output buffer.
//! - [`Client::poll_event`] yields [`Event`]s: file starts, data, progress,
//!   server text and finally exactly one `Done` or `Error`.
//! - [`Client::next_timeout`] is the earliest instant at which the client wants
//!   to be called again: a delayed packet becomes due (`packet_pause`), the
//!   retransmit deadline expires or the linger after a receive ends. Use it
//!   as the read timeout.
//!
//! Every call that can queue output takes `now`, so delays and deadlines are
//! computed from the caller's clock. After `Done` or `Error` keep calling
//! `poll_output` until it returns `None`: the final ACK or an E packet may
//! still be queued. After a receive (`Get`, or a long reply to `Host` or
//! `Directory`) the client lingers for [`Config::linger`] to re-ACK a
//! retransmitted `B` in case the final ACK was lost; keep driving it until
//! [`Client::next_timeout`] is `None`, or [`Client::start`] the next command,
//! which ends the linger.
//!
//! In server mode the HP periodically NAKs packet 0 while idle, so a stale NAK
//! can be in the pipe right after connecting. Discarding input for about
//! 0.5 s after opening the link is the transport's job; the state machine
//! also tolerates a stale NAK that slips through (it waits
//! [`Config::nak_grace`] before resending).
//!
//! # Time and WebAssembly
//!
//! The state machine never reads a clock: every call that needs the time
//! takes `now` from the caller. The type is [`time::Instant`], which is
//! `std::time::Instant` on every target except `wasm32`, where it is
//! `web_time::Instant` from the [`web-time`](https://docs.rs/web-time) crate
//! (`std::time::Instant::now()` panics on `wasm32-unknown-unknown`). A wasm
//! caller names it as `kermit_proto::time::Instant` and creates it with
//! `Instant::now()`, which `web-time` backs with `performance.now()` in the
//! browser. [`time::Duration`] is `core::time::Duration` everywhere.
//!
//! # Driver loop
//!
//! ```no_run
//! use std::io::{ErrorKind, Read, Write};
//! use std::net::TcpStream;
//! use std::time::{Duration, Instant};
//! use kermit_proto::{Client, Command, Config, Event};
//!
//! fn main() -> std::io::Result<()> {
//!     let mut link = TcpStream::connect("localhost:4848")?;
//!     let mut client = Client::new(Config::default());
//!     client
//!         .start(Instant::now(), Command::Host(b"6 7 *".to_vec()))
//!         .map_err(std::io::Error::other)?;
//!     let mut finished = false;
//!     let mut buf = [0u8; 512];
//!     loop {
//!         let now = Instant::now();
//!         while let Some(packet) = client.poll_output(now) {
//!             // A whole packet per call; the link must not split it.
//!             link.write_all(&packet)?;
//!         }
//!         while let Some(event) = client.poll_event() {
//!             match event {
//!                 Event::ServerText(text) => println!("{}", String::from_utf8_lossy(&text)),
//!                 Event::Done | Event::Error(_) => finished = true,
//!                 _ => {}
//!             }
//!         }
//!         let wait = match client.next_timeout() {
//!             Some(t) => t.saturating_duration_since(now),
//!             None if finished => break, // idle and nothing left to write
//!             None => Duration::from_millis(100),
//!         };
//!         link.set_read_timeout(Some(wait.max(Duration::from_millis(10))))?;
//!         match link.read(&mut buf) {
//!             Ok(0) => break,
//!             Ok(n) => client.handle_input(Instant::now(), &buf[..n]),
//!             Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
//!                 client.handle_timeout(Instant::now())
//!             }
//!             Err(e) => return Err(e),
//!         }
//!     }
//!     Ok(())
//! }
//! ```
//!
//! The lower layers are public for tools and tests: [`codec`] (framing and
//! block checks), [`prefix`] (control/8th-bit/repeat prefixing), [`params`]
//! (Send-Init negotiation) and [`trace`] (the text trace format).
//!
//! [`Instant`]: time::Instant

mod client;
pub mod codec;
pub mod params;
pub mod prefix;
pub mod trace;
#[cfg(test)]
mod trace_tests;

/// The clock types of the seam: `std::time` everywhere except `wasm32`,
/// where they come from `web-time` (see the crate docs, "Time and
/// WebAssembly").
pub mod time {
    #[cfg(not(target_arch = "wasm32"))]
    pub use std::time::{Duration, Instant};
    #[cfg(target_arch = "wasm32")]
    pub use web_time::{Duration, Instant};
}

pub use client::{Client, Command, Config, Error, Event, OutgoingFile, StartError};
pub use codec::BlockCheck;
pub use params::InitParams;
