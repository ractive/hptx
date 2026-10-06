//! The Kermit client (user Kermit) state machine: we are the host, the
//! calculator is the server.

use crate::time::{Duration, Instant};
use std::collections::VecDeque;
use std::fmt;

use crate::codec::{BlockCheck, CR, Deframer, FrameError, Framing, Packet, parse_frame, unchar};
use crate::params::{InitParams, Negotiated, negotiate};
use crate::prefix::{self, Quoting};

/// Tunables of a [`Client`].
///
/// Non-exhaustive: start from [`Config::default`] and set the fields you
/// need.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
    /// Block check we offer in our Send-Init (default type 3).
    pub block_check: BlockCheck,
    /// `true` (default): we offer QBIN `Y` (no 8th-bit prefixing unless the peer
    /// asks); `false`: we ask for 8th-bit prefixing with `&`.
    pub eight_bit_clean: bool,
    /// `true` (default): we offer REPT `~`; `false`: no repeat compression.
    pub repeat: bool,
    /// Our receive timeout per packet (default 20 s).
    pub timeout: Duration,
    /// Retransmissions per packet before giving up (default 5).
    pub retries: u32,
    /// Retransmissions of the packet that starts a server transaction (the
    /// `C`, `R`, `G` or `I`) while no reply to it has arrived; `None`
    /// (default) means [`Config::retries`]. Once the server answers (its
    /// `S`), [`Config::retries`] applies to the rest of the transaction.
    /// `Some(0)` sends a host command exactly once: a server cannot tell a
    /// resent `C` from a new one and runs it again, while a damaged reply
    /// packet is still re-requested.
    pub first_packet_retries: Option<u32>,
    /// Delay before each packet we send in reply to input (default 0).
    pub packet_pause: Duration,
    /// How long a NAK received while waiting for the reply to the first packet
    /// is tolerated before we resend (default 1 s); such a NAK is usually a
    /// stale one from the idle server.
    pub nak_grace: Duration,
    /// After we ACK the final `B` packet of a receive, keep answering a
    /// retransmitted `B` with the same ACK for this long (default 1 s), in
    /// case our ACK was damaged or lost. `Done` is emitted when the ACK is
    /// queued; the linger emits nothing. [`Client::is_idle`] is false while
    /// it lasts; [`Client::start`] ends it early. `Duration::ZERO` turns it
    /// off. A peer that only retransmits after its own timeout (the TIME
    /// we send, [`Config::timeout`]) is caught only by a linger that long.
    pub linger: Duration,
    /// Receive: most decoded data bytes accepted in one transaction (all
    /// files and server text together); more fails the transaction with
    /// [`Error::TooLarge`] and an E packet. Default 4 MiB, well above the
    /// largest HP object; `None` means no limit.
    pub max_size: Option<usize>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            block_check: BlockCheck::Type3,
            eight_bit_clean: true,
            repeat: true,
            timeout: Duration::from_secs(20),
            retries: 5,
            first_packet_retries: None,
            packet_pause: Duration::ZERO,
            nak_grace: Duration::from_secs(1),
            linger: Duration::from_secs(1),
            max_size: Some(DEFAULT_MAX_SIZE),
        }
    }
}

/// Default [`Config::max_size`]: 4 MiB.
const DEFAULT_MAX_SIZE: usize = 4 << 20;

/// A file to send: the name the server should store it under, and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutgoingFile {
    /// Name sent in the F packet.
    pub name: Vec<u8>,
    /// File contents.
    pub data: Vec<u8>,
}

/// One client transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Command {
    /// I exchange (parameters only).
    Info,
    /// Send files: S F D.. Z [F D.. Z].. B.
    Send(Vec<OutgoingFile>),
    /// R name; receive the file(s) the server sends back.
    Get(Vec<u8>),
    /// C text (host command); the reply arrives as [`Event::ServerText`].
    Host(Vec<u8>),
    /// G D (directory); the reply arrives as [`Event::ServerText`].
    Directory,
    /// G F: end server mode.
    Finish,
    /// G L: end server mode and log out.
    Logout,
}

/// Something that happened during a transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// A file starts. Receive: the name from the F packet. Send: the name from
    /// the ACK to F if non-empty, else the name we sent.
    FileStart {
        /// File name.
        name: Vec<u8>,
    },
    /// Decoded data of one received D packet (file transfer only, not X text).
    Data(Vec<u8>),
    /// Send progress after each acknowledged D packet.
    Progress {
        /// Input bytes of the current file acknowledged so far.
        sent: u64,
        /// Size of the current file.
        total: u64,
    },
    /// Z received or acknowledged.
    FileEnd {
        /// The file was discarded (Z data `D`, or we interrupted on an X/Z ACK).
        discarded: bool,
    },
    /// Reply text to Host/Directory.
    ServerText(Vec<u8>),
    /// Terminal failure; the client is idle.
    Error(Error),
    /// Terminal success. The client is idle, or lingers after the final ACK
    /// of a receive ([`Config::linger`]).
    Done,
}

/// Why a transaction failed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Decoded data of an E packet from the peer.
    Remote(Vec<u8>),
    /// Retries exhausted.
    Timeout,
    /// Unexpected packet or undecodable data.
    Protocol(String),
    /// [`Client::cancel`] was called.
    Cancelled,
    /// The received data exceeded [`Config::max_size`]; we sent an E packet.
    TooLarge {
        /// The limit that was exceeded.
        limit: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Remote(text) => write!(f, "remote error: {}", String::from_utf8_lossy(text)),
            Error::Timeout => f.write_str("timed out: too many retries"),
            Error::Protocol(msg) => write!(f, "protocol error: {msg}"),
            Error::Cancelled => f.write_str("cancelled"),
            Error::TooLarge { limit } => {
                write!(f, "received data exceeds the limit of {limit} bytes")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Default MAXL assumed for the peer before any Send-Init exchange.
const DEFAULT_MAXL: usize = 80;

/// Why [`Client::start`] refused a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StartError {
    /// A transaction is already running.
    Busy,
    /// The encoded command data (R name, C text) does not fit in one packet
    /// under the default MAXL of 80 with block check type 1.
    TooLong {
        /// Encoded data length.
        len: usize,
        /// Largest encoded data length that fits.
        max: usize,
    },
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::Busy => f.write_str("a transaction is already running"),
            StartError::TooLong { len, max } => write!(
                f,
                "command too long: {len} encoded bytes, at most {max} fit in one packet"
            ),
        }
    }
}

impl std::error::Error for StartError {}

/// What the reply to the command packet means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Info,
    Get,
    Text,
    Generic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Init,
    File,
    Data { chunk: usize },
    Eof { discarded: bool, skip_rest: bool },
    Break,
}

#[derive(Debug)]
struct Sending {
    files: Vec<OutgoingFile>,
    idx: usize,
    pos: usize,
    step: Step,
}

#[derive(Debug)]
enum Phase {
    Idle,
    /// Waiting for the reply to the command packet (seq 0).
    Await(Kind),
    /// Receiving a transfer; `text` is Some while in X (text) mode, `file`
    /// is true between an F and its Z.
    Receive {
        text: Option<Vec<u8>>,
        file: bool,
    },
    Send(Sending),
    /// The final ACK of a receive is out; re-ACK a retransmitted B (seq
    /// `self.seq`) until `self.deadline`.
    Linger,
}

/// Sans-IO Kermit client. See the crate documentation for the driver loop.
#[derive(Debug)]
pub struct Client {
    config: Config,
    ours: InitParams,
    deframer: Deframer,
    phase: Phase,
    neg: Option<Negotiated>,
    peer: Option<InitParams>,
    /// Receive: the expected seq. Send: the seq of the packet awaiting its ACK.
    seq: u8,
    last_sent: Vec<u8>,
    queue: VecDeque<(Instant, Vec<u8>)>,
    events: VecDeque<Event>,
    deadline: Option<Instant>,
    retries: u32,
    /// Receive: decoded data bytes accepted in this transaction.
    received: usize,
}

impl Client {
    /// New idle client.
    pub fn new(config: Config) -> Self {
        let ours = InitParams {
            maxl: 94,
            time: config.timeout.as_secs().clamp(1, 94) as u8,
            npad: 0,
            padc: 0,
            eol: CR,
            qctl: b'#',
            qbin: if config.eight_bit_clean { b'Y' } else { b'&' },
            chkt: config.block_check.to_char(),
            rept: if config.repeat { b'~' } else { b' ' },
        };
        Client {
            config,
            ours,
            deframer: Deframer::new(),
            phase: Phase::Idle,
            neg: None,
            peer: None,
            seq: 0,
            last_sent: Vec::new(),
            queue: VecDeque::new(),
            events: VecDeque::new(),
            deadline: None,
            retries: 0,
            received: 0,
        }
    }

