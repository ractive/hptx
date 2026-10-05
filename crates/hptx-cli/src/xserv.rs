//! `hptx xserv`: the XSERV command server of the 49g+/50g (and the 49G after
//! ROM 1.10), thin over [`hptx_core::xserv`].
//!
//! **Unverified on hardware.** The framing follows the wiki's description of
//! HP's client code (wiki: protocols/xserv); the emulated 49G (ROM 2.15) has
//! no XSERV, so only the bytes hptx would send are tested, against an
//! in-memory calculator. The client lives here, not in `hptx-core`, because
//! the core module is sans-IO and nothing about the exchange (ACK after the
//! command packet, reply packets) has been seen on a real calculator yet.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Subcommand;
use hptx_core::calc::validate_name;
use hptx_core::object::ObjectType;
use hptx_core::transport::{self, Transport};
use hptx_core::xmodem_proto::codec::{ACK, NAK};
use hptx_core::xserv::{Deframer, DirRecord, XservCommand, parse_dir_list};
use hptx_core::{Error, Model, XmodemOptions, XmodemSession};
use serde_json::{Value, json};

use crate::commands::{Ctx, object_type_name_binary, refuse_existing_file, write_file};
use crate::error::Hinted;
use crate::output::{Hint, Outcome, shell_quote};
use crate::util;
use crate::xmodem::check_name;

/// Tries for a command packet before giving up (HP's client: 5).
const COMMAND_TRIES: u32 = 5;
/// Bad or missing reply packets before giving up (HP's client: 4).
const REPLY_FAILURES: u32 = 4;
/// How long the line must be quiet before a command byte.
const QUIET: Duration = Duration::from_millis(200);

/// Shown under every `xserv` subcommand's help.
const UNVERIFIED: &str = "UNVERIFIED ON HARDWARE: no calculator has answered hptx's XSERV yet \
(the emulated 49G has none). Start XSERV on the calculator instead of SERVER.";

/// The `xserv` subcommands.
#[derive(Subcommand, Debug)]
pub enum XservCmd {
    /// List the current directory (command L).
    #[command(after_help = UNVERIFIED)]
    Ls,
    /// Download a variable (command G, then XModem).
    #[command(after_help = UNVERIFIED)]
    Get {
        /// Variable in the current directory.
        name: String,
        /// Output file [default: NAME]; - for stdout.
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
        /// Replace an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Upload a file as a variable (command P, then XModem).
    #[command(after_help = UNVERIFIED)]
    Put {
        /// File to upload; - for stdin (then --as is required).
        file: PathBuf,
        /// Variable name [default: the file name without extension].
        #[arg(long = "as", value_name = "NAME")]
        name: Option<String>,
        /// Print the bytes hptx would send, open no link.
        #[arg(long)]
        dry_run: bool,
    },
    /// Execute RPL on the calculator (command E); results stay on its stack.
    #[command(after_help = UNVERIFIED)]
    Eval {
        /// RPL words, joined with spaces.
        #[arg(required = true, value_name = "RPL", allow_negative_numbers = true)]
        words: Vec<String>,
        /// Print the bytes hptx would send, open no link.
        #[arg(long)]
        dry_run: bool,
    },
    /// Free memory (command M); the reply's format is unknown, shown raw.
    #[command(after_help = UNVERIFIED)]
    Mem,
}

/// An XSERV link: command bytes, framed packets, ACK/NAK.
pub struct XservClient {
    transport: Box<dyn Transport>,
    /// Wait for an ACK or a reply packet.
    timeout: Duration,
    /// Quiet period before a command byte.
    quiet: Duration,
}

impl XservClient {
    /// Wrap an open link.
    pub fn new(transport: Box<dyn Transport>, timeout: Duration) -> Self {
        XservClient {
            transport,
            timeout,
            quiet: QUIET,
        }
    }

    /// Give the link back (for the XModem transfer of `P` and `G`).
    pub fn into_transport(self) -> Box<dyn Transport> {
        self.transport
    }

