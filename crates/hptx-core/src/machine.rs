//! The sans-I/O face of hptx-core: a [`Machine`] is fed bytes, time and
//! link errors, and hands out bytes to write, progress events and the
//! result of each operation. It never reads a clock, sleeps, blocks or
//! touches a port, so it runs where the bytes arrive by callback: a browser
//! with Web Serial (WebAssembly), an event loop, a GUI thread.
//!
//! The calling convention is the one of [`kermit_proto::Client`], one level
//! up: a whole calculator operation ([`Op`]) instead of one Kermit
//! transaction. The operations are the ones of
//! [`Calculator`](crate::Calculator) and
//! [`XmodemSession`](crate::XmodemSession), which run the same code over a
//! blocking [`Transport`](crate::transport::Transport).
//!
//! # The seam
//!
//! - [`Machine::new`] takes the caller's clock, the [`Options`] and a seed
//!   for the sync markers (from a random source such as
//!   `crypto.getRandomValues`).
//! - [`Machine::start`] begins an operation; its first bytes are queued at
//!   once. One operation runs at a time.
//! - [`Machine::handle_input`] feeds bytes read from the link, in any
//!   chunking, with the time they arrived.
//! - [`Machine::handle_timeout`] tells the machine the time; call it when
//!   [`Machine::next_timeout`] is reached (early calls do nothing).
//! - [`Machine::handle_link_error`] reports a failed or closed link.
//! - [`Machine::poll_transmit`] hands out the next chunk to write. Write
//!   each one as one unit, without inter-byte gaps: the HP's receiver
//!   overruns on gaps.
//! - [`Machine::poll_event`] yields [`Progress`] events (Kermit or XModem):
//!   take them, or turn them off with [`Machine::set_events`], since data
//!   events carry the file contents.
//! - [`Machine::poll_result`] yields the [`Reply`] once the operation is
//!   over.
//!
//! After every `start` and `handle_*` call, write everything
//! `poll_transmit` hands out, then arm a timer for `next_timeout`.
//!
//! # Time
//!
//! Every call that can make progress takes `now`, a
//! [`time::Instant`](crate::time::Instant) of the caller's monotonic clock:
//! `std::time::Instant` on native targets, `web_time::Instant` on wasm32
//! (`Instant::now()` is `performance.now()` there). The machine keeps the
//! latest `now` it was given and never lets it run backwards.
//!
//! # A host loop
//!
//! ```
//! use hptx_core::machine::{Machine, Op, Reply};
//! use hptx_core::time::{Duration, Instant};
//! use hptx_core::Options;
//!
//! // Stand-ins for the host's serial port and clock.
//! fn write_to_port(_bytes: &[u8]) {}
//! fn wait_for_bytes_until(_deadline: Option<Instant>) -> Option<Vec<u8>> { None }
//!
//! let mut options = Options::default();
//! options.kermit.timeout = Duration::from_millis(50);
//! options.drain = Duration::ZERO;
//! let mut m = Machine::new(Instant::now(), options, 0x5eed);
//! m.start(Instant::now(), Op::List).unwrap();
//! let result = loop {
//!     while let Some(bytes) = m.poll_transmit() {
//!         write_to_port(&bytes);
//!     }
//!     while let Some(_event) = m.poll_event() {}
//!     if let Some(result) = m.poll_result() {
//!         break result;
//!     }
//!     match wait_for_bytes_until(m.next_timeout()) {
//!         Some(bytes) => m.handle_input(Instant::now(), &bytes),
//!         None => m.handle_timeout(Instant::now()),
//!     }
//! };
//! // Nobody answered.
//! assert!(result.is_err());
//! ```
//!
//! In a browser the loop is inverted: the Web Serial reader's callback calls
//! `handle_input`, a `setTimeout` for `next_timeout` calls
//! `handle_timeout`, and each of them is followed by the drain of
//! `poll_transmit`, `poll_event` and `poll_result`.

use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::Poll;

use kermit_proto::Command;