    /// Begin a transaction. First packet is available from
    /// `poll_output(now)` immediately.
    ///
    /// Starts from a clean slate: buffered input, output and events of the
    /// previous transaction that were not polled yet are dropped, and
    /// [`Client::peer_params`] is cleared. A linger after the previous
    /// transaction ([`Config::linger`]) ends.
    ///
    /// Fails with [`StartError::Busy`] while a transaction is running and with
    /// [`StartError::TooLong`] if the encoded command data does not fit in one
    /// packet; the client is unchanged in both cases.
    pub fn start(&mut self, now: Instant, command: Command) -> Result<(), StartError> {
        if !matches!(self.phase, Phase::Idle | Phase::Linger) {
            return Err(StartError::Busy);
        }
        let q = Quoting::default();
        let (kind, data, phase) = match command {
            Command::Info => (b'I', self.ours.encode(), Phase::Await(Kind::Info)),
            Command::Send(files) => (
                b'S',
                self.ours.encode(),
                Phase::Send(Sending {
                    files,
                    idx: 0,
                    pos: 0,
                    step: Step::Init,
                }),
            ),
            Command::Get(name) => (b'R', prefix::encode_all(&name, &q), Phase::Await(Kind::Get)),
            Command::Host(text) => (
                b'C',
                prefix::encode_all(&text, &q),
                Phase::Await(Kind::Text),
            ),
            Command::Directory => (b'G', b"D".to_vec(), Phase::Await(Kind::Text)),
            Command::Finish => (b'G', b"F".to_vec(), Phase::Await(Kind::Generic)),
            Command::Logout => (b'G', b"L".to_vec(), Phase::Await(Kind::Generic)),
        };
        // Commands go out before negotiation: default MAXL, block check 1.
        let max = Packet::max_data(DEFAULT_MAXL, BlockCheck::Type1);
        if data.len() > max {
            return Err(StartError::TooLong {
                len: data.len(),
                max,
            });
        }
        self.deframer.clear();
        self.queue.clear();
        self.events.clear();
        self.neg = None;
        self.peer = None;
        self.seq = 0;
        self.retries = 0;
        self.received = 0;
        self.deadline = None;
        self.phase = phase;
        let bytes = Packet::new(0, kind, data).wire(BlockCheck::Type1, &Framing::default());
        self.last_sent = bytes.clone();
        self.queue.push_back((now, bytes));
        Ok(())
    }

    /// Feed bytes read from the link.
    pub fn handle_input(&mut self, now: Instant, bytes: &[u8]) {
        if self.is_idle() {
            self.deframer.clear();
            return;
        }
        self.deframer.push(bytes);
        while !self.is_idle() {
            let Some(frame) = self.deframer.next_frame() else {
                break;
            };
            let check = match self.neg {
                Some(n) if frame.get(3) != Some(&b'S') => n.check,
                _ => BlockCheck::Type1,
            };
            let parsed = parse_frame(&frame, check);
            // SEQ as received, also for frames that fail the check.
            let raw_seq = frame.get(2).map(|&c| unchar(c));
            match self.phase {
                Phase::Idle => {}
                Phase::Await(kind) => self.on_await(now, kind, parsed),
                Phase::Receive { .. } => self.on_receive(now, parsed),
                Phase::Send(_) => self.on_send(now, parsed, raw_seq),
                Phase::Linger => self.on_linger(now, parsed),
            }
        }
        if self.is_idle() {
            self.deframer.clear();
        }
    }

    /// Call when the time from [`Client::next_timeout`] has passed.
    pub fn handle_timeout(&mut self, now: Instant) {
        if self.is_idle() {
            return;
        }
        if let Some(d) = self.deadline
            && now >= d
        {
            self.deadline = None;
            match self.phase {
                Phase::Linger => self.end_linger(),
                // Receiving: NAK the packet we expect (Kermit), not the last ACK.
                Phase::Receive { .. } => {
                    let nak = self.nak();
                    self.retry_with(now, now, nak);
                }
                _ => self.retry(now, now),
            }
        }
    }

    /// Next bytes to write, written atomically (one packet incl. padding and
    /// EOL). None if nothing is due yet at `now` (packet_pause). Keep calling
    /// after Done/Error until None: the final ACK or an E packet may still be
    /// queued.
    pub fn poll_output(&mut self, now: Instant) -> Option<Vec<u8>> {
        match self.queue.front() {
            Some((due, _)) if *due <= now => {}
            _ => return None,
        }
        let (_, bytes) = self.queue.pop_front()?;
        // The linger's deadline is fixed when it starts.
        if !matches!(self.phase, Phase::Idle | Phase::Linger) {
            // Overflow (absurdly large timeout) means no deadline.
            self.deadline = now.checked_add(self.config.timeout);
        }
        Some(bytes)
    }