    /// Send `command`: wait for a quiet line, the command byte, then its
    /// command packet until the calculator ACKs it.
    pub fn send(&mut self, command: &XservCommand) -> hptx_core::Result<()> {
        let packet = command.packet()?;
        transport::drain(self.transport.as_mut(), self.quiet)?;
        self.transport.write_packet(&[command.byte()])?;
        let Some(packet) = packet else {
            return Ok(());
        };
        for _ in 0..COMMAND_TRIES {
            self.transport.write_packet(&packet)?;
            if self.wait_ack()? == Some(true) {
                return Ok(());
            }
            transport::drain(self.transport.as_mut(), self.quiet)?;
        }
        Err(Error::Reply(format!(
            "XSERV {}: no ACK for the command packet after {COMMAND_TRIES} tries",
            char::from(command.byte())
        )))
    }

    /// `Some(true)` on ACK, `Some(false)` on NAK, `None` on timeout; other
    /// bytes are skipped.
    fn wait_ack(&mut self) -> std::io::Result<Option<bool>> {
        let deadline = Instant::now() + self.timeout;
        let mut buf = [0u8; 64];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let n = self.transport.read(&mut buf, left)?;
            for &b in buf.get(..n).unwrap_or_default() {
                match b {
                    ACK => return Ok(Some(true)),
                    NAK => return Ok(Some(false)),
                    _ => {}
                }
            }
        }
    }

    /// Read one reply packet: ACK a good one, NAK a bad or missing one.
    pub fn reply(&mut self) -> hptx_core::Result<Vec<u8>> {
        let mut deframer = Deframer::new();
        let mut failures = 0;
        let mut buf = [0u8; 1024];
        let mut deadline = Instant::now() + self.timeout;
        loop {
            while let Some(packet) = deframer.next_packet() {
                match packet {
                    Ok(data) => {
                        self.transport.write_packet(&[ACK])?;
                        return Ok(data);
                    }
                    Err(e) => {
                        failures += 1;
                        if failures >= REPLY_FAILURES {
                            return Err(Error::Reply(format!("XSERV reply: {e}")));
                        }
                        deframer.clear();
                        self.transport.write_packet(&[NAK])?;
                        deadline = Instant::now() + self.timeout;
                    }
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                failures += 1;
                if failures >= REPLY_FAILURES {
                    return Err(Error::Reply(format!(
                        "XSERV reply: nothing complete after {REPLY_FAILURES} tries"
                    )));
                }
                deframer.clear();
                self.transport.write_packet(&[NAK])?;
                deadline = Instant::now() + self.timeout;
                continue;
            }
            let n = self.transport.read(&mut buf, left)?;
            deframer.push(buf.get(..n).unwrap_or_default());
        }
    }
}

/// Hex dump, `00 2a ff`.
fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The bytes `command` puts on the line: the command byte, then its packet.
fn wire_bytes(command: &XservCommand) -> Result<(u8, Option<Vec<u8>>)> {
    Ok((command.byte(), command.packet()?))
}

/// Type name for a listing record.
fn record_type(r: &DirRecord) -> String {
    ObjectType::from_prolog(u32::from(r.prolog)).map_or_else(
        || format!("prolog #{:04X}", r.prolog),
        |t| t.name().to_string(),
    )
}

/// The listing as text and JSON.
fn listing_outcome(records: &[DirRecord], ctx: &Ctx) -> Outcome {
    let width = records
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0)
        .max(4);
    let mut text = format!(
        "{} variable{}\n",
        records.len(),
        if records.len() == 1 { "" } else { "s" }
    );
    let mut results = Vec::new();
    for r in records {
        let size = f64::from(r.size_nibbles) / 2.0;
        let kind = record_type(r);
        let pad = width - r.name.chars().count();
        let _ = writeln!(
            text,
            "  {}{} {:>8}  {:<16} #{:04X}h",
            r.name,
            " ".repeat(pad),
            util::number_text(size),
            kind,
            r.crc
        );
        results.push(json!({
            "name": r.name,
            "size": util::number(size),
            "type": kind,
            "prolog": r.prolog,
            "crc": r.crc,
            "directory": r.is_directory(),
        }));
    }
    let hints = records
        .iter()
        .find(|r| !r.is_directory())
        .map(|r| {
            vec![Hint::cmd(
                format!("Download {}", r.name),
                ctx.cmd(&format!("xserv get {}", shell_quote(&r.name))),
            )]
        })
        .unwrap_or_default();
    Outcome {
        total: Some(records.len() as u64),
        results: Value::from(results),
        dir: None,
        hints,
        text,
    }
}

