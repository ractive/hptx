//! Replays of traces recorded against the saturnng emulator
//! (`cargo run -p xmodem-proto --example record-xmodem`, see `emulator/README.md`).
//! Our side must produce the recorded bytes exactly; the calculator's side is
//! fed back in the recorded chunks. Replay starts at `# xmodem start`; the
//! Kermit `C` packet before it belongs to the caller, not to the machine.
//!
//! Keyboard-started traces: 'HPTXX' was put on the stack over Kermit, the
//! server left with FINISH, and XRECV or XSEND typed on the calculator. The
//! server-started traces show that XRECV/XSEND cannot run inside the Kermit
//! server ("Port Not Available"); see the module tests below.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::time::Instant;

use kermit_proto::trace::{self, Direction};

use crate::codec::{CAN, NAK};
use crate::{BlockSize, Check, Command, Config, Error, Event, Transfer};

const G49_XRECV: &str = include_str!("../traces/49g-xrecv.trace");
const G49_XRECV_1K: &str = include_str!("../traces/49g-xrecv-1k.trace");
const G49_XSEND: &str = include_str!("../traces/49g-xsend.trace");
const G49_XSEND_HPCRC: &str = include_str!("../traces/49g-xsend-hpcrc.trace");
const G49_XSEND_1K: &str = include_str!("../traces/49g-xsend-1k.trace");
const G49_SERVER_XRECV: &str = include_str!("../traces/49g-server-xrecv.trace");
const GX_XRECV: &str = include_str!("../traces/48gx-xrecv.trace");
const GX_XRECV_1K: &str = include_str!("../traces/48gx-xrecv-1k.trace");
const GX_XSEND: &str = include_str!("../traces/48gx-xsend.trace");
const GX_XSEND_HPCRC: &str = include_str!("../traces/48gx-xsend-hpcrc.trace");
const GX_SERVER_XSEND: &str = include_str!("../traces/48gx-server-xsend.trace");

/// Outcome of a replay: every event, the machine and the virtual clock.
struct Replay {
    events: Vec<Event>,
    xfer: Transfer,
    now: Instant,
}

/// Replay the part of `text` after `# xmodem start` on a virtual clock. An
/// Out line must match `poll_output`; when nothing is queued, the clock jumps
/// to `next_timeout` and `handle_timeout` runs (start-character intervals,
/// reply timeouts). In lines go to `handle_input`.
fn replay(cmd: Command, config: Config, text: &str) -> Replay {
    let start = text
        .find("# xmodem start")
        .expect("trace has no `# xmodem start` line");
    let mut now = Instant::now();
    let mut xfer = Transfer::new(config);
    xfer.start(now, cmd).unwrap();
    let mut events = Vec::new();
    for (i, (dir, bytes)) in trace::parse(&text[start..])
        .unwrap()
        .into_iter()
        .enumerate()
    {
        match dir {
            Direction::Out => {
                let mut got = xfer.poll_output(now);
                for _ in 0..100 {
                    if got.is_some() {
                        break;
                    }
                    let Some(due) = xfer.next_timeout() else {
                        break;
                    };
                    now = due;
                    xfer.handle_timeout(now);
                    got = xfer.poll_output(now);
                }
                assert_eq!(
                    got.as_deref().map(trace::escape),
                    Some(trace::escape(&bytes)),
                    "trace entry {i}: expected output > {}",
                    trace::escape(&bytes)
                );
            }
            Direction::In => xfer.handle_input(now, &bytes),
        }
        events.extend(std::iter::from_fn(|| xfer.poll_event()));
    }
    let extra = xfer.poll_output(now);
    assert_eq!(
        extra.as_deref().map(trace::escape),
        None,
        "unexpected trailing output"
    );
    Replay { events, xfer, now }
}

/// An HP binary-mode file holding a String object: `header`, prolog #02A2C,
/// a 5-nibble length (5 + 2 per character), the characters; nibbles low
/// first, as `scripts/e2e-cli.sh` builds it.
fn hp_string(header: &[u8; 8], chars: &[u8]) -> Vec<u8> {
    let len = 5 + 2 * chars.len();
    let mut nibbles = vec![0xC, 0x2, 0xA, 0x2, 0x0];
    nibbles.extend((0..5).map(|i| ((len >> (4 * i)) & 0xF) as u8));
    let mut out = header.to_vec();
    out.extend(nibbles.chunks(2).map(|p| p[0] | (p[1] << 4)));
    out.extend_from_slice(chars);
    out
}

