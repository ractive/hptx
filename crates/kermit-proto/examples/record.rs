//! Record a trace against a live calculator (or emulator) over TCP.
//!
//! Usage: `record ADDR COMMAND [ARGS]`, e.g. `record localhost:4848 host "6 7 *"`.
//! Commands: `info`, `host TEXT`, `dir`, `get NAME`, `send NAME FILE`, `finish`,
//! `logout`. `host` TEXT takes the trace escapes (`\x8d` for a byte outside
//! ASCII). The trace goes to stdout, events to stderr.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use kermit_proto::trace::{escape, unescape};
use kermit_proto::{Client, Command, Config, Event, OutgoingFile};

const USAGE: &str = "usage: record ADDR info|host TEXT|dir|get NAME|send NAME FILE|finish|logout";

fn parse_command(args: &[String]) -> Result<Command, Box<dyn std::error::Error>> {
    let rest = args.get(1..).unwrap_or_default();
    let cmd = match (args.first().map(String::as_str), rest) {
        (Some("info"), []) => Command::Info,
        (Some("host"), [_, ..]) => Command::Host(unescape(&rest.join(" "))?),
        (Some("dir"), []) => Command::Directory,
        (Some("get"), [name]) => Command::Get(name.as_bytes().to_vec()),
        (Some("send"), [name, file]) => Command::Send(vec![OutgoingFile {
            name: name.as_bytes().to_vec(),
            data: std::fs::read(file)?,
        }]),
        (Some("finish"), []) => Command::Finish,
        (Some("logout"), []) => Command::Logout,
        _ => return Err(USAGE.into()),
    };
    Ok(cmd)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (addr, cmd_args) = match args.split_first() {
        Some((addr, rest)) if !rest.is_empty() => (addr, rest),
        _ => return Err(USAGE.into()),
    };
    let command = parse_command(cmd_args)?;
    println!("# record {}", cmd_args.join(" "));

    let mut stream = TcpStream::connect(addr)?;
    let mut buf = [0u8; 1024];

    // Drain stale input (the idle server's periodic NAKs) for 500 ms.
    let drain_end = Instant::now() + Duration::from_millis(500);
    loop {
        let left = drain_end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        stream.set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
        match stream.read(&mut buf) {
            Ok(0) => return Err("connection closed".into()),
            Ok(n) => println!("# stale: {}", escape(&buf[..n])),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => break,
            Err(e) => return Err(e.into()),
        }
    }

    let mut client = Client::new(Config::default());
    client.start(Instant::now(), command)?;
    let mut outcome: Option<bool> = None;
    loop {
        let now = Instant::now();
        while let Some(bytes) = client.poll_output(now) {
            stream.write_all(&bytes)?;
            println!("> {}", escape(&bytes));
        }
        while let Some(event) = client.poll_event() {
            eprintln!("{event:?}");
            match event {
                Event::Done => outcome = Some(true),
                Event::Error(_) => outcome = Some(false),
                _ => {}
            }
        }
        let wait = match (client.next_timeout(), outcome) {
            (None, Some(_)) => break,
            (Some(t), _) => t.saturating_duration_since(now),
            (None, None) => Duration::from_millis(100),
        };
        stream.set_read_timeout(Some(wait.max(Duration::from_millis(10))))?;
        match stream.read(&mut buf) {
            Ok(0) => return Err("connection closed".into()),
            Ok(n) => {
                println!("< {}", escape(&buf[..n]));
                client.handle_input(Instant::now(), &buf[..n]);
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                client.handle_timeout(Instant::now());
            }
            Err(e) => return Err(e.into()),
        }
    }
    std::io::stdout().flush()?;
    if outcome == Some(true) {
        Ok(())
    } else {
        std::process::exit(1);
    }
}