#[cfg(test)]
use crate::Error;
use crate::Result;
use crate::calc::{CalcOps, CalcState, Model, TransferMode};
use crate::grob::Grob;
use crate::link::Link;
pub use crate::link::Progress;
use crate::reply::{Iopar, Listing, StackReply};
use crate::session::{KermitCore, Options, Transcript};
use crate::time::Instant;
use crate::xmodem::{
    self, XmodemDirection, XmodemOptions, XmodemPlan, XmodemReceived, XmodemReport,
};

/// One operation of a [`Machine`]; each is the
/// [`Calculator`](crate::Calculator) or
/// [`XmodemSession`](crate::XmodemSession) method of the same name, and
/// its result is the [`Reply`] variant named.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Op {
    /// Discard input for [`Options::drain`] (stale NAKs of the idle
    /// server), then [`Op::Sync`]: the start of every connection.
    /// [`Reply::Done`].
    Connect,
    /// [`Calculator::sync`](crate::Calculator::sync). [`Reply::Done`].
    Sync,
    /// One raw Kermit transaction. [`Reply::Transcript`].
    Transact(Command),
    /// A host command. [`Reply::Stack`].
    Run(String),
    /// [`Reply::Listing`].
    List,
    /// [`Reply::Path`].
    Path,
    /// Change to an absolute directory path. [`Reply::Done`].
    Cd(Vec<String>),
    /// [`Reply::Done`].
    Updir,
    /// [`Reply::Done`].
    Mkdir(String),
    /// [`Reply::Done`].
    Remove(String),
    /// [`Reply::Done`].
    Rename {
        /// The existing name.
        from: String,
        /// The new name, which must not exist.
        to: String,
    },
    /// Free memory. [`Reply::Real`].
    Mem,
    /// [`Reply::Version`].
    Version,
    /// [`Reply::Model`].
    Model,
    /// [`Reply::Iopar`].
    Iopar,
    /// [`Reply::Done`].
    SetIopar(Iopar),
    /// [`Reply::TransferMode`].
    TransferMode,
    /// [`Reply::Done`].
    SetTransferMode(TransferMode),
    /// GET a variable. [`Reply::Data`].
    Get {
        /// The variable name.
        name: String,
        /// The transfer mode.
        mode: TransferMode,
    },
    /// SEND a variable. [`Reply::Stored`].
    Put {
        /// The variable name.
        name: String,
        /// The file contents.
        data: Vec<u8>,
        /// The transfer mode.
        mode: TransferMode,
    },
    /// [`Reply::Pict`].
    Pict,
    /// [`Reply::Data`].
    Backup,
    /// [`Reply::Done`].
    Restore(Vec<u8>),
    /// [`Reply::Done`].
    PurgeRestoreLeftover,
    /// End server mode (`G F`). [`Reply::Done`].
    Finish,
    /// [`Reply::XmodemPlan`].
    PrepareXmodem {
        /// Which way the transfer goes.
        direction: XmodemDirection,
        /// The variable name.
        name: String,
    },
    /// Send a file to a calculator running `XRECV`. [`Reply::XmodemSent`].
    XmodemSend {
        /// The file.
        data: Vec<u8>,
        /// The XModem options (see [`XmodemOptions::for_model`]).
        options: XmodemOptions,
    },
    /// Receive a file from a calculator running `XSEND`.
    /// [`Reply::XmodemReceived`].
    XmodemReceive {
        /// The XModem options (see [`XmodemOptions::for_model`]).
        options: XmodemOptions,
    },
}

/// The result of an [`Op`].
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Reply {
    /// The operation has no value.
    Done,
    /// What a raw transaction produced.
    Transcript(Transcript),
    /// The stack after a host command.
    Stack(StackReply),
    /// A directory listing.
    Listing(Listing),
    /// The current directory, e.g. `["HOME", "D1"]`.
    Path(Vec<String>),
    /// A number (free memory).
    Real(f64),
    /// The ROM version text; `None` on the 48SX.
    Version(Option<String>),
    /// The model.
    Model(Model),
    /// The serial settings.
    Iopar(Iopar),
    /// The transfer mode.
    TransferMode(TransferMode),
    /// File contents (GET, backup).
    Data(Vec<u8>),
    /// The name the calculator stored a sent file under.
    Stored(String),
    /// The PICT graphic.
    Pict(Grob),
    /// What the user must type for an XModem transfer.
    XmodemPlan(XmodemPlan),
    /// An XModem send finished.
    XmodemSent(XmodemReport),
    /// An XModem receive finished.
    XmodemReceived(XmodemReceived),
}

