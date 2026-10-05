//! Replays of traces recorded against the saturnng emulator
//! (`cargo run -p kermit-proto --example record`, see `emulator/README.md`).
//! Our side must produce the recorded bytes exactly; the calculator's side is
//! fed back in the recorded chunks.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::client::test_support::{replay, replay_client};
use crate::{Command, Config, Error, Event, OutgoingFile};

const SX_INFO: &str = include_str!("../traces/48sx-info.trace");
const SX_HOST: &str = include_str!("../traces/48sx-host.trace");
const SX_DIR: &str = include_str!("../traces/48sx-dir.trace");
const SX_SEND: &str = include_str!("../traces/48sx-send.trace");
const SX_GET: &str = include_str!("../traces/48sx-get.trace");
const SX_GET_MISSING: &str = include_str!("../traces/48sx-get-missing.trace");
const GX_INFO: &str = include_str!("../traces/48gx-info.trace");
const GX_HOST: &str = include_str!("../traces/48gx-host.trace");
const GX_DIR: &str = include_str!("../traces/48gx-dir.trace");
const GX_SEND: &str = include_str!("../traces/48gx-send.trace");
const GX_GET: &str = include_str!("../traces/48gx-get.trace");
const GX_GET_MISSING: &str = include_str!("../traces/48gx-get-missing.trace");
const GX_GET_BINARY: &str = include_str!("../traces/48gx-get-binary.trace");
const G49_HOST: &str = include_str!("../traces/49g-host.trace");
const G49_DIR: &str = include_str!("../traces/49g-dir.trace");
const G49_FINISH: &str = include_str!("../traces/49g-finish.trace");

fn host_reply() -> Vec<u8> {
    let mut text = b"1:".to_vec();
    text.extend_from_slice(&[b' '; 20]);
    text.extend_from_slice(b"42\r\n");
    text
}

