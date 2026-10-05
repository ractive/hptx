//! Drives an [`xmodem_proto::Transfer`] over a [`Transport`]: XModem
//! transfers with the calculator's `XRECV` and `XSEND`.
//!
//! The calculator side cannot be started from the host: inside the Kermit
//! server both commands fail with "Port Not Available" (49G and 48GX,
//! verified on the emulators; traces `49g-server-xrecv`, `48gx-server-xsend`
//! in `xmodem-proto`). The flow is therefore:
//!
//! 1. [`Calculator::prepare_for_xmodem`](crate::Calculator::prepare_for_xmodem)
//!    checks the name, sets RPN mode on a 49G, ends the server (Kermit
//!    FINISH) and returns an [`XmodemPlan`] with what the user must type
//!    (`'NAME' XRECV` or `'NAME' XSEND`).
//! 2. [`Calculator::into_transport`](crate::Calculator::into_transport) hands
//!    over the open link; [`XmodemSession::new`] wraps it.
//! 3. [`XmodemSession::send`] or [`XmodemSession::receive`] waits up to
//!    [`XmodemOptions::start_timeout`] for the user to start the command,
//!    then runs the transfer.
//! 4. The calculator is out of server mode with an empty stack: the user
//!    types `SERVER` again, and [`XmodemSession::into_transport`] gives the
//!    link back for a new [`Session`](crate::Session).
//!
//! The loop has the same discipline as [`Session`](crate::Session): each
//! block or control byte goes out in one write, reads wait at most until the
//! machine's next deadline, `handle_timeout` runs after every read, and a
//! watchdog cancels a transfer that stops making progress (a line full of
//! noise never lets the machine's own timeouts fire).
//!
//! wiki: protocols/xmodem, protocols/xmodem-hp.

use std::time::{Duration, Instant};

use xmodem_proto::{BlockSize, Check, Command, Config, Event, Transfer};

use crate::calc::Model;
use crate::object::{HEADER_LEN, inspect, strip_padding};
use crate::transport::{self, Transport};
use crate::{Error, Result};

/// Read wait while the machine has no deadline.
const IDLE_WAIT: Duration = Duration::from_millis(100);
/// Shortest read wait.
const MIN_WAIT: Duration = Duration::from_millis(10);
/// Longest accepted [`XmodemOptions::start_timeout`].
pub const MAX_START_TIMEOUT: Duration = Duration::from_secs(600);
/// Shortest accepted [`XmodemOptions::start_timeout`].
pub const MIN_START_TIMEOUT: Duration = Duration::from_secs(1);

/// Tunables for an [`XmodemSession`].
#[derive(Clone, Debug)]
pub struct XmodemOptions {
    /// The XModem machine's configuration: block size (sender), check asked
    /// for first (receiver), timeouts, retries, pad byte. Its
    /// `recv_start_timeout` is replaced by [`XmodemOptions::start_timeout`].
    pub xmodem: Config,
    /// How long to wait for the user to start `XRECV`/`XSEND` on the
    /// calculator (default 60 s), clamped to
    /// [`MIN_START_TIMEOUT`]..=[`MAX_START_TIMEOUT`]. Sender: wait for the
    /// first start character. Receiver: keep sending start characters this
    /// long.
    pub start_timeout: Duration,
    /// How long to discard input before the transfer (default 0: the machine
    /// skips Kermit packets and noise before the first start character or
    /// block, and draining could swallow a start character the calculator
    /// already sent).
    pub drain: Duration,
}

impl Default for XmodemOptions {
    /// Model-independent defaults: ask for HP's CRC (`D`) three times, then
    /// checksum; send 1k blocks when the receiver asks for a CRC, 128-byte
    /// blocks when it asks for checksum.
    fn default() -> Self {
        XmodemOptions {
            xmodem: Config {
                block_size: BlockSize::B1k,
                ..Config::default()
            },
            start_timeout: Duration::from_secs(60),
            drain: Duration::ZERO,
        }
    }
}

