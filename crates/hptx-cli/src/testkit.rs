//! An in-memory Kermit server for command tests: answers `C` and `G D`
//! with text and takes SENDs, block check type 1, like the 48SX traces.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hptx_core::charset::{decode, encode};
use hptx_core::transport::MemoryTransport;
use hptx_core::{Calculator, Options, Session};
use kermit_proto::codec::{BlockCheck, Deframer, Framing, Packet, parse_frame};
use kermit_proto::prefix::{self, Quoting};

/// What the client sent, decoded: host commands as typed, `G D`, and
/// `SEND NAME` for a received F packet.
pub type Log = Arc<Mutex<Vec<String>>>;

/// A calculator over the fake server. `reply` maps a logged command to the
/// reply: stack or listing text, or for `SEND NAME` the name the server
/// says it stored the file under.
pub fn calc(reply: impl FnMut(&str) -> String + Send + 'static) -> (Calculator, Log) {
    let log: Log = Arc::default();
    let transport = MemoryTransport::new(server(Arc::clone(&log), reply));
    let mut kermit = Options::default().kermit;
    kermit.timeout = Duration::from_millis(200);
    kermit.linger = Duration::ZERO;
    let options = Options {
        kermit,
        drain: Duration::ZERO,
        turnaround: Duration::ZERO,
    };
    let session = Session::new(Box::new(transport), options).unwrap();
    (Calculator::new(session), log)
}

/// The commands sent so far.
pub fn sent(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

fn server(
    log: Log,
    mut reply: impl FnMut(&str) -> String + Send + 'static,
) -> impl FnMut(&[u8]) -> Vec<Vec<u8>> + Send {
    let check = BlockCheck::Type1;
    let mut deframer = Deframer::new();
    let mut queue: Vec<Packet> = Vec::new();
    move |bytes| {
        let wire = |p: &Packet| p.encode(check, &Framing::default()).unwrap();
        let ack = |seq: u8, data: Vec<u8>| wire(&Packet::new(seq, b'Y', data));
        deframer.push(bytes);
        let mut out = Vec::new();
        while let Some(frame) = deframer.next_frame() {
            let p = parse_frame(&frame, check).unwrap();
            let data = prefix::decode(&p.data, &Quoting::default()).unwrap();
            match p.kind {
                b'C' | b'G' => {
                    let command = if p.kind == b'G' {
                        format!("G {}", decode(&data))
                    } else {
                        decode(&data)
                    };
                    log.lock().unwrap().push(command.clone());
                    let text = encode(&reply(&command)).unwrap();
                    queue = vec![Packet::new(1, b'X', Vec::new())];
                    let mut rest = text.as_slice();
                    while !rest.is_empty() {
                        let (enc, used) = prefix::encode(rest, &Quoting::default(), 90);
                        let seq = u8::try_from(queue.len() + 1).unwrap();
                        queue.push(Packet::new(seq, b'D', enc));
                        rest = &rest[used..];
                    }
                    let seq = u8::try_from(queue.len() + 1).unwrap();
                    queue.push(Packet::new(seq, b'Z', Vec::new()));
                    queue.push(Packet::new(seq + 1, b'B', Vec::new()));
                    out.push(wire(&Packet::new(0, b'S', b"~* @-#Y1 ".to_vec())));
                }
                // We receive a SEND: our parameters, then ACK everything;
                // the ACK to F carries the stored name.
                b'S' => out.push(ack(p.seq, b"~* @-#Y1 ".to_vec())),
                b'F' => {
                    let command = format!("SEND {}", decode(&data));
                    log.lock().unwrap().push(command.clone());
                    out.push(ack(p.seq, encode(&reply(&command)).unwrap()));
                }
                b'D' | b'Z' | b'B' => out.push(ack(p.seq, Vec::new())),
                b'Y' => {
                    if let Some(next) = queue.iter().find(|q| q.seq == p.seq + 1) {
                        out.push(wire(next));
                    }
                }
                b'E' => queue.clear(),
                kind => panic!("unexpected packet {}", char::from(kind)),
            }
        }
        out
    }
}