/// `--dry-run`: the bytes, no link.
fn dry_run_outcome(command: &XservCommand, what: String) -> Result<Outcome> {
    let (byte, packet) = wire_bytes(command)?;
    let packet = packet.unwrap_or_default();
    Ok(Outcome {
        results: json!({
            "command": char::from(byte).to_string(),
            "packet_hex": hex(&packet),
            "dry_run": true,
        }),
        total: None,
        dir: None,
        hints: Vec::new(),
        text: format!(
            "{what}\ncommand byte  {}\npacket        {}",
            char::from(byte),
            hex(&packet)
        ),
    })
}

impl Ctx {
    /// Open the link to a calculator running XSERV and change to `--dir`.
    fn xserv_open(&mut self) -> Result<XservClient> {
        let addr = crate::port::resolve(self.global.port.as_deref())?;
        self.link.addr = Some(addr.clone());
        let link = transport::open(&addr).with_context(|| format!("cannot open {addr}"))?;
        let mut client = XservClient::new(link, self.link.timeout);
        if let Some(dir) = self.global.dir.clone() {
            let path = std::iter::once("HOME".to_string())
                .chain(util::parse_dir(&dir))
                .collect::<Vec<_>>()
                .join(" ");
            client
                .send(&XservCommand::Execute(path.clone()))
                .with_context(|| format!("xserv: cd {path}"))?;
        }
        Ok(client)
    }

    /// XModem options for an XSERV transfer: the 49G profile, the transfer
    /// starts at once.
    fn xserv_xmodem(&self) -> Result<XmodemOptions> {
        let mut options = XmodemOptions::for_model(Model::Hp49G)?;
        options.start_timeout = self.link.timeout;
        Ok(options)
    }

    pub(crate) fn xserv(&mut self, command: XservCmd) -> Result<Option<Outcome>> {
        let label = |cmd: &str| format!("xserv {cmd} (unverified on hardware)");
        match command {
            XservCmd::Ls => {
                let mut client = self.xserv_open()?;
                client.send(&XservCommand::List).context(label("ls"))?;
                let data = client.reply().context(label("ls"))?;
                let records = parse_dir_list(&data)
                    .map_err(|e| Error::Reply(e.to_string()))
                    .context(label("ls"))?;
                Ok(Some(listing_outcome(&records, self)))
            }
            XservCmd::Mem => {
                let mut client = self.xserv_open()?;
                client.send(&XservCommand::Memory).context(label("mem"))?;
                let data = client.reply().context(label("mem"))?;
                let text = String::from_utf8_lossy(&data).to_string();
                Ok(Some(Outcome {
                    results: json!({"hex": hex(&data), "text": text}),
                    total: None,
                    dir: None,
                    hints: Vec::new(),
                    text: format!("reply ({} bytes): {}\ntext: {text}", data.len(), hex(&data)),
                }))
            }
            XservCmd::Eval { words, dry_run } => {
                let rpl = words.join(" ");
                let cmd = XservCommand::Execute(rpl.clone());
                if dry_run {
                    return dry_run_outcome(&cmd, format!("Would execute {rpl}")).map(Some);
                }
                let mut client = self.xserv_open()?;
                client.send(&cmd).context(label("eval"))?;
                Ok(Some(Outcome {
                    results: json!({"command": rpl}),
                    total: None,
                    dir: None,
                    hints: Vec::new(),
                    text: format!("Executed {rpl}; results stay on the calculator's stack."),
                }))
            }
            XservCmd::Get {
                name,
                output,
                force,
            } => self.xserv_get(&name, output.as_deref(), force),
            XservCmd::Put {
                file,
                name,
                dry_run,
            } => self.xserv_put(&file, name, dry_run).map(Some),
        }
    }