fn received_data(events: &[Event]) -> Vec<u8> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Data(d) => Some(d.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn all_bytes_file() -> OutgoingFile {
    OutgoingFile {
        name: b"ALLB".to_vec(),
        data: (0..=255).collect(),
    }
}

fn server_text(events: &[Event]) -> Vec<u8> {
    match events {
        [Event::ServerText(text), Event::Done] => text.clone(),
        other => panic!("expected ServerText then Done, got {other:?}"),
    }
}

#[test]
fn sx_info() {
    let (client, events) = replay_client(Command::Info, Config::default(), SX_INFO);
    assert_eq!(events, vec![Event::Done]);
    let peer = client.peer_params().unwrap();
    assert_eq!((peer.maxl, peer.chkt, peer.qbin), (94, b'3', b' '));
}

#[test]
fn sx_host() {
    let events = replay(Command::Host(b"6 7 *".to_vec()), Config::default(), SX_HOST);
    assert_eq!(server_text(&events), host_reply());
}

#[test]
fn g49_host() {
    let events = replay(
        Command::Host(b"6 7 *".to_vec()),
        Config::default(),
        G49_HOST,
    );
    assert_eq!(server_text(&events), host_reply());
}

#[test]
fn sx_dir_has_no_header_line() {
    let events = replay(Command::Directory, Config::default(), SX_DIR);
    assert_eq!(server_text(&events), b"IOPAR 29.5 List 8861\r\n");
}

#[test]
fn g49_dir_has_header_line_and_trailing_dots() {
    let events = replay(Command::Directory, Config::default(), G49_DIR);
    let text = server_text(&events);
    assert!(
        text.starts_with(b"{ HOME } "),
        "{:?}",
        String::from_utf8_lossy(&text)
    );
    assert!(text.windows(17).any(|w| w == b"IOPAR 29.5 List 1"));
    assert!(text.ends_with(b".\r\n"));
}

#[test]
fn g49_finish() {
    assert_eq!(
        replay(Command::Finish, Config::default(), G49_FINISH),
        vec![Event::Done]
    );
}

#[test]
fn sx_send_all_byte_values() {
    let file = OutgoingFile {
        name: b"ALLB".to_vec(),
        data: (0..=255).collect(),
    };
    let events = replay(Command::Send(vec![file]), Config::default(), SX_SEND);
    assert_eq!(
        events.first(),
        Some(&Event::FileStart {
            name: b"ALLB".to_vec()
        })
    );
    assert!(events.contains(&Event::Progress {
        sent: 256,
        total: 256
    }));
    assert!(events.ends_with(&[Event::FileEnd { discarded: false }, Event::Done]));
}

#[test]
fn sx_get() {
    let events = replay(Command::Get(b"ALLB".to_vec()), Config::default(), SX_GET);
    assert_eq!(
        events.first(),
        Some(&Event::FileStart {
            name: b"ALLB".to_vec()
        })
    );
    assert!(events.ends_with(&[Event::FileEnd { discarded: false }, Event::Done]));
    let data: Vec<u8> = events
        .iter()
        .filter_map(|e| match e {
            Event::Data(d) => Some(d.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    // The SX transfers in ASCII mode by default: header, then the string.
    assert!(data.starts_with(b"%%HP: T(1)A(D)F(.);\r\n"));
    assert!(data.ends_with(&[0xfd, 0xfe, 0xff, b'"', b'\r', b'\n']));
}

#[test]
fn sx_get_missing_variable() {
    let events = replay(
        Command::Get(b"NOSUCH".to_vec()),
        Config::default(),
        SX_GET_MISSING,
    );
    assert_eq!(
        events,
        vec![Event::Error(Error::Remote(b"Undefined Name".to_vec()))]
    );
}

#[test]
fn stale_nak_before_recorded_host_reply() {
    // The stale NAK from an idle server arrives right after our C packet.
    let trace = SX_HOST.replacen("< ", "< \\x01# N3\\r\n< ", 1);
    let events = replay(Command::Host(b"6 7 *".to_vec()), Config::default(), &trace);
    assert_eq!(server_text(&events), host_reply());
}

#[test]
fn gx_info() {
    let (client, events) = replay_client(Command::Info, Config::default(), GX_INFO);
    assert_eq!(events, vec![Event::Done]);
    let peer = client.peer_params().unwrap();
    assert_eq!((peer.maxl, peer.chkt, peer.qbin), (94, b'3', b' '));
}

#[test]
fn gx_host() {
    let events = replay(Command::Host(b"6 7 *".to_vec()), Config::default(), GX_HOST);
    assert_eq!(server_text(&events), host_reply());
}

#[test]
fn gx_dir_has_header_line() {
    let events = replay(Command::Directory, Config::default(), GX_DIR);
    let text = server_text(&events);
    // Unlike the SX, the GX sends a `{ path } free-memory` line first.
    assert_eq!(text, b"{ HOME } 127847\r\nIOPAR 29.5 List 8861\r\n");
    assert!(!text.ends_with(b".\r\n"));
}

#[test]
fn gx_send_all_byte_values() {
    let events = replay(
        Command::Send(vec![all_bytes_file()]),
        Config::default(),
        GX_SEND,
    );
    assert_eq!(
        events.first(),
        Some(&Event::FileStart {
            name: b"ALLB".to_vec()
        })
    );
    assert!(events.contains(&Event::Progress {
        sent: 256,
        total: 256
    }));
    assert!(events.ends_with(&[Event::FileEnd { discarded: false }, Event::Done]));
}

#[test]
fn gx_get() {
    let events = replay(Command::Get(b"ALLB".to_vec()), Config::default(), GX_GET);
    assert_eq!(
        events.first(),
        Some(&Event::FileStart {
            name: b"ALLB".to_vec()
        })
    );
    assert!(events.ends_with(&[Event::FileEnd { discarded: false }, Event::Done]));
    // A fresh GX transfers in ASCII mode too (flag -35 clear).
    assert!(received_data(&events).starts_with(b"%%HP: T(1)A(D)F(.);\r\n"));
}

#[test]
fn gx_get_missing_variable() {
    let events = replay(
        Command::Get(b"NOSUCH".to_vec()),
        Config::default(),
        GX_GET_MISSING,
    );
    assert_eq!(
        events,
        vec![Event::Error(Error::Remote(b"Undefined Name".to_vec()))]
    );
}

#[test]
fn gx_get_binary() {
    let events = replay(
        Command::Get(b"ALLB".to_vec()),
        Config::default(),
        GX_GET_BINARY,
    );
    assert!(events.ends_with(&[Event::FileEnd { discarded: false }, Event::Done]));
    let data = received_data(&events);
    assert!(data.starts_with(b"HPHP48-R"), "{data:?}");
}
