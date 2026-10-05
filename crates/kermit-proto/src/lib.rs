//! Sans-IO Kermit protocol for talking to HP calculators.
//!
//! The crate never touches a port, a clock or a file. The caller owns all
//! three: it reads and writes the link, supplies the current [`Instant`]
//! (`std::time::Instant`) and loads or stores file contents. [`Client`] is a
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
//!   exactly one packet including padding and EOL; write it in one go
//!   (`write_all` of the whole buffer), because inter-byte gaps overrun the
//!   HP's receiver.
//! - [`Client::poll_event`] yields [`Event`]s: file starts, data, progress,
//!   server text and finally exactly one `Done` or `Error`.
//! - [`Client::next_timeout`] is the earliest instant at which the client wants
//!   to be called again: a delayed packet becomes due (`packet_pause`) or the
//!   retransmit deadline expires. Use it as the read timeout.
//!
//! Every call that can queue output takes `now`, so delays and deadlines are
//! computed from the caller's clock. After `Done` or `Error` keep calling
//! `poll_output` until it returns `None`: the final ACK or an E packet may
//! still be queued.
//!
//! In server mode the HP periodically NAKs packet 0 while idle, so a stale NAK
//! can be in the pipe right after connecting. Discarding input for about
//! 0.5 s after opening the link is the transport's job; the state machine
//! also tolerates a stale NAK that slips through (it waits
//! [`Config::nak_grace`] before resending).
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
//!             link.write_all(&packet)?; // one packet, one write
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
//! [`Instant`]: std::time::Instant

mod client;
pub mod codec;
pub mod params;
pub mod prefix;
pub mod trace;
#[cfg(test)]
mod trace_tests;

pub use client::{Client, Command, Config, Error, Event, OutgoingFile, StartError};
pub use codec::BlockCheck;
pub use params::InitParams;