    fn xserv_get(
        &mut self,
        name: &str,
        output: Option<&Path>,
        force: bool,
    ) -> Result<Option<Outcome>> {
        validate_name(name).with_context(|| format!("xserv get {name}"))?;
        let to_stdout = output == Some(Path::new("-"));
        let file = output.map_or_else(|| PathBuf::from(name), Path::to_path_buf);
        if !to_stdout {
            refuse_existing_file(&file, force)?;
        }
        let options = self.xserv_xmodem()?;
        let mut client = self.xserv_open()?;
        let what = format!("xserv get {name} (unverified on hardware)");
        client
            .send(&XservCommand::Get(name.to_string()))
            .with_context(|| what.clone())?;
        let mut session = XmodemSession::new(client.into_transport(), options);
        let got = session.receive().with_context(|| what.clone())?;
        if to_stdout {
            let mut out = std::io::stdout().lock();
            out.write_all(&got.data).context("writing to stdout")?;
            out.flush().context("writing to stdout")?;
            return Ok(None);
        }
        write_file(&file, &got.data, force)?;
        let kind = object_type_name_binary(&got.data);
        Ok(Some(Outcome {
            results: json!({
                "name": name,
                "file": file.display().to_string(),
                "bytes": got.data.len(),
                "received": got.received,
                "stripped": got.stripped,
                "check": check_name(got.check),
                "type": kind,
            }),
            total: None,
            dir: None,
            hints: Vec::new(),
            text: format!(
                "{name} -> {} ({} bytes, xserv, {})",
                file.display(),
                got.data.len(),
                check_name(got.check)
            ),
        }))
    }