/// [`Machine::start`] while an operation runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Busy;

impl fmt::Display for Busy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an operation is already running")
    }
}

impl std::error::Error for Busy {}

/// The protocol state that outlives an operation.
#[derive(Debug)]
struct Core {
    kermit: KermitCore,
    calc: CalcState,
}

type Task = Pin<Box<dyn Future<Output = (Box<Core>, Result<Reply>)> + Send>>;

/// A calculator link without I/O. See the [module documentation](self).
pub struct Machine {
    options: Options,
    link: Link,
    /// The protocol state while no operation runs (the task owns it
    /// meanwhile).
    core: Option<Box<Core>>,
    task: Option<Task>,
    result: Option<Result<Reply>>,
    /// Seed of the next [`CalcState`] (after an abort).
    seed: u64,
}

impl fmt::Debug for Machine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Machine")
            .field("busy", &self.is_busy())
            .field("next_timeout", &self.next_timeout())
            .finish_non_exhaustive()
    }
}

impl Machine {
    /// A machine for a newly opened link at `now`. `seed` should be random
    /// (it picks the sync markers that tell this connection's replies from
    /// a late one of an earlier client).
    pub fn new(now: Instant, options: Options, seed: u64) -> Self {
        let link = Link::new(now);
        let core = Core {
            kermit: KermitCore::new(link.clone(), &options),
            calc: CalcState::new(seed),
        };
        Machine {
            options,
            link,
            core: Some(Box::new(core)),
            task: None,
            result: None,
            seed: seed.rotate_left(17) ^ 0xA5A5_A5A5_A5A5_A5A5,
        }
    }

    /// Begin `op`; its first bytes are ready for [`Machine::poll_transmit`]
    /// at once. A result not yet taken with [`Machine::poll_result`] is
    /// dropped.
    pub fn start(&mut self, now: Instant, op: Op) -> std::result::Result<(), Busy> {
        let Some(mut core) = self.core.take() else {
            return Err(Busy);
        };
        self.link.set_now(now);
        self.link.clear_error();
        self.result = None;
        let drain = self.options.drain;
        let link = self.link.clone();
        self.task = Some(Box::pin(async move {
            let result = perform(&mut core, &link, drain, op).await;
            (core, result)
        }));
        self.step();
        Ok(())
    }

    /// Feed bytes read from the link at `now`.
    pub fn handle_input(&mut self, now: Instant, bytes: &[u8]) {
        self.link.set_now(now);
        self.link.push_input(bytes);
        self.step();
    }

    /// Tell the machine the time is `now`.
    pub fn handle_timeout(&mut self, now: Instant) {
        self.link.set_now(now);
        self.step();
    }

    /// The link failed or closed (a closed port is
    /// [`io::ErrorKind::UnexpectedEof`]): the running operation fails with
    /// [`Error::Io`](crate::Error::Io), except that a Kermit or XModem
    /// transfer already complete keeps its result. The next
    /// [`Machine::start`] clears the error.
    pub fn handle_link_error(&mut self, now: Instant, error: &io::Error) {
        self.link.set_now(now);
        self.link.fail(error);
        self.step();
    }

    /// The next chunk to write, one write each.
    pub fn poll_transmit(&mut self) -> Option<Vec<u8>> {
        self.link.take_output()
    }

    /// The next progress event.
    pub fn poll_event(&mut self) -> Option<Progress> {
        self.link.take_event()
    }

    /// Whether progress events are kept for [`Machine::poll_event`]
    /// (default `true`). Off, they are dropped as they happen.
    pub fn set_events(&mut self, keep: bool) {
        self.link.set_keep_events(keep);
    }

    /// The result of the last operation, once it is over.
    pub fn poll_result(&mut self) -> Option<Result<Reply>> {
        self.result.take()
    }

