//! Blocking links to a calculator: serial port and TCP (emulator) with the
//! `native` feature (default), the saturnus emulator in-process (HP 48SX,
//! 48GX or 49G, chosen in the `saturnus://MODEL@ROM` address) with the
//! `saturnus` feature, and in-memory (tests).
//!
//! A [`Transport`] writes whole packets and reads with a timeout. Each packet
//! goes out in one write: the HP's receiver overruns on inter-byte gaps. A
//! host without blocking reads (a browser) uses
//! [`machine::Machine`](crate::machine::Machine) instead.

use std::collections::VecDeque;
use std::io::{self, ErrorKind};
#[cfg(feature = "native")]
use std::io::{Read, Write};
#[cfg(feature = "native")]
use std::net::TcpStream;

use crate::time::{Duration, Instant};

use kermit_proto::trace::{self, Direction};

use crate::{Error, Result};

#[cfg(feature = "saturnus")]
mod saturnus;
#[cfg(feature = "saturnus")]
pub use saturnus::SaturnusTransport;

/// A byte link to a calculator.
pub trait Transport: Send {
    /// Write one whole packet with a single write (then flush).
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<()>;
    /// Read what is available, waiting at most `timeout`. `Ok(0)` = timeout
    /// passed without data. A closed link is `Err(ErrorKind::UnexpectedEof)`.
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
}

/// Serial line speed: the HP default (IOPAR) and what the emulator expects.
pub const BAUD: u32 = 9600;

/// Shortest timeout handed to the OS; zero would mean "block forever".
#[cfg(feature = "native")]
const MIN_TIMEOUT: Duration = Duration::from_millis(1);

/// Open a link: `tcp://host:port` for an emulator, `saturnus://[MODEL@]ROM`
/// for the in-process saturnus emulator booted from the ROM image at the
/// path `ROM` (needs the `saturnus` feature; see [`parse_saturnus`]), e.g.
/// `saturnus:///abs/sxrom-j` (an HP 48SX) or
/// `saturnus://49g@/abs/rom.49g`; anything else without a `scheme://`
/// prefix is a serial device path (`/dev/ttyUSB0`, `COM3`). Serial and
/// TCP need the `native` feature: without it they are [`Error::Address`].
pub fn open(addr: &str) -> Result<Box<dyn Transport>> {
    if addr.starts_with("saturnus://") {
        let (model, rom) = parse_saturnus(addr)?;
        return open_saturnus(model, rom);
    }
    if let Some(host_port) = addr.strip_prefix("tcp://") {
        if host_port.is_empty() {
            return Err(Error::Address(addr.to_string()));
        }
        return open_tcp(addr, host_port);
    }
    if addr.is_empty() || addr.contains("://") {
        return Err(Error::Address(addr.to_string()));
    }
    open_serial(addr)
}

#[cfg(feature = "native")]
fn open_tcp(_addr: &str, host_port: &str) -> Result<Box<dyn Transport>> {
    Ok(Box::new(TcpTransport::connect(host_port)?))
}

#[cfg(not(feature = "native"))]
fn open_tcp(addr: &str, _host_port: &str) -> Result<Box<dyn Transport>> {
    Err(Error::Address(format!(
        "{addr} (hptx-core was built without the `native` feature)"
    )))
}

#[cfg(feature = "native")]
fn open_serial(addr: &str) -> Result<Box<dyn Transport>> {
    Ok(Box::new(SerialTransport::open(addr)?))
}

#[cfg(not(feature = "native"))]
fn open_serial(addr: &str) -> Result<Box<dyn Transport>> {
    Err(Error::Address(format!(
        "{addr} (hptx-core was built without the `native` feature)"
    )))
}

