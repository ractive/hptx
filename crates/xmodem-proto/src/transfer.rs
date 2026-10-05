//! The XModem state machine, both roles. wiki: protocols/xmodem,
//! protocols/xmodem-hp.

use crate::time::{Duration, Instant};
use std::collections::VecDeque;
use std::fmt;

use crate::codec::{
    ACK, BlockSize, CAN, CR_KERMIT, Check, EOT, NAK, SOH, STX, SUB, decode_block, frame_block,
    frame_len,
};

/// Tunables of a [`Transfer`].
///
/// Non-exhaustive: start from [`Config::default`] and set the fields you
/// need.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
    /// Receiver: the check asked for first. [`Check::HpCrc`] (default) sends
    /// `D`, [`Check::Crc16`] sends `C`; both fall back to NAK (checksum)
    /// after [`Config::crc_attempts`] unanswered start characters.
    /// [`Check::Checksum`] sends NAK from the start. The sender ignores this
    /// and uses whatever the receiver asks for (NAK, `C` or `D`). Before
    /// the first ACK the sender follows a later `C` or `D`, and a NAK after
    /// `D` (the 49G's fallback); a NAK in CRC-16 mode only resends block 1
    /// in CRC-16, since a standard receiver NAKs a damaged block that way.
    ///
    /// `D` is the default because no HP calculator answers `C`: the 49G's
    /// XSEND answers `D` with HP-CRC blocks (1k blocks when the object is big
    /// enough) and ignores `C`; the 48GX's XSEND ignores both and answers the
    /// NAK. Verified on the emulated 49G and 48GX (traces `49g-xsend*`,
    /// `48gx-xsend*`).
    pub check: Check,
    /// Receiver: `C`s (or `D`s) sent before falling back to checksum
    /// (default 3).
    pub crc_attempts: u32,
    /// Receiver: wait after each `C` or `D` before the next start character
    /// (default 3 s).
    pub crc_interval: Duration,
    /// Sender: block size for full blocks (default 128). 1k blocks go out
    /// only when the receiver asked for a CRC (`C` or `D`), unless
    /// [`Config::checksum_1k`] is set: a receiver that opens with NAK gets
    /// 128-byte blocks.
    pub block_size: BlockSize,
    /// Sender: also send 1k blocks when the receiver asked for checksum mode
    /// (default false). The 48GX, which knows only checksum mode, NAKs every
    /// 1k block and cancels after nine (trace `48gx-xrecv-1k`); XModem-1K
    /// is classically used with CRC only.
    pub checksum_1k: bool,
    /// Sender with 1k blocks: send the tail of the file in 128-byte blocks once
    /// no more than 896 bytes (7 x 128) remain, so no block carries 128 or
    /// more bytes of padding (default true). The 49G does not convert a
    /// received object with more than about 255 bytes of padding (as HP's
    /// Conn4x source notes).
    pub short_tail: bool,
    /// Sender: padding byte for the last block (default 0x1A, SUB).
    pub pad: u8,
    /// Sender: how long to wait for the receiver's first start character
    /// (default 60 s). Receiver: the overall wait for the first block is
    /// bounded by the start-character retries instead, or by
    /// [`Config::recv_start_timeout`].
    pub start_timeout: Duration,
    /// Wait for an ACK/NAK (sender) or for the next block (receiver)
    /// (default 10 s).
    pub timeout: Duration,
    /// Receiver: longest gap between two bytes of one block (default 1 s).
    pub byte_timeout: Duration,
    /// Receiver: after a bad block, input must be silent this long before we
    /// NAK, so the sender hears the NAK (default 1 s).
    pub purge: Duration,
    /// Retries per block (or per EOT, or NAKs while starting) before giving
    /// up (default 10).
    pub retries: u32,
    /// Receiver: keep sending start characters until this much time has
    /// passed since [`Transfer::start`], instead of stopping after
    /// `crc_attempts + retries` of them (default `None`: count only). For
    /// a transfer a human starts on the calculator keyboard. The switch
    /// from `C`/`D` to NAK still happens after [`Config::crc_attempts`].
    pub recv_start_timeout: Option<Duration>,
    /// Receiver: after we ACK the EOT, keep answering a retransmitted EOT
    /// with ACK for this long (default 1 s), in case our ACK was damaged or
    /// lost. `FileEnd` and `Done` are emitted when the ACK is queued; the
    /// linger emits nothing. [`Transfer::is_idle`] is false while it lasts;
    /// [`Transfer::start`] ends it early. `Duration::ZERO` turns it off. A
    /// sender that only retransmits after its reply timeout is caught only
    /// by a linger that long.
    pub linger: Duration,
    /// Receiver: most data bytes (padding included) accepted; a block that
    /// would go beyond fails the transfer with [`Error::TooLarge`] and CANs.
    /// Default 4 MiB, well above the largest HP object; `None` means no
    /// limit. XModem has no length field and block numbers wrap, so without
    /// a limit a peer can grow the buffer without bound.
    pub max_size: Option<usize>,
}

/// Default [`Config::max_size`]: 4 MiB.
const DEFAULT_MAX_SIZE: usize = 4 << 20;

impl Default for Config {
    fn default() -> Self {
        Config {
            check: Check::HpCrc,
            crc_attempts: 3,
            crc_interval: Duration::from_secs(3),
            block_size: BlockSize::B128,
            checksum_1k: false,
            short_tail: true,
            pad: SUB,
            start_timeout: Duration::from_secs(60),
            timeout: Duration::from_secs(10),
            byte_timeout: Duration::from_secs(1),
            purge: Duration::from_secs(1),
            retries: 10,
            recv_start_timeout: None,
            linger: Duration::from_secs(1),
            max_size: Some(DEFAULT_MAX_SIZE),
        }
    }
}

/// One transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Command {
    /// We are the sender (the calculator runs `XRECV`); the whole file.
    Send(Vec<u8>),
    /// We are the receiver (the calculator runs `XSEND`).
    Receive,
}

/// Something that happened during a transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// The check the transfer runs with is settled: sender, on the receiver's
    /// start character, and again if a later start character before the
    /// first ACK changes it (`C` or `D` at any time before the first ACK; NAK
    /// only after `D`, the 49G's fallback to checksum: after `C` a NAK asks
    /// for a damaged block 1 again in CRC-16); receiver, on the first block.
    Started {
        /// The block check in use.
        check: Check,
    },
    /// After each acknowledged block (send) or accepted new block (receive).
    Progress {
        /// Bytes of the file acknowledged (send, at most the file size) or
        /// received (receive, padding included).
        bytes: u64,
        /// File size when sending; `None` when receiving.
        total: Option<u64>,
    },
    /// Receive: EOT acknowledged; the file as received.
    FileEnd {
        /// Every data byte received, padding of the last block included:
        /// stripping it is the caller's job. XModem carries no file length.
        /// For an HP object (the calculators send binary objects: an 8-byte
        /// `HPHP48-x`/`HPHP49-x` header, then the object), the real end is
        /// found by walking the object: read its prolog and the size its
        /// type implies (a length field for strings, code and the like, the
        /// nested objects for composites), and cut there.
        data: Vec<u8>,
        /// Size of the last block (128 or 1024; 0 for an empty transfer):
        /// the most padding the file can carry.
        last_block: usize,
        /// Trailing bytes of the last block equal to its final byte, when that
        /// byte is 0x1A or 0x00: the likely padding. A hint only; real data
        /// may end in such bytes, and the 49G's XSEND pads with whatever
        /// follows the object in memory (hint 0), while the 48GX pads with
        /// zeros. Only the object-length walk finds the real end.
        padding: usize,
    },
    /// Terminal failure; the machine is idle.
    Error(Error),
    /// Terminal success. The machine is idle, or, as receiver, lingers
    /// after the final ACK ([`Config::linger`]).
    Done,
}