    /// When to call [`Machine::handle_timeout`] at the latest; `None` while
    /// no operation runs.
    pub fn next_timeout(&self) -> Option<Instant> {
        self.task.as_ref().and(self.link.wake())
    }

    /// Whether an operation runs.
    pub fn is_busy(&self) -> bool {
        self.task.is_some()
    }

    /// Drop the running operation at once, without telling the calculator.
    /// Queued bytes are forgotten; the calculator may still be in the
    /// middle of a transaction and answers the next command late, which the
    /// next [`Op::Connect`] or [`Op::Sync`] sorts out. The cached transfer
    /// mode is forgotten; the session keeps its options. No-op when idle.
    pub fn abort(&mut self, now: Instant) {
        if self.task.take().is_none() {
            return;
        }
        self.link.set_now(now);
        self.link.reset();
        let mut kermit = KermitCore::new(self.link.clone(), &self.options);
        kermit.ended_at(now);
        let seed = self.seed;
        self.seed = seed.rotate_left(17) ^ 0xA5A5_A5A5_A5A5_A5A5;
        self.core = Some(Box::new(Core {
            kermit,
            calc: CalcState::new(seed),
        }));
    }

    fn step(&mut self) {
        let Some(task) = self.task.as_mut() else {
            return;
        };
        if let Poll::Ready((core, result)) = self.link.poll(task.as_mut()) {
            self.core = Some(core);
            self.task = None;
            self.result = Some(result);
        }
    }
}

