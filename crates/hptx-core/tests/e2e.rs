//! End-to-end tests against the saturnng emulator in `emulator/`.
//!
//! Skipped unless `HPTX_E2E_ADDR` is set, e.g. `tcp://localhost:4848`.

use std::error::Error;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

type TestResult = Result<(), Box<dyn Error>>;

/// Kermit I-packet, byte for byte as `emulator/kermit-probe.py` sends it:
/// seq 0, MAXL=94 TIME=10 NPAD=0 PADC=0 EOL=CR QCTL=# QBIN=N CHKT=1.
const I_PACKET: &[u8] = b"\x01+ I~* @-#N1L\r";

fn e2e_addr() -> Option<String> {
    std::env::var("HPTX_E2E_ADDR")
        .ok()
        .filter(|a| !a.is_empty())
}

fn connect(addr: &str) -> Result<TcpStream, Box<dyn Error>> {
    let host_port = addr
        .strip_prefix("tcp://")
        .ok_or_else(|| format!("HPTX_E2E_ADDR must look like tcp://host:port, got {addr:?}"))?;
    let stream = TcpStream::connect(host_port)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    Ok(stream)
}

/// Discard stale input for about 500 ms. In server mode the calculator sends
/// a NAK when its idle timeout expires; the pty buffers it for the next client.
fn drain(stream: &mut TcpStream) -> TestResult {
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut buf = [0u8; 256];
    while Instant::now() < deadline {
        match stream.read(&mut buf) {
            Ok(0) => return Err("connection closed while draining".into()),
            Ok(n) => eprintln!("discarded stale input: {:?}", &buf[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e.into()),
        }
    }
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    Ok(())
}

/// Read until a `\r`-terminated reply arrives, or the 10 s read timeout hits.
fn read_reply(stream: &mut TcpStream) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut reply = Vec::new();
    let mut byte = [0u8; 1];
    while reply.last() != Some(&b'\r') {
        if stream.read(&mut byte)? == 0 {
            return Err(format!("connection closed after {reply:?}").into());
        }
        reply.push(byte[0]);
    }
    Ok(reply)
}

/// Smoke test proving the CI emulator pipeline works: the calculator in
/// SERVER mode ACKs a Kermit I-packet (see wiki: protocols/kermit-hp).
/// Replaced by kermit-proto-based tests in iteration 3.
#[test]
fn server_acks_i_packet() -> TestResult {
    let Some(addr) = e2e_addr() else {
        return Ok(());
    };
    let mut stream = connect(&addr)?;
    drain(&mut stream)?;
    stream.write_all(I_PACKET)?;
    let reply = read_reply(&mut stream)?;
    eprintln!("I-packet reply: {:?}", String::from_utf8_lossy(&reply));
    assert_eq!(reply.first(), Some(&0x01), "reply must start with SOH");
    assert_eq!(reply.get(3), Some(&b'Y'), "reply must be an ACK");
    Ok(())
}
