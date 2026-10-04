//! Links to a calculator: serial port, TCP (emulator) and in-memory (tests).
//!
//! A [`Transport`] writes whole packets and reads with a timeout. Each packet
//! goes out in one write: the HP's receiver overruns on inter-byte gaps.

use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use kermit_proto::trace::{self, Direction};

use crate::{Error, Result};

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
const MIN_TIMEOUT: Duration = Duration::from_millis(1);

/// Open a link: `tcp://host:port` for an emulator, anything else without a
/// `scheme://` prefix is a serial device path (`/dev/ttyUSB0`, `COM3`).
pub fn open(addr: &str) -> Result<Box<dyn Transport>> {
    if let Some(host_port) = addr.strip_prefix("tcp://") {
        if host_port.is_empty() {
            return Err(Error::Address(addr.to_string()));
        }
        return Ok(Box::new(TcpTransport::connect(host_port)?));
    }
    if addr.is_empty() || addr.contains("://") {
        return Err(Error::Address(addr.to_string()));
    }
    Ok(Box::new(SerialTransport::open(addr)?))
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

/// TCP link, e.g. to the emulator's serial bridge.
#[derive(Debug)]
pub struct TcpTransport {
    stream: TcpStream,
}

impl TcpTransport {
    /// Connect to `host:port` with Nagle disabled.
    pub fn connect(host_port: &str) -> Result<Self> {
        let stream = TcpStream::connect(host_port)?;
        stream.set_nodelay(true)?;
        Ok(TcpTransport { stream })
    }
}

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

/// Serial port at [`BAUD`], 8N1, no flow control.
pub struct SerialTransport {
    port: Box<dyn serialport::SerialPort>,
}

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
    use std::net::TcpListener;
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

    #[test]
    fn open_addresses() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(open(&format!("tcp://127.0.0.1:{port}")).is_ok());
        assert!(matches!(open(""), Err(Error::Address(a)) if a.is_empty()));
        assert!(matches!(open("udp://x:1"), Err(Error::Address(a)) if a == "udp://x:1"));
        assert!(matches!(open("tcp://"), Err(Error::Address(_))));
    }
}
