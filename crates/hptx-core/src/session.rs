//! Drives a [`kermit_proto::Client`] over a [`Transport`].
//!
//! The loop is the one from `kermit-proto`'s docs: write each queued packet
//! with one write (the HP overruns on inter-byte gaps), handle the events,
//! read until the client's next deadline and feed input or a timeout back.
//! After `Done` or `Error` it keeps going until the final ACK or E packet is
//! out, and through the client's linger after a receive (re-ACKing a
//! repeated `B`). Input left over from the idle server (periodic NAKs) is
//! drained when the session is created.

use std::time::{Duration, Instant};

use kermit_proto::{Client, Command, Config, Event, StartError};

use crate::charset;
use crate::transport::{self, Transport};
use crate::{Error, Result};

/// Read wait while a transaction runs but the client has no deadline.
const IDLE_WAIT: Duration = Duration::from_millis(100);
/// Shortest read wait.
const MIN_WAIT: Duration = Duration::from_millis(10);

/// Tunables for a [`Session`].
#[derive(Clone, Debug)]
pub struct Options {
    /// Kermit client configuration. The default lingers after a receive
    /// ([`Config::linger`]) as long as the turnaround, which the linger
    /// counts toward: no extra delay between transactions.
    pub kermit: Config,
    /// How long to discard input after opening (stale NAKs).
    pub drain: Duration,
    /// Pause between the end of one transaction and the start of the next.
    /// The HP drops a command packet that arrives right after the final ACK
    /// of the previous transaction, and only answers after its own timeout
    /// (about 6 s) NAKs it; 100 ms was enough on the emulator. Counted
    /// from the `Done` or `Error`, so the client's linger runs inside it.
    pub turnaround: Duration,
}

impl Default for Options {
    fn default() -> Self {
        let turnaround = Duration::from_millis(200);
        let mut kermit = Config::default();
        kermit.linger = turnaround;
        Options {
            kermit,
            drain: Duration::from_millis(500),
            turnaround,
        }
    }
}

/// A file received with a GET.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivedFile {
    /// Name from the F packet, decoded from the HP character set.
    pub name: String,
    /// File contents.
    pub data: Vec<u8>,
}

/// What a transaction produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transcript {
    /// Received files (GET); discarded files are left out.
    pub files: Vec<ReceivedFile>,
    /// Names the calculator stored sent files under (SEND), decoded.
    pub stored_names: Vec<String>,
    /// All server text (host command and directory replies), raw HP bytes.
    pub text: Vec<u8>,
}

/// A Kermit connection to a calculator in server mode.
pub struct Session {
    transport: Box<dyn Transport>,
    client: Client,
    config: Config,
    turnaround: Duration,
    /// When the last transaction ended.
    last_end: Option<Instant>,
}

impl Session {
    /// Wrap `transport`, discarding input for `options.drain` first.
    pub fn new(mut transport: Box<dyn Transport>, options: Options) -> Result<Self> {
        transport::drain(transport.as_mut(), options.drain)?;
        Ok(Session {
            transport,
            client: Client::new(options.kermit.clone()),
            config: options.kermit,
            turnaround: options.turnaround,
            last_end: None,
        })
    }

    /// Open `addr` (see [`transport::open`]) with default [`Options`].
    pub fn open(addr: &str) -> Result<Self> {
        Session::new(transport::open(addr)?, Options::default())
    }

    /// Give the link back, e.g. for an XModem transfer after
    /// [`Calculator::prepare_for_xmodem`](crate::Calculator::prepare_for_xmodem)
    /// ended server mode.
    pub fn into_transport(self) -> Box<dyn Transport> {
        self.transport
    }

    /// The Kermit configuration in use.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Use `config` from the next transaction on.
    pub fn set_config(&mut self, config: Config) {
        self.client = Client::new(config.clone());
        self.config = config;
    }

    /// Run one transaction to completion.
    pub fn transact(&mut self, command: Command) -> Result<Transcript> {
        self.transact_with(command, &mut |_| {})
    }

    /// Run one transaction, passing every event to `progress` first.
    pub fn transact_with(
        &mut self,
        command: Command,
        progress: &mut dyn FnMut(&Event),
    ) -> Result<Transcript> {
        if let Some(end) = self.last_end {
            std::thread::sleep((end + self.turnaround).saturating_duration_since(Instant::now()));
        }
        let result = self.run(command, progress);
        if result.is_err() {
            self.client = Client::new(self.config.clone());
            self.last_end = Some(Instant::now());
        }
        result
    }