/// A calculator model the in-process emulator can be asked to run, by its
/// name in a `saturnus://MODEL@ROM` address. Only the 48SX, 48GX and 49G
/// boot; the others are named so that they can be refused with a clear
/// [`Error::Emulator`]: the 42S has no serial port, the 38G, 39G and 40G
/// have no Kermit server command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmulatorModel {
    /// HP 48SX (the default without a `MODEL@` part).
    Hp48sx,
    /// HP 48GX.
    Hp48gx,
    /// HP 49G.
    Hp49g,
    /// HP 42S: refused, no serial port.
    Hp42s,
    /// HP 38G: refused, no Kermit server.
    Hp38g,
    /// HP 39G: refused, no Kermit server.
    Hp39g,
    /// HP 40G: refused, no Kermit server.
    Hp40g,
}

impl EmulatorModel {
    /// Every model with its address name.
    const ALL: [(EmulatorModel, &'static str); 7] = [
        (EmulatorModel::Hp48sx, "48sx"),
        (EmulatorModel::Hp48gx, "48gx"),
        (EmulatorModel::Hp49g, "49g"),
        (EmulatorModel::Hp42s, "42s"),
        (EmulatorModel::Hp38g, "38g"),
        (EmulatorModel::Hp39g, "39g"),
        (EmulatorModel::Hp40g, "40g"),
    ];

    /// The model's name in addresses (`48sx`).
    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(m, _)| *m == self)
            .map_or("?", |(_, n)| n)
    }

    /// The model named `name` (`48sx`, `HP48SX`; any case, optional `hp`).
    pub fn from_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        let bare = lower.strip_prefix("hp").unwrap_or(&lower);
        Self::ALL.iter().find(|(_, n)| *n == bare).map(|(m, _)| *m)
    }
}

/// Whether `token` looks like a model name: ASCII letters and digits,
/// starting with a digit or with `hp` and a digit (`48sx`, `HP49G`).
fn model_token(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    let bare = lower.strip_prefix("hp").unwrap_or(&lower);
    bare.starts_with(|c: char| c.is_ascii_digit())
        && bare.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Split a `saturnus://[MODEL@]ROM` address into the model and the ROM
/// path. Without `MODEL@` the model is the 48SX. A ROM path may itself
/// contain `@`, so only a prefix before the first `@` that looks like a
/// model name (letters and digits, starting with a digit or `hp` and a
/// digit) counts as one. An empty ROM path or model is
/// [`Error::Address`]; an unknown model name is [`Error::Emulator`] listing
/// the accepted ones.
pub fn parse_saturnus(addr: &str) -> Result<(EmulatorModel, &std::path::Path)> {
    let bad = || Error::Address(addr.to_string());
    let spec = addr.strip_prefix("saturnus://").ok_or_else(bad)?;
    let (model, rom) = match spec.split_once('@') {
        Some(("", _)) => return Err(bad()),
        Some((token, rom)) if model_token(token) => {
            let model = EmulatorModel::from_name(token).ok_or_else(|| {
                Error::Emulator(format!(
                    "unknown model {token:?} in {addr}: saturnus:// boots 48sx, 48gx or 49g"
                ))
            })?;
            (model, rom)
        }
        _ => (EmulatorModel::Hp48sx, spec),
    };
    if rom.is_empty() {
        return Err(bad());
    }
    Ok((model, std::path::Path::new(rom)))
}

/// Boot the saturnus emulator in-process as `model` from the ROM image at
/// `rom` and start its Kermit server (48SX, 48GX and 49G; other models are
/// [`Error::Emulator`]).
#[cfg(feature = "saturnus")]
pub fn open_saturnus(model: EmulatorModel, rom: &std::path::Path) -> Result<Box<dyn Transport>> {
    Ok(Box::new(SaturnusTransport::open(model, rom)?))
}

/// Without the `saturnus` feature there is no in-process emulator.
#[cfg(not(feature = "saturnus"))]
pub fn open_saturnus(model: EmulatorModel, rom: &std::path::Path) -> Result<Box<dyn Transport>> {
    Err(Error::Emulator(format!(
        "cannot boot {} as the {}: hptx-core was built without the `saturnus` feature",
        rom.display(),
        model.name()
    )))
}

/// Read and discard everything that arrives within `period` from now; returns
/// the number of bytes discarded. The idle HP server sends periodic NAKs that
/// sit in the buffers until we read them (wiki: protocols/kermit-hp,
/// emulator/README.md "Stale NAK on connect").
pub fn drain(transport: &mut dyn Transport, period: Duration) -> io::Result<usize> {
    let end = Instant::now() + period;
    let mut buf = [0u8; 256];
    let mut discarded = 0;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(discarded);
        }
        discarded += transport.read(&mut buf, left)?;
    }
}