impl XmodemOptions {
    /// Defaults for `model`. Errors for the 48S/SX, which has no XModem.
    ///
    /// - 49G: ask with `D` (HP's CRC) for the whole start window, so a user
    ///   who takes longer than a few seconds to type `XSEND` still gets CRC
    ///   mode; 1k blocks with the 128-byte short tail (verified on the
    ///   emulated 49G).
    /// - 48G/GX: checksum only, 128-byte blocks; it ignores `C` and `D` and
    ///   rejects 1k blocks (verified on the emulated 48GX). A 3 s reply
    ///   timeout, which also paces the receiver's start NAKs.
    /// - Unknown: [`XmodemOptions::default`].
    pub fn for_model(model: Model) -> Result<Self> {
        let base = XmodemOptions::default();
        match model {
            Model::Hp48Sx => Err(Error::Unsupported(
                "the HP 48S/SX has no XModem; use Kermit".into(),
            )),
            Model::Hp49G => Ok(XmodemOptions {
                xmodem: Config {
                    check: Check::HpCrc,
                    // Bounded by the start window, not by the count.
                    crc_attempts: u32::MAX,
                    block_size: BlockSize::B1k,
                    ..base.xmodem
                },
                ..base
            }),
            Model::Hp48Gx => Ok(XmodemOptions {
                xmodem: Config {
                    check: Check::Checksum,
                    block_size: BlockSize::B128,
                    // Also the pace of the start NAKs: with 10 s the user
                    // waits up to 10 s after typing XSEND. Conn4x uses 2 s
                    // for data (wiki: protocols/xmodem-hp).
                    timeout: Duration::from_secs(3),
                    ..base.xmodem
                },
                ..base
            }),
            Model::Unknown => Ok(base),
        }
    }

    /// The machine configuration actually used: the start window applied to
    /// both roles.
    fn config(&self) -> Config {
        let start = self
            .start_timeout
            .clamp(MIN_START_TIMEOUT, MAX_START_TIMEOUT);
        Config {
            start_timeout: start,
            recv_start_timeout: Some(start),
            ..self.xmodem.clone()
        }
    }
}

/// Outcome of [`XmodemSession::send`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmodemReport {
    /// The block check the receiver asked for.
    pub check: Check,
    /// Bytes of the file sent and acknowledged.
    pub bytes: u64,
}

/// Outcome of [`XmodemSession::receive`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmodemReceived {
    /// The file: padding of the last block cut when the object walk
    /// succeeded, otherwise every byte received.
    pub data: Vec<u8>,
    /// The block check the transfer ran with.
    pub check: Check,
    /// Bytes received, padding included.
    pub received: usize,
    /// Size of the last block (128 or 1024; 0 for an empty transfer), the
    /// padding allowance passed to [`strip_padding`] is this minus one, the
    /// most padding a sender can add.
    pub last_block: usize,
    /// Padding bytes cut. `None` when the object-length walk failed (not an
    /// HP binary object, unknown prolog, or more excess than one block):
    /// `data` is then exactly what was received, padding and all.
    pub stripped: Option<usize>,
}

/// An XModem link to a calculator that runs (or is about to run) `XRECV` or
/// `XSEND`. See the module documentation for the flow.
pub struct XmodemSession {
    transport: Box<dyn Transport>,
    options: XmodemOptions,
}

/// What [`XmodemSession::run`] collected.
struct Outcome {
    check: Option<Check>,
    bytes: u64,
    file: Option<(Vec<u8>, usize)>,
}

impl XmodemSession {
    /// Wrap an open `transport` (e.g. from
    /// [`Calculator::into_transport`](crate::Calculator::into_transport)).
    pub fn new(transport: Box<dyn Transport>, options: XmodemOptions) -> Self {
        XmodemSession { transport, options }
    }

    /// The options in use.
    pub fn options(&self) -> &XmodemOptions {
        &self.options
    }

    /// Give the link back, e.g. for a new [`Session`](crate::Session) once
    /// the user has typed `SERVER` again.
    pub fn into_transport(self) -> Box<dyn Transport> {
        self.transport
    }

    /// Send `data` to a calculator running `XRECV`.
    pub fn send(&mut self, data: &[u8]) -> Result<XmodemReport> {
        self.send_with(data, &mut |_| {})
    }

    /// [`XmodemSession::send`], passing every event to `progress` first.
    pub fn send_with(
        &mut self,
        data: &[u8],
        progress: &mut dyn FnMut(&Event),
    ) -> Result<XmodemReport> {
        let outcome = self.run(Command::Send(data.to_vec()), progress)?;
        Ok(XmodemReport {
            check: outcome.check.unwrap_or(self.options.xmodem.check),
            bytes: outcome.bytes,
        })
    }

    /// Receive a file from a calculator running `XSEND` and cut the padding
    /// of its last block with the object-length walk (see
    /// [`XmodemReceived::stripped`]).
    pub fn receive(&mut self) -> Result<XmodemReceived> {
        self.receive_with(&mut |_| {})
    }