    fn run(&mut self, command: Command, progress: &mut dyn FnMut(&Event)) -> Result<Transcript> {
        let sending = matches!(command, Command::Send(_));
        let command_text = match &command {
            Command::Host(bytes) | Command::Get(bytes) => charset::decode(bytes),
            _ => String::new(),
        };
        self.client
            .start(Instant::now(), command)
            .map_err(|e| match e {
                StartError::TooLong { len, max } => Error::CommandTooLong {
                    command: command_text,
                    len,
                    max,
                },
                StartError::Busy => Error::Reply("kermit client busy".into()),
                other => Error::Reply(format!("kermit client: {other}")),
            })?;

        let mut transcript = Transcript::default();
        let mut current: Option<ReceivedFile> = None;
        let mut outcome: Option<Result<()>> = None;
        let mut buf = [0u8; 1024];
        'run: loop {
            let now = Instant::now();
            while let Some(packet) = self.client.poll_output(now) {
                match self.transport.write_packet(&packet) {
                    Ok(()) => {}
                    // A re-ACK during the linger: the transaction is
                    // complete, a failing link does not undo it.
                    Err(_) if matches!(outcome, Some(Ok(()))) => break 'run,
                    Err(e) => return Err(e.into()),
                }
            }
            while let Some(event) = self.client.poll_event() {
                progress(&event);
                match event {
                    Event::FileStart { name } if sending => {
                        transcript.stored_names.push(charset::decode(&name));
                    }
                    Event::FileStart { name } => {
                        current = Some(ReceivedFile {
                            name: charset::decode(&name),
                            data: Vec::new(),
                        });
                    }
                    Event::Data(data) => {
                        if let Some(file) = current.as_mut() {
                            file.data.extend_from_slice(&data);
                        }
                    }
                    Event::FileEnd { discarded } => {
                        if let Some(file) = current.take()
                            && !discarded
                        {
                            transcript.files.push(file);
                        }
                    }
                    Event::ServerText(text) => transcript.text.extend_from_slice(&text),
                    Event::Progress { .. } => {}
                    Event::Done => {
                        outcome = Some(Ok(()));
                        // The final ACK is out (it is queued with Done):
                        // the turnaround starts, the linger runs inside it.
                        self.last_end = Some(now);
                    }
                    Event::Error(kermit_proto::Error::Remote(text)) => {
                        outcome = Some(Err(Error::Remote(charset::decode(&text))));
                    }
                    Event::Error(e) => outcome = Some(Err(Error::Kermit(e))),
                    _ => {}
                }
            }
            let wait = match (self.client.next_timeout(), &outcome) {
                (None, Some(_)) => break,
                (Some(t), _) => t.saturating_duration_since(now),
                (None, None) => IDLE_WAIT,
            };
            let n = match self.transport.read(&mut buf, wait.max(MIN_WAIT)) {
                Ok(n) => n,
                // Lingering after Done: the transcript is complete, a
                // failing link only ends the linger.
                Err(_) if matches!(outcome, Some(Ok(()))) => break,
                Err(e) => return Err(e.into()),
            };
            let now = Instant::now();
            if n > 0 {
                self.client.handle_input(now, &buf[..n]);
            }
            // Also after input: handle_input does not check the deadline, so
            // a peer that keeps sending garbage (noise, wrong speed) would
            // otherwise stall the transaction forever. A no-op until the
            // deadline has passed.
            self.client.handle_timeout(now);
        }
        match outcome {
            Some(Err(e)) => Err(e),
            _ => Ok(transcript),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::transport::MemoryTransport;
    use kermit_proto::OutgoingFile;
    use kermit_proto::codec::{BlockCheck, Deframer, Framing, Packet, parse_frame};
    use std::sync::{Arc, Mutex};

    const SX_HOST: &str = include_str!("../../kermit-proto/traces/48sx-host.trace");
    const SX_GET: &str = include_str!("../../kermit-proto/traces/48sx-get.trace");
    const SX_GET_MISSING: &str = include_str!("../../kermit-proto/traces/48sx-get-missing.trace");
    const SX_SEND: &str = include_str!("../../kermit-proto/traces/48sx-send.trace");
    const GX_GET_BINARY: &str = include_str!("../../kermit-proto/traces/48gx-get-binary.trace");

    /// No pauses: zero drain and turnaround, and no linger after a receive
    /// (the in-memory peers never repeat a B).
    fn options() -> Options {
        let mut kermit = Options::default().kermit;
        kermit.linger = Duration::ZERO;
        Options {
            kermit,
            drain: Duration::ZERO,
            turnaround: Duration::ZERO,
        }
    }

    fn session(trace: &str, options: Options) -> Session {
        Session::new(
            Box::new(MemoryTransport::from_trace(trace).unwrap()),
            options,
        )
        .unwrap()
    }

    fn host_reply() -> Vec<u8> {
        let mut text = b"1:".to_vec();
        text.extend_from_slice(&[b' '; 20]);
        text.extend_from_slice(b"42\r\n");
        text
    }

    #[test]
    fn host_command() {
        let mut s = session(SX_HOST, options());
        let mut events = 0;
        let t = s
            .transact_with(Command::Host(b"6 7 *".to_vec()), &mut |_| events += 1)
            .unwrap();
        assert_eq!(t.text, host_reply());
        assert!(t.files.is_empty() && t.stored_names.is_empty());
        assert_eq!(events, 2);
    }

    #[test]
    fn get_missing_is_remote_error() {
        let mut s = session(SX_GET_MISSING, options());
        let err = s.transact(Command::Get(b"NOSUCH".to_vec())).unwrap_err();
        assert!(
            matches!(&err, Error::Remote(m) if m == "Undefined Name"),
            "{err:?}"
        );
    }

    #[test]
    fn get_ascii() {
        let mut s = session(SX_GET, options());
        let t = s.transact(Command::Get(b"ALLB".to_vec())).unwrap();
        assert_eq!(t.files.len(), 1);
        assert_eq!(t.files[0].name, "ALLB");
        assert!(t.files[0].data.starts_with(b"%%HP: T(1)A(D)F(.);\r\n"));
    }

    #[test]
    fn get_binary() {
        let mut s = session(GX_GET_BINARY, options());
        let t = s.transact(Command::Get(b"ALLB".to_vec())).unwrap();
        assert_eq!(t.files.len(), 1);
        assert!(t.files[0].data.starts_with(b"HPHP48-R"));
    }

    #[test]
    fn send_reports_stored_name() {
        let mut s = session(SX_SEND, options());
        let file = OutgoingFile {
            name: b"ALLB".to_vec(),
            data: (0..=255).collect(),
        };
        let t = s.transact(Command::Send(vec![file])).unwrap();
        assert_eq!(t.stored_names, vec!["ALLB".to_string()]);
        assert!(t.files.is_empty());
    }

    #[test]
    fn turnaround_pause_between_transactions() {
        let trace = format!("{SX_HOST}\n{SX_HOST}");
        let options = Options {
            turnaround: Duration::from_millis(50),
            ..options()
        };
        let mut s = session(&trace, options);
        s.transact(Command::Host(b"6 7 *".to_vec())).unwrap();
        let start = Instant::now();
        s.transact(Command::Host(b"6 7 *".to_vec())).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn stale_nak_drained() {
        let trace = format!("< \\x01# N3\\r\n{SX_HOST}");
        let mut s = session(
            &trace,
            Options {
                drain: Duration::from_millis(50),
                turnaround: Duration::ZERO,
                ..options()
            },
        );
        let t = s.transact(Command::Host(b"6 7 *".to_vec())).unwrap();
        assert_eq!(t.text, host_reply());
    }

    #[test]
    fn error_resets_client() {
        let mut s = session(SX_HOST, options());
        let err = s.transact(Command::Host(b"6 7 +".to_vec())).unwrap_err();
        assert!(matches!(err, Error::Io(_)), "{err:?}");
        // The mismatching write was not consumed, so a clean client can
        // replay the trace from the start.
        let t = s.transact(Command::Host(b"6 7 *".to_vec())).unwrap();
        assert_eq!(t.text, host_reply());
    }

    #[test]
    fn command_too_long() {
        let mut s = session("", options());
        let err = s.transact(Command::Host(vec![b'1'; 200])).unwrap_err();
        assert!(
            matches!(&err, Error::CommandTooLong { command, len, .. } if command.len() == 200 && *len == 200),
            "{err:?}"
        );
    }

    /// A Kermit server that sends file `X` (two D packets) in reply to R and
    /// drops the first transmission of D seq 2.
    fn lossy_server(naks: Arc<Mutex<Vec<u8>>>) -> impl FnMut(&[u8]) -> Vec<Vec<u8>> + Send {
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
                        naks.lock().unwrap().push(2);
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

    /// A server that sends file `X` ("hi") in reply to R, and a link that
    /// fails every read once our ACK to its B is out.
    struct LostAfterFinalAck {
        inner: MemoryTransport,
        acked: Arc<Mutex<bool>>,
    }

    impl Transport for LostAfterFinalAck {
        fn write_packet(&mut self, packet: &[u8]) -> std::io::Result<()> {
            self.inner.write_packet(packet)
        }

        fn read(&mut self, buf: &mut [u8], timeout: Duration) -> std::io::Result<usize> {
            if *self.acked.lock().unwrap() {
                return Err(std::io::Error::other("link lost"));
            }
            self.inner.read(buf, timeout)
        }
    }

    #[test]
    fn link_lost_while_lingering_keeps_the_transcript() {
        let acked = Arc::new(Mutex::new(false));
        let flag = Arc::clone(&acked);
        let check = BlockCheck::Type1;
        let mut deframer = Deframer::new();
        let server = move |bytes: &[u8]| {
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
                    (b'Y', 1) => replies.push(wire(2, b'D', b"hi")),
                    (b'Y', 2) => replies.push(wire(3, b'Z', b"")),
                    (b'Y', 3) => replies.push(wire(4, b'B', b"")),
                    (b'Y', 4) => *flag.lock().unwrap() = true,
                    (kind, seq) => panic!("unexpected packet {} seq {seq}", char::from(kind)),
                }
            }
            replies
        };
        let mut kermit = Options::default().kermit;
        kermit.linger = Duration::from_secs(5);
        let options = Options {
            kermit,
            ..options()
        };
        let transport = LostAfterFinalAck {
            inner: MemoryTransport::new(server),
            acked: Arc::clone(&acked),
        };
        let mut s = Session::new(Box::new(transport), options).unwrap();
        let start = Instant::now();
        let t = s.transact(Command::Get(b"X".to_vec())).unwrap();
        assert!(*acked.lock().unwrap());
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "linger ended early"
        );
        assert_eq!(
            t.files,
            vec![ReceivedFile {
                name: "X".into(),
                data: b"hi".to_vec()
            }]
        );
    }