/// TCP link, e.g. to the emulator's serial bridge (feature `native`).
#[cfg(feature = "native")]
#[derive(Debug)]
pub struct TcpTransport {
    stream: TcpStream,
}

#[cfg(feature = "native")]
impl TcpTransport {
    /// Connect to `host:port` with Nagle disabled.
    pub fn connect(host_port: &str) -> Result<Self> {
        let stream = TcpStream::connect(host_port)?;
        stream.set_nodelay(true)?;
        Ok(TcpTransport { stream })
    }
}

#[cfg(feature = "native")]
impl Transport for TcpTransport {
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<()> {
        self.stream.write_all(packet)?;
        self.stream.flush()
    }

    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.stream
            .set_read_timeout(Some(timeout.max(MIN_TIMEOUT)))?;
        match self.stream.read(buf) {
            Ok(0) => Err(io::Error::from(ErrorKind::UnexpectedEof)),
            Ok(n) => Ok(n),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(0),
            Err(e) => Err(e),
        }
    }
}

/// Serial port at [`BAUD`], 8N1, no flow control (feature `native`).
#[cfg(feature = "native")]
pub struct SerialTransport {
    port: Box<dyn serialport::SerialPort>,
}

#[cfg(feature = "native")]
impl SerialTransport {
    /// Open and configure the serial device at `path`.
    pub fn open(path: &str) -> Result<Self> {
        let mut port = serialport::new(path, BAUD)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None)
            .open()?;
        // Early HP 49G units drive TX weakly and need a level buffer powered
        // from DTR/RTS (wiki: hardware/uart).
        port.write_data_terminal_ready(true)?;
        port.write_request_to_send(true)?;
        Ok(SerialTransport { port })
    }
}

#[cfg(feature = "native")]
impl Transport for SerialTransport {
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<()> {
        self.port.write_all(packet)?;
        self.port.flush()
    }

    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.port.set_timeout(timeout.max(MIN_TIMEOUT))?;
        match self.port.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(e),
        }
    }
}

type Peer = Box<dyn FnMut(&[u8]) -> Vec<Vec<u8>> + Send>;

enum Script {
    Peer(Peer),
    /// Remaining trace chunks; the front is the next expected `>` chunk.
    Trace(VecDeque<(Direction, Vec<u8>)>),
}

/// In-memory link for tests: a closure or a recorded trace plays the
/// calculator.
pub struct MemoryTransport {
    script: Script,
    readable: VecDeque<Vec<u8>>,
}

impl MemoryTransport {
    /// Each written packet is passed to `peer`; the chunks it returns become
    /// readable in order, one chunk per [`Transport::read`].
    pub fn new(peer: impl FnMut(&[u8]) -> Vec<Vec<u8>> + Send + 'static) -> Self {
        MemoryTransport {
            script: Script::Peer(Box::new(peer)),
            readable: VecDeque::new(),
        }
    }

    /// Replay a [`kermit_proto::trace`] text. `<` chunks before the first `>`
    /// are readable at once (stale input). Each write must equal the next `>`
    /// chunk (else `InvalidData`, and the chunk stays expected); after a
    /// matching write the `<` chunks up to the next `>` become readable.
    pub fn from_trace(text: &str) -> Result<Self> {
        let chunks: VecDeque<_> = trace::parse(text).map_err(Error::Reply)?.into();
        let mut transport = MemoryTransport {
            script: Script::Trace(chunks),
            readable: VecDeque::new(),
        };
        transport.release_input();
        Ok(transport)
    }

    /// Move leading `<` chunks of a trace to the readable queue.
    fn release_input(&mut self) {
        if let Script::Trace(chunks) = &mut self.script {
            while let Some((Direction::In, _)) = chunks.front() {
                if let Some((_, bytes)) = chunks.pop_front() {
                    self.readable.push_back(bytes);
                }
            }
        }
    }
}