    /// Next event, in order.
    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// Earliest of: when a queued packet becomes due, the retransmit deadline.
    pub fn next_timeout(&self) -> Option<Instant> {
        let due = self.queue.iter().map(|(t, _)| *t).min();
        match (due, self.deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// No transaction running and no linger after one (output may still be
    /// queued).
    pub fn is_idle(&self) -> bool {
        matches!(self.phase, Phase::Idle)
    }

    /// Abort: queue an E packet "Cancelled", emit Error(Cancelled), go idle.
    /// No-op when idle. During the linger after `Done` it only ends the
    /// linger: no E packet, no event.
    pub fn cancel(&mut self, now: Instant) {
        match self.phase {
            Phase::Idle => {}
            Phase::Linger => self.end_linger(),
            _ => self.fail(now, Error::Cancelled, Some(b"Cancelled")),
        }
    }

    /// Parameters the peer sent in its S, or ACK to our S/I, in the current
    /// (or last finished) transaction; cleared by [`Client::start`].
    pub fn peer_params(&self) -> Option<InitParams> {
        self.peer
    }

    // ---- helpers ----

    fn check(&self) -> BlockCheck {
        self.neg.map_or(BlockCheck::Type1, |n| n.check)
    }

    fn framing(&self) -> Framing {
        self.neg.map_or_else(Framing::default, |n| n.framing)
    }

    fn send_quoting(&self) -> Quoting {
        self.neg.map_or_else(Quoting::default, |n| n.send)
    }

    fn recv_quoting(&self) -> Quoting {
        self.neg.map_or_else(Quoting::default, |n| n.recv)
    }

    /// When a packet sent in reply to input at `now` becomes due. A pause that
    /// overflows `Instant` is ignored rather than delaying forever.
    fn paused(&self, now: Instant) -> Instant {
        now.checked_add(self.config.packet_pause).unwrap_or(now)
    }

    /// NAK for the expected seq with the negotiated check and framing.
    fn nak(&self) -> Vec<u8> {
        Packet::new(self.seq, b'N', Vec::new()).wire(self.check(), &self.framing())
    }

    /// Queue a new packet in reply to input; it becomes the retransmit copy.
    fn reply(&mut self, now: Instant, bytes: Vec<u8>) {
        self.last_sent = bytes.clone();
        self.deadline = None;
        let due = self.paused(now);
        self.queue.push_back((due, bytes));
    }

    fn reply_packet(&mut self, now: Instant, seq: u8, kind: u8, data: Vec<u8>) {
        let bytes = Packet::new(seq, kind, data).wire(self.check(), &self.framing());
        self.reply(now, bytes);
    }

    /// Count a retry and re-queue `last_sent` due at `due`, or give up.
    fn retry(&mut self, now: Instant, due: Instant) {
        let bytes = self.last_sent.clone();
        self.retry_with(now, due, bytes);
    }

    /// Count a retry and queue `bytes` (not stored as `last_sent`) due at
    /// `due`, or give up.
    fn retry_with(&mut self, now: Instant, due: Instant, bytes: Vec<u8>) {
        self.retries += 1;
        let limit = match (&self.phase, self.config.first_packet_retries) {
            (Phase::Await(_), Some(first)) => first,
            _ => self.config.retries,
        };
        if self.retries > limit {
            self.fail(now, Error::Timeout, Some(b"Too many retries"));
        } else {
            self.deadline = None;
            self.queue.push_back((due, bytes));
        }
    }

    fn stale_nak(&mut self, now: Instant) {
        // A grace period that overflows `Instant` leaves the deadline as is.
        if let Some(grace) = now.checked_add(self.config.nak_grace) {
            self.deadline = Some(self.deadline.map_or(grace, |d| d.min(grace)));
        }
    }

    fn finish(&mut self, event: Event) {
        self.events.push_back(event);
        self.phase = Phase::Idle;
        self.deadline = None;
    }

    /// Done after the final ACK to B (queued at `now`): linger for
    /// [`Config::linger`] after it goes out, or go idle at once.
    fn finish_lingering(&mut self, now: Instant) {
        self.finish(Event::Done);
        // A linger that overflows `Instant` would never end: skip it.
        if let Some(end) = self.paused(now).checked_add(self.config.linger)
            && !self.config.linger.is_zero()
        {
            self.phase = Phase::Linger;
            self.deadline = Some(end);
        }
    }

    fn end_linger(&mut self) {
        self.phase = Phase::Idle;
        self.deadline = None;
        self.deframer.clear();
    }

    /// Drop queued copies of `bytes` that have not gone out yet: the reply
    /// that made them obsolete has arrived.
    fn drop_queued(&mut self, bytes: &[u8]) {
        self.queue.retain(|(_, b)| b.as_slice() != bytes);
    }

    /// Queue an E packet (if `text`), emit Error, go idle. Output still queued
    /// (a held-back ACK, D or F) is dropped first so nothing follows the E.
    fn fail(&mut self, now: Instant, err: Error, text: Option<&[u8]>) {
        self.queue.clear();
        if let Some(text) = text {
            let data = prefix::encode_all(text, &self.send_quoting());
            let bytes = Packet::new(self.seq, b'E', data).wire(self.check(), &self.framing());
            self.queue.push_back((now, bytes));
        }
        self.finish(Event::Error(err));
    }

    fn protocol(&mut self, now: Instant, msg: String, text: &[u8]) {
        self.fail(now, Error::Protocol(msg), Some(text));
    }

    fn remote(&mut self, data: &[u8], q: &Quoting) {
        let text = prefix::decode(data, q).unwrap_or_else(|_| data.to_vec());
        self.finish(Event::Error(Error::Remote(text)));
    }

    /// Decode received data, failing the transaction on a prefix error.
    fn decode(&mut self, now: Instant, data: &[u8], q: &Quoting) -> Option<Vec<u8>> {
        match prefix::decode(data, q) {
            Ok(v) => Some(v),
            Err(e) => {
                self.protocol(now, e.to_string(), b"Bad prefix");
                None
            }
        }
    }

    // ---- receiving ----

    fn on_await(&mut self, now: Instant, kind: Kind, parsed: Result<Packet, FrameError>) {
        let Ok(p) = parsed else { return };
        match p.kind {
            b'N' => self.stale_nak(now),
            b'E' => {
                if kind == Kind::Info {
                    self.finish(Event::Done);
                } else {
                    self.remote(&p.data, &Quoting::default());
                }
            }
            b'S' if p.seq == 0 => {
                let theirs = InitParams::decode(&p.data);
                let n = negotiate(&self.ours, &theirs);
                self.peer = Some(theirs);
                // The ACK to S uses type 1 but already the peer's framing.
                let bytes =
                    Packet::new(0, b'Y', self.ours.encode()).wire(BlockCheck::Type1, &n.framing);
                self.reply(now, bytes);
                self.neg = Some(n);
                self.seq = 1;
                self.retries = 0;
                self.phase = Phase::Receive {
                    text: None,
                    file: false,
                };
            }
            b'Y' if p.seq == 0 => match kind {
                Kind::Info => {
                    self.peer = Some(InitParams::decode(&p.data));
                    self.finish(Event::Done);
                }
                Kind::Text => {
                    if !p.data.is_empty() {
                        let Some(text) = self.decode(now, &p.data, &Quoting::default()) else {
                            return;
                        };
                        self.events.push_back(Event::ServerText(text));
                    }
                    self.finish(Event::Done);
                }
                Kind::Generic => self.finish(Event::Done),
                Kind::Get => {
                    self.protocol(now, "unexpected ACK to R".to_string(), b"Unexpected packet")
                }
            },
            _ => {}
        }
    }

    fn on_receive(&mut self, now: Instant, parsed: Result<Packet, FrameError>) {
        let n = self.seq;
        let p = match parsed {
            Ok(p) => p,
            Err(FrameError::BadCheck) => {
                // Not stored as last_sent: a duplicate still gets the last ACK.
                let nak = self.nak();
                let due = self.paused(now);
                self.retry_with(now, due, nak);
                return;
            }
            Err(FrameError::BadLength) => return,
        };
        if p.kind == b'E' {
            let q = self.recv_quoting();
            self.remote(&p.data, &q);
            return;
        }
        if p.seq != n {
            if p.seq == (n + 63) % 64 && p.kind != b'N' {
                self.retries = 0;
                let due = self.paused(now);
                self.queue.push_back((due, self.last_sent.clone()));
                self.deadline = None;
            }
            return;
        }
        if p.kind != b'N' {
            // Packet n arrived: a NAK for it still queued is obsolete.
            let nak = self.nak();
            self.drop_queued(&nak);
        }
        let q = self.recv_quoting();
        match p.kind {
            b'F' | b'D' | b'Z' => {
                let Some(data) = self.decode(now, &p.data, &q) else {
                    return;
                };
                if p.kind == b'D' && !self.accept(now, data.len()) {
                    return;
                }
                let Phase::Receive { text, file } = &mut self.phase else {
                    return;
                };
                match (p.kind, text.as_mut()) {
                    (b'F', _) => {
                        *text = None;
                        *file = true;
                        self.events.push_back(Event::FileStart { name: data });
                    }
                    (b'D', Some(buf)) => buf.extend_from_slice(&data),
                    (b'D', None) => self.events.push_back(Event::Data(data)),
                    (_, Some(_)) => {
                        let buf = text.take().unwrap_or_default();
                        self.events.push_back(Event::ServerText(buf));
                    }
                    (_, None) => {
                        *file = false;
                        self.events.push_back(Event::FileEnd {
                            discarded: data == b"D",
                        });
                    }
                }
            }
            b'X' => {
                if let Phase::Receive { text, .. } = &mut self.phase {
                    *text = Some(Vec::new());
                }
            }
            b'A' => {}
            b'B' => {
                let open = match &self.phase {
                    Phase::Receive { text, file } => *file || text.is_some(),
                    _ => false,
                };
                if open {
                    // Acknowledging it would pass off a truncated file (or
                    // reply text) as a success.
                    self.protocol(
                        now,
                        "B before the Z of the current file".to_string(),
                        b"Unexpected B",
                    );
                    return;
                }
                self.reply_packet(now, n, b'Y', Vec::new());
                self.finish_lingering(now);
                return;
            }
            b'N' => return,
            other => {
                self.protocol(
                    now,
                    format!("unexpected packet {:?}", char::from(other)),
                    b"Unexpected packet",
                );
                return;
            }
        }
        self.reply_packet(now, n, b'Y', Vec::new());
        self.seq = (n + 1) % 64;
        self.retries = 0;
    }

    /// Count `len` received data bytes against [`Config::max_size`]; false
    /// (and the transaction failed) if they do not fit.
    fn accept(&mut self, now: Instant, len: usize) -> bool {
        self.received = self.received.saturating_add(len);
        match self.config.max_size {
            Some(limit) if self.received > limit => {
                self.fail(now, Error::TooLarge { limit }, Some(b"Too large"));
                false
            }
            _ => true,
        }
    }

    /// After the final ACK: answer a retransmitted B with the same ACK (one
    /// pending copy at most); anything else is ignored.
    fn on_linger(&mut self, now: Instant, parsed: Result<Packet, FrameError>) {
        if let Ok(p) = parsed
            && p.kind == b'B'
            && p.seq == self.seq
            && !self.queue.iter().any(|(_, b)| *b == self.last_sent)
        {
            let due = self.paused(now);
            self.queue.push_back((due, self.last_sent.clone()));
        }
    }

    // ---- sending ----

    fn on_send(&mut self, now: Instant, parsed: Result<Packet, FrameError>, raw_seq: Option<u8>) {
        let n = self.seq;
        let init = matches!(&self.phase, Phase::Send(s) if s.step == Step::Init);
        let p = match parsed {
            Ok(p) => p,
            Err(FrameError::BadCheck) => {
                // A damaged reply for the outstanding packet acts as a NAK.
                // Anything else (e.g. a late duplicate ACK(0) to S, which uses
                // check type 1) is ignored.
                if raw_seq == Some(n) {
                    let due = self.paused(now);
                    self.retry(now, due);
                }
                return;
            }
            Err(FrameError::BadLength) => return,
        };
        match p.kind {
            b'E' => {
                let q = self.recv_quoting();
                self.remote(&p.data, &q);
            }
            b'Y' if p.seq == n => {
                self.drop_obsolete_retry();
                self.on_ack(now, &p.data);
            }
            b'N' if p.seq == (n + 1) % 64 => {
                self.drop_obsolete_retry();
                self.on_ack(now, &[]);
            }
            b'N' if p.seq == n => {
                if init {
                    self.stale_nak(now);
                } else {
                    let due = self.paused(now);
                    self.retry(now, due);
                }
            }
            _ => {}
        }
    }

    /// Packet n is acknowledged: a retransmission of it still queued (from a
    /// NAK or damaged reply in the same input, or held back by
    /// `packet_pause`) would only delay the next packet.
    fn drop_obsolete_retry(&mut self) {
        let sent = std::mem::take(&mut self.last_sent);
        self.drop_queued(&sent);
        self.last_sent = sent;
    }

    fn send_next(&mut self, now: Instant, kind: u8, data: Vec<u8>) {
        self.seq = (self.seq + 1) % 64;
        self.reply_packet(now, self.seq, kind, data);
    }

    fn max_data(&self) -> usize {
        let maxl = self.neg.map_or(DEFAULT_MAXL, |n| usize::from(n.peer_maxl));
        Packet::max_data(maxl, self.check())
    }

    fn sending(&mut self) -> Option<&mut Sending> {
        match &mut self.phase {
            Phase::Send(s) => Some(s),
            _ => None,
        }
    }

    fn on_ack(&mut self, now: Instant, data: &[u8]) {
        self.retries = 0;
        let Some(step) = self.sending().map(|s| s.step) else {
            return;
        };
        match step {
            Step::Init => {
                let theirs = InitParams::decode(data);
                self.peer = Some(theirs);
                self.neg = Some(negotiate(&self.ours, &theirs));
                self.next_file(now);
            }
            Step::File => {
                let name = if data.is_empty() {
                    self.sending()
                        .and_then(|s| s.files.get(s.idx))
                        .map(|f| f.name.clone())
                        .unwrap_or_default()
                } else {
                    let q = self.recv_quoting();
                    let Some(name) = self.decode(now, data, &q) else {
                        return;
                    };
                    name
                };
                self.events.push_back(Event::FileStart { name });
                self.next_data(now);
            }
            Step::Data { chunk } => {
                let Some(s) = self.sending() else { return };
                s.pos += chunk;
                let sent = s.pos as u64;
                let total = s.files.get(s.idx).map_or(0, |f| f.data.len()) as u64;
                self.events.push_back(Event::Progress { sent, total });
                if data == b"X" || data == b"Z" {
                    if let Some(s) = self.sending() {
                        s.step = Step::Eof {
                            discarded: true,
                            skip_rest: data == b"Z",
                        };
                    }
                    let d = prefix::encode_all(b"D", &self.send_quoting());
                    self.send_next(now, b'Z', d);
                } else {
                    self.next_data(now);
                }
            }
            Step::Eof {
                discarded,
                skip_rest,
            } => {
                self.events.push_back(Event::FileEnd { discarded });
                if let Some(s) = self.sending() {
                    s.idx = if skip_rest { s.files.len() } else { s.idx + 1 };
                }
                self.next_file(now);
            }
            Step::Break => self.finish(Event::Done),
        }
    }

    /// Send F for the current file, or B when all files are done. Fails the
    /// transaction if the encoded name does not fit in one packet.
    fn next_file(&mut self, now: Instant) {
        let max = self.max_data();
        let q = self.send_quoting();
        let Some(s) = self.sending() else { return };
        let encoded = s.files.get(s.idx).map(|f| {
            let (enc, used) = prefix::encode(&f.name, &q, max);
            (enc, used == f.name.len())
        });
        match encoded {
            Some((_, false)) => {
                self.protocol(
                    now,
                    format!("file name does not fit in a packet of {max} data bytes"),
                    b"File name too long",
                );
            }
            Some((name, true)) => {
                s.pos = 0;
                s.step = Step::File;
                self.send_next(now, b'F', name);
            }
            None => {
                s.step = Step::Break;
                self.send_next(now, b'B', Vec::new());
            }
        }
    }

    /// Send the next D chunk of the current file, or Z at its end.
    fn next_data(&mut self, now: Instant) {
        let max = self.max_data();
        let q = self.send_quoting();
        let Some(s) = self.sending() else { return };
        let rest = s
            .files
            .get(s.idx)
            .and_then(|f| f.data.get(s.pos..))
            .unwrap_or_default();
        if rest.is_empty() {
            s.step = Step::Eof {
                discarded: false,
                skip_rest: false,
            };
            self.send_next(now, b'Z', Vec::new());
        } else {
            let (enc, used) = prefix::encode(rest, &q, max);
            s.step = Step::Data { chunk: used };
            self.send_next(now, b'D', enc);
        }
    }
}

/// Test helpers shared by trace tests.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod test_support {
    use super::*;
    use crate::trace::{self, Direction};

    /// Replay `trace` at a fixed `now`: Out lines must match `poll_output`, In
    /// lines are fed to `handle_input`. Returns the client and all events.
    pub(crate) fn replay_client(cmd: Command, config: Config, text: &str) -> (Client, Vec<Event>) {
        let now = Instant::now();
        let mut c = Client::new(config);
        c.start(now, cmd).unwrap();
        let mut events = Vec::new();
        for (i, (dir, bytes)) in trace::parse(text).unwrap().into_iter().enumerate() {
            match dir {
                Direction::Out => {
                    let got = c.poll_output(now);
                    assert_eq!(
                        got.as_deref().map(trace::escape),
                        Some(trace::escape(&bytes)),
                        "trace entry {i}: expected output > {}",
                        trace::escape(&bytes)
                    );
                }
                Direction::In => c.handle_input(now, &bytes),
            }
            while let Some(e) = c.poll_event() {
                events.push(e);
            }
        }
        let extra = c.poll_output(now);
        assert_eq!(
            extra.as_deref().map(trace::escape),
            None,
            "unexpected trailing output"
        );
        (c, events)
    }

    /// [`replay_client`] returning only the events.
    pub(crate) fn replay(cmd: Command, config: Config, text: &str) -> Vec<Event> {
        replay_client(cmd, config, text).1
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::test_support::{replay, replay_client};
    use super::*;
    use crate::trace;

    const T1: BlockCheck = BlockCheck::Type1;
    const T3: BlockCheck = BlockCheck::Type3;
    /// Our params with the default config.
    const OURS: &[u8] = b"~4 @-#Y3~";

    fn wire(seq: u8, kind: u8, data: &[u8], check: BlockCheck) -> Vec<u8> {
        Packet::new(seq, kind, data.to_vec())
            .encode(check, &Framing::default())
            .unwrap()
    }

    fn out(seq: u8, kind: u8, data: &[u8], check: BlockCheck) -> String {
        format!("> {}\n", trace::escape(&wire(seq, kind, data, check)))
    }

    fn inp(seq: u8, kind: u8, data: &[u8], check: BlockCheck) -> String {
        format!("< {}\n", trace::escape(&wire(seq, kind, data, check)))
    }

    fn hp_text() -> Vec<u8> {
        let mut v = b"1:".to_vec();
        v.extend_from_slice(&[b' '; 20]);
        v.extend_from_slice(b"42\r\n");
        v
    }

    fn cfg() -> Config {
        Config::default()
    }

    /// The HP's server-side Send-Init reply prefix: S, our ACK.
    fn s_exchange(theirs: &[u8]) -> String {
        inp(0, b'S', theirs, T1) + &out(0, b'Y', OURS, T1)
    }

    #[test]
    fn our_params_encode() {
        assert_eq!(Client::new(cfg()).ours.encode(), OURS);
    }

    #[test]
    fn host_long_reply_real_bytes() {
        let mut t = String::from("> \\x01( C6 7 *C\\r\n< \\x01+ S~* @-#Y3$\\r\n");
        t += &out(0, b'Y', OURS, T1);
        t += &inp(1, b'X', b"", T3);
        t += &out(1, b'Y', b"", T3);
        t += &inp(2, b'D', b"1:                    42#M#J", T3);
        t += &out(2, b'Y', b"", T3);
        t += &inp(3, b'Z', b"", T3);
        t += &out(3, b'Y', b"", T3);
        t += &inp(4, b'B', b"", T3);
        t += &out(4, b'Y', b"", T3);
        let ev = replay(Command::Host(b"6 7 *".to_vec()), cfg(), &t);
        assert_eq!(ev, vec![Event::ServerText(hp_text()), Event::Done]);
    }

    #[test]
    fn host_short_reply() {
        let t = out(0, b'C', b"VARS", T1) + &inp(0, b'Y', b"{ A B }#M#J", T1);
        let ev = replay(Command::Host(b"VARS".to_vec()), cfg(), &t);
        assert_eq!(
            ev,
            vec![Event::ServerText(b"{ A B }\r\n".to_vec()), Event::Done]
        );
    }

    #[test]
    fn directory_and_finish() {
        let t = out(0, b'G', b"D", T1) + &inp(0, b'Y', b"X 12", T1);
        let ev = replay(Command::Directory, cfg(), &t);
        assert_eq!(ev, vec![Event::ServerText(b"X 12".to_vec()), Event::Done]);

        let t = out(0, b'G', b"F", T1) + &inp(0, b'Y', b"", T1);
        assert_eq!(replay(Command::Finish, cfg(), &t), vec![Event::Done]);
        let t = out(0, b'G', b"L", T1) + &inp(0, b'Y', b"", T1);
        assert_eq!(replay(Command::Logout, cfg(), &t), vec![Event::Done]);
    }

    #[test]
    fn info_exchange() {
        let mut i = b"\x01, I~4 @-#Y3~".to_vec();
        let chk = T1.compute(&i[1..]);
        i.extend_from_slice(&chk);
        i.push(CR);
        assert_eq!(wire(0, b'I', OURS, T1), i);
        let t = format!("> {}\n< \\x01+ Y~& @-# 3,\\r\n", trace::escape(&i));
        let (c, ev) = replay_client(Command::Info, cfg(), &t);
        assert_eq!(ev, vec![Event::Done]);
        assert!(c.is_idle());
        assert_eq!(c.peer_params(), Some(InitParams::decode(b"~& @-# 3")));
        assert_eq!(c.peer_params().map(|p| p.time), Some(6));

        let t = format!("> {}\n", trace::escape(&i)) + &inp(0, b'E', b"no I", T1);
        assert_eq!(replay(Command::Info, cfg(), &t), vec![Event::Done]);
    }

    fn get_trace(theirs: &[u8], d1: &[u8], d2: &[u8]) -> String {
        let mut t = out(0, b'R', b"FILE", T1);
        t += &s_exchange(theirs);
        t += &inp(1, b'F', b"FILE", T3);
        t += &out(1, b'Y', b"", T3);
        t += &inp(2, b'D', d1, T3);
        t += &out(2, b'Y', b"", T3);
        t += &inp(3, b'D', d2, T3);
        t += &out(3, b'Y', b"", T3);
        t += &inp(4, b'Z', b"", T3);
        t += &out(4, b'Y', b"", T3);
        t += &inp(5, b'B', b"", T3);
        t += &out(5, b'Y', b"", T3);
        t
    }

    fn get_events(d1: &[u8], d2: &[u8]) -> Vec<Event> {
        vec![
            Event::FileStart {
                name: b"FILE".to_vec(),
            },
            Event::Data(d1.to_vec()),
            Event::Data(d2.to_vec()),
            Event::FileEnd { discarded: false },
            Event::Done,
        ]
    }

    #[test]
    fn get_eight_bit_clean() {
        // Peer QBIN 'Y', we offer 'Y': no 8th-bit prefixing; repeat agreed.
        let d1: &[u8] = &[b'H', b'P', 0xC1, b'#', b'#', 0xA3, b'#', b'~'];
        let d2: &[u8] = &[b'#', 0xC1, b'#', b'M', b'~', b'%', b'x'];
        let t = get_trace(b"~* @-#Y3~", d1, d2);
        let ev = replay(Command::Get(b"FILE".to_vec()), cfg(), &t);
        assert_eq!(
            ev,
            get_events(
                &[b'H', b'P', 0xC1, b'#', 0xA3, b'~'],
                &[0x81, b'\r', b'x', b'x', b'x', b'x', b'x']
            )
        );
    }

    #[test]
    fn get_eight_bit_prefixed() {
        let config = Config {
            eight_bit_clean: false,
            ..cfg()
        };
        let mut t = out(0, b'R', b"FILE", T1);
        t += &inp(0, b'S', b"~* @-#Y3~", T1);
        t += &out(0, b'Y', b"~4 @-#&3~", T1);
        t += &get_trace(b"", b"&A&#A#&", b"&#?")
            .lines()
            .skip(3)
            .map(|l| format!("{l}\n"))
            .collect::<String>();
        let ev = replay(Command::Get(b"FILE".to_vec()), config, &t);
        assert_eq!(ev, get_events(&[0xC1, 0x81, b'&'], &[0xFF]));
    }

    #[test]
    fn get_ack_is_protocol_error() {
        let t = out(0, b'R', b"X", T1)
            + &inp(0, b'Y', b"", T1)
            + &out(0, b'E', b"Unexpected packet", T1);
        let ev = replay(Command::Get(b"X".to_vec()), cfg(), &t);
        assert!(matches!(ev.as_slice(), [Event::Error(Error::Protocol(_))]));
    }

    const PEER_SMALL: &[u8] = b"4* @-#Y3"; // maxl 20, no repeat

    fn file(name: &[u8], data: &[u8]) -> OutgoingFile {
        OutgoingFile {
            name: name.to_vec(),
            data: data.to_vec(),
        }
    }

    fn send_start() -> String {
        out(0, b'S', OURS, T1) + &inp(0, b'Y', PEER_SMALL, T1)
    }

    const DATA40: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMN";

    #[test]
    fn send_one_file_with_stored_name() {
        // Peer MAXL 20, type 3: 15 data chars per D packet.
        let mut t = send_start();
        t += &out(1, b'F', b"local", T3);
        t += &inp(1, b'Y', b"STORED", T3);
        t += &out(2, b'D', &DATA40[..15], T3);
        t += &inp(2, b'Y', b"", T3);
        t += &out(3, b'D', &DATA40[15..30], T3);
        t += &inp(3, b'Y', b"", T3);
        t += &out(4, b'D', &DATA40[30..], T3);
        t += &inp(4, b'Y', b"", T3);
        t += &out(5, b'Z', b"", T3);
        t += &inp(5, b'Y', b"", T3);
        t += &out(6, b'B', b"", T3);
        t += &inp(6, b'Y', b"", T3);
        let (c, ev) = replay_client(Command::Send(vec![file(b"local", DATA40)]), cfg(), &t);
        assert_eq!(
            ev,
            vec![
                Event::FileStart {
                    name: b"STORED".to_vec()
                },
                Event::Progress {
                    sent: 15,
                    total: 40
                },
                Event::Progress {
                    sent: 30,
                    total: 40
                },
                Event::Progress {
                    sent: 40,
                    total: 40
                },
                Event::FileEnd { discarded: false },
                Event::Done,
            ]
        );
        assert_eq!(c.peer_params(), Some(InitParams::decode(PEER_SMALL)));
    }

    #[test]
    fn send_naks_and_duplicates() {
        let mut t = send_start();
        t += &out(1, b'F', b"A", T3);
        // NAK for n+1 counts as the ACK for n (no stored name).
        t += &inp(2, b'N', b"", T3);
        t += &out(2, b'D', &DATA40[..15], T3);
        // NAK for n: immediate resend.
        t += &inp(2, b'N', b"", T3);
        t += &out(2, b'D', &DATA40[..15], T3);
        t += &inp(2, b'Y', b"", T3);
        t += &out(3, b'D', &DATA40[15..20], T3);
        // Duplicate ACK ignored.
        t += &inp(2, b'Y', b"", T3);
        t += &inp(3, b'Y', b"", T3);
        t += &out(4, b'Z', b"", T3);
        t += &inp(4, b'Y', b"", T3);
        t += &out(5, b'B', b"", T3);
        t += &inp(5, b'Y', b"", T3);
        let ev = replay(Command::Send(vec![file(b"A", &DATA40[..20])]), cfg(), &t);
        assert_eq!(
            ev,
            vec![
                Event::FileStart {
                    name: b"A".to_vec()
                },
                Event::Progress {
                    sent: 15,
                    total: 20
                },
                Event::Progress {
                    sent: 20,
                    total: 20
                },
                Event::FileEnd { discarded: false },
                Event::Done,
            ]
        );
    }

    #[test]
    fn send_bad_check_resends() {
        let mut t = send_start();
        t += &out(1, b'F', b"A", T3);
        let mut bad = wire(1, b'Y', b"", T3);
        bad[4] ^= 1;
        t += &format!("< {}\n", trace::escape(&bad));
        t += &out(1, b'F', b"A", T3);
        let ev = replay(Command::Send(vec![file(b"A", b"")]), cfg(), &t);
        assert!(ev.is_empty());
    }

    #[test]
    fn send_two_files_and_empty() {
        let mut t = send_start();
        t += &out(1, b'F', b"E", T3);
        t += &inp(1, b'Y', b"", T3);
        t += &out(2, b'Z', b"", T3);
        t += &inp(2, b'Y', b"", T3);
        t += &out(3, b'F', b"T", T3);
        t += &inp(3, b'Y', b"", T3);
        t += &out(4, b'D', b"hi#M#J", T3);
        t += &inp(4, b'Y', b"", T3);
        t += &out(5, b'Z', b"", T3);
        t += &inp(5, b'Y', b"", T3);
        t += &out(6, b'B', b"", T3);
        t += &inp(6, b'Y', b"", T3);
        let ev = replay(
            Command::Send(vec![file(b"E", b""), file(b"T", b"hi\r\n")]),
            cfg(),
            &t,
        );
        assert_eq!(
            ev,
            vec![
                Event::FileStart {
                    name: b"E".to_vec()
                },
                Event::FileEnd { discarded: false },
                Event::FileStart {
                    name: b"T".to_vec()
                },
                Event::Progress { sent: 4, total: 4 },
                Event::FileEnd { discarded: false },
                Event::Done,
            ]
        );
    }

    #[test]
    fn send_interrupted_by_x() {
        let mut t = send_start();
        t += &out(1, b'F', b"A", T3);
        t += &inp(1, b'Y', b"", T3);
        t += &out(2, b'D', &DATA40[..15], T3);
        t += &inp(2, b'Y', b"X", T3);
        t += &out(3, b'Z', b"D", T3);
        t += &inp(3, b'Y', b"", T3);
        t += &out(4, b'F', b"B", T3);
        t += &inp(4, b'Y', b"", T3);
        t += &out(5, b'Z', b"", T3);
        t += &inp(5, b'Y', b"", T3);
        t += &out(6, b'B', b"", T3);
        t += &inp(6, b'Y', b"", T3);
        let ev = replay(
            Command::Send(vec![file(b"A", DATA40), file(b"B", b"")]),
            cfg(),
            &t,
        );
        assert_eq!(
            ev,
            vec![
                Event::FileStart {
                    name: b"A".to_vec()
                },
                Event::Progress {
                    sent: 15,
                    total: 40
                },
                Event::FileEnd { discarded: true },
                Event::FileStart {
                    name: b"B".to_vec()
                },
                Event::FileEnd { discarded: false },
                Event::Done,
            ]
        );
    }

    #[test]
    fn send_interrupted_by_z_skips_rest() {
        let mut t = send_start();
        t += &out(1, b'F', b"A", T3);
        t += &inp(1, b'Y', b"", T3);
        t += &out(2, b'D', &DATA40[..15], T3);
        t += &inp(2, b'Y', b"Z", T3);
        t += &out(3, b'Z', b"D", T3);
        t += &inp(3, b'Y', b"", T3);
        t += &out(4, b'B', b"", T3);
        t += &inp(4, b'Y', b"", T3);
        let ev = replay(
            Command::Send(vec![file(b"A", DATA40), file(b"B", b"")]),
            cfg(),
            &t,
        );
        assert_eq!(ev[2], Event::FileEnd { discarded: true });
        assert_eq!(ev.len(), 4);
        assert_eq!(ev[3], Event::Done);
    }

    #[test]
    fn stale_nak_before_reply() {
        let mut t = String::from("> \\x01( C6 7 *C\\r\n< \\x01# N3\\r\n< \\x01+ S~* @-#Y3$\\r\n");
        t += &out(0, b'Y', OURS, T1);
        t += &inp(1, b'B', b"", T3);
        t += &out(1, b'Y', b"", T3);
        let ev = replay(Command::Host(b"6 7 *".to_vec()), cfg(), &t);
        assert_eq!(ev, vec![Event::Done]);
    }

    #[test]
    fn stale_nak_then_silence() {
        let config = cfg();
        let grace = config.nak_grace;
        let now = Instant::now();
        let mut c = Client::new(config);
        c.start(now, Command::Host(b"6 7 *".to_vec())).unwrap();
        let cmd = c.poll_output(now).unwrap();
        assert_eq!(cmd, b"\x01( C6 7 *C\r");
        c.handle_input(now, b"\x01# N3\r");
        assert_eq!(c.poll_output(now), None);
        assert_eq!(c.next_timeout(), Some(now + grace));
        c.handle_timeout(now + grace - Duration::from_millis(1));
        assert_eq!(c.poll_output(now + grace - Duration::from_millis(1)), None);
        c.handle_timeout(now + grace);
        assert_eq!(c.poll_output(now + grace), Some(cmd));
        assert_eq!(c.poll_event(), None);
    }

    #[test]
    fn stale_nak_while_waiting_for_s_ack() {
        let mut t = out(0, b'S', OURS, T1);
        t += "< \\x01# N3\\r\n";
        t += &inp(0, b'Y', PEER_SMALL, T1);
        t += &out(1, b'B', b"", T3);
        t += &inp(1, b'Y', b"", T3);
        assert_eq!(replay(Command::Send(vec![]), cfg(), &t), vec![Event::Done]);
    }

    #[test]
    fn input_while_idle_dropped() {
        let now = Instant::now();
        let mut c = Client::new(cfg());
        c.handle_input(now, b"\x01# N3\r\x01+ S~* @-#Y3$\r");
        c.handle_timeout(now + Duration::from_secs(100));
        assert_eq!(c.poll_event(), None);
        assert_eq!(c.poll_output(now), None);
        assert_eq!(c.next_timeout(), None);
        assert!(c.is_idle());
        // A partial packet before start does not leak into the transaction.
        c.handle_input(now, b"\x01+ S~*");
        c.start(now, Command::Finish).unwrap();
        c.handle_input(now, b" @-#Y3$\r");
        assert_eq!(c.poll_event(), None);
    }

    #[test]
    fn receive_duplicate_bad_check_and_error() {
        let mut t = out(0, b'R', b"F", T1);
        t += &s_exchange(b"~* @-#Y3");
        t += &inp(1, b'F', b"F", T3);
        t += &out(1, b'Y', b"", T3);
        t += &inp(2, b'D', b"abc", T3);
        t += &out(2, b'Y', b"", T3);
        // Duplicate: our ACK was lost, re-ACK.
        t += &inp(2, b'D', b"abc", T3);
        t += &out(2, b'Y', b"", T3);
        // Bad check: NAK the expected seq.
        let mut bad = wire(3, b'D', b"def", T3);
        bad[5] ^= 1;
        t += &format!("< {}\n", trace::escape(&bad));
        t += &out(3, b'N', b"", T3);
        t += &inp(3, b'E', b"Disk full", T3);
        let ev = replay(Command::Get(b"F".to_vec()), cfg(), &t);
        assert_eq!(
            ev,
            vec![
                Event::FileStart {
                    name: b"F".to_vec()
                },
                Event::Data(b"abc".to_vec()),
                Event::Error(Error::Remote(b"Disk full".to_vec())),
            ]
        );
    }

    #[test]
    fn receive_unexpected_and_bad_prefix() {
        let mut t = out(0, b'R', b"F", T1);
        t += &s_exchange(b"~* @-#Y3");
        t += &inp(1, b'Q', b"", T3);
        t += &out(1, b'E', b"Unexpected packet", T3);
        let ev = replay(Command::Get(b"F".to_vec()), cfg(), &t);
        assert!(matches!(ev.as_slice(), [Event::Error(Error::Protocol(_))]));

        let mut t = out(0, b'R', b"F", T1);
        t += &s_exchange(b"~* @-#Y3");
        t += &inp(1, b'F', b"ab#", T3);
        t += &out(1, b'E', b"Bad prefix", T3);
        let ev = replay(Command::Get(b"F".to_vec()), cfg(), &t);
        assert!(matches!(ev.as_slice(), [Event::Error(Error::Protocol(_))]));
    }

    #[test]
    fn remote_error_reply() {
        let t = out(0, b'C', b"X", T1) + &inp(0, b'E', b"Bad command", T1);
        let ev = replay(Command::Host(b"X".to_vec()), cfg(), &t);
        assert_eq!(
            ev,
            vec![Event::Error(Error::Remote(b"Bad command".to_vec()))]
        );
    }

    #[test]
    fn timeouts_and_retries() {
        let config = Config {
            retries: 2,
            ..cfg()
        };
        let timeout = config.timeout;
        let now = Instant::now();
        let mut c = Client::new(config);
        c.start(now, Command::Finish).unwrap();
        assert_eq!(c.next_timeout(), Some(now));
        let pkt = c.poll_output(now).unwrap();
        assert_eq!(c.next_timeout(), Some(now + timeout));
        let mut t = now;
        // Too early: nothing happens.
        c.handle_timeout(now + timeout - Duration::from_millis(1));
        assert_eq!(c.poll_output(now + timeout), None);
        for _ in 0..2 {
            t += timeout;
            c.handle_timeout(t);
            assert_eq!(c.poll_output(t), Some(pkt.clone()));
            assert_eq!(c.next_timeout(), Some(t + timeout));
        }
        t += timeout;
        c.handle_timeout(t);
        assert_eq!(
            c.poll_output(t),
            Some(wire(0, b'E', b"Too many retries", T1))
        );
        assert_eq!(c.poll_output(t), None);
        assert_eq!(c.poll_event(), Some(Event::Error(Error::Timeout)));
        assert!(c.is_idle());
        assert_eq!(c.next_timeout(), None);
    }

    #[test]
    fn packet_pause_delays_replies() {
        let pause = Duration::from_millis(100);
        let config = Config {
            packet_pause: pause,
            ..cfg()
        };
        let now = Instant::now();
        let mut c = Client::new(config);
        c.start(now, Command::Host(b"6 7 *".to_vec())).unwrap();
        assert_eq!(c.poll_output(now).unwrap(), b"\x01( C6 7 *C\r");
        c.handle_input(now, b"\x01+ S~* @-#Y3$\r");
        assert_eq!(c.poll_output(now), None);
        assert_eq!(c.next_timeout(), Some(now + pause));
        assert_eq!(c.poll_output(now + pause), Some(wire(0, b'Y', OURS, T1)));
    }

    #[test]
    fn start_rejects_command_too_long() {
        let now = Instant::now();
        let mut c = Client::new(cfg());
        // Default MAXL 80, check type 1: 77 data bytes fit.
        assert_eq!(
            c.start(now, Command::Host(vec![b'x'; 78])),
            Err(StartError::TooLong { len: 78, max: 77 })
        );
        // Control characters double in size when encoded.
        assert_eq!(
            c.start(now, Command::Get(vec![b'\r'; 39])),
            Err(StartError::TooLong { len: 78, max: 77 })
        );
        assert!(c.is_idle());
        assert_eq!(c.poll_output(now), None);
        c.start(now, Command::Host(vec![b'x'; 77])).unwrap();
        let pkt = c.poll_output(now).unwrap();
        assert_eq!(pkt, wire(0, b'C', &[b'x'; 77], T1));
        assert_eq!(pkt[1], b'~' - 14); // LEN 80
    }

    /// Get: R out, S in, ACK out, F(1) in, ACK(1) out.
    fn get_started(config: Config) -> (Client, Instant) {
        let now = Instant::now();
        let mut c = Client::new(config);
        c.start(now, Command::Get(b"F".to_vec())).unwrap();
        assert_eq!(c.poll_output(now), Some(wire(0, b'R', b"F", T1)));
        c.handle_input(now, &wire(0, b'S', b"~* @-#Y3", T1));
        assert_eq!(c.poll_output(now), Some(wire(0, b'Y', OURS, T1)));
        (c, now)
    }

    /// `first_packet_retries = Some(0)` (hptx's host commands, audit PR #18
    /// #1): the `C` is never resent, neither after a timeout nor after a
    /// NAK; once the server's `S` is in, a damaged reply packet is still
    /// NAKed and re-requested with the normal retries.
    #[test]
    fn first_packet_retries_only_cover_the_command() {
        let config = Config {
            first_packet_retries: Some(0),
            nak_grace: Duration::from_secs(20),
            ..cfg()
        };
        let timeout = config.timeout;
        let now = Instant::now();
        let mut c = Client::new(config.clone());
        c.start(now, Command::Host(b"X".to_vec())).unwrap();
        assert_eq!(c.poll_output(now), Some(wire(0, b'C', b"X", T1)));
        c.handle_input(now, &wire(0, b'N', b"", T1));
        c.handle_timeout(now + timeout);
        assert_eq!(
            c.poll_output(now + timeout),
            Some(wire(0, b'E', b"Too many retries", T1))
        );
        assert_eq!(c.poll_output(now + timeout), None);
        assert_eq!(c.poll_event(), Some(Event::Error(Error::Timeout)));

        // The same command answered: S, X, then a damaged D is NAKed twice
        // and the good copy accepted.
        let mut c = Client::new(config);
        c.start(now, Command::Host(b"X".to_vec())).unwrap();
        c.poll_output(now).unwrap();
        c.handle_input(now, &wire(0, b'S', b"~* @-#Y3", T1));
        assert_eq!(c.poll_output(now), Some(wire(0, b'Y', OURS, T1)));
        c.handle_input(now, &wire(1, b'X', b"", T3));
        assert_eq!(c.poll_output(now), Some(wire(1, b'Y', b"", T3)));
        let mut bad = wire(2, b'D', b"42", T3);
        bad[5] ^= 1;
        for _ in 0..2 {
            c.handle_input(now, &bad);
            assert_eq!(c.poll_output(now), Some(wire(2, b'N', b"", T3)));
        }
        // A lost packet: the timeout NAKs it too.
        c.handle_timeout(now + timeout);
        assert_eq!(c.poll_output(now + timeout), Some(wire(2, b'N', b"", T3)));
        c.handle_input(now + timeout, &wire(2, b'D', b"42", T3));
        assert_eq!(c.poll_output(now + timeout), Some(wire(2, b'Y', b"", T3)));
        assert!(
            !all_events(&mut c)
                .iter()
                .any(|e| matches!(e, Event::Error(_)))
        );
    }

    #[test]
    fn receive_timeout_naks_expected_packet() {
        let config = Config {
            retries: 2,
            ..cfg()
        };
        let timeout = config.timeout;
        let (mut c, mut t) = get_started(config);
        // Timeout right after the S ACK: NAK(1), which the sender reads as ACK(0).
        t += timeout;
        c.handle_timeout(t);
        assert_eq!(c.poll_output(t), Some(wire(1, b'N', b"", T3)));
        c.handle_input(t, &wire(1, b'F', b"F", T3));
        assert_eq!(c.poll_output(t), Some(wire(1, b'Y', b"", T3)));
        // Timeouts while waiting for packet 2: NAK(2), counted as retries.
        for _ in 0..2 {
            t += timeout;
            c.handle_timeout(t);
            assert_eq!(c.poll_output(t), Some(wire(2, b'N', b"", T3)));
        }
        // A duplicate still gets the last ACK, not the NAK.
        c.handle_input(t, &wire(1, b'F', b"F", T3));
        assert_eq!(c.poll_output(t), Some(wire(1, b'Y', b"", T3)));
        // The duplicate reset the retry count: two more NAKs, then give up.
        for _ in 0..2 {
            t += timeout;
            c.handle_timeout(t);
            assert_eq!(c.poll_output(t), Some(wire(2, b'N', b"", T3)));
        }
        t += timeout;
        c.handle_timeout(t);
        assert_eq!(
            c.poll_output(t),
            Some(wire(2, b'E', b"Too many retries", T3))
        );
        assert_eq!(
            c.poll_event(),
            Some(Event::FileStart {
                name: b"F".to_vec()
            })
        );
        assert_eq!(c.poll_event(), Some(Event::Error(Error::Timeout)));
    }

    #[test]
    fn send_ignores_bad_check_for_other_seq() {
        let mut t = send_start();
        t += &out(1, b'F', b"A", T3);
        // Late duplicate ACK(0) to S: type 1, fails the type 3 check, ignored.
        t += &inp(0, b'Y', PEER_SMALL, T1);
        // Damaged frame with an unrelated seq: ignored.
        let mut bad = wire(5, b'Y', b"", T3);
        bad[4] ^= 1;
        t += &format!("< {}\n", trace::escape(&bad));
        t += &inp(1, b'Y', b"", T3);
        t += &out(2, b'Z', b"", T3);
        let (c, ev) = replay_client(Command::Send(vec![file(b"A", b"")]), cfg(), &t);
        assert_eq!(
            ev,
            vec![Event::FileStart {
                name: b"A".to_vec()
            }]
        );
        assert_eq!(c.retries, 0);
    }

    #[test]
    fn send_file_name_too_long_fails() {
        // Peer MAXL 20, type 3: 15 data bytes; the name needs 16.
        let mut t = send_start();
        t += &out(0, b'E', b"File name too long", T3);
        let ev = replay(
            Command::Send(vec![file(b"ABCDEFGHIJKLMNOP", b"x")]),
            cfg(),
            &t,
        );
        assert!(matches!(ev.as_slice(), [Event::Error(Error::Protocol(_))]));
        // 15 bytes fit.
        let mut t = send_start();
        t += &out(1, b'F', b"ABCDEFGHIJKLMNO", T3);
        let ev = replay(
            Command::Send(vec![file(b"ABCDEFGHIJKLMNO", b"")]),
            cfg(),
            &t,
        );
        assert!(ev.is_empty());
    }

    #[test]
    fn cancel_drops_queued_output() {
        let pause = Duration::from_millis(100);
        let config = Config {
            packet_pause: pause,
            ..cfg()
        };
        let now = Instant::now();
        let mut c = Client::new(config);
        c.start(now, Command::Host(b"6 7 *".to_vec())).unwrap();
        assert!(c.poll_output(now).is_some());
        // The ACK to S is held back by the pause when we cancel.
        c.handle_input(now, b"\x01+ S~* @-#Y3$\r");
        c.cancel(now);
        assert_eq!(c.poll_output(now), Some(wire(1, b'E', b"Cancelled", T3)));
        assert_eq!(c.poll_output(now + pause), None);
        assert_eq!(c.next_timeout(), None);
        assert_eq!(c.poll_event(), Some(Event::Error(Error::Cancelled)));
    }

    #[test]
    fn huge_durations_do_not_overflow() {
        let config = Config {
            timeout: Duration::MAX,
            packet_pause: Duration::MAX,
            nak_grace: Duration::MAX,
            ..cfg()
        };
        let now = Instant::now();
        let mut c = Client::new(config);
        c.start(now, Command::Host(b"6 7 *".to_vec())).unwrap();
        assert!(c.poll_output(now).is_some());
        // Deadline overflows: none.
        assert_eq!(c.next_timeout(), None);
        // Grace overflows: no deadline either.
        c.handle_input(now, b"\x01# N3\r");
        assert_eq!(c.next_timeout(), None);
        // Pause overflows: the reply is not delayed.
        c.handle_input(now, b"\x01+ S~* @-#Y3$\r");
        let ack = wire(0, b'Y', &c.ours.encode(), T1);
        assert_eq!(c.poll_output(now), Some(ack));
        c.handle_timeout(now);
        assert_eq!(c.poll_output(now), None);
        assert_eq!(c.poll_event(), None);
    }

    /// [`get_started`], then F(1) "F" with its ACK.
    fn get_file_open(config: Config) -> (Client, Instant) {
        let (mut c, now) = get_started(config);
        c.handle_input(now, &wire(1, b'F', b"F", T3));
        assert_eq!(c.poll_output(now), Some(wire(1, b'Y', b"", T3)));
        (c, now)
    }

    fn all_events(c: &mut Client) -> Vec<Event> {
        std::iter::from_fn(|| c.poll_event()).collect()
    }

    #[test]
    fn lost_final_ack_is_answered_while_lingering() {
        let config = cfg();
        let linger = config.linger;
        let (mut c, now) = get_file_open(config);
        c.handle_input(now, &wire(2, b'Z', b"", T3));
        assert_eq!(c.poll_output(now), Some(wire(2, b'Y', b"", T3)));
        c.handle_input(now, &wire(3, b'B', b"", T3));
        let ack = wire(3, b'Y', b"", T3);
        assert_eq!(c.poll_output(now), Some(ack.clone()));
        assert_eq!(
            all_events(&mut c),
            vec![
                Event::FileStart {
                    name: b"F".to_vec()
                },
                Event::FileEnd { discarded: false },
                Event::Done
            ]
        );
        assert!(!c.is_idle());
        assert_eq!(c.next_timeout(), Some(now + linger));
        // Our ACK was lost: the server sends B again and gets the same ACK,
        // without a second Done. Other packets are ignored.
        let t = now + linger / 2;
        c.handle_input(t, &wire(3, b'B', b"", T3));
        assert_eq!(c.poll_output(t), Some(ack.clone()));
        c.handle_input(t, &wire(2, b'Z', b"", T3));
        c.handle_input(t, b"\x01# N3\r");
        assert_eq!(c.poll_output(t), None);
        assert_eq!(c.poll_event(), None);
        // The re-ACK does not extend the linger.
        assert_eq!(c.next_timeout(), Some(now + linger));
        c.handle_timeout(now + linger);
        assert!(c.is_idle());
        assert_eq!(c.next_timeout(), None);
        c.handle_input(now + linger, &wire(3, b'B', b"", T3));
        assert_eq!(c.poll_output(now + linger), None);
        assert_eq!(c.poll_event(), None);
    }

    #[test]
    fn repeated_bs_while_lingering_queue_one_ack() {
        let (mut c, now) = get_started(cfg());
        c.handle_input(now, &wire(1, b'B', b"", T3));
        let ack = wire(1, b'Y', b"", T3);
        assert_eq!(c.poll_output(now), Some(ack.clone()));
        let burst: Vec<u8> = (0..10).flat_map(|_| wire(1, b'B', b"", T3)).collect();
        c.handle_input(now, &burst);
        assert_eq!(c.poll_output(now), Some(ack.clone()));
        assert_eq!(c.poll_output(now), None);
        // Once it is out, the next repeat gets another.
        c.handle_input(now, &wire(1, b'B', b"", T3));
        assert_eq!(c.poll_output(now), Some(ack));
        assert_eq!(c.poll_output(now), None);
    }

    #[test]
    fn start_and_cancel_end_the_linger() {
        let (mut c, now) = get_started(cfg());
        c.handle_input(now, &wire(1, b'B', b"", T3));
        assert_eq!(c.poll_output(now), Some(wire(1, b'Y', b"", T3)));
        assert_eq!(all_events(&mut c), vec![Event::Done]);
        c.cancel(now);
        assert!(c.is_idle());
        assert_eq!(c.poll_output(now), None);
        assert_eq!(c.poll_event(), None);

        let (mut c, now) = get_started(cfg());
        c.handle_input(now, &wire(1, b'B', b"", T3));
        assert!(!c.is_idle());
        c.start(now, Command::Finish).unwrap();
        assert_eq!(c.poll_output(now), Some(wire(0, b'G', b"F", T1)));
        assert_eq!(c.poll_output(now), None);
        assert_eq!(c.poll_event(), None);
    }

    #[test]
    fn zero_linger_goes_idle_at_once() {
        let config = Config {
            linger: Duration::ZERO,
            ..cfg()
        };
        let (mut c, now) = get_started(config);
        c.handle_input(now, &wire(1, b'B', b"", T3));
        assert!(c.is_idle());
        assert_eq!(c.poll_output(now), Some(wire(1, b'Y', b"", T3)));
        assert_eq!(c.next_timeout(), None);
    }

    #[test]
    fn b_before_z_is_a_protocol_error() {
        let (mut c, now) = get_file_open(cfg());
        c.handle_input(now, &wire(2, b'D', b"abc", T3));
        assert_eq!(c.poll_output(now), Some(wire(2, b'Y', b"", T3)));
        c.handle_input(now, &wire(3, b'B', b"", T3));
        assert_eq!(c.poll_output(now), Some(wire(3, b'E', b"Unexpected B", T3)));
        let ev = all_events(&mut c);
        assert!(matches!(
            ev.as_slice(),
            [_, Event::Data(_), Event::Error(Error::Protocol(_))]
        ));
        assert!(c.is_idle());
    }

    #[test]
    fn b_with_text_buffered_is_a_protocol_error() {
        let (mut c, now) = get_started(cfg());
        c.handle_input(now, &wire(1, b'X', b"", T3));
        assert_eq!(c.poll_output(now), Some(wire(1, b'Y', b"", T3)));
        c.handle_input(now, &wire(2, b'D', b"partial", T3));
        assert_eq!(c.poll_output(now), Some(wire(2, b'Y', b"", T3)));
        c.handle_input(now, &wire(3, b'B', b"", T3));
        assert_eq!(c.poll_output(now), Some(wire(3, b'E', b"Unexpected B", T3)));
        assert!(matches!(
            all_events(&mut c).as_slice(),
            [Event::Error(Error::Protocol(_))]
        ));
    }

    #[test]
    fn ack_drops_a_queued_retransmission() {
        let pause = Duration::from_millis(100);
        let config = Config {
            packet_pause: pause,
            ..cfg()
        };
        let now = Instant::now();
        let mut c = Client::new(config);
        c.start(now, Command::Send(vec![file(b"A", b"x")])).unwrap();
        assert_eq!(c.poll_output(now), Some(wire(0, b'S', OURS, T1)));
        c.handle_input(now, &wire(0, b'Y', PEER_SMALL, T1));
        let t = now + pause;
        assert_eq!(c.poll_output(t), Some(wire(1, b'F', b"A", T3)));
        // A NAK and the ACK for F in one read: the resend the NAK queued
        // (held back by the pause) is dropped, D goes out next.
        let mut input = wire(1, b'N', b"", T3);
        input.extend(wire(1, b'Y', b"", T3));
        c.handle_input(t, &input);
        let t = t + pause;
        assert_eq!(c.poll_output(t), Some(wire(2, b'D', b"x", T3)));
        assert_eq!(c.poll_output(t), None);
        // Same with a NAK for n + 1 as the ACK.
        let mut input = wire(2, b'N', b"", T3);
        input.extend(wire(3, b'N', b"", T3));
        c.handle_input(t, &input);
        let t = t + pause;
        assert_eq!(c.poll_output(t), Some(wire(3, b'Z', b"", T3)));
        assert_eq!(c.poll_output(t), None);
    }

    #[test]
    fn packet_drops_a_queued_nak() {
        let pause = Duration::from_millis(100);
        let config = Config {
            packet_pause: pause,
            ..cfg()
        };
        let now = Instant::now();
        let mut c = Client::new(config);
        c.start(now, Command::Get(b"F".to_vec())).unwrap();
        assert!(c.poll_output(now).is_some());
        c.handle_input(now, &wire(0, b'S', b"~* @-#Y3", T1));
        assert_eq!(c.poll_output(now + pause), Some(wire(0, b'Y', OURS, T1)));
        // A damaged F, then the good retransmission in the same read.
        let mut bad = wire(1, b'F', b"F", T3);
        bad[5] ^= 1;
        bad.extend(wire(1, b'F', b"F", T3));
        c.handle_input(now + pause, &bad);
        let t = now + pause * 2;
        assert_eq!(c.poll_output(t), Some(wire(1, b'Y', b"", T3)));
        assert_eq!(c.poll_output(t), None);
    }

    #[test]
    fn bad_check_naks_count_as_retries() {
        let config = Config {
            retries: 2,
            ..cfg()
        };
        let (mut c, now) = get_file_open(config);
        let mut bad = wire(2, b'D', b"abc", T3);
        bad[5] ^= 1;
        for _ in 0..2 {
            c.handle_input(now, &bad);
            assert_eq!(c.poll_output(now), Some(wire(2, b'N', b"", T3)));
        }
        c.handle_input(now, &bad);
        assert_eq!(
            c.poll_output(now),
            Some(wire(2, b'E', b"Too many retries", T3))
        );
        assert_eq!(
            all_events(&mut c).last(),
            Some(&Event::Error(Error::Timeout))
        );
        // A good packet in between resets the count.
        let config = Config {
            retries: 1,
            ..cfg()
        };
        let (mut c, now) = get_file_open(config);
        c.handle_input(now, &bad);
        assert_eq!(c.poll_output(now), Some(wire(2, b'N', b"", T3)));
        c.handle_input(now, &wire(2, b'D', b"abc", T3));
        assert_eq!(c.poll_output(now), Some(wire(2, b'Y', b"", T3)));
        let mut bad = wire(3, b'D', b"def", T3);
        bad[5] ^= 1;
        c.handle_input(now, &bad);
        assert_eq!(c.poll_output(now), Some(wire(3, b'N', b"", T3)));
    }

    #[test]
    fn receive_size_cap() {
        let config = Config {
            max_size: Some(5),
            ..cfg()
        };
        let (mut c, now) = get_file_open(config);
        c.handle_input(now, &wire(2, b'D', b"abc", T3));
        assert_eq!(c.poll_output(now), Some(wire(2, b'Y', b"", T3)));
        c.handle_input(now, &wire(3, b'D', b"de", T3));
        assert_eq!(c.poll_output(now), Some(wire(3, b'Y', b"", T3)));
        c.handle_input(now, &wire(4, b'D', b"f", T3));
        assert_eq!(c.poll_output(now), Some(wire(4, b'E', b"Too large", T3)));
        assert_eq!(
            all_events(&mut c).last(),
            Some(&Event::Error(Error::TooLarge { limit: 5 }))
        );
        assert!(c.is_idle());
        // Server text counts too.
        let config = Config {
            max_size: Some(2),
            ..cfg()
        };
        let (mut c, now) = get_started(config);
        c.handle_input(now, &wire(1, b'X', b"", T3));
        assert_eq!(c.poll_output(now), Some(wire(1, b'Y', b"", T3)));
        c.handle_input(now, &wire(2, b'D', b"abc", T3));
        assert_eq!(c.poll_output(now), Some(wire(2, b'E', b"Too large", T3)));
        assert_eq!(Config::default().max_size, Some(4 << 20));
    }

    #[test]
    fn start_clears_the_previous_transaction() {
        let pause = Duration::from_millis(100);
        let config = Config {
            packet_pause: pause,
            ..cfg()
        };
        let now = Instant::now();
        let mut c = Client::new(config);
        c.start(now, Command::Info).unwrap();
        assert!(c.poll_output(now).is_some());
        c.handle_input(now, b"\x01+ Y~& @-# 3,\r");
        assert!(c.peer_params().is_some());
        assert!(c.is_idle());
        // Done not polled; a failed transaction leaves an E packet queued.
        c.start(now, Command::Host(b"X".to_vec())).unwrap();
        assert_eq!(c.peer_params(), None);
        assert_eq!(c.poll_event(), None);
        assert!(c.poll_output(now).is_some());
        c.handle_input(now, b"\x01+ S~* @-#Y3$\r");
        c.cancel(now);
        c.start(now, Command::Finish).unwrap();
        assert_eq!(c.poll_output(now + pause), Some(wire(0, b'G', b"F", T1)));
        assert_eq!(c.poll_output(now + pause), None);
        assert_eq!(c.poll_event(), None);
    }

    #[test]
    fn busy_and_cancel() {
        let now = Instant::now();
        let mut c = Client::new(cfg());
        c.start(now, Command::Directory).unwrap();
        assert_eq!(c.start(now, Command::Finish), Err(StartError::Busy));
        assert!(c.poll_output(now).is_some());
        c.cancel(now);
        assert_eq!(c.poll_output(now), Some(wire(0, b'E', b"Cancelled", T1)));
        assert_eq!(c.poll_event(), Some(Event::Error(Error::Cancelled)));
        assert!(c.is_idle());
        c.cancel(now);
        assert_eq!(c.poll_output(now), None);
        assert_eq!(c.poll_event(), None);
        assert!(c.start(now, Command::Finish).is_ok());
    }
}
