//! Record an XModem trace against a live calculator (or emulator) over TCP.
//!
//! Usage: `record-xmodem ADDR [OPTIONS] send FILE | receive [OUT]`, e.g.
//! `record-xmodem localhost:4852 --host "'HPTXX' XRECV" send all.hp`.
//!
//! Options:
//! - `--host TEXT`: first send one raw Kermit `C` packet (block check 1, every
//!   control character prefixed) carrying TEXT to the calculator's Kermit
//!   server. TEXT takes the trace escapes. Note that `XRECV`/`XSEND` started
//!   this way fail with "Port Not Available" (49G, 48GX); for a transfer, put
//!   the name on the stack, leave the server and type the command on the
//!   calculator, then run this without `--host`.
//! - `--1k`: send 1k blocks (with the 128-byte short tail) when the receiver
//!   asks for a CRC; add `--checksum-1k` to send them in checksum mode too
//!   (how `48gx-xrecv-1k` was recorded).
//! - `--check sum|crc|hp`: as receiver, ask for checksum (NAK), CRC-16 (`C`)
//!   or HP's CRC (`D`, default) first.
//! - `--no-short-tail`: with `--1k`, pad the last 1k block instead of sending
//!   the tail in 128-byte blocks.
//! - `--pad HEX`: padding byte for the last block (default 1a).
//! - `--start-timeout SECS`: sender's wait for the start character (60).
//! - `--linger SECS`: keep logging input after the transfer ends (3).
//!
//! The trace (format: `kermit_proto::trace`) goes to stdout, events to stderr.
//! Every byte in and out is logged, including Kermit packets before, during
//! or after the transfer. The line `# xmodem start` marks where the XModem
//! machine takes over; replay tests start there.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use kermit_proto::trace::{escape, unescape};
use xmodem_proto::{BlockSize, Check, Command, Config, Event, Transfer};

const USAGE: &str = "usage: record-xmodem ADDR [--host TEXT] [--1k] [--checksum-1k] [--no-short-tail] \
                     [--check sum|crc|hp] [--pad HEX] \
                     [--start-timeout SECS] [--linger SECS] send FILE | receive [OUT]";