impl Transport for MemoryTransport {
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<()> {
        match &mut self.script {
            Script::Peer(peer) => {
                let replies = peer(packet);
                self.readable.extend(replies);
            }
            Script::Trace(chunks) => {
                match chunks.front() {
                    Some((Direction::Out, expected)) if expected == packet => {}
                    Some((_, expected)) => {
                        return Err(io::Error::new(
                            ErrorKind::InvalidData,
                            format!(
                                "trace mismatch: wrote {}, expected {}",
                                trace::escape(packet),
                                trace::escape(expected)
                            ),
                        ));
                    }
                    None => {
                        return Err(io::Error::new(
                            ErrorKind::InvalidData,
                            format!("trace exhausted: wrote {}", trace::escape(packet)),
                        ));
                    }
                }
                chunks.pop_front();
                self.release_input();
            }
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let Some(mut chunk) = self.readable.pop_front() else {
            // Behave like an idle link so the Kermit timeouts fire.
            std::thread::sleep(timeout);
            return Ok(0);
        };
        if chunk.len() > buf.len() {
            let rest = chunk.split_off(buf.len());
            self.readable.push_front(rest);
        }
        buf[..chunk.len()].copy_from_slice(&chunk);
        Ok(chunk.len())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    #[cfg(feature = "native")]
    use std::net::TcpListener;
    #[cfg(feature = "native")]
    use std::sync::mpsc;

    const SHORT: Duration = Duration::from_millis(20);

    #[test]
    fn memory_peer_splits_large_chunks() {
        let mut t = MemoryTransport::new(|p| vec![p.to_vec(), b"x".to_vec()]);
        t.write_packet(b"abcde").unwrap();
        let mut buf = [0u8; 3];
        assert_eq!(t.read(&mut buf, SHORT).unwrap(), 3);
        assert_eq!(&buf, b"abc");
        assert_eq!(t.read(&mut buf, SHORT).unwrap(), 2);
        assert_eq!(&buf[..2], b"de");
        assert_eq!(t.read(&mut buf, SHORT).unwrap(), 1);
        assert_eq!(t.read(&mut buf, Duration::ZERO).unwrap(), 0);
    }

    #[test]
    fn memory_trace_replay() {
        let mut t = MemoryTransport::from_trace("< stale\n> out\n< in1\n< in2\n> end\n").unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(t.read(&mut buf, SHORT).unwrap(), 5);
        assert_eq!(t.read(&mut buf, Duration::ZERO).unwrap(), 0);
        let err = t.write_packet(b"wrong").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains("expected out"), "{err}");
        t.write_packet(b"out").unwrap();
        assert_eq!(t.read(&mut buf, SHORT).unwrap(), 3);
        assert_eq!(&buf[..3], b"in1");
        assert_eq!(t.read(&mut buf, SHORT).unwrap(), 3);
        t.write_packet(b"end").unwrap();
        let err = t.write_packet(b"more").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(MemoryTransport::from_trace("? bad\n").is_err());
    }

    #[test]
    fn drain_discards_stale_input() {
        let mut t = MemoryTransport::from_trace("< \\x01# N3\\r\n> x\n").unwrap();
        assert_eq!(drain(&mut t, Duration::ZERO).unwrap(), 0);
        assert_eq!(drain(&mut t, SHORT).unwrap(), 6);
        let mut buf = [0u8; 8];
        assert_eq!(t.read(&mut buf, Duration::ZERO).unwrap(), 0);
    }

    #[cfg(feature = "native")]
    #[test]
    fn tcp_transport() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (close_tx, close_rx) = mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut packet = [0u8; 7];
            s.read_exact(&mut packet).unwrap();
            s.write_all(b"reply").unwrap();
            close_rx.recv().unwrap();
            packet
        });

        let mut t = TcpTransport::connect(&format!("127.0.0.1:{port}")).unwrap();
        t.write_packet(b"\x01packet").unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 16];
        while got.len() < 5 {
            let n = t.read(&mut buf, Duration::from_secs(1)).unwrap();
            assert!(n > 0, "no reply");
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, b"reply");

        let start = Instant::now();
        assert_eq!(t.read(&mut buf, SHORT).unwrap(), 0);
        assert!(start.elapsed() >= Duration::from_millis(15));

        close_tx.send(()).unwrap();
        assert_eq!(&server.join().unwrap(), b"\x01packet");
        let err = t.read(&mut buf, Duration::from_secs(1)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
    }

    #[cfg(feature = "native")]
    #[test]
    fn open_addresses() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(open(&format!("tcp://127.0.0.1:{port}")).is_ok());
        assert!(matches!(open(""), Err(Error::Address(a)) if a.is_empty()));
        assert!(matches!(open("udp://x:1"), Err(Error::Address(a)) if a == "udp://x:1"));
        assert!(matches!(open("tcp://"), Err(Error::Address(_))));
        assert!(matches!(open("saturnus://"), Err(Error::Address(_))));
        assert!(matches!(open("saturnus://49g@"), Err(Error::Address(_))));
        assert!(matches!(
            open("saturnus:///nonexistent/rom"),
            Err(Error::Emulator(_))
        ));
        assert!(matches!(
            open("saturnus://49g@/nonexistent/rom"),
            Err(Error::Emulator(_))
        ));
    }

    #[test]
    fn saturnus_addresses() {
        fn parse(a: &str) -> Result<(EmulatorModel, String)> {
            parse_saturnus(a).map(|(m, p)| (m, p.to_string_lossy().into_owned()))
        }
        let ok = |m, p: &str| (m, p.to_string());
        assert_eq!(
            parse("saturnus:///r/sxrom-j").unwrap(),
            ok(EmulatorModel::Hp48sx, "/r/sxrom-j")
        );
        assert_eq!(
            parse("saturnus://49g@/r/rom.49g").unwrap(),
            ok(EmulatorModel::Hp49g, "/r/rom.49g")
        );
        assert_eq!(
            parse("saturnus://HP48GX@/r/gxrom-r").unwrap(),
            ok(EmulatorModel::Hp48gx, "/r/gxrom-r")
        );
        assert_eq!(
            parse("saturnus://48sx@roms/sx@j").unwrap(),
            ok(EmulatorModel::Hp48sx, "roms/sx@j")
        );
        // An `@` inside the path is not a model separator.
        assert_eq!(
            parse("saturnus:///r/a@b/rom").unwrap(),
            ok(EmulatorModel::Hp48sx, "/r/a@b/rom")
        );
        assert_eq!(
            parse("saturnus://roms@2/rom").unwrap(),
            ok(EmulatorModel::Hp48sx, "roms@2/rom")
        );
        assert_eq!(
            parse(r"saturnus://C:\r@1\rom").unwrap(),
            ok(EmulatorModel::Hp48sx, r"C:\r@1\rom")
        );
        for (name, model) in [
            ("42s", EmulatorModel::Hp42s),
            ("38G", EmulatorModel::Hp38g),
            ("39g", EmulatorModel::Hp39g),
            ("hp40g", EmulatorModel::Hp40g),
        ] {
            let addr = format!("saturnus://{name}@/r/rom");
            assert_eq!(parse(&addr).unwrap().0, model);
        }
        assert!(matches!(
            parse("saturnus://@/r/rom"),
            Err(Error::Address(_))
        ));
        assert!(matches!(parse("saturnus://48sx@"), Err(Error::Address(_))));
        assert!(matches!(parse("tcp://h:1"), Err(Error::Address(_))));
        match parse("saturnus://48s@/r/rom") {
            Err(Error::Emulator(msg)) => assert!(msg.contains("48sx, 48gx or 49g"), "{msg}"),
            other => panic!("expected Error::Emulator, got {other:?}"),
        }
        for (model, name) in EmulatorModel::ALL {
            assert_eq!(model.name(), name);
            assert_eq!(EmulatorModel::from_name(name), Some(model));
        }
    }
}