    /// Reads that never stop delivering noise: no SOH, so no frame ever
    /// completes. Fails the read after `MAX_READS` so a regression cannot
    /// hang the test.
    struct NoisyLine {
        reads: usize,
    }

    const MAX_READS: usize = 5_000;

    impl Transport for NoisyLine {
        fn write_packet(&mut self, _packet: &[u8]) -> std::io::Result<()> {
            Ok(())
        }

        fn read(&mut self, buf: &mut [u8], _timeout: Duration) -> std::io::Result<usize> {
            self.reads += 1;
            if self.reads > MAX_READS {
                return Err(std::io::Error::other("read cap hit: timeout never fired"));
            }
            std::thread::sleep(Duration::from_millis(1));
            let noise = b"zq~";
            buf[..noise.len()].copy_from_slice(noise);
            Ok(noise.len())
        }
    }

    #[test]
    fn continuous_garbage_still_times_out() {
        let mut kermit = Options::default().kermit;
        kermit.timeout = Duration::from_millis(20);
        kermit.retries = 2;
        let options = Options {
            kermit,
            drain: Duration::ZERO,
            turnaround: Duration::ZERO,
        };
        let mut s = Session {
            transport: Box::new(NoisyLine { reads: 0 }),
            client: Client::new(options.kermit.clone()),
            config: options.kermit,
            turnaround: options.turnaround,
            last_end: None,
        };
        let start = Instant::now();
        let err = s.transact(Command::Host(b"6 7 *".to_vec())).unwrap_err();
        assert!(
            matches!(err, Error::Kermit(kermit_proto::Error::Timeout)),
            "{err:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn lost_packet_is_recovered() {
        let naks = Arc::new(Mutex::new(Vec::new()));
        let transport = MemoryTransport::new(lossy_server(Arc::clone(&naks)));
        let mut kermit = options().kermit;
        kermit.timeout = Duration::from_millis(100);
        let options = Options {
            kermit,
            drain: Duration::ZERO,
            turnaround: Duration::ZERO,
        };
        let mut s = Session::new(Box::new(transport), options).unwrap();
        let start = Instant::now();
        let t = s.transact(Command::Get(b"X".to_vec())).unwrap();
        assert!(start.elapsed() < Duration::from_millis(800));
        assert_eq!(
            t.files,
            vec![ReceivedFile {
                name: "X".into(),
                data: b"hello world".to_vec()
            }]
        );
        assert_eq!(*naks.lock().unwrap(), vec![2]);
    }
}