struct Args {
    addr: String,
    host: Option<Vec<u8>>,
    config: Config,
    linger: Duration,
    command: Command,
    out: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Args, Box<dyn std::error::Error>> {
    let mut it = args.iter();
    let addr = it.next().ok_or(USAGE)?.clone();
    let mut host = None;
    let mut config = Config::default();
    let mut linger = Duration::from_secs(3);
    let secs = |v: Option<&String>| -> Result<Duration, Box<dyn std::error::Error>> {
        Ok(Duration::from_secs_f64(v.ok_or(USAGE)?.parse()?))
    };
    loop {
        match it.next().map(String::as_str) {
            Some("--host") => host = Some(unescape(it.next().ok_or(USAGE)?)?),
            Some("--1k") => config.block_size = BlockSize::B1k,
            Some("--checksum-1k") => config.checksum_1k = true,
            Some("--check") => {
                config.check = match it.next().map(String::as_str) {
                    Some("sum") => Check::Checksum,
                    Some("crc") => Check::Crc16,
                    Some("hp") => Check::HpCrc,
                    _ => return Err(USAGE.into()),
                }
            }
            Some("--no-short-tail") => config.short_tail = false,
            Some("--pad") => config.pad = u8::from_str_radix(it.next().ok_or(USAGE)?, 16)?,
            Some("--start-timeout") => config.start_timeout = secs(it.next())?,
            Some("--linger") => linger = secs(it.next())?,
            Some("send") => {
                let file = it.next().ok_or(USAGE)?;
                return Ok(Args {
                    addr,
                    host,
                    config,
                    linger,
                    command: Command::Send(std::fs::read(file)?),
                    out: None,
                });
            }
            Some("receive") => {
                return Ok(Args {
                    addr,
                    host,
                    config,
                    linger,
                    command: Command::Receive,
                    out: it.next().cloned(),
                });
            }
            _ => return Err(USAGE.into()),
        }
    }
}

/// A Kermit `C` packet, sequence 0, block check 1, control prefix `#`.
fn kermit_host_packet(text: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    for &b in text {
        let low = b & 0x7F;
        if low < 0x20 || low == 0x7F {
            data.push(b'#');
            data.push(b ^ 0x40);
        } else if low == b'#' {
            data.push(b'#');
            data.push(b);
        } else {
            data.push(b);
        }
    }
    let tochar = |n: usize| (n as u8).wrapping_add(32);
    let mut body = vec![tochar(data.len() + 3), tochar(0), b'C'];
    body.extend_from_slice(&data);
    let s: u32 = body.iter().map(|&b| u32::from(b)).sum();
    let check = tochar(((s + ((s & 192) >> 6)) & 63) as usize);
    let mut packet = vec![0x01];
    packet.extend_from_slice(&body);
    packet.push(check);
    packet.push(b'\r');
    packet
}

/// Read and log everything until the line has been quiet for `quiet`.
fn log_until_quiet(
    stream: &mut TcpStream,
    quiet: Duration,
    prefix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut buf = [0u8; 2048];
    stream.set_read_timeout(Some(quiet.max(Duration::from_millis(1))))?;
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return Err("connection closed".into()),
            Ok(n) => println!("{prefix}{}", escape(&buf[..n])),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a = parse_args(&args)?;
    println!(
        "# record-xmodem {}",
        escape(args.get(1..).unwrap_or_default().join(" ").as_bytes())
    );

    let mut stream = TcpStream::connect(&a.addr)?;
    // Drain stale input (the idle server's periodic NAKs, start characters
    // already sent) for 500 ms.
    log_until_quiet(&mut stream, Duration::from_millis(500), "# stale: ")?;

    if let Some(text) = &a.host {
        let packet = kermit_host_packet(text);
        stream.write_all(&packet)?;
        println!("> {}", escape(&packet));
    }
    println!("# xmodem start");

    let mut xfer = Transfer::new(a.config);
    xfer.start(Instant::now(), a.command)?;
    let mut buf = [0u8; 2048];
    let mut outcome: Option<bool> = None;
    loop {
        let now = Instant::now();
        while let Some(bytes) = xfer.poll_output(now) {
            stream.write_all(&bytes)?;
            println!("> {}", escape(&bytes));
        }
        while let Some(event) = xfer.poll_event() {
            match &event {
                Event::FileEnd {
                    data,
                    last_block,
                    padding,
                } => {
                    eprintln!(
                        "FileEnd {{ {} bytes, last_block {last_block}, padding {padding} }}",
                        data.len()
                    );
                    if let Some(out) = &a.out {
                        std::fs::write(out, data)?;
                    }
                }
                other => eprintln!("{other:?}"),
            }
            match event {
                Event::Done => outcome = Some(true),
                Event::Error(_) => outcome = Some(false),
                _ => {}
            }
        }
        let wait = match (xfer.next_timeout(), outcome) {
            (None, Some(_)) => break,
            (Some(t), _) => t.saturating_duration_since(now),
            (None, None) => Duration::from_millis(100),
        };
        stream.set_read_timeout(Some(wait.max(Duration::from_millis(10))))?;
        match stream.read(&mut buf) {
            Ok(0) => return Err("connection closed".into()),
            Ok(n) => {
                println!("< {}", escape(&buf[..n]));
                xfer.handle_input(Instant::now(), &buf[..n]);
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                xfer.handle_timeout(Instant::now());
            }
            Err(e) => return Err(e.into()),
        }
    }
    println!("# xmodem end");
    // Whatever the calculator sends afterwards (Kermit reply, idle NAKs).
    log_until_quiet(&mut stream, a.linger, "< ")?;
    std::io::stdout().flush()?;
    if outcome == Some(true) {
        Ok(())
    } else {
        std::process::exit(1);
    }
}
