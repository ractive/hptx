//! HP calculator layer on top of `kermit-proto`: transports, the Kermit
//! driver loop, server commands, the HP object format and GROBs.
//!
//! - [`transport`]: serial port, TCP and in-memory links ([`transport::open`]).
//! - [`session`]: drives a [`kermit_proto::Client`] over a transport.
//! - [`calc`]: what a user does with a calculator: list, get, put, run host
//!   commands, screenshot, backup and restore ([`Calculator`]).
//! - [`reply`]: parsers for the server's text replies (`G D` listings and the
//!   stack text returned for a `C` host command).
//! - [`object`]: `HPHP48-x` / `HPHP49-x` binary files, prologs, object
//!   length walk, `%%HP:` ASCII headers.
//! - [`grob`]: GROB decoding and PNG export.
//! - [`charset`]: the HP character set and its ASCII trigraphs.
//! - [`xmodem`]: XModem transfers with `XRECV`/`XSEND` started on the
//!   calculator ([`XmodemSession`]).
//! - [`xserv`]: XSERV (49g+/50g) packet framing; unverified on hardware.
//!
//! Wiki references (`~/devel/hp-literature/`): `protocols/server-commands`,
//! `protocols/hp-object-format`, `protocols/iopar`, `hardware/uart`,
//! `protocols/xmodem-hp`, `protocols/xserv`.

pub mod calc;
pub mod charset;
mod error;
pub mod grob;
pub mod object;
pub mod reply;
pub mod session;
pub mod transport;
pub mod xmodem;
pub mod xserv;

pub use calc::{Calculator, Model, TransferMode};
pub use error::{Error, Result};
pub use session::{Options, Session};
pub use xmodem::{XmodemOptions, XmodemSession};
pub use xmodem_proto;