/// Why a transfer failed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Retries exhausted, or no start character within `start_timeout`.
    Timeout,
    /// The peer sent two CANs in a row.
    RemoteCancelled,
    /// A block out of sequence (lost sync); we sent CANs.
    Protocol(String),
    /// [`Transfer::cancel`] was called; we sent CANs.
    Cancelled,
    /// The received data exceeded [`Config::max_size`]; we sent CANs.
    TooLarge {
        /// The limit that was exceeded.
        limit: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Timeout => f.write_str("timed out: too many retries"),
            Error::RemoteCancelled => f.write_str("cancelled by the peer"),
            Error::Protocol(msg) => write!(f, "protocol error: {msg}"),
            Error::Cancelled => f.write_str("cancelled"),
            Error::TooLarge { limit } => {
                write!(f, "received data exceeds the limit of {limit} bytes")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Why [`Transfer::start`] refused a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StartError {
    /// A transfer is already running.
    Busy,
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a transfer is already running")
    }
}

impl std::error::Error for StartError {}

/// What we wait for after a queued write is handed out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wait {
    /// Nothing (terminal output).
    None,
    /// The normal reply timeout.
    Reply,
    /// The next start character after a `C`.
    CrcInterval,
}

#[derive(Debug)]
enum Phase {
    Idle,
    /// Sender: waiting for NAK, `C` or `D`. `skip`: bytes left to skip of a
    /// Kermit packet (SOH .. CR) the calculator may still send.
    SendStart {
        skip: u8,
    },
    /// Sender: block at `pos` of `len` data bytes is out, awaiting ACK.
    SendBlock {
        len: usize,
    },
    /// Sender: EOT is out, awaiting ACK.
    SendEot,
    /// Receiver: start characters going out, no block yet.
    RecvStart,
    /// Receiver: blocks flowing.
    RecvBlocks,
    /// Receiver: EOT acknowledged; re-ACK a retransmitted EOT until
    /// `deadline`.
    RecvLinger,
}

/// Sans-IO XModem transfer, sender or receiver. See the crate documentation
/// for the driver loop.
#[derive(Debug)]
pub struct Transfer {
    config: Config,
    phase: Phase,
    check: Check,
    /// Send: the file. Receive: the data received so far.
    data: Vec<u8>,
    /// Send: offset of the current block.
    pos: usize,
    /// Send: current block number. Receive: the next expected block number.
    blk: u8,
    /// Send: some block has been ACKed (later `C`s are ignored).
    acked_any: bool,
    last_sent: Vec<u8>,
    queue: VecDeque<(Vec<u8>, Wait)>,
    events: VecDeque<Event>,
    deadline: Option<Instant>,
    retries: u32,
    /// Consecutive CANs seen where a control byte was expected.
    cans: u8,
    /// Receive: start characters sent so far.
    starts: u32,
    /// Receive: partial frame.
    buf: Vec<u8>,
    /// Receive: when the last byte of `buf` arrived.
    last_byte: Option<Instant>,
    /// Receive: discarding input until the line is quiet, then NAK.
    purging: bool,
    /// Receive: size of the last accepted block.
    last_block: usize,
    /// Receive: when [`Transfer::start`] ran (for
    /// [`Config::recv_start_timeout`]).
    started_at: Option<Instant>,
    /// Receive, starting: the start-character deadline, held while a block
    /// arrives (the byte timeout governs it) and restored if it is dropped.
    held_deadline: Option<Instant>,
}

const CANCEL: [u8; 3] = [CAN, CAN, CAN];

/// Longest Kermit packet after its SOH: LEN, at most 94 more bytes, CR.
const KERMIT_MAX_PACKET: u8 = 96;

impl Transfer {
    /// New idle machine.
    pub fn new(config: Config) -> Self {
        Transfer {
            check: config.check,
            config,
            phase: Phase::Idle,
            data: Vec::new(),
            pos: 0,
            blk: 1,
            acked_any: false,
            last_sent: Vec::new(),
            queue: VecDeque::new(),
            events: VecDeque::new(),
            deadline: None,
            retries: 0,
            cans: 0,
            starts: 0,
            buf: Vec::new(),
            last_byte: None,
            purging: false,
            last_block: 0,
            started_at: None,
            held_deadline: None,
        }
    }

    /// Begin a transfer. A sender waits for the receiver's start character
    /// (nothing to write yet); a receiver's first start character is available
    /// from `poll_output(now)` at once.
    ///
    /// Output and events of the previous transfer that were not polled yet
    /// are dropped; a linger after it ([`Config::linger`]) ends.
    pub fn start(&mut self, now: Instant, command: Command) -> Result<(), StartError> {
        if !matches!(self.phase, Phase::Idle | Phase::RecvLinger) {
            return Err(StartError::Busy);
        }
        self.queue.clear();
        self.events.clear();
        self.held_deadline = None;
        self.pos = 0;
        self.blk = 1;
        self.acked_any = false;
        self.retries = 0;
        self.cans = 0;
        self.starts = 0;
        self.buf.clear();
        self.last_byte = None;
        self.purging = false;
        self.last_block = 0;
        self.last_sent.clear();
        self.started_at = Some(now);
        match command {
            Command::Send(data) => {
                self.data = data;
                self.phase = Phase::SendStart { skip: 0 };
                self.deadline = now.checked_add(self.config.start_timeout);
            }
            Command::Receive => {
                self.data = Vec::new();
                self.check = self.config.check;
                self.phase = Phase::RecvStart;
                self.deadline = None;
                self.send_start_char(now);
            }
        }
        Ok(())
    }

    /// Feed bytes read from the link, in any chunking.
    pub fn handle_input(&mut self, now: Instant, bytes: &[u8]) {
        for &b in bytes {
            if self.is_idle() {
                return;
            }
            match self.phase {
                Phase::Idle => {}
                Phase::SendStart { .. } | Phase::SendBlock { .. } | Phase::SendEot => {
                    self.on_send_byte(now, b)
                }
                Phase::RecvStart | Phase::RecvBlocks => self.on_recv_byte(now, b),
                // A retransmitted EOT: our ACK was lost or damaged. One
                // pending ACK answers any number of them.
                Phase::RecvLinger if b == EOT => {
                    if !self.queue.iter().any(|(q, _)| q.as_slice() == [ACK]) {
                        self.push(vec![ACK], Wait::None);
                    }
                }
                Phase::RecvLinger => {}
            }
        }
    }

    /// Call when the time from [`Transfer::next_timeout`] has passed (or
    /// whenever a read times out; it does nothing early).
    pub fn handle_timeout(&mut self, now: Instant) {
        match self.next_timeout() {
            Some(due) if now >= due => {}
            _ => return,
        }
        self.deadline = None;
        match self.phase {
            Phase::Idle => {}
            Phase::SendStart { .. } => self.finish(Event::Error(Error::Timeout)),
            Phase::SendBlock { .. } | Phase::SendEot => self.resend(),
            Phase::RecvStart => {
                if !self.buf.is_empty() {
                    // A block stalled: drop it and resume the start-character
                    // interval that was running when it began.
                    self.drop_partial();
                    if self.deadline.is_some_and(|d| now < d) {
                        return;
                    }
                }
                self.purging = false;
                self.send_start_char(now);
            }
            Phase::RecvBlocks => {
                if self.purging || !self.buf.is_empty() {
                    // The line went quiet after a bad or partial block.
                    self.buf.clear();
                    self.purging = false;
                }
                self.nak();
            }
            Phase::RecvLinger => self.phase = Phase::Idle,
        }
    }