async fn perform(
    core: &mut Core,
    link: &Link,
    drain: crate::time::Duration,
    op: Op,
) -> Result<Reply> {
    let mut c = CalcOps {
        k: &mut core.kermit,
        st: &mut core.calc,
    };
    Ok(match op {
        Op::Connect => {
            link.drain(drain).await?;
            c.sync().await?;
            Reply::Done
        }
        Op::Sync => c.sync().await.map(|()| Reply::Done)?,
        Op::Transact(command) => Reply::Transcript(c.k.transact(command).await?),
        Op::Run(command) => Reply::Stack(c.run(&command).await?),
        Op::List => Reply::Listing(c.list().await?),
        Op::Path => Reply::Path(c.path().await?),
        Op::Cd(path) => {
            let path: Vec<&str> = path.iter().map(String::as_str).collect();
            c.cd(&path).await?;
            Reply::Done
        }
        Op::Updir => c.updir().await.map(|()| Reply::Done)?,
        Op::Mkdir(name) => c.mkdir(&name).await.map(|()| Reply::Done)?,
        Op::Remove(name) => c.remove(&name).await.map(|()| Reply::Done)?,
        Op::Rename { from, to } => c.rename(&from, &to).await.map(|()| Reply::Done)?,
        Op::Mem => Reply::Real(c.mem().await?),
        Op::Version => Reply::Version(c.version().await?),
        Op::Model => Reply::Model(c.model().await?),
        Op::Iopar => Reply::Iopar(c.iopar().await?),
        Op::SetIopar(iopar) => c.set_iopar(&iopar).await.map(|()| Reply::Done)?,
        Op::TransferMode => Reply::TransferMode(c.transfer_mode().await?),
        Op::SetTransferMode(mode) => c.set_transfer_mode(mode).await.map(|()| Reply::Done)?,
        Op::Get { name, mode } => Reply::Data(c.get(&name, mode).await?),
        Op::Put { name, data, mode } => Reply::Stored(c.put(&name, &data, mode).await?),
        Op::Pict => Reply::Pict(c.pict().await?),
        Op::Backup => Reply::Data(c.backup().await?),
        Op::Restore(data) => c.restore(&data).await.map(|()| Reply::Done)?,
        Op::PurgeRestoreLeftover => c.purge_restore_leftover().await.map(|()| Reply::Done)?,
        Op::Finish => c.finish().await.map(|()| Reply::Done)?,
        Op::PrepareXmodem { direction, name } => {
            Reply::XmodemPlan(c.prepare_for_xmodem(direction, &name).await?)
        }
        Op::XmodemSend { data, options } => {
            Reply::XmodemSent(xmodem::send(link, &options, &data).await?)
        }
        Op::XmodemReceive { options } => {
            Reply::XmodemReceived(xmodem::receive(link, &options).await?)
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::time::Duration;
    use kermit_proto::codec::{BlockCheck, Deframer, Framing, Packet, parse_frame};
    use std::sync::{Arc, Mutex};
    use xmodem_proto::codec::{ACK, EOT, NAK, decode_block, encode_block};
    use xmodem_proto::{BlockSize, Check};

    /// 10 bits per byte at 9600 baud, rounded up to whole microseconds.
    fn wire(bytes: usize) -> Duration {
        Duration::from_micros(1042 * bytes as u64)
    }

    /// Drive `m` against `peer` on a fake clock: every chunk written goes to
    /// the peer, whose answer arrives after both wire times; with nothing
    /// to write and no result, the clock jumps to the machine's deadline. No
    /// threads, no sleeps.
    fn run(
        m: &mut Machine,
        clock: &mut Instant,
        peer: &mut dyn FnMut(&[u8]) -> Vec<Vec<u8>>,
    ) -> Result<Reply> {
        for _ in 0..100_000 {
            while let Some(chunk) = m.poll_transmit() {
                *clock += wire(chunk.len());
                for answer in peer(&chunk) {
                    *clock += wire(answer.len());
                    m.handle_input(*clock, &answer);
                }
            }
            if let Some(result) = m.poll_result() {
                return result;
            }
            let deadline = m.next_timeout().expect("waits without a deadline");
            *clock = (*clock).max(deadline);
            m.handle_timeout(*clock);
        }
        panic!("no result");
    }

    fn options(timeout: Duration) -> Options {
        let mut options = Options::default();
        options.kermit.timeout = timeout;
        options.kermit.linger = Duration::ZERO;
        options.drain = Duration::ZERO;
        options.turnaround = Duration::ZERO;
        options
    }

    /// A Kermit server that sends file `X` (two D packets) in reply to R
    /// and drops the first transmission of D seq 2, so only the client's
    /// timeout and NAK recover it.
    fn lossy_server(naks: Arc<Mutex<u32>>) -> impl FnMut(&[u8]) -> Vec<Vec<u8>> {
        let check = BlockCheck::Type1;
        let mut deframer = Deframer::new();
        let mut dropped = false;
        move |bytes| {
            let wire = |seq: u8, kind: u8, data: &[u8]| {
                Packet::new(seq, kind, data.to_vec())
                    .encode(check, &Framing::default())
                    .unwrap()
            };
            deframer.push(bytes);
            let mut replies = Vec::new();
            while let Some(frame) = deframer.next_frame() {
                let p = parse_frame(&frame, check).unwrap();
                match (p.kind, p.seq) {
                    (b'R', _) => replies.push(wire(0, b'S', b"~* @-#Y1 ")),
                    (b'Y', 0) => replies.push(wire(1, b'F', b"X")),
                    (b'Y', 1) if !dropped => dropped = true,
                    (b'Y', 1) => {}
                    (b'N', 2) => {
                        *naks.lock().unwrap() += 1;
                        replies.push(wire(2, b'D', b"hello "));
                    }
                    (b'Y', 2) => replies.push(wire(3, b'D', b"world")),
                    (b'Y', 3) => replies.push(wire(4, b'Z', b"")),
                    (b'Y', 4) => replies.push(wire(5, b'B', b"")),
                    (b'Y', 5) => {}
                    (kind, seq) => panic!("unexpected packet {} seq {seq}", char::from(kind)),
                }
            }
            replies
        }
    }

    /// A Kermit GET through the machine: the dropped packet costs one
    /// 20-second timeout of the fake clock and no real time.
    #[test]
    fn kermit_get_recovers_a_lost_packet_on_a_fake_clock() {
        let t0 = Instant::now();
        let mut clock = t0;
        let timeout = Duration::from_secs(20);
        let mut m = Machine::new(clock, options(timeout), 1);
        let naks = Arc::new(Mutex::new(0));
        let mut peer = lossy_server(Arc::clone(&naks));
        m.start(clock, Op::Transact(Command::Get(b"X".to_vec())))
            .unwrap();
        assert!(m.is_busy());
        let reply = run(&mut m, &mut clock, &mut peer).unwrap();
        let Reply::Transcript(t) = reply else {
            panic!("{reply:?}");
        };
        assert_eq!(t.files.len(), 1);
        assert_eq!(t.files[0].name, "X");
        assert_eq!(t.files[0].data, b"hello world");
        assert_eq!(*naks.lock().unwrap(), 1);
        assert!(clock - t0 >= timeout, "{:?}", clock - t0);
        assert!(t0.elapsed() < Duration::from_secs(2));
        assert!(!m.is_busy());
        assert_eq!(m.next_timeout(), None);
        let mut files = 0;
        while let Some(event) = m.poll_event() {
            if matches!(
                event,
                Progress::Kermit(kermit_proto::Event::FileStart { .. })
            ) {
                files += 1;
            }
        }
        assert_eq!(files, 1);
    }

    /// Calculator operations through the machine: connect (sync with the
    /// seeded marker), then a listing.
    #[test]
    fn connect_and_list_through_the_machine() {
        let log = Arc::default();
        let mut peer = crate::calc::tests::fake_server(Arc::clone(&log), |cmd: &str| {
            if cmd.starts_with("\"HPTX-") {
                format!("1:      {cmd}\r\n")
            } else if cmd == "DROP" {
                "Empty Stack\r\n".into()
            } else if cmd == "G D" {
                "{ HOME D1 } 127847\r\nX 10.5 Real Number 1234\r\n".into()
            } else {
                panic!("unexpected {cmd}")
            }
        });
        let mut clock = Instant::now();
        let mut m = Machine::new(clock, options(Duration::from_secs(5)), 7);
        m.set_events(false);
        m.start(clock, Op::Connect).unwrap();
        assert_eq!(m.start(clock, Op::List), Err(Busy));
        assert_eq!(run(&mut m, &mut clock, &mut peer).unwrap(), Reply::Done);
        m.start(clock, Op::List).unwrap();
        let Reply::Listing(listing) = run(&mut m, &mut clock, &mut peer).unwrap() else {
            panic!("not a listing");
        };
        assert_eq!(listing.path, Some(vec!["HOME".into(), "D1".into()]));
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(m.poll_event(), None);
        let sent = log.lock().unwrap().clone();
        assert_eq!(sent.len(), 3, "{sent:?}");
        assert!(sent[0].starts_with("\"HPTX-"));
        assert_eq!(sent[1..], ["DROP", "G D"]);
    }

    /// A 48GX-like XModem sender (checksum, 128-byte blocks) answering the
    /// machine's start NAKs.
    fn xsend_peer(file: Vec<u8>) -> impl FnMut(&[u8]) -> Vec<Vec<u8>> {
        let blocks: Vec<Vec<u8>> = file
            .chunks(128)
            .enumerate()
            .map(|(i, chunk)| {
                encode_block((i + 1) as u8, BlockSize::B128, chunk, 0, Check::Checksum).unwrap()
            })
            .collect();
        let mut next = 0;
        move |bytes| match bytes.last() {
            Some(&NAK) if next == 0 => {
                next = 1;
                vec![blocks[0].clone()]
            }
            Some(&ACK) if next < blocks.len() => {
                next += 1;
                vec![blocks[next - 1].clone()]
            }
            Some(&ACK) if next == blocks.len() => {
                next += 1;
                vec![vec![EOT]]
            }
            _ => Vec::new(),
        }
    }

    fn fast_xmodem() -> XmodemOptions {
        let mut options = XmodemOptions::for_model(Model::Hp48Gx).unwrap();
        options.xmodem.linger = Duration::ZERO;
        options
    }

    /// An HP String object file with `n` characters.
    fn hp_string(n: usize) -> Vec<u8> {
        let len = 5 + 2 * n;
        let mut nibbles = vec![0xC, 0x2, 0xA, 0x2, 0x0];
        nibbles.extend((0..5).map(|i| ((len >> (4 * i)) & 0xF) as u8));
        let mut out = b"HPHP48-R".to_vec();
        out.extend(crate::object::pack(&nibbles));
        out.extend((0..n).map(|i| (i % 251) as u8));
        out
    }

    /// An XModem receive through the machine on the fake clock: the
    /// sender answers the first start NAK, the padding is cut.
    #[test]
    fn xmodem_receive_through_the_machine() {
        let file = hp_string(300);
        let mut peer = xsend_peer(file.clone());
        let t0 = Instant::now();
        let mut clock = t0;
        let mut m = Machine::new(clock, options(Duration::from_secs(5)), 3);
        m.start(
            clock,
            Op::XmodemReceive {
                options: fast_xmodem(),
            },
        )
        .unwrap();
        let Reply::XmodemReceived(got) = run(&mut m, &mut clock, &mut peer).unwrap() else {
            panic!("not received");
        };
        assert_eq!(got.data, file);
        assert_eq!(got.check, Check::Checksum);
        assert_eq!(got.received, 384);
        assert_eq!(got.stripped, Some(384 - file.len()));
        assert!(t0.elapsed() < Duration::from_secs(2));
    }

    /// An XModem send through the machine: the receiver waits two of its
    /// 3-second periods before the first start NAK (the user typing
    /// `XRECV`), which only the fake clock sees.
    #[test]
    fn xmodem_send_through_the_machine() {
        let file = hp_string(200);
        let received = Arc::new(Mutex::new(Vec::new()));
        let got = Arc::clone(&received);
        let mut blocks = 0u8;
        let mut peer = move |bytes: &[u8]| -> Vec<Vec<u8>> {
            if bytes == [EOT] {
                return vec![vec![ACK]];
            }
            let block = decode_block(bytes, Check::Checksum).unwrap();
            blocks += 1;
            assert_eq!(block.num, blocks);
            got.lock().unwrap().extend_from_slice(&block.data);
            vec![vec![ACK]]
        };
        let t0 = Instant::now();
        let mut clock = t0;
        let mut m = Machine::new(clock, options(Duration::from_secs(5)), 4);
        m.start(
            clock,
            Op::XmodemSend {
                data: file.clone(),
                options: fast_xmodem(),
            },
        )
        .unwrap();
        // Nothing to send before the receiver asks.
        assert_eq!(m.poll_transmit(), None);
        clock += Duration::from_secs(6);
        m.handle_timeout(clock);
        m.handle_input(clock, &[NAK]);
        let Reply::XmodemSent(report) = run(&mut m, &mut clock, &mut peer).unwrap() else {
            panic!("not sent");
        };
        assert_eq!(report.check, Check::Checksum);
        assert_eq!(report.bytes, file.len() as u64);
        let data = received.lock().unwrap().clone();
        // Two 128-byte blocks, the second padded.
        assert_eq!(data.len(), 256);
        assert_eq!(&data[..file.len()], &file[..]);
        assert!(t0.elapsed() < Duration::from_secs(2));
    }

    /// A link error fails the running operation; an abort frees the
    /// machine for the next one.
    #[test]
    fn link_error_and_abort() {
        let mut clock = Instant::now();
        let mut m = Machine::new(clock, options(Duration::from_secs(5)), 5);
        m.start(clock, Op::List).unwrap();
        assert!(m.poll_transmit().is_some());
        clock += Duration::from_millis(5);
        m.handle_link_error(clock, &io::Error::from(io::ErrorKind::UnexpectedEof));
        assert!(
            matches!(m.poll_result(), Some(Err(Error::Io(e))) if e.kind() == io::ErrorKind::UnexpectedEof)
        );
        assert!(!m.is_busy());

        let mut m = Machine::new(clock, options(Duration::from_secs(5)), 6);
        m.start(clock, Op::List).unwrap();
        assert!(m.next_timeout().is_some());
        m.abort(clock);
        assert!(!m.is_busy());
        assert_eq!(m.poll_transmit(), None);
        assert_eq!(m.next_timeout(), None);
        assert!(m.poll_result().is_none());
        m.start(clock, Op::List).unwrap();
        assert!(m.poll_transmit().is_some());
    }
}