    /// [`XmodemSession::receive`], passing every event to `progress` first.
    pub fn receive_with(&mut self, progress: &mut dyn FnMut(&Event)) -> Result<XmodemReceived> {
        let outcome = self.run(Command::Receive, progress)?;
        let (raw, last_block) = outcome
            .file
            .ok_or_else(|| Error::Reply("XModem transfer ended without a file".into()))?;
        // The object's length per the walk; the cut succeeded iff the result
        // has exactly that length.
        let object_len = inspect(&raw)
            .ok()
            .and_then(|i| i.size_nibbles)
            .map(|n| HEADER_LEN + n.div_ceil(2));
        // Padding never fills a whole block (a sender does not send a block
        // of pure padding), so a cut may remove at most last_block - 1
        // bytes; a walk that comes out a full block short leaves the data
        // untouched instead of losing real bytes.
        let data = strip_padding(&raw, last_block.saturating_sub(1)).to_vec();
        let stripped = match object_len {
            Some(len) if data.len() == len => Some(raw.len() - len),
            _ => None,
        };
        Ok(XmodemReceived {
            check: outcome.check.unwrap_or(self.options.xmodem.check),
            received: raw.len(),
            last_block,
            stripped,
            data,
        })
    }

    fn run(&mut self, command: Command, progress: &mut dyn FnMut(&Event)) -> Result<Outcome> {
        transport::drain(self.transport.as_mut(), self.options.drain)?;
        let config = self.options.config();
        // Watchdog: the start window (plus one reply timeout for the last
        // start character), then the time the machine needs to give up on
        // one block. Reset on every sign of progress.
        let start_limit = config.start_timeout.saturating_add(config.timeout);
        let stall_limit = config
            .timeout
            .saturating_mul(config.retries.saturating_add(2))
            .saturating_add(config.purge)
            .saturating_add(config.byte_timeout);
        let mut xfer = Transfer::new(config);
        let begin = Instant::now();
        xfer.start(begin, command)
            .map_err(|e| Error::Reply(format!("XModem: {e}")))?;
        let mut watchdog = begin.checked_add(start_limit);
        let mut stalled = false;

        let mut out = Outcome {
            check: None,
            bytes: 0,
            file: None,
        };
        let mut result: Option<Result<()>> = None;
        let mut buf = [0u8; 2048];
        loop {
            let now = Instant::now();
            while let Some(bytes) = xfer.poll_output(now) {
                self.transport.write_packet(&bytes)?;
            }
            while let Some(event) = xfer.poll_event() {
                progress(&event);
                match event {
                    Event::Started { check } => {
                        out.check = Some(check);
                        watchdog = now.checked_add(stall_limit);
                    }
                    Event::Progress { bytes, .. } => {
                        out.bytes = bytes;
                        watchdog = now.checked_add(stall_limit);
                    }
                    Event::FileEnd {
                        data, last_block, ..
                    } => out.file = Some((data, last_block)),
                    Event::Done => result = Some(Ok(())),
                    Event::Error(xmodem_proto::Error::Cancelled) if stalled => {
                        result = Some(Err(Error::Xmodem(xmodem_proto::Error::Timeout)));
                    }
                    Event::Error(e) => result = Some(Err(Error::Xmodem(e))),
                }
            }
            if result.is_some() && xfer.next_timeout().is_none() {
                // Done or failed, and the final ACK or CANs are written.
                if xfer.poll_output(now).is_none() {
                    break;
                }
                continue;
            }
            if result.is_none() && watchdog.is_some_and(|w| now >= w) {
                stalled = true;
                xfer.cancel(now);
                continue;
            }
            let until = match (xfer.next_timeout(), watchdog) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let wait = until.map_or(IDLE_WAIT, |t| t.saturating_duration_since(now));
            let n = self.transport.read(&mut buf, wait.max(MIN_WAIT))?;
            let now = Instant::now();
            if n > 0 {
                xfer.handle_input(now, buf.get(..n).unwrap_or_default());
            }
            // Also after input: the machine checks deadlines only here.
            xfer.handle_timeout(now);
        }
        match result {
            Some(Err(e)) => Err(e),
            _ => Ok(out),
        }
    }
}

/// Which way an XModem transfer goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XmodemDirection {
    /// Computer to calculator: the calculator runs `XRECV`.
    ToCalculator,
    /// Calculator to computer: the calculator runs `XSEND`.
    FromCalculator,
}

impl XmodemDirection {
    /// The calculator command for this direction.
    pub fn calculator_command(self) -> &'static str {
        match self {
            XmodemDirection::ToCalculator => "XRECV",
            XmodemDirection::FromCalculator => "XSEND",
        }
    }
}

/// What [`Calculator::prepare_for_xmodem`](crate::Calculator::prepare_for_xmodem)
/// did and what the user must do next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmodemPlan {
    /// The detected model.
    pub model: Model,
    /// Which way the transfer goes.
    pub direction: XmodemDirection,
    /// The variable name.
    pub name: String,
    /// What to type on the calculator, e.g. `'NAME' XRECV`, then ENTER.
    pub keys: String,
    /// The calculator was in algebraic mode (49G flag -95 set) and was
    /// switched to RPN so that `keys` work; `-95 SF` switches back.
    pub switched_to_rpn: bool,
    /// The variable already exists (relevant for `XRECV`, see
    /// [`XmodemPlan::notes`]).
    pub exists: bool,
    /// Further instructions for the user, one sentence each: the `.1`
    /// suffix for an existing name, the RPN switch, `SERVER` afterwards.
    pub notes: Vec<String>,
}