/// 269 bytes: every byte value once.
fn all_bytes(header: &[u8; 8]) -> Vec<u8> {
    hp_string(header, &(0..=255).collect::<Vec<u8>>())
}

/// 1824 bytes = 1024 + 800: one 1k block, then 7 short-tail blocks.
fn big(header: &[u8; 8]) -> Vec<u8> {
    hp_string(header, &(0..1811).map(|i| i as u8).collect::<Vec<u8>>())
}

const H49: &[u8; 8] = b"HPHP49-C";
const H48: &[u8; 8] = b"HPHP48-R";

fn file_end(events: &[Event]) -> (Vec<u8>, usize, usize) {
    events
        .iter()
        .find_map(|e| match e {
            Event::FileEnd {
                data,
                last_block,
                padding,
            } => Some((data.clone(), *last_block, *padding)),
            _ => None,
        })
        .expect("no FileEnd")
}

fn started(events: &[Event]) -> Option<Check> {
    events.iter().find_map(|e| match e {
        Event::Started { check } => Some(*check),
        _ => None,
    })
}

fn cfg_1k() -> Config {
    Config {
        block_size: BlockSize::B1k,
        ..Config::default()
    }
}

fn cfg_receive(check: Check) -> Config {
    Config {
        check,
        ..Config::default()
    }
}

// ---- 49G ----

#[test]
fn g49_xrecv_asks_for_hp_crc() {
    // XRECV opens with `D`: 128-byte blocks with HP's CRC, SUB padding.
    let r = replay(Command::Send(all_bytes(H49)), Config::default(), G49_XRECV);
    assert_eq!(started(&r.events), Some(Check::HpCrc));
    assert_eq!(r.events.last(), Some(&Event::Done));
}

#[test]
fn g49_xrecv_1k_with_short_tail() {
    // One 1k block, then the 800-byte tail in seven 128-byte blocks; the 49G
    // stored the 1811-character string (checked with SIZE).
    let r = replay(Command::Send(big(H49)), cfg_1k(), G49_XRECV_1K);
    assert_eq!(started(&r.events), Some(Check::HpCrc));
    assert!(r.events.contains(&Event::Progress {
        bytes: 1024,
        total: Some(1824)
    }));
    assert_eq!(r.events.last(), Some(&Event::Done));
}

#[test]
fn g49_xsend_ignores_c_and_falls_back_to_checksum() {
    // Three `C`s go unanswered; the NAK gets checksum blocks. The 49G pads
    // the last block with memory contents, so the padding hint is 0 and only
    // the object walk can find the end.
    let r = replay(Command::Receive, cfg_receive(Check::Crc16), G49_XSEND);
    assert_eq!(started(&r.events), Some(Check::Checksum));
    let (data, last_block, padding) = file_end(&r.events);
    let file = all_bytes(H49);
    assert_eq!(data.len(), 384);
    assert_eq!(&data[..file.len()], file.as_slice());
    assert_eq!((last_block, padding), (128, 0));
    assert!(data[file.len()..].iter().any(|&b| b != 0));
    assert_eq!(r.events.last(), Some(&Event::Done));
}

#[test]
fn g49_xsend_hp_crc() {
    let r = replay(Command::Receive, cfg_receive(Check::HpCrc), G49_XSEND_HPCRC);
    assert_eq!(started(&r.events), Some(Check::HpCrc));
    let (data, _, _) = file_end(&r.events);
    let file = all_bytes(H49);
    assert_eq!(&data[..file.len()], file.as_slice());
    assert_eq!(r.events.last(), Some(&Event::Done));
}

#[test]
fn g49_xsend_1k_then_128_byte_tail() {
    // Asked with `D`, the 49G sends one 1k block and the tail in 128-byte
    // blocks, the same split hptx uses as sender.
    let r = replay(Command::Receive, cfg_receive(Check::HpCrc), G49_XSEND_1K);
    assert_eq!(started(&r.events), Some(Check::HpCrc));
    let (data, last_block, _) = file_end(&r.events);
    let file = big(H49);
    assert_eq!(data.len(), 1024 + 7 * 128);
    assert_eq!(&data[..file.len()], file.as_slice());
    assert_eq!(last_block, 128);
}