    fn xserv_put(&mut self, file: &Path, name: Option<String>, dry_run: bool) -> Result<Outcome> {
        let from_stdin = file == Path::new("-");
        let name = match (name, from_stdin) {
            (Some(n), _) => n,
            (None, true) => {
                return Err(Hinted::new(
                    "xserv put -: no variable name",
                    "give one with --as NAME",
                )
                .into());
            }
            (None, false) => util::name_from_file(file).unwrap_or_default(),
        };
        validate_name(&name).with_context(|| format!("xserv put {name}"))?;
        let data = if from_stdin {
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .context("reading stdin")?;
            buf
        } else {
            std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?
        };
        let label = file.display().to_string();
        let cmd = XservCommand::Put(name.clone());
        if dry_run {
            return dry_run_outcome(
                &cmd,
                format!(
                    "Would store {label} ({} bytes) as {name}, then send it by XModem",
                    data.len()
                ),
            );
        }
        let options = self.xserv_xmodem()?;
        let mut client = self.xserv_open()?;
        let what = format!("xserv put {name} (unverified on hardware)");
        client.send(&cmd).with_context(|| what.clone())?;
        let mut session = XmodemSession::new(client.into_transport(), options);
        let report = session.send(&data).with_context(|| what.clone())?;
        Ok(Outcome {
            results: json!({
                "file": label,
                "name": name,
                "bytes": report.bytes,
                "check": check_name(report.check),
            }),
            total: None,
            dir: None,
            hints: vec![Hint::cmd("List the directory", self.cmd("xserv ls"))],
            text: format!(
                "{label} -> {name} ({} bytes, xserv, {})",
                report.bytes,
                check_name(report.check)
            ),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use hptx_core::transport::MemoryTransport;
    use hptx_core::xserv::encode_packet;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<Vec<u8>>>>;

    /// A calculator that logs every write and answers with `reply`.
    fn client(mut reply: impl FnMut(&[u8]) -> Vec<Vec<u8>> + Send + 'static) -> (XservClient, Log) {
        let log: Log = Arc::default();
        let seen = log.clone();
        let transport = MemoryTransport::new(move |w| {
            seen.lock().unwrap().push(w.to_vec());
            reply(w)
        });
        let mut c = XservClient::new(Box::new(transport), Duration::from_millis(20));
        c.quiet = Duration::from_millis(1);
        (c, log)
    }

    /// ACK every framed packet (two or more bytes), nothing else.
    fn ack_packets(w: &[u8]) -> Vec<Vec<u8>> {
        if w.len() > 1 { vec![vec![ACK]] } else { vec![] }
    }

    #[test]
    fn get_sends_g_then_the_name_packet() {
        let (mut c, log) = client(ack_packets);
        c.send(&XservCommand::Get("ABC".into())).unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec![b"G".to_vec(), vec![0x00, 0x03, b'A', b'B', b'C', 0xC6]]
        );
    }

    #[test]
    fn put_resends_the_packet_after_a_nak() {
        let mut naks = 1;
        let (mut c, log) = client(move |w| {
            if w.len() == 1 {
                return vec![];
            }
            if naks > 0 {
                naks -= 1;
                return vec![vec![NAK]];
            }
            vec![vec![ACK]]
        });
        c.send(&XservCommand::Put("X".into())).unwrap();
        let packet = vec![0x00, 0x01, b'X', b'X'];
        assert_eq!(
            *log.lock().unwrap(),
            vec![b"P".to_vec(), packet.clone(), packet]
        );
    }

    #[test]
    fn eval_sends_the_rpl_in_the_hp_charset() {
        let (mut c, log) = client(ack_packets);
        c.send(&XservCommand::Execute("HOME \\->A".into())).unwrap();
        let log = log.lock().unwrap();
        assert_eq!(log[0], b"E");
        let mut data = b"HOME ".to_vec();
        data.extend([0x8D, b'A']);
        assert_eq!(log[1], encode_packet(&data).unwrap());
    }

    #[test]
    fn no_ack_gives_up_after_five_tries() {
        let (mut c, log) = client(|_| vec![]);
        let err = c.send(&XservCommand::Get("X".into())).unwrap_err();
        assert!(err.to_string().contains("no ACK"), "{err}");
        assert_eq!(log.lock().unwrap().len(), 1 + 5);
    }

    #[test]
    fn ls_acks_the_reply_and_parses_it() {
        let records = [&[1][..], b"S", &[0x2C, 0x2A, 0x0A, 0, 0, 0xEF, 0xBE]].concat();
        let reply = encode_packet(&records).unwrap();
        let (mut c, log) = client(move |w| {
            if w == b"L" {
                vec![reply.clone()]
            } else {
                vec![]
            }
        });
        c.send(&XservCommand::List).unwrap();
        let data = c.reply().unwrap();
        assert_eq!(*log.lock().unwrap(), vec![b"L".to_vec(), vec![ACK]]);
        let list = parse_dir_list(&data).unwrap();
        assert_eq!(list[0].name, "S");
        assert_eq!(record_type(&list[0]), "String");
        assert_eq!(list[0].size_nibbles, 10);
    }

    #[test]
    fn a_bad_reply_is_naked_and_resent() {
        let good = encode_packet(b"42").unwrap();
        let mut bad = good.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0xFF;
        let (mut c, log) = client(move |w| match w {
            b"M" => vec![bad.clone()],
            [NAK] => vec![good.clone()],
            _ => vec![],
        });
        c.send(&XservCommand::Memory).unwrap();
        assert_eq!(c.reply().unwrap(), b"42");
        assert_eq!(
            *log.lock().unwrap(),
            vec![b"M".to_vec(), vec![NAK], vec![ACK]]
        );
    }

    #[test]
    fn dry_run_shows_the_bytes() {
        let o = dry_run_outcome(&XservCommand::Put("AB".into()), "x".into()).unwrap();
        assert_eq!(o.results["command"], "P");
        assert_eq!(o.results["packet_hex"], "00 02 41 42 83");
    }
}