impl XmodemPlan {
    /// The full instruction text: the keys first, then the notes.
    pub fn instructions(&self) -> String {
        let mut text = format!("On the calculator, type {} and press ENTER.", self.keys);
        for note in &self.notes {
            text.push(' ');
            text.push_str(note);
        }
        text
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::object;
    use crate::transport::MemoryTransport;
    use std::collections::VecDeque;
    use std::io;
    use std::sync::{Arc, Mutex};
    use xmodem_proto::codec::{ACK, CAN, EOT, NAK, SOH, STX, decode_block, encode_block};

    const G49_XSEND_HPCRC: &str = include_str!("../../xmodem-proto/traces/49g-xsend-hpcrc.trace");
    const G49_XSEND: &str = include_str!("../../xmodem-proto/traces/49g-xsend.trace");
    const GX_XSEND: &str = include_str!("../../xmodem-proto/traces/48gx-xsend.trace");
    const G49_XRECV: &str = include_str!("../../xmodem-proto/traces/49g-xrecv.trace");
    const GX_XRECV: &str = include_str!("../../xmodem-proto/traces/48gx-xrecv.trace");

    const H49: &[u8; 8] = b"HPHP49-C";
    const H48: &[u8; 8] = b"HPHP48-R";

    /// A String object file: `header`, prolog #02A2C, 5-nibble length,
    /// characters (as the traces were recorded).
    fn hp_string(header: &[u8; 8], chars: &[u8]) -> Vec<u8> {
        let len = 5 + 2 * chars.len();
        let mut nibbles = vec![0xC, 0x2, 0xA, 0x2, 0x0];
        nibbles.extend((0..5).map(|i| ((len >> (4 * i)) & 0xF) as u8));
        let mut out = header.to_vec();
        out.extend(object::pack(&nibbles));
        out.extend_from_slice(chars);
        out
    }

    /// 269 bytes: every byte value once.
    fn all_bytes(header: &[u8; 8]) -> Vec<u8> {
        hp_string(header, &(0..=255).collect::<Vec<u8>>())
    }

    /// Fast timeouts for in-memory peers.
    fn fast(mut options: XmodemOptions) -> XmodemOptions {
        options.xmodem.crc_interval = Duration::from_millis(30);
        options.xmodem.timeout = Duration::from_millis(60);
        options.xmodem.byte_timeout = Duration::from_millis(30);
        options.xmodem.purge = Duration::from_millis(20);
        options.start_timeout = MIN_START_TIMEOUT;
        options.drain = Duration::ZERO;
        options
    }

    /// The part of a trace the XModem machine handles.
    fn xmodem_part(trace: &str) -> &str {
        let at = trace.find("# xmodem start").unwrap();
        &trace[at..]
    }

    // ---- recorded payloads ----

    #[test]
    fn receive_49g_payload_strips_garbage_padding() {
        // The 49G's XSEND pads the last block with whatever follows the
        // object in memory; only the object walk finds the end.
        let transport = MemoryTransport::from_trace(xmodem_part(G49_XSEND_HPCRC)).unwrap();
        let options = fast(XmodemOptions::for_model(Model::Hp49G).unwrap());
        let mut s = XmodemSession::new(Box::new(transport), options);
        let got = s.receive().unwrap();
        assert_eq!(got.data, all_bytes(H49));
        assert_eq!(got.check, Check::HpCrc);
        assert_eq!((got.received, got.last_block), (384, 128));
        assert_eq!(got.stripped, Some(384 - 269));
    }

    #[test]
    fn allowance_is_one_less_than_a_block() {
        // A walk that ends exactly one block early must not cut a whole
        // block of real data; one byte less is the most padding possible.
        let object = all_bytes(H49);
        let mut raw = object.clone();
        raw.extend(std::iter::repeat_n(0u8, 128));
        assert_eq!(strip_padding(&raw, 127).len(), raw.len());
        raw.pop();
        assert_eq!(strip_padding(&raw, 127), &object[..]);
    }

    #[test]
    fn receive_49g_checksum_payload() {
        // Recorded asking with `C` three times; the 49G answered the NAK.
        let transport = MemoryTransport::from_trace(xmodem_part(G49_XSEND)).unwrap();
        let mut options = fast(XmodemOptions::default());
        options.xmodem.check = Check::Crc16;
        let mut s = XmodemSession::new(Box::new(transport), options);
        let got = s.receive().unwrap();
        assert_eq!(got.check, Check::Checksum);
        assert_eq!(got.data, all_bytes(H49));
    }

    #[test]
    fn receive_48gx_payload_strips_zero_padding() {
        let transport = MemoryTransport::from_trace(xmodem_part(GX_XSEND)).unwrap();
        let mut options = fast(XmodemOptions::for_model(Model::Hp48Gx).unwrap());
        // As recorded: three `C`s, then NAK.
        options.xmodem.check = Check::Crc16;
        options.xmodem.crc_attempts = 3;
        let mut s = XmodemSession::new(Box::new(transport), options);
        let got = s.receive().unwrap();
        assert_eq!(got.check, Check::Checksum);
        assert_eq!(got.data, all_bytes(H48));
        assert_eq!(got.stripped, Some(115));
    }

    #[test]
    fn send_replays_recorded_xrecv() {
        for (trace, header, model) in [
            (G49_XRECV, H49, Model::Hp49G),
            (GX_XRECV, H48, Model::Hp48Gx),
        ] {
            let transport = MemoryTransport::from_trace(xmodem_part(trace)).unwrap();
            let mut options = fast(XmodemOptions::for_model(model).unwrap());
            // Recorded with the crate defaults: SUB padding.
            options.xmodem.pad = xmodem_proto::codec::SUB;
            let mut s = XmodemSession::new(Box::new(transport), options);
            let report = s.send(&all_bytes(header)).unwrap();
            assert_eq!(report.bytes, 269);
        }
    }

    #[test]
    fn walk_failure_keeps_every_byte() {
        // Not an HP object: nothing is cut, and the report says so.
        let peer = FakeCalc::sender(b"plain text, no header".to_vec(), Behaviour::hp49());
        let mut s = XmodemSession::new(
            Box::new(peer.clone()),
            fast(XmodemOptions::for_model(Model::Hp49G).unwrap()),
        );
        let got = s.receive().unwrap();
        assert_eq!(got.stripped, None);
        assert_eq!(got.data.len(), 128);
        assert!(got.data.starts_with(b"plain text"));
    }

    #[test]
    fn exact_object_reports_zero_stripped() {
        let peer = FakeCalc::sender(
            hp_string(H49, &[b'x'; 128 - HEADER_LEN - 5]),
            Behaviour::hp49(),
        );
        let mut s = XmodemSession::new(
            Box::new(peer),
            fast(XmodemOptions::for_model(Model::Hp49G).unwrap()),
        );
        let got = s.receive().unwrap();
        assert_eq!(got.received, 128);
        assert_eq!(got.stripped, Some(0));
    }

    // ---- scripted calculators ----

    /// How a fake calculator behaves.
    #[derive(Clone, Debug)]
    struct Behaviour {
        /// The start character it sends as receiver (`D` or NAK).
        start: u8,
        /// The start characters it answers as sender.
        answers: Vec<u8>,
        /// Accepts STX blocks (49G) or NAKs them and cancels after nine (48GX).
        accepts_1k: bool,
        /// Padding byte as sender; `None` = memory garbage.
        pad: Option<u8>,
        /// Swallow the first transmission of this block number.
        drop_block: Option<u8>,
        /// Corrupt the first transmission of this block number.
        corrupt_block: Option<u8>,
    }

    impl Behaviour {
        fn hp49() -> Self {
            Behaviour {
                start: b'D',
                answers: vec![b'D', NAK],
                accepts_1k: true,
                pad: None,
                drop_block: None,
                corrupt_block: None,
            }
        }

        fn hp48gx() -> Self {
            Behaviour {
                start: NAK,
                answers: vec![NAK],
                accepts_1k: false,
                pad: Some(0),
                drop_block: None,
                corrupt_block: None,
            }
        }
    }

    #[derive(Default, Debug)]
    struct State {
        /// Bytes waiting to be read by the host.
        readable: VecDeque<u8>,
        /// Receiver: data so far; sender: unused.
        received: Vec<u8>,
        /// Receiver: next expected block; sender: current block.
        blk: u8,
        /// STX blocks NAKed so far (48GX).
        rejected: u32,
        /// Receiver: EOT seen. Sender: final ACK seen.
        done: bool,
        /// Sender: started.
        started: bool,
        /// Sender: offset of the current block.
        pos: usize,
        /// Sender: EOT sent.
        eot: bool,
        /// Blocks swallowed or corrupted already.
        mangled: Vec<u8>,
        /// Every byte the host wrote.
        written: Vec<Vec<u8>>,
        /// Receiver: check in use.
        check: Option<Check>,
        cans: Vec<u8>,
    }

    /// A calculator running XRECV (`file == None`) or XSEND.
    #[derive(Clone)]
    struct FakeCalc {
        file: Option<Vec<u8>>,
        behaviour: Behaviour,
        state: Arc<Mutex<State>>,
    }

    impl FakeCalc {
        fn receiver(behaviour: Behaviour) -> Self {
            let state = State {
                readable: VecDeque::from([behaviour.start]),
                blk: 1,
                check: Check::from_start_char(behaviour.start),
                ..State::default()
            };
            FakeCalc {
                file: None,
                behaviour,
                state: Arc::new(Mutex::new(state)),
            }
        }

        fn sender(file: Vec<u8>, behaviour: Behaviour) -> Self {
            let state = State {
                blk: 1,
                ..State::default()
            };
            FakeCalc {
                file: Some(file),
                behaviour,
                state: Arc::new(Mutex::new(state)),
            }
        }

        fn state(&self) -> std::sync::MutexGuard<'_, State> {
            self.state.lock().unwrap()
        }

        /// Sender: the current block (or EOT) as the calculator sends it.
        fn current(&self, st: &mut State) -> Vec<u8> {
            let file = self.file.as_deref().unwrap_or_default();
            if st.pos >= file.len() {
                st.eot = true;
                return vec![EOT];
            }
            let check = st.check.unwrap_or(Check::Checksum);
            let end = (st.pos + 128).min(file.len());
            let mut data = file[st.pos..end].to_vec();
            let fill = |i: usize| match self.behaviour.pad {
                Some(p) => p,
                None => (i * 37 + 11) as u8,
            };
            while data.len() < 128 {
                data.push(fill(data.len()));
            }
            let mut block = encode_block(st.blk, BlockSize::B128, &data, 0, check);
            if self.behaviour.corrupt_block == Some(st.blk) && !st.mangled.contains(&st.blk) {
                st.mangled.push(st.blk);
                block[10] ^= 0xFF;
            }
            if self.behaviour.drop_block == Some(st.blk) && !st.mangled.contains(&st.blk) {
                st.mangled.push(st.blk);
                return Vec::new();
            }
            block
        }

        fn on_write(&self, bytes: &[u8]) {
            let mut st = self.state();
            st.written.push(bytes.to_vec());
            if self.file.is_some() {
                self.on_write_as_sender(&mut st, bytes);
            } else {
                self.on_write_as_receiver(&mut st, bytes);
            }
        }

        fn on_write_as_sender(&self, st: &mut State, bytes: &[u8]) {
            if bytes.contains(&CAN) {
                st.cans.extend(bytes.iter().filter(|&&b| b == CAN));
                st.done = true;
                return;
            }
            for &b in bytes {
                if !st.started {
                    if self.behaviour.answers.contains(&b) {
                        st.started = true;
                        st.check = Check::from_start_char(b);
                        let block = self.current(st);
                        st.readable.extend(block);
                    }
                    continue;
                }
                match b {
                    ACK if st.eot => st.done = true,
                    ACK => {
                        st.pos += 128;
                        st.blk = st.blk.wrapping_add(1);
                        let block = self.current(st);
                        st.readable.extend(block);
                    }
                    NAK => {
                        let block = self.current(st);
                        st.readable.extend(block);
                    }
                    _ => {}
                }
            }
        }

        fn on_write_as_receiver(&self, st: &mut State, bytes: &[u8]) {
            let check = st.check.unwrap_or(Check::Checksum);
            match bytes.first() {
                Some(&EOT) => {
                    st.done = true;
                    st.readable.push_back(ACK);
                }
                Some(&CAN) => {
                    st.cans.extend_from_slice(bytes);
                    st.done = true;
                }
                Some(&STX) if !self.behaviour.accepts_1k => {
                    st.rejected += 1;
                    if st.rejected >= 9 {
                        st.readable.extend([CAN, CAN, CAN]);
                    } else {
                        st.readable.push_back(NAK);
                    }
                }
                Some(&(SOH | STX)) => {
                    let num = bytes.get(1).copied().unwrap_or_default();
                    if self.behaviour.drop_block == Some(num) && !st.mangled.contains(&num) {
                        // Lost on the line: no reply; the host times out.
                        st.mangled.push(num);
                        return;
                    }
                    match decode_block(bytes, check) {
                        Ok(block) if block.num == st.blk => {
                            st.received.extend_from_slice(&block.data);
                            st.blk = st.blk.wrapping_add(1);
                            st.readable.push_back(ACK);
                        }
                        Ok(_) => st.readable.push_back(ACK),
                        Err(_) => st.readable.push_back(NAK),
                    }
                }
                _ => {}
            }
        }
    }