#[test]
fn g49_server_xrecv_never_starts() {
    // Inside the Kermit server XRECV fails with "Port Not Available"; the
    // server answers the C packet with a Send-Init (S) that it repeats. The
    // sender must not mistake any of it for a start character, and gives up
    // without sending CANs into the server.
    let mut r = replay(
        Command::Send(all_bytes(H49)),
        Config::default(),
        G49_SERVER_XRECV,
    );
    assert_eq!(r.events, vec![]);
    let due = r.xfer.next_timeout().expect("sender waits for a start");
    r.xfer.handle_timeout(due.max(r.now));
    assert_eq!(
        std::iter::from_fn(|| r.xfer.poll_event()).collect::<Vec<_>>(),
        vec![Event::Error(Error::Timeout)]
    );
    assert_eq!(r.xfer.poll_output(due), None);
}

// ---- 48GX ----

#[test]
fn gx_xrecv_checksum() {
    // XRECV opens with NAK: checksum, 128-byte blocks.
    let r = replay(Command::Send(all_bytes(H48)), Config::default(), GX_XRECV);
    assert_eq!(started(&r.events), Some(Check::Checksum));
    assert_eq!(r.events.last(), Some(&Event::Done));
}

#[test]
fn gx_xrecv_rejects_1k_blocks() {
    // The 48GX NAKs every STX block and cancels with three CANs after nine.
    // Recorded with 1k forced in checksum mode; by default the sender answers
    // a NAK start with 128-byte blocks.
    let cfg = Config {
        checksum_1k: true,
        ..cfg_1k()
    };
    let r = replay(Command::Send(big(H48)), cfg, GX_XRECV_1K);
    assert_eq!(r.events.last(), Some(&Event::Error(Error::RemoteCancelled)));
    let blocks = trace::parse(GX_XRECV_1K)
        .unwrap()
        .iter()
        .filter(|(d, b)| *d == Direction::Out && b.len() == 3 + 1024 + 1)
        .count();
    assert_eq!(blocks, 9);
}

#[test]
fn gx_xsend_falls_back_from_c() {
    let r = replay(Command::Receive, cfg_receive(Check::Crc16), GX_XSEND);
    assert_eq!(started(&r.events), Some(Check::Checksum));
    let (data, last_block, padding) = file_end(&r.events);
    let file = all_bytes(H48);
    assert_eq!(&data[..file.len()], file.as_slice());
    // The 48GX pads with zeros: 128 - 13 bytes of the last block.
    assert_eq!((last_block, padding), (128, 115));
}

#[test]
fn gx_xsend_falls_back_from_d() {
    // `D` is ignored as well. The object is the empty string that the
    // cancelled 1k XRECV above left in HPTXX.
    let r = replay(Command::Receive, cfg_receive(Check::HpCrc), GX_XSEND_HPCRC);
    assert_eq!(started(&r.events), Some(Check::Checksum));
    let (data, _, padding) = file_end(&r.events);
    assert_eq!(&data[..13], hp_string(H48, b"").as_slice());
    // The hint also swallows the length field's two zero bytes: a hint only.
    assert_eq!(padding, 128 - 11);
}

#[test]
fn gx_server_xsend_never_starts() {
    // Inside the server XSEND fails ("Port Not Available"); the server's
    // repeated S packet is skipped, our NAKs run out after
    // `Config::retries` and we cancel.
    let cfg = cfg_receive(Check::Checksum);
    let r = replay(Command::Receive, cfg, GX_SERVER_XSEND);
    assert_eq!(r.events, vec![Event::Error(Error::Timeout)]);
    let outs: Vec<Vec<u8>> = trace::parse(GX_SERVER_XSEND)
        .unwrap()
        .into_iter()
        .filter(|(d, _)| *d == Direction::Out)
        .map(|(_, b)| b)
        .collect();
    assert_eq!(outs.iter().filter(|b| **b == [NAK]).count(), 10);
    assert_eq!(outs.last(), Some(&vec![CAN, CAN, CAN]));
}