    /// Next bytes to write, written atomically: a whole block, a single
    /// control byte, or the CAN sequence. Keep calling after `Done`/`Error`
    /// until `None`: a final ACK or the CANs may still be queued.
    pub fn poll_output(&mut self, now: Instant) -> Option<Vec<u8>> {
        let (bytes, wait) = self.queue.pop_front()?;
        // The linger's deadline is fixed when it starts.
        if !matches!(self.phase, Phase::Idle | Phase::RecvLinger) {
            let span = match wait {
                Wait::None => None,
                Wait::Reply => Some(self.config.timeout),
                Wait::CrcInterval => Some(self.config.crc_interval),
            };
            // Overflow (absurd timeout) means no deadline.
            self.deadline = span.and_then(|s| now.checked_add(s));
        }
        Some(bytes)
    }

    /// Next event, in order.
    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// The earliest instant at which [`Transfer::handle_timeout`] has work.
    pub fn next_timeout(&self) -> Option<Instant> {
        if self.is_idle() {
            return None;
        }
        let byte = match (self.purging, self.last_byte) {
            (true, Some(t)) => t.checked_add(self.config.purge),
            (false, Some(t)) if !self.buf.is_empty() => t.checked_add(self.config.byte_timeout),
            _ => None,
        };
        match (byte, self.deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// No transfer running and no linger after one (output may still be
    /// queued).
    pub fn is_idle(&self) -> bool {
        matches!(self.phase, Phase::Idle)
    }

    /// The check in use (receive: requested so far; settled after
    /// [`Event::Started`]).
    pub fn check(&self) -> Check {
        self.check
    }

    /// Abort: queue the CAN sequence, emit `Error(Cancelled)`, go idle.
    /// No-op when idle. During the linger after `Done` it only ends the
    /// linger: no CANs, no event.
    pub fn cancel(&mut self, _now: Instant) {
        match self.phase {
            Phase::Idle => {}
            Phase::RecvLinger => {
                self.phase = Phase::Idle;
                self.deadline = None;
            }
            _ => self.fail(Error::Cancelled, true),
        }
    }

    // ---- helpers ----

    fn push(&mut self, bytes: Vec<u8>, wait: Wait) {
        self.queue.push_back((bytes, wait));
    }

    /// Queue `bytes` as the retransmit copy.
    fn send(&mut self, bytes: Vec<u8>) {
        self.last_sent = bytes.clone();
        self.deadline = None;
        self.push(bytes, Wait::Reply);
    }

    fn finish(&mut self, event: Event) {
        self.events.push_back(event);
        self.phase = Phase::Idle;
        self.deadline = None;
        self.buf.clear();
        self.last_byte = None;
        self.purging = false;
    }

    /// Emit Error, optionally queue CANs (dropping anything still queued),
    /// go idle.
    fn fail(&mut self, err: Error, cancel: bool) {
        self.queue.clear();
        if cancel {
            self.push(CANCEL.to_vec(), Wait::None);
        }
        self.finish(Event::Error(err));
    }

    /// Count a retry; true if the caller may retry, otherwise the transfer
    /// failed with a timeout.
    fn retry(&mut self) -> bool {
        self.retries += 1;
        if self.retries > self.config.retries {
            self.fail(Error::Timeout, true);
            false
        } else {
            true
        }
    }

    /// A CAN where a control byte was expected; true once two came in a row.
    fn on_can(&mut self, b: u8) -> bool {
        if b == CAN {
            self.cans += 1;
            if self.cans >= 2 {
                self.fail(Error::RemoteCancelled, false);
                return true;
            }
        } else {
            self.cans = 0;
        }
        false
    }

    // ---- sender ----

    /// Data length of the block at `pos`.
    fn block_len(&self) -> (BlockSize, usize) {
        let left = self.data.len().saturating_sub(self.pos);
        let size = match self.config.block_size {
            BlockSize::B1k if self.check == Check::Checksum && !self.config.checksum_1k => {
                BlockSize::B128
            }
            BlockSize::B1k if self.config.short_tail && left <= 7 * 128 => BlockSize::B128,
            s => s,
        };
        (size, left.min(size.len()))
    }

    fn send_block(&mut self) {
        if self.pos >= self.data.len() {
            self.phase = Phase::SendEot;
            self.send(vec![EOT]);
            return;
        }
        let (size, len) = self.block_len();
        let chunk = self.data.get(self.pos..self.pos + len).unwrap_or_default();
        let bytes = frame_block(self.blk, size, chunk, self.config.pad, self.check);
        self.phase = Phase::SendBlock { len };
        self.send(bytes);
    }

    fn resend(&mut self) {
        if self.retry() {
            let bytes = self.last_sent.clone();
            self.deadline = None;
            self.push(bytes, Wait::Reply);
        }
    }

    fn on_send_byte(&mut self, _now: Instant, b: u8) {
        if self.on_can(b) {
            return;
        }
        match self.phase {
            Phase::SendStart { skip } => {
                // A Kermit packet (SOH .. CR) from the calculator, e.g. the
                // idle server's NAK or a reply whose D packets hold a `D`, may
                // still be in the pipe; its bytes are not start characters.
                // A packet is at most 96 bytes, so a lost CR cannot hide
                // start characters for long.
                if skip > 0 {
                    let skip = if b == CR_KERMIT { 0 } else { skip - 1 };
                    self.phase = Phase::SendStart { skip };
                    return;
                }
                if b == SOH {
                    self.phase = Phase::SendStart {
                        skip: KERMIT_MAX_PACKET,
                    };
                    return;
                }
                let Some(check) = Check::from_start_char(b) else {
                    return;
                };
                self.check = check;
                self.events.push_back(Event::Started { check });
                self.send_block();
            }
            // A reply cannot answer a write that is still queued.
            _ if !self.queue.is_empty() => {}
            // Before the first ACK the receiver may still be starting: `C`
            // or `D` asks for block 1 again in the check it names. A NAK
            // switches to checksum only from HP's CRC (the 49G's XRECV falls
            // back from `D` to NAK); in CRC-16 or checksum mode it is a plain
            // NAK for a damaged block 1, which a standard CRC-16 receiver
            // sends while staying in CRC mode.
            Phase::SendBlock { .. } if !self.acked_any && Check::from_start_char(b).is_some() => {
                let Some(asked) = Check::from_start_char(b) else {
                    return;
                };
                let check = match (asked, self.check) {
                    (Check::Checksum, Check::Crc16) => Check::Crc16,
                    (asked, _) => asked,
                };
                if self.retry() {
                    if check != self.check {
                        self.check = check;
                        self.events.push_back(Event::Started { check });
                    }
                    // Re-encoded: the check (and with it the block size)
                    // may have changed.
                    self.send_block();
                }
            }
            Phase::SendBlock { len } => match b {
                ACK => {
                    self.retries = 0;
                    self.acked_any = true;
                    self.pos += len;
                    self.blk = self.blk.wrapping_add(1);
                    self.events.push_back(Event::Progress {
                        bytes: self.pos as u64,
                        total: Some(self.data.len() as u64),
                    });
                    self.send_block();
                }
                NAK => self.resend(),
                _ => {}
            },
            Phase::SendEot => match b {
                ACK => self.finish(Event::Done),
                NAK => self.resend(),
                _ => {}
            },
            _ => {}
        }
    }

    // ---- receiver ----

    fn send_start_char(&mut self, now: Instant) {
        let crc = self.config.check != Check::Checksum && self.starts < self.config.crc_attempts;
        let limit = match self.config.check {
            Check::Checksum => self.config.retries,
            Check::Crc16 | Check::HpCrc => {
                self.config.crc_attempts.saturating_add(self.config.retries)
            }
        };
        let exhausted = match (self.config.recv_start_timeout, self.started_at) {
            // Overflow (absurd timeout) means no limit.
            (Some(window), Some(at)) => at.checked_add(window).is_some_and(|end| now >= end),
            _ => self.starts >= limit,
        };
        if exhausted {
            self.fail(Error::Timeout, true);
            return;
        }
        self.starts += 1;
        let (check, wait) = if crc {
            (self.config.check, Wait::CrcInterval)
        } else {
            (Check::Checksum, Wait::Reply)
        };
        self.check = check;
        self.last_sent = vec![check.start_char()];
        self.push(vec![check.start_char()], wait);
    }

    fn nak(&mut self) {
        if self.retry() {
            self.send(vec![NAK]);
        }
    }

    /// Starting: drop the partial frame and restore the start-character
    /// deadline held while it arrived.
    fn drop_partial(&mut self) {
        self.buf.clear();
        self.last_byte = None;
        if let Some(d) = self.held_deadline.take() {
            self.deadline = Some(d);
        }
    }

    /// Bad frame while blocks flow: discard input until the line is quiet,
    /// then NAK (from `handle_timeout`).
    fn purge(&mut self, now: Instant) {
        self.buf.clear();
        self.purging = true;
        self.last_byte = Some(now);
        self.deadline = None;
    }

    fn on_recv_byte(&mut self, now: Instant, b: u8) {
        if self.purging {
            self.last_byte = Some(now);
            return;
        }
        if self.buf.is_empty() {
            if self.on_can(b) {
                return;
            }
            match b {
                SOH | STX => {
                    self.buf.push(b);
                    self.last_byte = Some(now);
                    if matches!(self.phase, Phase::RecvStart) {
                        // A block may be arriving: the byte timeout governs
                        // it, not the start-character interval, which would
                        // otherwise discard it (or switch the check) halfway.
                        self.held_deadline = self.deadline.take();
                    }
                }
                EOT if matches!(self.phase, Phase::RecvBlocks) => self.on_eot(now),
                // Line noise or a Kermit packet before the transfer.
                _ => {}
            }
            return;
        }
        self.buf.push(b);
        self.last_byte = Some(now);
        if self.buf.len() == 3 && self.buf.get(1).map(|n| !n) != self.buf.get(2).copied() {
            // Not a block header: noise, or a Kermit packet (SOH LEN SEQ ..).
            if matches!(self.phase, Phase::RecvStart) {
                self.drop_partial();
            } else {
                self.purge(now);
            }
            return;
        }
        let size = match self.buf.first().and_then(|&h| BlockSize::from_header(h)) {
            Some(s) => s,
            None => return,
        };
        if self.buf.len() < frame_len(size, self.check) {
            return;
        }
        let frame = std::mem::take(&mut self.buf);
        self.last_byte = None;
        self.on_frame(now, size, &frame);
    }

    fn on_frame(&mut self, now: Instant, size: BlockSize, frame: &[u8]) {
        let block = match decode_block(frame, self.check) {
            Ok(b) => b,
            Err(_) => {
                if matches!(self.phase, Phase::RecvStart) {
                    // The check we asked for does not fit; keep asking.
                    self.drop_partial();
                    return;
                }
                self.purge(now);
                return;
            }
        };
        if matches!(self.phase, Phase::RecvStart) {
            self.phase = Phase::RecvBlocks;
            self.held_deadline = None;
            self.events.push_back(Event::Started { check: self.check });
        }
        if block.num == self.blk {
            if let Some(limit) = self.config.max_size
                && self.data.len().saturating_add(block.data.len()) > limit
            {
                self.fail(Error::TooLarge { limit }, true);
                return;
            }
            self.retries = 0;
            self.blk = self.blk.wrapping_add(1);
            self.data.extend_from_slice(&block.data);
            self.last_block = size.len();
            self.events.push_back(Event::Progress {
                bytes: self.data.len() as u64,
                total: None,
            });
            self.send(vec![ACK]);
        } else if block.num == self.blk.wrapping_sub(1) {
            // Our ACK was lost: acknowledge the repeat and drop it.
            self.send(vec![ACK]);
        } else {
            self.fail(
                Error::Protocol(format!(
                    "block {} out of sequence, expected {}",
                    block.num, self.blk
                )),
                true,
            );
        }
    }

    fn on_eot(&mut self, now: Instant) {
        self.push(vec![ACK], Wait::None);
        let data = std::mem::take(&mut self.data);
        let last_block = self.last_block;
        let padding = match data.last() {
            Some(&p) if p == SUB || p == 0 => data
                .iter()
                .rev()
                .take(last_block)
                .take_while(|&&b| b == p)
                .count(),
            _ => 0,
        };
        self.events.push_back(Event::FileEnd {
            data,
            last_block,
            padding,
        });
        self.finish(Event::Done);
        // A linger that overflows `Instant` would never end: skip it.
        if let Some(end) = now.checked_add(self.config.linger)
            && !self.config.linger.is_zero()
        {
            self.phase = Phase::RecvLinger;
            self.deadline = Some(end);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::codec::{STX, crc16, encode_block};

    fn t0() -> Instant {
        Instant::now()
    }

    fn drain(t: &mut Transfer, now: Instant) -> Vec<Vec<u8>> {
        std::iter::from_fn(|| t.poll_output(now)).collect()
    }

    fn events(t: &mut Transfer) -> Vec<Event> {
        std::iter::from_fn(|| t.poll_event()).collect()
    }

    fn block(num: u8, data: &[u8], check: Check) -> Vec<u8> {
        encode_block(num, BlockSize::B128, data, SUB, check).unwrap()
    }

    fn sender(data: &[u8], config: Config) -> (Transfer, Instant) {
        let now = t0();
        let mut t = Transfer::new(config);
        t.start(now, Command::Send(data.to_vec())).unwrap();
        (t, now)
    }

    /// Receiver asking with `C` (CRC-16), the standard XModem start.
    fn crc() -> Config {
        Config {
            check: Check::Crc16,
            ..Config::default()
        }
    }

    #[test]
    fn default_receiver_asks_with_d() {
        let (mut t, now) = receiver(Config::default());
        assert_eq!(drain(&mut t, now), vec![b"D".to_vec()]);
    }

    fn receiver(config: Config) -> (Transfer, Instant) {
        let now = t0();
        let mut t = Transfer::new(config);
        t.start(now, Command::Receive).unwrap();
        (t, now)
    }

    // ---- sender ----

    #[test]
    fn send_checksum_on_nak() {
        let (mut t, now) = sender(b"hello", Config::default());
        assert_eq!(t.poll_output(now), None, "sender waits for the receiver");
        t.handle_input(now, &[NAK]);
        assert_eq!(
            drain(&mut t, now),
            vec![block(1, b"hello", Check::Checksum)]
        );
        t.handle_input(now, &[ACK]);
        assert_eq!(drain(&mut t, now), vec![vec![EOT]]);
        t.handle_input(now, &[ACK]);
        assert_eq!(
            events(&mut t),
            vec![
                Event::Started {
                    check: Check::Checksum
                },
                Event::Progress {
                    bytes: 5,
                    total: Some(5)
                },
                Event::Done
            ]
        );
        assert!(t.is_idle());
    }

    #[test]
    fn send_crc_on_c() {
        let (mut t, now) = sender(&[0x55; 200], Config::default());
        t.handle_input(now, b"C");
        let out = drain(&mut t, now);
        assert_eq!(out, vec![block(1, &[0x55; 128], Check::Crc16)]);
        assert_eq!(out[0].len(), 133);
        t.handle_input(now, &[ACK]);
        assert_eq!(
            drain(&mut t, now),
            vec![block(2, &[0x55; 72], Check::Crc16)]
        );
        // `C` after the first ACK is ignored.
        t.handle_input(now, b"C");
        assert_eq!(drain(&mut t, now), Vec::<Vec<u8>>::new());
        // Two ACKs in one read: the second cannot answer the unwritten EOT.
        t.handle_input(now, &[ACK, ACK]);
        assert_eq!(drain(&mut t, now), vec![vec![EOT]]);
        t.handle_input(now, &[ACK]);
        assert_eq!(events(&mut t).last(), Some(&Event::Done));
    }

    #[test]
    fn send_hp_crc_on_d() {
        let cfg = Config {
            block_size: BlockSize::B1k,
            ..Config::default()
        };
        let data = vec![0xA5; 1500];
        let (mut t, now) = sender(&data, cfg);
        t.handle_input(now, b"D");
        let out = drain(&mut t, now);
        assert_eq!(
            out,
            vec![encode_block(1, BlockSize::B1k, &data[..1024], SUB, Check::HpCrc).unwrap()]
        );
        let crc = crate::codec::hp_crc(&data[..1024]);
        assert_eq!(&out[0][1027..], &crc.to_be_bytes());
        // An extra `D` before the first ACK resends.
        t.handle_input(now, b"D");
        assert_eq!(drain(&mut t, now), out);
        assert_eq!(
            events(&mut t)[0],
            Event::Started {
                check: Check::HpCrc
            }
        );
    }

    #[test]
    fn receive_hp_crc_then_fallback() {
        let cfg = Config {
            check: Check::HpCrc,
            crc_attempts: 2,
            ..Config::default()
        };
        let (mut t, now) = receiver(cfg);
        let mut starts = drain(&mut t, now);
        let at = t.next_timeout().unwrap();
        t.handle_timeout(at);
        starts.extend(drain(&mut t, at));
        let at = t.next_timeout().unwrap();
        t.handle_timeout(at);
        starts.extend(drain(&mut t, at));
        assert_eq!(starts, vec![b"D".to_vec(), b"D".to_vec(), vec![NAK]]);
    }

    #[test]
    fn receive_hp_crc_block() {
        let cfg = Config {
            check: Check::HpCrc,
            ..Config::default()
        };
        let (mut t, now) = receiver(cfg);
        drain(&mut t, now);
        t.handle_input(
            now,
            &encode_block(1, BlockSize::B1k, b"hp", 0, Check::HpCrc).unwrap(),
        );
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
    }

    #[test]
    fn send_ignores_noise_and_kermit_packets_before_start() {
        let (mut t, now) = sender(b"x", Config::default());
        // A stale Kermit NAK and a reply packet holding a `C`.
        t.handle_input(now, b"\x01# N3\r\x01& DCC\r junk");
        assert_eq!(t.poll_output(now), None);
        t.handle_input(now, b"C");
        assert_eq!(drain(&mut t, now), vec![block(1, b"x", Check::Crc16)]);
    }

    #[test]
    fn send_kermit_skip_is_bounded() {
        // A Kermit packet whose CR was lost: start characters count again
        // after at most 96 bytes.
        let (mut t, now) = sender(b"x", Config::default());
        t.handle_input(now, &[0x01]);
        t.handle_input(now, &[b'C'; 96]);
        assert_eq!(t.poll_output(now), None);
        t.handle_input(now, b"C");
        assert_eq!(drain(&mut t, now), vec![block(1, b"x", Check::Crc16)]);
    }

    #[test]
    fn send_extra_c_before_first_ack_resends() {
        let (mut t, now) = sender(b"x", Config::default());
        t.handle_input(now, b"C");
        let first = drain(&mut t, now);
        t.handle_input(now, b"C");
        assert_eq!(drain(&mut t, now), first);
    }

    #[test]
    fn send_reply_before_write_is_ignored() {
        // Two start characters in one read: the second cannot answer block 1.
        let (mut t, now) = sender(b"x", Config::default());
        t.handle_input(now, b"CC");
        assert_eq!(drain(&mut t, now).len(), 1);
    }

    #[test]
    fn send_nak_resends_then_gives_up_with_cans() {
        let cfg = Config {
            retries: 2,
            ..Config::default()
        };
        let (mut t, now) = sender(b"abc", cfg);
        t.handle_input(now, &[NAK]);
        let b = drain(&mut t, now);
        t.handle_input(now, &[NAK]);
        assert_eq!(drain(&mut t, now), b);
        t.handle_input(now, &[NAK]);
        assert_eq!(drain(&mut t, now), b);
        t.handle_input(now, &[NAK]);
        assert_eq!(drain(&mut t, now), vec![CANCEL.to_vec()]);
        assert_eq!(events(&mut t).last(), Some(&Event::Error(Error::Timeout)));
    }

    #[test]
    fn send_timeout_resends() {
        let (mut t, now) = sender(b"abc", Config::default());
        t.handle_input(now, &[NAK]);
        let b = drain(&mut t, now);
        let due = t.next_timeout().unwrap();
        assert_eq!(due, now + Duration::from_secs(10));
        t.handle_timeout(due - Duration::from_millis(1));
        assert_eq!(t.poll_output(due), None, "nothing early");
        t.handle_timeout(due);
        assert_eq!(drain(&mut t, due), b);
    }

    #[test]
    fn send_start_timeout() {
        let (mut t, now) = sender(b"abc", Config::default());
        let due = t.next_timeout().unwrap();
        assert_eq!(due, now + Duration::from_secs(60));
        t.handle_timeout(due);
        assert_eq!(events(&mut t), vec![Event::Error(Error::Timeout)]);
        assert_eq!(t.poll_output(due), None);
    }

    #[test]
    fn send_remote_can() {
        let (mut t, now) = sender(b"abc", Config::default());
        t.handle_input(now, &[NAK]);
        drain(&mut t, now);
        // One CAN is noise; two in a row abort.
        t.handle_input(now, &[CAN, ACK]);
        assert_eq!(drain(&mut t, now), vec![vec![EOT]]);
        t.handle_input(now, &[CAN, CAN]);
        assert_eq!(
            events(&mut t).last(),
            Some(&Event::Error(Error::RemoteCancelled))
        );
        assert_eq!(t.poll_output(now), None);
    }

    #[test]
    fn send_eot_nak_resends_eot() {
        let (mut t, now) = sender(b"a", Config::default());
        t.handle_input(now, &[NAK]);
        drain(&mut t, now);
        t.handle_input(now, &[ACK]);
        assert_eq!(drain(&mut t, now), vec![vec![EOT]]);
        t.handle_input(now, &[NAK]);
        assert_eq!(drain(&mut t, now), vec![vec![EOT]]);
        t.handle_input(now, &[ACK]);
        assert_eq!(events(&mut t).last(), Some(&Event::Done));
    }

    #[test]
    fn send_empty_file_is_just_eot() {
        let (mut t, now) = sender(b"", Config::default());
        t.handle_input(now, &[NAK]);
        assert_eq!(drain(&mut t, now), vec![vec![EOT]]);
        t.handle_input(now, &[ACK]);
        assert_eq!(events(&mut t).last(), Some(&Event::Done));
    }

    #[test]
    fn send_1k_with_short_tail() {
        fn headers(len: usize) -> Vec<u8> {
            let cfg = Config {
                block_size: BlockSize::B1k,
                pad: 0,
                ..Config::default()
            };
            let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let (mut t, now) = sender(&data, cfg);
            t.handle_input(now, b"C");
            let mut out = Vec::new();
            loop {
                let block = drain(&mut t, now);
                if block == vec![vec![EOT]] {
                    return out;
                }
                out.push(block[0][0]);
                t.handle_input(now, &[ACK]);
            }
        }
        // 896 bytes (7 x 128) left: 128-byte blocks; 897 left: one 1k block.
        assert_eq!(headers(1024 + 896), [vec![STX], vec![SOH; 7]].concat());
        assert_eq!(headers(1024 + 897), vec![STX, STX]);
        assert_eq!(headers(100), vec![SOH]);
    }

    #[test]
    fn send_1k_without_short_tail() {
        let cfg = Config {
            block_size: BlockSize::B1k,
            short_tail: false,
            ..Config::default()
        };
        let (mut t, now) = sender(&[1; 300], cfg);
        t.handle_input(now, b"D");
        let out = drain(&mut t, now);
        assert_eq!(out[0].len(), 3 + 1024 + 2);
        assert_eq!(out[0][0], STX);
        assert_eq!(out[0][303], SUB);
    }

    #[test]
    fn send_1k_needs_a_crc_receiver() {
        // A receiver that opens with NAK (the 48GX) gets 128-byte blocks even
        // with 1k configured; `checksum_1k` forces 1k anyway.
        let cfg = Config {
            block_size: BlockSize::B1k,
            short_tail: false,
            ..Config::default()
        };
        let (mut t, now) = sender(&[1; 300], cfg.clone());
        t.handle_input(now, &[NAK]);
        let out = drain(&mut t, now);
        assert_eq!(out, vec![block(1, &[1; 128], Check::Checksum)]);

        let forced = Config {
            checksum_1k: true,
            ..cfg
        };
        let (mut t, now) = sender(&[1; 300], forced);
        t.handle_input(now, &[NAK]);
        let out = drain(&mut t, now);
        assert_eq!(out[0].len(), 3 + 1024 + 1);
        assert_eq!(out[0][0], STX);
    }

    #[test]
    fn send_block_number_wraps_to_zero() {
        let (mut t, now) = sender(&[0; 128 * 257], Config::default());
        t.handle_input(now, &[NAK]);
        let mut nums = Vec::new();
        for _ in 0..257 {
            nums.push(drain(&mut t, now)[0][1]);
            t.handle_input(now, &[ACK]);
        }
        assert_eq!(&nums[254..], &[255, 0, 1]);
    }

    #[test]
    fn cancel_sends_cans() {
        let (mut t, now) = sender(b"a", Config::default());
        t.cancel(now);
        assert_eq!(drain(&mut t, now), vec![CANCEL.to_vec()]);
        assert_eq!(events(&mut t), vec![Event::Error(Error::Cancelled)]);
        assert!(t.start(now, Command::Receive).is_ok());
        assert_eq!(t.start(now, Command::Receive), Err(StartError::Busy));
    }

    // ---- receiver ----

    #[test]
    fn receive_crc() {
        let (mut t, now) = receiver(crc());
        assert_eq!(drain(&mut t, now), vec![b"C".to_vec()]);
        t.handle_input(now, &block(1, b"hi", Check::Crc16));
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
        t.handle_input(now, &[EOT]);
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
        let ev = events(&mut t);
        assert_eq!(
            ev[0],
            Event::Started {
                check: Check::Crc16
            }
        );
        let Event::FileEnd {
            data,
            last_block,
            padding,
        } = &ev[2]
        else {
            panic!("{ev:?}")
        };
        assert_eq!(data.len(), 128);
        assert!(data.starts_with(b"hi"));
        assert_eq!((*last_block, *padding), (128, 126));
        assert_eq!(ev[3], Event::Done);
    }

    #[test]
    fn receive_falls_back_to_checksum() {
        let (mut t, now) = receiver(crc());
        let mut at = now;
        let mut starts = Vec::new();
        for _ in 0..4 {
            starts.extend(drain(&mut t, at));
            at = t.next_timeout().unwrap();
            t.handle_timeout(at);
        }
        assert_eq!(
            starts,
            vec![b"C".to_vec(), b"C".to_vec(), b"C".to_vec(), vec![NAK]]
        );
        // Three C at 3 s, then the NAK waits the full 10 s.
        assert_eq!(at, now + Duration::from_secs(19));
        drain(&mut t, at);
        t.handle_input(at, &block(1, b"48", Check::Checksum));
        assert_eq!(drain(&mut t, at), vec![vec![ACK]]);
        assert_eq!(
            events(&mut t)[0],
            Event::Started {
                check: Check::Checksum
            }
        );
    }

    #[test]
    fn receive_checksum_from_start() {
        let cfg = Config {
            check: Check::Checksum,
            ..Config::default()
        };
        let (mut t, now) = receiver(cfg);
        assert_eq!(drain(&mut t, now), vec![vec![NAK]]);
    }

    #[test]
    fn receive_start_gives_up() {
        let cfg = Config {
            retries: 1,
            crc_attempts: 1,
            ..crc()
        };
        let (mut t, mut at) = receiver(cfg);
        let mut out = Vec::new();
        for _ in 0..2 {
            out.extend(drain(&mut t, at));
            at = t.next_timeout().unwrap();
            t.handle_timeout(at);
        }
        out.extend(drain(&mut t, at));
        assert_eq!(out, vec![b"C".to_vec(), vec![NAK], CANCEL.to_vec()]);
        assert_eq!(events(&mut t), vec![Event::Error(Error::Timeout)]);
    }

    #[test]
    fn receive_start_window_outlasts_the_count() {
        // With `recv_start_timeout` the receiver keeps asking until the window
        // closes, however many start characters that takes.
        let cfg = Config {
            retries: 1,
            crc_attempts: 2,
            crc_interval: Duration::from_secs(1),
            timeout: Duration::from_secs(1),
            recv_start_timeout: Some(Duration::from_secs(5)),
            ..Config::default()
        };
        let (mut t, start) = receiver(cfg);
        let mut at = start;
        let mut out = Vec::new();
        while !t.is_idle() {
            out.extend(drain(&mut t, at));
            at = t.next_timeout().unwrap();
            t.handle_timeout(at);
        }
        out.extend(drain(&mut t, at));
        assert_eq!(at.duration_since(start), Duration::from_secs(5));
        let mut want = vec![b"D".to_vec(), b"D".to_vec()];
        want.extend(vec![vec![NAK]; 3]);
        want.push(CANCEL.to_vec());
        assert_eq!(out, want);
        assert_eq!(events(&mut t), vec![Event::Error(Error::Timeout)]);
    }

    #[test]
    fn receive_skips_kermit_packet_before_first_block() {
        let (mut t, now) = receiver(crc());
        drain(&mut t, now);
        t.handle_input(now, b"\x01# N3\r");
        t.handle_input(now, &block(1, b"ok", Check::Crc16));
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
    }

    #[test]
    fn receive_duplicate_block_acked_and_dropped() {
        let (mut t, now) = receiver(crc());
        drain(&mut t, now);
        t.handle_input(now, &block(1, b"one", Check::Crc16));
        t.handle_input(now, &block(1, b"one", Check::Crc16));
        t.handle_input(now, &block(2, b"two", Check::Crc16));
        assert_eq!(drain(&mut t, now), vec![vec![ACK]; 3]);
        t.handle_input(now, &[EOT]);
        let ev = events(&mut t);
        let data = ev
            .iter()
            .find_map(|e| match e {
                Event::FileEnd { data, .. } => Some(data.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(data.len(), 256);
        assert!(data[128..].starts_with(b"two"));
    }

    #[test]
    fn receive_out_of_sequence_cancels() {
        let (mut t, now) = receiver(crc());
        drain(&mut t, now);
        t.handle_input(now, &block(1, b"one", Check::Crc16));
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
        t.handle_input(now, &block(3, b"three", Check::Crc16));
        assert_eq!(drain(&mut t, now), vec![CANCEL.to_vec()]);
        assert!(matches!(
            events(&mut t).last(),
            Some(Event::Error(Error::Protocol(_)))
        ));
    }

    #[test]
    fn receive_bad_check_purges_then_naks() {
        let (mut t, now) = receiver(crc());
        drain(&mut t, now);
        t.handle_input(now, &block(1, b"one", Check::Crc16));
        drain(&mut t, now);
        let mut bad = block(2, b"two", Check::Crc16);
        bad[20] ^= 0xFF;
        t.handle_input(now, &bad);
        assert_eq!(t.poll_output(now), None, "no NAK while the line is busy");
        // More garbage extends the quiet period.
        let later = now + Duration::from_millis(500);
        t.handle_input(later, b"xyz");
        assert_eq!(t.next_timeout(), Some(later + Duration::from_secs(1)));
        t.handle_timeout(later + Duration::from_secs(1));
        assert_eq!(drain(&mut t, later), vec![vec![NAK]]);
        let at = later + Duration::from_secs(1);
        t.handle_input(at, &block(2, b"two", Check::Crc16));
        assert_eq!(drain(&mut t, at), vec![vec![ACK]]);
    }

    #[test]
    fn receive_split_block_and_byte_timeout() {
        let (mut t, now) = receiver(crc());
        drain(&mut t, now);
        let b = block(1, b"split", Check::Crc16);
        t.handle_input(now, &b[..50]);
        t.handle_input(now, &b[50..]);
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
        // A partial block that stalls is NAKed after the byte timeout.
        let b2 = block(2, b"stall", Check::Crc16);
        t.handle_input(now, &b2[..10]);
        assert_eq!(t.next_timeout(), Some(now + Duration::from_secs(1)));
        t.handle_timeout(now + Duration::from_secs(1));
        assert_eq!(drain(&mut t, now), vec![vec![NAK]]);
    }

    #[test]
    fn receive_block_timeout_naks_and_gives_up() {
        let cfg = Config {
            retries: 1,
            ..crc()
        };
        let (mut t, now) = receiver(cfg);
        drain(&mut t, now);
        t.handle_input(now, &block(1, b"a", Check::Crc16));
        drain(&mut t, now);
        let at = now + Duration::from_secs(10);
        assert_eq!(t.next_timeout(), Some(at));
        t.handle_timeout(at);
        assert_eq!(drain(&mut t, at), vec![vec![NAK]]);
        t.handle_timeout(at + Duration::from_secs(10));
        assert_eq!(drain(&mut t, at), vec![CANCEL.to_vec()]);
        assert_eq!(events(&mut t).last(), Some(&Event::Error(Error::Timeout)));
    }

    #[test]
    fn receive_remote_can() {
        let (mut t, now) = receiver(crc());
        drain(&mut t, now);
        t.handle_input(now, &block(1, b"a", Check::Crc16));
        drain(&mut t, now);
        t.handle_input(now, &[CAN, CAN]);
        assert_eq!(
            events(&mut t).last(),
            Some(&Event::Error(Error::RemoteCancelled))
        );
        assert_eq!(t.poll_output(now), None);
    }

    #[test]
    fn receive_1k_and_mixed_blocks() {
        let (mut t, now) = receiver(crc());
        drain(&mut t, now);
        t.handle_input(
            now,
            &encode_block(1, BlockSize::B1k, &[9; 1024], 0, Check::Crc16).unwrap(),
        );
        t.handle_input(now, &block(2, &[0; 3], Check::Crc16));
        assert_eq!(drain(&mut t, now), vec![vec![ACK]; 2]);
        t.handle_input(now, &[EOT]);
        let ev = events(&mut t);
        let Some(Event::FileEnd {
            data,
            last_block,
            padding,
        }) = ev.iter().find(|e| matches!(e, Event::FileEnd { .. }))
        else {
            panic!("{ev:?}")
        };
        assert_eq!(data.len(), 1152);
        // SUB padding of the 128-byte tail block.
        assert_eq!((*last_block, *padding), (128, 125));
    }

    #[test]
    fn receive_empty_transfer_eot_first_is_ignored_before_start() {
        // EOT before any block is noise while still starting.
        let (mut t, now) = receiver(crc());
        drain(&mut t, now);
        t.handle_input(now, &[EOT]);
        assert_eq!(t.poll_output(now), None);
        assert!(!t.is_idle());
    }

    /// A CRC receiver that has accepted block 1 ("one").
    fn receiving(config: Config) -> (Transfer, Instant) {
        let (mut t, now) = receiver(config);
        drain(&mut t, now);
        t.handle_input(now, &block(1, b"one", Check::Crc16));
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
        (t, now)
    }

    #[test]
    fn lost_final_ack_is_answered_while_lingering() {
        let cfg = crc();
        let linger = cfg.linger;
        let (mut t, now) = receiving(cfg);
        t.handle_input(now, &[EOT]);
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
        let ev = events(&mut t);
        assert!(matches!(
            ev.as_slice(),
            [.., Event::FileEnd { .. }, Event::Done]
        ));
        assert!(!t.is_idle());
        assert_eq!(t.next_timeout(), Some(now + linger));
        // The sender did not get our ACK and repeats EOT: ACK again, no
        // second FileEnd or Done. Other bytes are ignored.
        let at = now + linger / 2;
        t.handle_input(at, &[EOT]);
        assert_eq!(drain(&mut t, at), vec![vec![ACK]]);
        t.handle_input(at, &block(2, b"x", Check::Crc16));
        assert_eq!(drain(&mut t, at), Vec::<Vec<u8>>::new());
        assert_eq!(events(&mut t), vec![]);
        // The linger is bounded: the re-ACK does not extend it.
        assert_eq!(t.next_timeout(), Some(now + linger));
        t.handle_timeout(now + linger);
        assert!(t.is_idle());
        assert_eq!(t.next_timeout(), None);
        t.handle_input(now + linger, &[EOT]);
        assert_eq!(t.poll_output(now + linger), None);
        assert_eq!(events(&mut t), vec![]);
    }

    #[test]
    fn start_and_cancel_end_the_linger() {
        let (mut t, now) = receiving(crc());
        t.handle_input(now, &[EOT]);
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
        events(&mut t);
        t.cancel(now);
        assert!(t.is_idle());
        assert_eq!(t.poll_output(now), None, "no CANs after Done");
        assert_eq!(events(&mut t), vec![]);

        let (mut t, now) = receiving(crc());
        t.handle_input(now, &[EOT]);
        t.start(now, Command::Receive).unwrap();
        assert_eq!(drain(&mut t, now), vec![b"C".to_vec()]);
        assert_eq!(events(&mut t), vec![], "the old events are dropped");
    }

    #[test]
    fn zero_linger_goes_idle_at_once() {
        let cfg = Config {
            linger: Duration::ZERO,
            ..crc()
        };
        let (mut t, now) = receiving(cfg);
        t.handle_input(now, &[EOT]);
        assert!(t.is_idle());
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
        assert_eq!(t.next_timeout(), None);
    }

    #[test]
    fn block_straddling_the_start_interval_is_kept() {
        // The last `D` before the switch to checksum goes out at 0; a 1k
        // HP-CRC block starts at 2.5 s and ends at 3.9 s, past the 3 s
        // interval. The byte timeout governs the block, so the check stays.
        let cfg = Config {
            crc_attempts: 1,
            ..Config::default()
        };
        let interval = cfg.crc_interval;
        let (mut t, now) = receiver(cfg);
        assert_eq!(drain(&mut t, now), vec![b"D".to_vec()]);
        assert_eq!(t.next_timeout(), Some(now + interval));
        let b = encode_block(1, BlockSize::B1k, b"hp", 0, Check::HpCrc).unwrap();
        let ms = |n| now + Duration::from_millis(n);
        t.handle_input(ms(2500), &b[..400]);
        assert_eq!(t.next_timeout(), Some(ms(3500)));
        t.handle_timeout(ms(3000));
        assert_eq!(t.poll_output(ms(3000)), None);
        t.handle_input(ms(3200), &b[400..800]);
        t.handle_timeout(ms(3500));
        assert_eq!(t.poll_output(ms(3500)), None);
        t.handle_input(ms(3900), &b[800..]);
        assert_eq!(drain(&mut t, ms(3900)), vec![vec![ACK]]);
        assert_eq!(
            events(&mut t)[0],
            Event::Started {
                check: Check::HpCrc
            }
        );
    }

    #[test]
    fn stalled_first_block_resumes_the_start_interval() {
        let cfg = crc();
        let (mut t, now) = receiver(cfg);
        assert_eq!(drain(&mut t, now), vec![b"C".to_vec()]);
        let ms = |n| now + Duration::from_millis(n);
        // A fragment at 0.5 s, then silence: dropped after the byte timeout
        // (1.5 s); the interval running since 0 still ends at 3 s.
        t.handle_input(ms(500), &block(1, b"x", Check::Crc16)[..20]);
        assert_eq!(t.next_timeout(), Some(ms(1500)));
        t.handle_timeout(ms(1500));
        assert_eq!(t.poll_output(ms(1500)), None);
        assert_eq!(t.next_timeout(), Some(ms(3000)));
        t.handle_timeout(ms(3000));
        assert_eq!(drain(&mut t, ms(3000)), vec![b"C".to_vec()]);
        // A fragment that stalls past the interval: the next start character
        // goes out when it is dropped.
        t.handle_input(ms(5500), &block(1, b"x", Check::Crc16)[..20]);
        t.handle_timeout(ms(6500));
        assert_eq!(drain(&mut t, ms(6500)), vec![b"C".to_vec()]);
        // A Kermit packet (not a block header) restores the interval too.
        t.handle_input(ms(7000), b"\x01# N3\r");
        assert_eq!(t.next_timeout(), Some(ms(9500)));
    }

    #[test]
    fn send_start_character_reselects_the_check() {
        // The receiver's first `D` gets a 1k HP-CRC block; before any ACK it
        // falls back to NAK (checksum): block 1 again, as a 128-byte checksum
        // block. A later `C` switches to CRC-16 with 1k blocks again.
        let cfg = Config {
            block_size: BlockSize::B1k,
            short_tail: false,
            ..Config::default()
        };
        let data = vec![7u8; 2000];
        let (mut t, now) = sender(&data, cfg);
        t.handle_input(now, b"D");
        let out = drain(&mut t, now);
        assert_eq!(out[0].len(), 3 + 1024 + 2);
        t.handle_input(now, &[NAK]);
        assert_eq!(
            drain(&mut t, now),
            vec![block(1, &data[..128], Check::Checksum)]
        );
        t.handle_input(now, b"C");
        assert_eq!(
            drain(&mut t, now),
            vec![encode_block(1, BlockSize::B1k, &data[..1024], SUB, Check::Crc16).unwrap()]
        );
        // The same character again only resends.
        t.handle_input(now, b"C");
        assert_eq!(drain(&mut t, now)[0].len(), 3 + 1024 + 2);
        assert_eq!(
            events(&mut t),
            vec![
                Event::Started {
                    check: Check::HpCrc
                },
                Event::Started {
                    check: Check::Checksum
                },
                Event::Started {
                    check: Check::Crc16
                },
            ]
        );
        // After the first ACK, NAK means resend and `C`/`D` are ignored.
        t.handle_input(now, &[ACK]);
        let second = drain(&mut t, now);
        t.handle_input(now, b"D");
        assert_eq!(drain(&mut t, now), Vec::<Vec<u8>>::new());
        t.handle_input(now, &[NAK]);
        assert_eq!(drain(&mut t, now), second);
    }

    #[test]
    fn send_nak_in_crc16_mode_resends_block_1_in_crc16() {
        // A standard CRC-16 receiver NAKs a damaged block 1 and stays in CRC
        // mode: the same CRC block again, no switch to checksum.
        let (mut t, now) = sender(b"x", Config::default());
        t.handle_input(now, b"C");
        let first = drain(&mut t, now);
        assert_eq!(first, vec![block(1, b"x", Check::Crc16)]);
        t.handle_input(now, &[NAK]);
        assert_eq!(drain(&mut t, now), first);
        t.handle_input(now, &[ACK]);
        assert_eq!(drain(&mut t, now), vec![vec![EOT]]);
        assert_eq!(
            events(&mut t)[..1],
            [Event::Started {
                check: Check::Crc16
            }]
        );
        assert_eq!(t.check(), Check::Crc16);
        // A checksum receiver's NAK simply resends too.
        let (mut t, now) = sender(b"x", Config::default());
        t.handle_input(now, &[NAK]);
        let first = drain(&mut t, now);
        t.handle_input(now, &[NAK]);
        assert_eq!(drain(&mut t, now), first);
    }

    #[test]
    fn repeated_eots_while_lingering_queue_one_ack() {
        let (mut t, now) = receiving(crc());
        t.handle_input(now, &[EOT]);
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
        t.handle_input(now, &[EOT; 10]);
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
        t.handle_input(now, &[EOT, EOT]);
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
    }

    #[test]
    fn send_start_characters_count_as_retries() {
        let cfg = Config {
            retries: 1,
            ..Config::default()
        };
        let (mut t, now) = sender(b"x", cfg);
        t.handle_input(now, b"D");
        drain(&mut t, now);
        t.handle_input(now, &[NAK]);
        assert_eq!(drain(&mut t, now), vec![block(1, b"x", Check::Checksum)]);
        t.handle_input(now, &[NAK]);
        assert_eq!(drain(&mut t, now), vec![CANCEL.to_vec()]);
        assert_eq!(events(&mut t).last(), Some(&Event::Error(Error::Timeout)));
    }

    #[test]
    fn receive_size_cap() {
        let cfg = Config {
            max_size: Some(256),
            ..crc()
        };
        let (mut t, now) = receiving(cfg);
        t.handle_input(now, &block(2, b"two", Check::Crc16));
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
        // A duplicate is not counted.
        t.handle_input(now, &block(2, b"two", Check::Crc16));
        assert_eq!(drain(&mut t, now), vec![vec![ACK]]);
        t.handle_input(now, &block(3, b"three", Check::Crc16));
        assert_eq!(drain(&mut t, now), vec![CANCEL.to_vec()]);
        assert_eq!(
            events(&mut t).last(),
            Some(&Event::Error(Error::TooLarge { limit: 256 }))
        );
        assert!(t.is_idle());
        assert_eq!(Config::default().max_size, Some(4 << 20));
    }

    #[test]
    fn crc_bytes_are_high_first_on_the_wire() {
        let data = [0x31u8; 128];
        let b = block(1, &data, Check::Crc16);
        let crc = crc16(&data);
        assert_eq!(b[131], (crc >> 8) as u8);
        assert_eq!(b[132], (crc & 0xFF) as u8);
    }
}