    impl Transport for FakeCalc {
        fn write_packet(&mut self, packet: &[u8]) -> io::Result<()> {
            self.on_write(packet);
            Ok(())
        }

        fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
            let mut st = self.state();
            if st.readable.is_empty() {
                drop(st);
                std::thread::sleep(timeout.min(Duration::from_millis(20)));
                return Ok(0);
            }
            let n = st.readable.len().min(buf.len());
            for (slot, byte) in buf.iter_mut().zip(st.readable.drain(..n)) {
                *slot = byte;
            }
            Ok(n)
        }
    }

    fn big_file(header: &[u8; 8]) -> Vec<u8> {
        hp_string(header, &(0..1811).map(|i| i as u8).collect::<Vec<u8>>())
    }

    #[test]
    fn send_to_49g_uses_hp_crc_and_1k() {
        let peer = FakeCalc::receiver(Behaviour::hp49());
        let options = fast(XmodemOptions::for_model(Model::Hp49G).unwrap());
        let mut s = XmodemSession::new(Box::new(peer.clone()), options);
        let file = big_file(H49);
        let mut events = Vec::new();
        let report = s.send_with(&file, &mut |e| events.push(e.clone())).unwrap();
        assert_eq!(report.check, Check::HpCrc);
        assert_eq!(report.bytes, file.len() as u64);
        let st = peer.state();
        assert!(st.done);
        assert_eq!(&st.received[..file.len()], file.as_slice());
        // One 1k block, then the short tail.
        assert_eq!(st.written[0][0], STX);
        assert!(st.written[1..].iter().all(|w| w[0] != STX));
        assert!(events.contains(&Event::Done));
    }

    #[test]
    fn send_to_48gx_uses_checksum_and_128_byte_blocks() {
        // 1k configured, but the 48GX opens with NAK: 128-byte blocks.
        let peer = FakeCalc::receiver(Behaviour::hp48gx());
        let mut options = fast(XmodemOptions::default());
        options.xmodem.block_size = BlockSize::B1k;
        let mut s = XmodemSession::new(Box::new(peer.clone()), options);
        let file = big_file(H48);
        let report = s.send(&file).unwrap();
        assert_eq!(report.check, Check::Checksum);
        let st = peer.state();
        assert_eq!(st.rejected, 0);
        assert_eq!(&st.received[..file.len()], file.as_slice());
        assert!(st.written.iter().all(|w| w[0] != STX));
    }

    #[test]
    fn send_1k_forced_on_48gx_is_cancelled() {
        let peer = FakeCalc::receiver(Behaviour::hp48gx());
        let mut options = fast(XmodemOptions::default());
        options.xmodem.checksum_1k = true;
        let mut s = XmodemSession::new(Box::new(peer.clone()), options);
        let err = s.send(&big_file(H48)).unwrap_err();
        assert!(
            matches!(err, Error::Xmodem(xmodem_proto::Error::RemoteCancelled)),
            "{err:?}"
        );
        assert_eq!(peer.state().rejected, 9);
    }

    #[test]
    fn send_recovers_a_dropped_block() {
        let peer = FakeCalc::receiver(Behaviour {
            drop_block: Some(2),
            ..Behaviour::hp48gx()
        });
        let mut s = XmodemSession::new(
            Box::new(peer.clone()),
            fast(XmodemOptions::for_model(Model::Hp48Gx).unwrap()),
        );
        let file = all_bytes(H48);
        s.send(&file).unwrap();
        let st = peer.state();
        assert_eq!(&st.received[..file.len()], file.as_slice());
        // Block 2 went out twice (the first copy was lost).
        let twos = st
            .written
            .iter()
            .filter(|w| w.first() == Some(&SOH) && w.get(1) == Some(&2))
            .count();
        assert_eq!(twos, 2);
    }

    #[test]
    fn receive_from_49g_recovers_dropped_and_corrupt_blocks() {
        let file = big_file(H49);
        let peer = FakeCalc::sender(
            file.clone(),
            Behaviour {
                drop_block: Some(3),
                corrupt_block: Some(5),
                ..Behaviour::hp49()
            },
        );
        let mut s = XmodemSession::new(
            Box::new(peer.clone()),
            fast(XmodemOptions::for_model(Model::Hp49G).unwrap()),
        );
        let got = s.receive().unwrap();
        assert_eq!(got.check, Check::HpCrc);
        assert_eq!(got.data, file);
        let st = peer.state();
        assert!(st.done);
        assert_eq!(st.mangled, vec![3, 5]);
        // Our NAKs asked for both again.
        assert!(st.written.iter().filter(|w| **w == [NAK]).count() >= 2);
    }

    #[test]
    fn receive_from_48gx_ignores_d_and_uses_checksum() {
        // Unknown model: `D` three times, then NAK, which the 48GX answers.
        let file = all_bytes(H48);
        let peer = FakeCalc::sender(file.clone(), Behaviour::hp48gx());
        let mut options = fast(XmodemOptions::default());
        options.xmodem.crc_attempts = 3;
        let mut s = XmodemSession::new(Box::new(peer.clone()), options);
        let got = s.receive().unwrap();
        assert_eq!(got.check, Check::Checksum);
        assert_eq!(got.data, file);
        let st = peer.state();
        assert_eq!(
            &st.written[..4],
            &[b"D".to_vec(), b"D".to_vec(), b"D".to_vec(), vec![NAK]]
        );
    }

    #[test]
    fn nobody_starts_send_times_out() {
        let peer = FakeCalc::receiver(Behaviour {
            start: 0,
            ..Behaviour::hp49()
        });
        peer.state().readable.clear();
        let mut s = XmodemSession::new(
            Box::new(peer),
            fast(XmodemOptions::for_model(Model::Hp49G).unwrap()),
        );
        let start = Instant::now();
        let err = s.send(b"x").unwrap_err();
        assert!(
            matches!(err, Error::Xmodem(xmodem_proto::Error::Timeout)),
            "{err:?}"
        );
        let took = start.elapsed();
        assert!(
            took >= MIN_START_TIMEOUT && took < Duration::from_secs(3),
            "{took:?}"
        );
    }

    #[test]
    fn nobody_starts_receive_times_out_with_cans() {
        let peer = FakeCalc::sender(
            b"x".to_vec(),
            Behaviour {
                answers: vec![],
                ..Behaviour::hp49()
            },
        );
        let mut s = XmodemSession::new(
            Box::new(peer.clone()),
            fast(XmodemOptions::for_model(Model::Hp49G).unwrap()),
        );
        let start = Instant::now();
        let err = s.receive().unwrap_err();
        assert!(
            matches!(err, Error::Xmodem(xmodem_proto::Error::Timeout)),
            "{err:?}"
        );
        let took = start.elapsed();
        assert!(
            took >= MIN_START_TIMEOUT && took < Duration::from_secs(3),
            "{took:?}"
        );
        // Only `D`s for the 49G (no fallback within the window), then CANs.
        let st = peer.state();
        assert!(st.written.len() > 3);
        assert!(
            st.written[..st.written.len() - 1]
                .iter()
                .all(|w| *w == b"D")
        );
        assert_eq!(st.cans, vec![CAN, CAN, CAN]);
    }

    /// A line that delivers one good block, then noise forever: block
    /// headers whose frames never check out, so the receiver keeps purging
    /// and its own timeouts never fire. Only the watchdog ends this.
    struct NoisyLine {
        reads: usize,
        first: Option<Vec<u8>>,
    }

    impl Transport for NoisyLine {
        fn write_packet(&mut self, _packet: &[u8]) -> io::Result<()> {
            Ok(())
        }

        fn read(&mut self, buf: &mut [u8], _timeout: Duration) -> io::Result<usize> {
            self.reads += 1;
            if self.reads > 20_000 {
                return Err(io::Error::other("read cap hit: watchdog never fired"));
            }
            if let Some(block) = self.first.take() {
                buf[..block.len()].copy_from_slice(&block);
                return Ok(block.len());
            }
            std::thread::sleep(Duration::from_millis(1));
            let noise = [SOH, 2, 0xFD, 7, 7, 7];
            buf[..noise.len()].copy_from_slice(&noise);
            Ok(noise.len())
        }
    }

    #[test]
    fn continuous_noise_still_ends() {
        let mut options = fast(XmodemOptions::default());
        options.xmodem.retries = 1;
        let first = encode_block(1, BlockSize::B128, b"x", 0, Check::HpCrc);
        let line = NoisyLine {
            reads: 0,
            first: Some(first),
        };
        let mut s = XmodemSession::new(Box::new(line), options);
        let start = Instant::now();
        let err = s.receive().unwrap_err();
        assert!(
            matches!(err, Error::Xmodem(xmodem_proto::Error::Timeout)),
            "{err:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn options_for_models() {
        assert!(matches!(
            XmodemOptions::for_model(Model::Hp48Sx),
            Err(Error::Unsupported(_))
        ));
        let gx = XmodemOptions::for_model(Model::Hp48Gx).unwrap();
        assert_eq!(
            (gx.xmodem.check, gx.xmodem.block_size),
            (Check::Checksum, BlockSize::B128)
        );
        let g49 = XmodemOptions::for_model(Model::Hp49G).unwrap();
        assert_eq!(
            (g49.xmodem.check, g49.xmodem.block_size),
            (Check::HpCrc, BlockSize::B1k)
        );
        let clamp = XmodemOptions {
            start_timeout: Duration::from_secs(100_000),
            ..XmodemOptions::default()
        };
        assert_eq!(clamp.config().start_timeout, MAX_START_TIMEOUT);
        assert_eq!(clamp.config().recv_start_timeout, Some(MAX_START_TIMEOUT));
        let zero = XmodemOptions {
            start_timeout: Duration::ZERO,
            ..XmodemOptions::default()
        };
        assert_eq!(zero.config().start_timeout, MIN_START_TIMEOUT);
    }
}
