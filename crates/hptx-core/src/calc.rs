//! What a user does with a calculator in Kermit server mode: list, change
//! directory, get, put, run host commands, fetch PICT, backup and restore.
//!
//! Everything is built from `C` host commands, `G D` listings, GET and SEND
//! (wiki: protocols/server-commands). A host command returns the whole stack
//! as display text and leaves its results on the user's stack, so every
//! internal query drops what it pushed. A failed command replies `Error: X`
//! (no E packet) and leaves its arguments on the stack. Evaluating an
//! undefined name pushes the name and evaluating a variable runs it, so names
//! are checked against the listing before they are evaluated.

use std::time::Duration;

use kermit_proto::{Command, OutgoingFile};

use crate::charset::{decode, encode, encode_command};
use crate::grob::Grob;
use crate::object::{HEADER_LEN, KERMIT_PADDING_ALLOWANCE, ObjectType, inspect};
use crate::object::{object_size, strip_padding, unpack};
use crate::reply::{Iopar, Listing, StackReply, parse_list, parse_listing, parse_real};
use crate::reply::{parse_name, parse_stack, parse_string};
use crate::session::Session;
use crate::transport::Transport;
use crate::xmodem::{XmodemDirection, XmodemPlan};
use crate::{Error, Result};

/// Temporary variable for [`Calculator::pict`].
const PICT_VAR: &str = "HPTXTMP";
/// Temporary variable and port-0 object for [`Calculator::backup`].
const BACKUP_VAR: &str = "HPTXBK";
/// Temporary variable and port-0 object for [`Calculator::restore`].
const RESTORE_VAR: &str = "HPTXRS";
/// Reply timeout for the final `RESTORE`, which never gets a reply (the
/// session's timeout when that is shorter).
const RESTORE_TIMEOUT: Duration = Duration::from_secs(3);

/// Calculator model, as far as the ROM version text tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// HP 48S/SX: no `VERSION` command, no XModem.
    Hp48Sx,
    /// HP 48G/GX: `HP48-x, Copyright HP 1993`; XModem with checksum only.
    Hp48Gx,
    /// HP 49G: XModem with checksum, HP's CRC (`D`) and 1k blocks.
    Hp49G,
    /// Anything else.
    Unknown,
}

impl Model {
    /// The model from the `VERSION` text ([`Calculator::version`]): `None`
    /// is the 48S/SX, which lacks the command; the 48G/GX says
    /// `HP48-R, Copyright HP 1993`; the 49G also says `HP48-C Revision ...`
    /// (cut by its display width) and is told apart by its copyright year
    /// (1999 or later) or an `HP49`.
    pub fn from_version(version: Option<&str>) -> Model {
        let Some(v) = version else {
            return Model::Hp48Sx;
        };
        let year = v
            .rsplit(|c: char| !c.is_ascii_digit())
            .find(|w| w.len() == 4)
            .and_then(|w| w.parse::<u32>().ok());
        if v.contains("HP49") || year.is_some_and(|y| y >= 1999) {
            Model::Hp49G
        } else if v.contains("HP48") {
            Model::Hp48Gx
        } else {
            Model::Unknown
        }
    }

    /// Display name, e.g. `HP 48G/GX`.
    pub fn name(self) -> &'static str {
        match self {
            Model::Hp48Sx => "HP 48S/SX",
            Model::Hp48Gx => "HP 48G/GX",
            Model::Hp49G => "HP 49G",
            Model::Unknown => "unknown",
        }
    }

    /// Whether the model has `XRECV`/`XSEND` (unknown models are given the
    /// benefit of the doubt).
    pub fn has_xmodem(self) -> bool {
        self != Model::Hp48Sx
    }
}

/// Kermit transfer format, flag -35 on the calculator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferMode {
    /// Flag -35 set: GET returns `HPHP48-x` / `HPHP49-x` + object; PUT of a
    /// file that is not an HP binary object stores it as a String.
    Binary,
    /// Flag -35 clear (a fresh calculator): objects travel as `%%HP:` text.
    Ascii,
}

/// A calculator in Kermit server mode.
pub struct Calculator {
    session: Session,
    /// Last transfer mode set or read by us; `None` = unknown.
    mode: Option<TransferMode>,
}

impl Calculator {
    /// Wrap an open session. The transfer mode is unknown until read or set.
    pub fn new(session: Session) -> Self {
        Calculator {
            session,
            mode: None,
        }
    }

    /// Open `addr` (see [`crate::transport::open`]) with default options
    /// and [`sync`](Calculator::sync).
    pub fn open(addr: &str) -> Result<Self> {
        let mut calc = Calculator::new(Session::open(addr)?);
        calc.sync()?;
        Ok(calc)
    }

    /// Get in step with the server at the start of a session.
    ///
    /// When a client dies while a host command runs, the calculator still
    /// finishes the command and offers its reply (an S packet repeated
    /// every 5 s for about a minute). The next client's first command is
    /// eaten as a bad ACK and the late reply arrives in its place. Sequence
    /// numbers restart at zero for every command and host commands and
    /// `G D` both answer with text, so the late reply cannot be told apart
    /// on the wire, and repeating an arbitrary command would repeat its
    /// effect. Instead the session starts with a sacrificial command that
    /// pushes a marker string unique to this session ([`sync_marker`],
    /// short enough that no model truncates it). A reply whose level 1 is
    /// the marker is ours: the marker is dropped (every copy of it on top
    /// of the stack, in case an earlier attempt was not eaten after all).
    /// Any other reply was a late one and our command was eaten: the marker
    /// command is sent once more. Nothing but the marker is ever dropped
    /// and nothing else is ever resent. Link errors are returned; a second
    /// odd reply is [`Error::Reply`]: hptx and the calculator are not in
    /// step, and nothing is run on a stack it cannot vouch for.
    pub fn sync(&mut self) -> Result<()> {
        self.sync_with(&sync_marker())
    }

    fn sync_with(&mut self, marker: &str) -> Result<()> {
        let command = format!("\"{marker}\"");
        for attempt in 0..2 {
            // The first attempt is sent once and gets one timeout period
            // (20 s by default), not the whole retry budget; the second has
            // the normal budget: the marker is the one command that may
            // safely run twice, only markers are ever dropped.
            let result = if attempt == 0 {
                self.host(&command)
            } else {
                self.host_retrying(&command)
            };
            let reply = match result {
                // The calculator aborted a transfer left over from the dead
                // client (seen: "Transfer Failed" on the 48SX): an odd
                // reply like any other.
                Err(Error::Remote(_)) => continue,
                // The late reply and our command crossed and the exchange
                // stalled (seen on the emulated 49G under load: the marker
                // ran, its reply never came). Once more: the second reply
                // shows both markers and both are dropped.
                Err(Error::NoReply { .. }) if attempt == 0 => continue,
                reply => reply?,
            };
            let ours = reply
                .levels
                .iter()
                .take_while(|level| parse_string(level).as_deref() == Some(marker))
                .count();
            if reply.error.is_none() && ours > 0 {
                let mut left = ours;
                while left > 0 {
                    let n = left.min(2);
                    self.drop_levels(n)?;
                    left -= n;
                }
                return Ok(());
            }
        }
        Err(Error::Reply(format!(
            "not in step with the calculator: neither reply showed the sync marker {marker}"
        )))
    }

    /// The underlying session. Forgets the cached transfer mode, since the
    /// caller may change flag -35 through it.
    pub fn session(&mut self) -> &mut Session {
        self.mode = None;
        &mut self.session
    }

    /// Run a host command (Unicode or ASCII trigraphs such as `\->`) and
    /// return the stack. An `Error:` reply is `Ok` with `error` set. Forgets
    /// the cached transfer mode, since the command may change flag -35.
    /// The command is sent once: no reply in time is [`Error::NoReply`],
    /// never a second run.
    pub fn run(&mut self, command: &str) -> Result<StackReply> {
        self.mode = None;
        self.host(command)
    }

    /// Send `command` as a `C` packet exactly once and parse the stack
    /// reply. A `C` resent after a lost or late ACK runs again on the
    /// calculator, which cannot tell it from a new command, so the packet
    /// is never retransmitted: no retries, and a NAK (usually a stale one
    /// from the idle server) does not shorten the wait. No reply within the
    /// timeout is [`Error::NoReply`].
    fn host(&mut self, command: &str) -> Result<StackReply> {
        let timeout = self.session.config().timeout;
        self.host_once(command, timeout)
    }

    /// [`host`](Calculator::host) with a reply timeout of `timeout`.
    fn host_once(&mut self, command: &str, timeout: Duration) -> Result<StackReply> {
        let normal = self.session.config().clone();
        let mut once = normal.clone();
        once.timeout = timeout;
        once.retries = 0;
        once.nak_grace = timeout;
        self.session.set_config(once);
        let result = self.host_retrying(command);
        self.session.set_config(normal);
        match result {
            Err(Error::Kermit(kermit_proto::Error::Timeout)) => Err(Error::NoReply {
                command: command.to_string(),
            }),
            other => other,
        }
    }

    /// Send `command` as a `C` packet with the session's retries and parse
    /// the stack reply. Only for a command that may run twice (the sync
    /// marker).
    fn host_retrying(&mut self, command: &str) -> Result<StackReply> {
        let bytes = encode_command(command)?;
        let transcript = self.session.transact(Command::Host(bytes))?;
        Ok(parse_stack(&decode(&transcript.text)))
    }

    /// Run `parts` joined by spaces as one command; if that is too long for
    /// a packet, run each part on its own in order. The first reply with an
    /// error becomes [`Error::Calculator`].
    fn exec(&mut self, parts: &[String]) -> Result<StackReply> {
        match self.host(&parts.join(" ")) {
            Err(Error::CommandTooLong { .. }) if parts.len() > 1 => {
                let mut last = StackReply::default();
                for part in parts {
                    last = checked(self.host(part)?)?;
                }
                Ok(last)
            }
            reply => checked(reply?),
        }
    }

    /// Run `command`, read `levels` stack levels (level 1 first) and drop
    /// them again.
    fn query(&mut self, command: &str, levels: usize) -> Result<Vec<String>> {
        let reply = checked(self.host(command)?)?;
        if reply.levels.len() < levels {
            return Err(Error::Reply(format!(
                "{command}: expected {levels} stack level(s), got {:?}",
                reply.levels
            )));
        }
        let values = reply.levels[..levels].to_vec();
        self.drop_levels(levels)?;
        Ok(values)
    }

    /// Drop `n` levels (1 or 2) pushed by an internal query.
    fn drop_levels(&mut self, n: usize) -> Result<()> {
        let command = if n >= 2 { "DROP2" } else { "DROP" };
        checked(self.host(command)?).map(|_| ())
    }

    /// The current directory listing (`G D`).
    pub fn list(&mut self) -> Result<Listing> {
        let transcript = self.session.transact(Command::Directory)?;
        parse_listing(&decode(&transcript.text))
    }

    /// The current directory path, e.g. `["HOME", "D1"]`: from the listing
    /// header (48GX, 49G) or a `PATH` query (48SX, whose listing has none).
    pub fn path(&mut self) -> Result<Vec<String>> {
        if let Some(path) = self.list()?.path {
            return Ok(path);
        }
        let [value]: [String; 1] = self
            .query("PATH", 1)?
            .try_into()
            .map_err(|_| Error::Reply("PATH: no value".into()))?;
        parse_list(&value).ok_or_else(|| Error::Reply(format!("PATH: not a list: {value:?}")))
    }

    /// Change to the absolute directory `path`, e.g. `["HOME", "D1"]` (the
    /// leading `HOME` is optional). Each component is checked against the
    /// listing before it is evaluated.
    pub fn cd(&mut self, path: &[&str]) -> Result<()> {
        let components = match path.split_first() {
            Some((&"HOME", rest)) => rest,
            _ => path,
        };
        for name in components {
            validate_name(name)?;
        }
        self.exec(&["HOME".to_string()])?;
        for name in components {
            let listing = self.list()?;
            let is_dir = listing
                .entries
                .iter()
                .any(|e| e.name == *name && e.is_directory());
            if !is_dir {
                return Err(Error::Reply(format!("{name}: no such directory")));
            }
            self.exec(&[(*name).to_string()])?;
        }
        Ok(())
    }

    /// Go to the parent directory (`UPDIR`).
    pub fn updir(&mut self) -> Result<()> {
        self.exec(&["UPDIR".to_string()]).map(|_| ())
    }

    /// Create directory `name` in the current directory (`CRDIR`).
    pub fn mkdir(&mut self, name: &str) -> Result<()> {
        self.exec(&[format!("{} CRDIR", quote(name)?)]).map(|_| ())
    }

    /// Delete variable `name` from the current directory; a directory is
    /// deleted with its contents (`PGDIR`).
    pub fn remove(&mut self, name: &str) -> Result<()> {
        let quoted = quote(name)?;
        let is_dir = self
            .list()?
            .entries
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.is_directory())
            .ok_or_else(|| Error::Reply(format!("{name}: no such variable")))?;
        let purge = if is_dir { "PGDIR" } else { "PURGE" };
        self.exec(&[format!("{quoted} {purge}")]).map(|_| ())
    }

    /// Rename variable `from` to `to` in the current directory (`RCL`, `STO`,
    /// then `PURGE` or `PGDIR`). `to` must not exist yet.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let quoted_from = quote(from)?;
        let quoted_to = quote(to)?;
        let listing = self.list()?;
        let is_dir = listing
            .entries
            .iter()
            .find(|e| e.name == from)
            .map(|e| e.is_directory())
            .ok_or_else(|| Error::Reply(format!("{from}: no such variable")))?;
        if listing.entries.iter().any(|e| e.name == to) {
            return Err(Error::Reply(format!("{to}: already exists")));
        }
        let purge = if is_dir { "PGDIR" } else { "PURGE" };
        self.exec(&[
            format!("{quoted_from} RCL"),
            format!("{quoted_to} STO"),
            format!("{quoted_from} {purge}"),
        ])
        .map(|_| ())
    }

    /// Free memory in bytes (`MEM`).
    pub fn mem(&mut self) -> Result<f64> {
        let values = self.query("MEM", 1)?;
        let value = values.first().map(String::as_str).unwrap_or_default();
        parse_real(value).ok_or_else(|| Error::Reply(format!("MEM: not a number: {value:?}")))
    }

    /// The ROM version text, e.g. `Version HP48-R, Copyright HP 1993`;
    /// `None` on the 48SX, which has no `VERSION` command.
    pub fn version(&mut self) -> Result<Option<String>> {
        let reply = checked(self.host("VERSION")?)?;
        // 48SX: the undefined name evaluates to itself.
        let level1 = reply.level(1).unwrap_or_default().trim();
        if level1 == "VERSION" || parse_name(level1).as_deref() == Some("VERSION") {
            self.drop_levels(1)?;
            return Ok(None);
        }
        let (Some(l2), Some(l1)) = (reply.level(2), reply.level(1)) else {
            return Err(Error::Reply(format!(
                "VERSION: expected two strings, got {:?}",
                reply.levels
            )));
        };
        let bad = |s: &str| Error::Reply(format!("VERSION: not a string: {s:?}"));
        let version = parse_string(l2).ok_or_else(|| bad(l2))?;
        let copyright = parse_string(l1).ok_or_else(|| bad(l1))?;
        let version = version.split_whitespace().collect::<Vec<_>>().join(" ");
        self.drop_levels(2)?;
        Ok(Some(format!("{version}, {copyright}")))
    }

    /// The serial settings (`IOPAR`).
    pub fn iopar(&mut self) -> Result<Iopar> {
        let values = self.query("IOPAR", 1)?;
        let value = values.first().map(String::as_str).unwrap_or_default();
        Iopar::parse(value).ok_or_else(|| Error::Reply(format!("IOPAR: bad list: {value:?}")))
    }

    /// Store `iopar` as `IOPAR` in HOME and return to the current directory.
    /// Takes effect when the calculator reopens the port (e.g. the next
    /// `SERVER`), not in the running session.
    pub fn set_iopar(&mut self, iopar: &Iopar) -> Result<()> {
        self.exec(&[
            "PATH HOME".to_string(),
            format!("{} 'IOPAR' STO", iopar.to_rpl()),
            "EVAL".to_string(),
        ])
        .map(|_| ())
    }

    /// Read the transfer mode (flag -35) and cache it.
    pub fn transfer_mode(&mut self) -> Result<TransferMode> {
        let values = self.query("-35 FS?", 1)?;
        let value = values.first().map(String::as_str).unwrap_or_default();
        let mode = match parse_real(value) {
            Some(1.0) => TransferMode::Binary,
            Some(0.0) => TransferMode::Ascii,
            _ => return Err(Error::Reply(format!("-35 FS?: not a flag: {value:?}"))),
        };
        self.mode = Some(mode);
        Ok(mode)
    }

    /// Set the transfer mode (flag -35) unless the cached mode already is
    /// `mode`.
    pub fn set_transfer_mode(&mut self, mode: TransferMode) -> Result<()> {
        if self.mode == Some(mode) {
            return Ok(());
        }
        let command = match mode {
            TransferMode::Binary => "-35 SF",
            TransferMode::Ascii => "-35 CF",
        };
        self.exec(&[command.to_string()])?;
        self.mode = Some(mode);
        Ok(())
    }

    /// GET variable `name` from the current directory in `mode`. Binary
    /// data is cut after the object (the calculator pads the last packet).
    pub fn get(&mut self, name: &str, mode: TransferMode) -> Result<Vec<u8>> {
        validate_name(name)?;
        self.set_transfer_mode(mode)?;
        let transcript = self.session.transact(Command::Get(encode(name)?))?;
        let [file]: [_; 1] = transcript.files.try_into().map_err(|files: Vec<_>| {
            Error::Reply(format!(
                "GET {name}: expected one file, got {}",
                files.len()
            ))
        })?;
        Ok(match mode {
            TransferMode::Binary => strip_padding(&file.data, KERMIT_PADDING_ALLOWANCE).to_vec(),
            TransferMode::Ascii => file.data,
        })
    }

    /// SEND `data` as variable `name` in `mode` and return the name the
    /// calculator stored it under (it can differ, e.g. a `.1` suffix when
    /// the name exists).
    pub fn put(&mut self, name: &str, data: &[u8], mode: TransferMode) -> Result<String> {
        validate_name(name)?;
        self.set_transfer_mode(mode)?;
        let file = OutgoingFile {
            name: encode(name)?,
            data: data.to_vec(),
        };
        let transcript = self.session.transact(Command::Send(vec![file]))?;
        transcript
            .stored_names
            .into_iter()
            .next()
            .ok_or_else(|| Error::Reply(format!("SEND {name}: no file stored")))
    }

    /// Fetch the graphics screen PICT (plots, drawings) via `PICT RCL`, a
    /// temporary variable `HPTXTMP` and a binary GET; the variable is purged
    /// afterwards. A calculator that never drew anything has a 0x0 PICT;
    /// `ERASE` makes it 131x64. The display itself cannot be captured over
    /// the link: `LCD→` in server mode only sees the server's own banner.
    /// Leaves the calculator in binary transfer mode.
    pub fn pict(&mut self) -> Result<Grob> {
        self.refuse_existing(PICT_VAR)?;
        self.exec(&[format!("PICT RCL '{PICT_VAR}' STO")])?;
        let data = self.get(PICT_VAR, TransferMode::Binary);
        let purge = self.exec(&[format!("'{PICT_VAR}' PURGE")]);
        let data = data?;
        purge?;
        Grob::from_file(&data)
    }

    /// Back up HOME: `ARCHIVE` into port 0, recall it into a temporary
    /// variable `HPTXBK`, GET that in binary mode, then remove both. Returns
    /// the binary transfer file of the Directory object. `ARCHIVE :IO:name`
    /// fails in server mode ("Port Not Available"), hence the detour.
    /// Leaves the calculator in binary transfer mode.
    pub fn backup(&mut self) -> Result<Vec<u8>> {
        self.refuse_existing(BACKUP_VAR)?;
        self.exec(&[format!(":0:{BACKUP_VAR} ARCHIVE")])?;
        let result = self.backup_steps();
        if result.is_err() {
            // Best effort: the error being reported matters more.
            let _ = self.exec(&[format!(":0:{BACKUP_VAR} PURGE")]);
            let _ = self.exec(&[format!("'{BACKUP_VAR}' PGDIR")]);
        }
        let data = result?;
        let info = inspect(&data)?;
        if info.object_type != Some(ObjectType::Directory) {
            return Err(Error::Object(format!(
                "backup is not a directory (prolog {:05X})",
                info.prolog
            )));
        }
        Ok(data)
    }

    fn backup_steps(&mut self) -> Result<Vec<u8>> {
        // STO before PURGE, else "Object In Use".
        self.exec(&[
            format!(":0:{BACKUP_VAR} RCL"),
            format!("'{BACKUP_VAR}' STO"),
            format!(":0:{BACKUP_VAR} PURGE"),
        ])?;
        let data = self.get(BACKUP_VAR, TransferMode::Binary)?;
        self.exec(&[format!("'{BACKUP_VAR}' PGDIR")])?;
        Ok(data)
    }

    /// Replace HOME with the backup `data` (from [`Calculator::backup`]):
    /// binary PUT as `HPTXRS`, copy to port 0, `RESTORE` from there. The
    /// calculator warm-starts and leaves server mode; restart `SERVER` on
    /// it, then call [`Calculator::purge_restore_leftover`] to delete
    /// `:0:HPTXRS`. `data` must be a Directory whose object walk succeeds
    /// (a truncated file never reaches `RESTORE`).
    pub fn restore(&mut self, data: &[u8]) -> Result<()> {
        check_backup(data)?;
        self.refuse_existing(RESTORE_VAR)?;
        let stored = self.put(RESTORE_VAR, data, TransferMode::Binary)?;
        if stored != RESTORE_VAR {
            return Err(Error::Reply(format!(
                "backup stored as {stored}, expected {RESTORE_VAR}"
            )));
        }
        let copied = self.exec(&[
            format!("'{RESTORE_VAR}' RCL"),
            format!(":0:{RESTORE_VAR} STO"),
            format!("'{RESTORE_VAR}' PGDIR"),
        ]);
        if let Err(e) = copied {
            let _ = self.exec(&[format!("'{RESTORE_VAR}' PGDIR")]);
            return Err(e);
        }
        self.restore_from_port()
    }

    /// `RESTORE` from `:0:HPTXRS`. The warm start ends server mode, so no
    /// reply ever comes; but no reply is also what a lost `C` gives. A
    /// probe tells the two apart: the calculator still answering means
    /// `RESTORE` did not run ([`Error::Reply`], `:0:HPTXRS` stays).
    fn restore_from_port(&mut self) -> Result<()> {
        self.mode = None;
        let timeout = self.session.config().timeout;
        let restore = format!(":0:{RESTORE_VAR} RESTORE");
        match self.host_once(&restore, RESTORE_TIMEOUT.min(timeout)) {
            Err(Error::NoReply { .. }) => {}
            Err(e) => return Err(e),
            Ok(reply) => {
                checked(reply)?;
                return Err(Error::Reply("calculator did not restart".into()));
            }
        }
        let marker = sync_marker();
        let probe = self.host_once(&format!("\"{marker}\""), timeout);
        let still_running = || {
            Error::Reply(format!(
                "RESTORE did not run: the calculator still answers in server mode; HOME is \
                 unchanged and the backup stays in :0:{RESTORE_VAR}"
            ))
        };
        match probe {
            Err(Error::NoReply { .. }) => Ok(()),
            Err(Error::Remote(_)) => Err(still_running()),
            Err(e) => Err(e),
            Ok(reply) => {
                if reply.error.is_none()
                    && reply.level(1).and_then(parse_string).as_deref() == Some(marker.as_str())
                {
                    // Best effort: the probe's own marker; the error matters
                    // more.
                    let _ = self.drop_levels(1);
                }
                Err(still_running())
            }
        }
    }

    /// Delete the backup object `:0:HPTXRS` that [`Calculator::restore`]
    /// leaves in port 0, once `SERVER` runs again.
    pub fn purge_restore_leftover(&mut self) -> Result<()> {
        self.exec(&[format!(":0:{RESTORE_VAR} PURGE")]).map(|_| ())
    }

    /// End server mode (`G F`).
    pub fn finish(&mut self) -> Result<()> {
        self.session.transact(Command::Finish).map(|_| ())
    }

    /// The underlying session, consuming the calculator.
    pub fn into_session(self) -> Session {
        self.session
    }

    /// The open link, consuming the calculator, e.g. for an
    /// [`XmodemSession`](crate::XmodemSession) after
    /// [`Calculator::prepare_for_xmodem`].
    pub fn into_transport(self) -> Box<dyn Transport> {
        self.session.into_transport()
    }

    /// The model, from the `VERSION` text (see [`Model::from_version`]).
    pub fn model(&mut self) -> Result<Model> {
        Ok(Model::from_version(self.version()?.as_deref()))
    }

    /// Get the calculator ready for an XModem transfer of variable `name`
    /// in the current directory and say what the user must type.
    ///
    /// `XRECV` and `XSEND` cannot be started through the Kermit server (they
    /// fail with "Port Not Available" on the 49G and 48GX), so this checks
    /// the name (it must exist for `XSEND`), switches a 49G in algebraic
    /// mode to RPN (flag -95; typed `'NAME' XRECV` needs RPN), and ends
    /// server mode with Kermit FINISH. An existing `NAME` is refused for
    /// `XRECV` on the 48G/GX ("XRECV Error: Name Conflict"); the 49G stores
    /// the object as `NAME.1` instead, which the plan's notes say. The user then types
    /// [`XmodemPlan::keys`] on the calculator while an
    /// [`XmodemSession`](crate::XmodemSession) on
    /// [`Calculator::into_transport`] waits. Afterwards the calculator is
    /// out of server mode with an empty stack: `SERVER` must be typed again.
    /// Errors for the 48S/SX, which has no XModem.
    pub fn prepare_for_xmodem(
        &mut self,
        direction: XmodemDirection,
        name: &str,
    ) -> Result<XmodemPlan> {
        let quoted = quote(name)?;
        let model = self.model()?;
        if !model.has_xmodem() {
            return Err(Error::Unsupported(format!(
                "the {} has no XModem; use Kermit",
                model.name()
            )));
        }
        let exists = self.list()?.entries.iter().any(|e| e.name == name);
        if direction == XmodemDirection::FromCalculator && !exists {
            return Err(Error::Reply(format!("{name}: no such variable")));
        }
        if direction == XmodemDirection::ToCalculator && exists && model == Model::Hp48Gx {
            // XRECV stops with "XRECV Error: Name Conflict" before any start
            // character and leaves the name on the stack (emulated 48GX,
            // 2026-10-05).
            return Err(Error::Reply(format!("{name}: already exists")));
        }
        let mut switched_to_rpn = false;
        if model == Model::Hp49G {
            let flag = self.query("-95. FS?", 1)?;
            let alg = flag.first().and_then(|v| parse_real(v)) == Some(1.0);
            if alg {
                self.exec(&["-95. CF".to_string()])?;
                switched_to_rpn = true;
            }
        }
        self.finish()?;
        self.mode = None;

        let command = direction.calculator_command();
        let mut notes = Vec::new();
        if direction == XmodemDirection::ToCalculator && exists {
            notes.push(match model {
                Model::Hp49G => format!(
                    "{name} exists: the 49G does not overwrite it but stores the object as {name}.1."
                ),
                _ => format!(
                    "{name} exists: the 49G stores the object as {name}.1 and the 48G/GX \
                     refuses with \"Name Conflict\"; this model is unknown."
                ),
            });
        }
        if direction == XmodemDirection::ToCalculator && model == Model::Hp48Gx {
            notes.push(format!(
                "If the transfer fails, {name} may be left holding an empty string."
            ));
        }
        if switched_to_rpn {
            notes.push(
                "The calculator was in algebraic mode and is now in RPN mode \
                 (flag -95 cleared); type -95 SF to switch back."
                    .to_string(),
            );
        }
        notes.push(
            "The calculator has left server mode; type SERVER on it after the transfer."
                .to_string(),
        );
        Ok(XmodemPlan {
            model,
            direction,
            name: name.to_string(),
            keys: format!("{quoted} {command}"),
            switched_to_rpn,
            exists,
            notes,
        })
    }

    /// Fail if `name` exists in the current directory.
    fn refuse_existing(&mut self, name: &str) -> Result<()> {
        if self.list()?.entries.iter().any(|e| e.name == name) {
            return Err(Error::Reply(format!(
                "{name} exists in the current directory; remove it first"
            )));
        }
        Ok(())
    }
}

/// The string [`Calculator::sync`] pushes: `HPTX-` and six random hex
/// digits (13 characters with the quotes, far below every model's display
/// width), plain ASCII without RPL delimiters.
pub fn sync_marker() -> String {
    use std::hash::{BuildHasher, Hasher};
    // RandomState is seeded per process from the OS; mix in the time so two
    // states in one process differ too.
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    if let Ok(since) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.write_u128(since.as_nanos());
    }
    format!("HPTX-{:06x}", hasher.finish() & 0xFF_FFFF)
}

/// Check that `data` is a backup [`Calculator::restore`] can upload: a
/// binary Directory object whose size walk succeeds.
fn check_backup(data: &[u8]) -> Result<()> {
    let info = inspect(data).map_err(|e| Error::Object(format!("not a backup: {e}")))?;
    if info.object_type != Some(ObjectType::Directory) {
        return Err(Error::Object(format!(
            "not a backup: prolog {:05X} is not a directory",
            info.prolog
        )));
    }
    let nibbles = unpack(data.get(HEADER_LEN..).unwrap_or_default());
    object_size(&nibbles, 0).map_err(|e| Error::Object(format!("not a backup: {e}")))?;
    Ok(())
}

/// Turn a reply with an error into [`Error::Calculator`].
fn checked(reply: StackReply) -> Result<StackReply> {
    match reply.error {
        Some(message) => Err(Error::Calculator {
            message,
            stack: reply.levels,
        }),
        None => Ok(reply),
    }
}

/// `'NAME'` after [`validate_name`].
fn quote(name: &str) -> Result<String> {
    validate_name(name)?;
    Ok(format!("'{name}'"))
}

/// Check that `name` is a plain global variable name: 1 to 127 characters,
/// not starting with a digit or `.`, no whitespace, control characters,
/// backslash or RPL delimiters and operators, encodable in the HP character
/// set. The names Windows reserves for devices (`CON`, `NUL`, `COM1`, ...,
/// also with an extension such as `NUL.X`, any case) are refused on every
/// platform: a name is the default file name of a download, and the
/// calculator has commands of most of these names anyway.
pub fn validate_name(name: &str) -> Result<()> {
    let bad = || Error::Name(name.to_string());
    let count = name.chars().count();
    let first = name.chars().next().ok_or_else(bad)?;
    if count > 127 || first.is_ascii_digit() || first == '.' {
        return Err(bad());
    }
    let forbidden = |c: char| {
        c.is_whitespace()
            || c.is_control()
            || matches!(
                c,
                '\'' | '"'
                    | '\\'
                    | '«'
                    | '»'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '('
                    | ')'
                    | '#'
                    | ':'
                    | ','
                    | ';'
                    | '+'
                    | '-'
                    | '*'
                    | '/'
                    | '^'
                    | '='
                    | '<'
                    | '>'
            )
    };
    if name.chars().any(forbidden) || is_windows_device(name) {
        return Err(bad());
    }
    encode(name).map_err(|_| bad())?;
    Ok(())
}

/// `CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9` or `LPT1`-`LPT9`, in any case,
/// alone or before a `.`.
fn is_windows_device(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    match stem.as_bytes() {
        b"CON" | b"PRN" | b"AUX" | b"NUL" => true,
        [b'C', b'O', b'M', d] | [b'L', b'P', b'T', d] => (b'1'..=b'9').contains(d),
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::session::Options;
    use crate::transport::MemoryTransport;
    use kermit_proto::codec::{BlockCheck, Deframer, Framing, Packet, parse_frame};
    use kermit_proto::prefix::{self, Quoting};
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<String>>>;

    /// A Kermit server answering `C` and `G D` like the 48SX trace
    /// `48sx-host.trace`, with block check type 1: S, then X, D.., Z, B,
    /// each after the ACK of the previous one. `reply` maps the decoded
    /// command (`G D` for the listing) to the reply text.
    fn fake_server(
        log: Log,
        mut reply: impl FnMut(&str) -> String + Send + 'static,
    ) -> impl FnMut(&[u8]) -> Vec<Vec<u8>> + Send {
        let check = BlockCheck::Type1;
        let mut deframer = Deframer::new();
        let mut queue: Vec<Packet> = Vec::new();
        move |bytes| {
            let wire = |p: &Packet| p.encode(check, &Framing::default()).unwrap();
            deframer.push(bytes);
            let mut out = Vec::new();
            while let Some(frame) = deframer.next_frame() {
                let p = parse_frame(&frame, check).unwrap();
                let data = prefix::decode(&p.data, &Quoting::default()).unwrap();
                match p.kind {
                    b'G' if data == b"F" => {
                        // FINISH: a plain ACK.
                        log.lock().unwrap().push("G F".into());
                        out.push(wire(&Packet::new(p.seq, b'Y', Vec::new())));
                    }
                    b'C' | b'G' => {
                        let command = if p.kind == b'G' {
                            format!("G {}", decode(&data))
                        } else {
                            decode(&data)
                        };
                        log.lock().unwrap().push(command.clone());
                        let reply = reply(&command);
                        // `SILENT` never answers (the client times out);
                        // `E:message` plays an E packet instead of a reply.
                        if reply == "SILENT" {
                            queue.clear();
                            continue;
                        }
                        if let Some(message) = reply.strip_prefix("E:") {
                            queue.clear();
                            out.push(wire(&Packet::new(0, b'E', message.as_bytes().to_vec())));
                            continue;
                        }
                        let text = encode(&reply).unwrap();
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
                    b'Y' => {
                        if let Some(next) = queue.iter().find(|q| q.seq == p.seq + 1) {
                            out.push(wire(next));
                        }
                    }
                    // The client gave up (timeout).
                    b'E' => queue.clear(),
                    kind => panic!("unexpected packet {}", char::from(kind)),
                }
            }
            out
        }
    }

    fn calc(reply: impl FnMut(&str) -> String + Send + 'static) -> (Calculator, Log) {
        let log: Log = Arc::default();
        let transport = MemoryTransport::new(fake_server(Arc::clone(&log), reply));
        let mut kermit = Options::default().kermit;
        kermit.timeout = Duration::from_millis(200);
        // The fake server never repeats a B: no linger, no wait.
        kermit.linger = Duration::ZERO;
        let options = Options {
            kermit,
            drain: Duration::ZERO,
            turnaround: Duration::ZERO,
        };
        let session = Session::new(Box::new(transport), options).unwrap();
        (Calculator::new(session), log)
    }

    fn sent(log: &Log) -> Vec<String> {
        log.lock().unwrap().clone()
    }

    const EMPTY: &str = "Empty Stack\r\n";
    const GX_DIR: &str =
        "{ HOME D1 } 127847\r\nX 10.5 Real Number 1234\r\nSUB 5.5 Directory 4321\r\n";
    const SX_DIR: &str = "X 10.5 Real Number 1234\r\nSUB 5.5 Directory 4321\r\n";

    #[test]
    fn mem_drops_its_value() {
        let (mut c, log) = calc(|cmd| match cmd {
            "MEM" => "1:              12345\r\n".into(),
            _ => EMPTY.into(),
        });
        assert_eq!(c.mem().unwrap(), 12345.0);
        assert_eq!(sent(&log), ["MEM", "DROP"]);
    }

    #[test]
    fn path_from_listing_header() {
        let (mut c, log) = calc(|cmd| match cmd {
            "G D" => GX_DIR.into(),
            _ => panic!("unexpected {cmd}"),
        });
        assert_eq!(c.path().unwrap(), ["HOME", "D1"]);
        assert_eq!(sent(&log), ["G D"]);
    }

    const MARKER: &str = "HPTX-0a1b2c";
    const MARKER_CMD: &str = "\"HPTX-0a1b2c\"";

    /// Sync with [`MARKER`] against a server whose replies to the marker
    /// command are `replies` in order (then an empty stack); the log of
    /// what was sent.
    fn sync_against(replies: Vec<&'static str>) -> Vec<String> {
        let (result, log) = try_sync_against(replies);
        result.unwrap();
        log
    }

    /// [`sync_against`] without the unwrap.
    fn try_sync_against(replies: Vec<&'static str>) -> (Result<()>, Vec<String>) {
        let mut replies = replies.into_iter();
        let (mut c, log) = calc(move |cmd| match cmd {
            MARKER_CMD => replies.next().unwrap_or(EMPTY).into(),
            "DROP" | "DROP2" => EMPTY.into(),
            _ => panic!("unexpected {cmd}"),
        });
        let result = c.sync_with(MARKER);
        (result, sent(&log))
    }

    fn assert_not_in_step(result: Result<()>) {
        assert!(
            matches!(&result, Err(Error::Reply(m)) if m.contains("not in step")),
            "{result:?}"
        );
    }

    #[test]
    fn sync_marker_is_short_and_plain() {
        let m = sync_marker();
        assert_eq!(m.len(), 11, "{m}");
        assert!(m.starts_with("HPTX-"));
        assert!(m[5..].chars().all(|c| c.is_ascii_hexdigit()), "{m}");
        assert_ne!(sync_marker(), sync_marker());
    }

    #[test]
    fn sync_in_step_costs_two_transactions() {
        let one = "1:      \"HPTX-0a1b2c\"\r\n";
        assert_eq!(sync_against(vec![one]), [MARKER_CMD, "DROP"]);
    }

    /// The first command after an aborted client is eaten by the server and
    /// answered with the late reply of the aborted command (here a stack
    /// display): sync sends the marker again and drops only the marker.
    #[test]
    fn sync_skips_a_stale_reply() {
        let stale = "2:                  1\r\n1:                  2\r\n";
        let ours = "3:                  1\r\n2:                  2\r\n1:      \"HPTX-0a1b2c\"\r\n";
        assert_eq!(
            sync_against(vec![stale, ours]),
            [MARKER_CMD, MARKER_CMD, "DROP"]
        );
    }

    /// A late reply that is a 49G path cut at the display width (no closing
    /// brace, fixture `49g-vars.txt` style) is not taken for ours.
    #[test]
    fn sync_skips_a_truncated_path_reply() {
        let stale = "1: {HOME,HPTXAAAA,HPTXBB\r\n";
        let ours = "2: {HOME,HPTXAAAA,HPTXBB\r\n1:      \"HPTX-0a1b2c\"\r\n";
        assert_eq!(
            sync_against(vec![stale, ours]),
            [MARKER_CMD, MARKER_CMD, "DROP"]
        );
    }

    /// The aborted command's own result was a path: it stays on the stack.
    #[test]
    fn sync_keeps_a_path_shaped_stale_result() {
        let stale = "1:          { HOME }\r\n";
        let ours = "2:          { HOME }\r\n1:      \"HPTX-0a1b2c\"\r\n";
        // One DROP for the marker; the path at level 2 is the user's.
        assert_eq!(
            sync_against(vec![stale, ours]),
            [MARKER_CMD, MARKER_CMD, "DROP"]
        );
    }

    /// In a deep 49G directory the stack shows truncated values; the marker
    /// at level 1 is recognised all the same and only it is dropped.
    #[test]
    fn sync_in_a_deep_49g_directory() {
        let ours = "2: {D1,G,TG,B,C,A,P,L,S,R\r\n1: \"HPTX-0a1b2c\"\r\n";
        assert_eq!(sync_against(vec![ours]), [MARKER_CMD, "DROP"]);
    }

    /// Our first marker command was not eaten after all: both copies are
    /// ours and both go.
    #[test]
    fn sync_drops_every_copy_of_its_marker() {
        let stale = "Error: Bad Argument Type\r\n";
        let ours =
            "3:                  7\r\n2:      \"HPTX-0a1b2c\"\r\n1:      \"HPTX-0a1b2c\"\r\n";
        assert_eq!(
            sync_against(vec![stale, ours]),
            [MARKER_CMD, MARKER_CMD, "DROP2"]
        );
    }

    /// An E packet (the calculator aborting a leftover transfer) is an odd
    /// reply, not an error: the marker goes again.
    #[test]
    fn sync_survives_an_error_packet() {
        let ours = "1:      \"HPTX-0a1b2c\"\r\n";
        assert_eq!(
            sync_against(vec!["E:Transfer Failed", ours]),
            [MARKER_CMD, MARKER_CMD, "DROP"]
        );
        let (result, log) = try_sync_against(vec!["E:Transfer Failed", "E:Transfer Failed"]);
        assert_eq!(log, [MARKER_CMD, MARKER_CMD]);
        assert_not_in_step(result);
    }

    /// The first marker command ran but its reply never came (timeout): the
    /// second reply shows both markers and both go. A second timeout is a
    /// link failure.
    #[test]
    fn sync_retries_once_after_a_timeout() {
        let mut n = 0;
        let (mut c, log) = calc(move |cmd| {
            n += 1;
            match (cmd, n) {
                (MARKER_CMD, 1) => "SILENT".into(),
                (MARKER_CMD, _) => {
                    "3:                  7\r\n2:      \"HPTX-0a1b2c\"\r\n1:      \"HPTX-0a1b2c\"\r\n"
                        .into()
                }
                ("DROP2", _) => EMPTY.into(),
                _ => panic!("unexpected {cmd}"),
            }
        });
        c.sync_with(MARKER).unwrap();
        assert_eq!(sent(&log), [MARKER_CMD, MARKER_CMD, "DROP2"]);
        // The normal budget is back for everything after the sync.
        assert_eq!(
            c.session().config().retries,
            Options::default().kermit.retries
        );

        let (mut c, log) = calc(|_| "SILENT".into());
        let err = c.sync_with(MARKER).unwrap_err();
        assert!(
            matches!(err, Error::Kermit(kermit_proto::Error::Timeout)),
            "{err:?}"
        );
        // The first attempt is one try (no retransmission), the second the
        // normal budget (1 + 5); only the marker is ever sent.
        let log = sent(&log);
        assert_eq!(log.len(), 1 + 6, "{log:?}");
        assert!(log.iter().all(|c| c == MARKER_CMD), "{log:?}");
    }

    /// Audit PR #18, #2: two odd replies in a row are an error (they used
    /// to be accepted as success). Nothing is dropped and there is no third
    /// attempt: a marker below level 1 or another marker is not ours to
    /// drop.
    #[test]
    fn sync_never_drops_what_it_did_not_push() {
        let error = "Error: Bad Argument Type\r\n1:                  1\r\n";
        let (result, log) = try_sync_against(vec![error, EMPTY]);
        assert_eq!(log, [MARKER_CMD, MARKER_CMD]);
        assert_not_in_step(result);
        let below = "2:      \"HPTX-0a1b2c\"\r\n1:                  1\r\n";
        let other = "1:      \"HPTX-ffffff\"\r\n";
        let (result, log) = try_sync_against(vec![below, other]);
        assert_eq!(log, [MARKER_CMD, MARKER_CMD]);
        assert_not_in_step(result);
    }

    /// Audit PR #18, #1: a host command goes out once. Its reply is late
    /// (never comes here) and the server's NAK crosses it: no `C` is
    /// resent, neither after the NAK nor after the timeout, and the error
    /// says the command may have run.
    #[test]
    fn host_command_is_never_resent() {
        let log: Log = Arc::default();
        let mut server = fake_server(Arc::clone(&log), |_| "SILENT".into());
        let check = BlockCheck::Type1;
        let mut deframer = Deframer::new();
        let transport = MemoryTransport::new(move |bytes: &[u8]| {
            let mut out = server(bytes);
            // A NAK right after each C: a stale one from the idle server,
            // or the server asking for the C again.
            deframer.push(bytes);
            while let Some(frame) = deframer.next_frame() {
                if parse_frame(&frame, check).unwrap().kind == b'C' {
                    let nak = Packet::new(0, b'N', Vec::new());
                    out.push(nak.encode(check, &Framing::default()).unwrap());
                }
            }
            out
        });
        let mut kermit = Options::default().kermit;
        kermit.timeout = Duration::from_millis(300);
        let options = Options {
            kermit,
            drain: Duration::ZERO,
            turnaround: Duration::ZERO,
        };
        let mut c = Calculator::new(Session::new(Box::new(transport), options).unwrap());
        let start = std::time::Instant::now();
        let err = c.run("1 'X' STO+").unwrap_err();
        assert!(
            matches!(&err, Error::NoReply { command } if command == "1 'X' STO+"),
            "{err:?}"
        );
        assert!(err.to_string().contains("may have run"), "{err}");
        assert_eq!(sent(&log), ["1 'X' STO+"]);
        // The NAK did not cut the wait short (the old grace was 1 s, here
        // longer than the timeout; the deadline is the full timeout).
        assert!(start.elapsed() >= Duration::from_millis(300));
        // The session's own budget is back for the next transaction.
        assert_eq!(
            c.session().config().retries,
            Options::default().kermit.retries
        );
    }

    /// Audit PR #18, #3: a backup whose directory walk fails (here cut in
    /// half) is refused before anything is sent; it used to pass on its
    /// Directory prolog alone and reach RESTORE.
    #[test]
    fn restore_refuses_a_truncated_backup() {
        let path = format!("{}/fixtures/48sx-D1.hp", env!("CARGO_MANIFEST_DIR"));
        let full = std::fs::read(path).unwrap();
        let cut = &full[..full.len() / 2];
        assert_eq!(
            inspect(cut).unwrap().object_type,
            Some(ObjectType::Directory)
        );
        let (mut c, log) = calc(|cmd| panic!("unexpected {cmd}"));
        let err = c.restore(cut).unwrap_err();
        assert!(
            matches!(&err, Error::Object(m) if m.starts_with("not a backup")),
            "{err:?}"
        );
        assert!(sent(&log).is_empty());
    }

    /// Audit PR #18, #9: no reply to RESTORE is success only when the probe
    /// after it gets no reply either (the warm start ended server mode).
    #[test]
    fn restore_probe_after_the_silent_restore() {
        const RESTORE: &str = ":0:HPTXRS RESTORE";
        // RESTORE ran: the calculator is gone, the probe times out.
        let (mut c, log) = calc(|_| "SILENT".into());
        c.restore_from_port().unwrap();
        let log = sent(&log);
        assert_eq!(log.len(), 2, "{log:?}");
        assert_eq!(log[0], RESTORE);
        assert!(log[1].starts_with("\"HPTX-"), "{log:?}");

        // The RESTORE command was lost: the server still answers the probe.
        let (mut c, log) = calc(|cmd| match cmd {
            RESTORE => "SILENT".into(),
            "DROP" => EMPTY.into(),
            probe => format!("1: {probe}\r\n"),
        });
        let err = c.restore_from_port().unwrap_err();
        assert!(
            matches!(&err, Error::Reply(m) if m.starts_with("RESTORE did not run")),
            "{err:?}"
        );
        let log = sent(&log);
        assert_eq!(log.len(), 3, "{log:?}");
        assert_eq!(log[2], "DROP", "the probe's marker is dropped");
    }

    #[test]
    fn path_query_without_header() {
        let (mut c, log) = calc(|cmd| match cmd {
            "G D" => SX_DIR.into(),
            "PATH" => "1:          { HOME D1 }\r\n".into(),
            _ => EMPTY.into(),
        });
        assert_eq!(c.path().unwrap(), ["HOME", "D1"]);
        assert_eq!(sent(&log), ["G D", "PATH", "DROP"]);
    }

    #[test]
    fn version_48sx() {
        let (mut c, log) = calc(|cmd| match cmd {
            "VERSION" => "1:  'VERSION'\r\n".into(),
            _ => EMPTY.into(),
        });
        assert_eq!(c.version().unwrap(), None);
        assert_eq!(sent(&log), ["VERSION", "DROP"]);
    }

    #[test]
    fn version_48gx() {
        let (mut c, log) = calc(|cmd| match cmd {
            "VERSION" => "2:     \"Version HP48-R\"\r\n1:  \"Copyright HP 1993\"\r\n".into(),
            _ => EMPTY.into(),
        });
        assert_eq!(
            c.version().unwrap().as_deref(),
            Some("Version HP48-R, Copyright HP 1993")
        );
        assert_eq!(sent(&log), ["VERSION", "DROP2"]);
    }

    #[test]
    fn version_49g_two_line_string() {
        let (mut c, _) = calc(|cmd| match cmd {
            "VERSION" => {
                "2: \"Version HP49-C\r\nRevision #1.19-6\r\n1:  \"Copyright HP 2009\"\r\n".into()
            }
            _ => EMPTY.into(),
        });
        assert_eq!(
            c.version().unwrap().as_deref(),
            Some("Version HP49-C Revision #1.19-6, Copyright HP 2009")
        );
    }

    #[test]
    fn model_from_version() {
        use Model::*;
        assert_eq!(Model::from_version(None), Hp48Sx);
        assert_eq!(
            Model::from_version(Some("Version HP48-R, Copyright HP 1993")),
            Hp48Gx
        );
        assert_eq!(
            Model::from_version(Some("Version HP48-C Revision #2.15, Copyright HP 2009")),
            Hp49G
        );
        assert_eq!(
            Model::from_version(Some("Version HP49-C, Copyright HP 2000")),
            Hp49G
        );
        assert_eq!(Model::from_version(Some("something else")), Unknown);
        assert_eq!(Hp48Gx.name(), "HP 48G/GX");
        assert!(!Hp48Sx.has_xmodem() && Hp48Gx.has_xmodem() && Hp49G.has_xmodem());
    }

    const G49_VERSION: &str =
        "2: \"Version HP48-C\r\nRevision #2.15\r\n1:  \"Copyright HP 2009\"\r\n";
    const GX_VERSION: &str = "2:     \"Version HP48-R\"\r\n1:  \"Copyright HP 1993\"\r\n";

    #[test]
    fn prepare_xrecv_on_49g_in_alg_mode() {
        let (mut c, log) = calc(|cmd| match cmd {
            "VERSION" => G49_VERSION.into(),
            "G D" => "{ HOME } 2000.\r\nHPTXX 10.5 String 1234.\r\n".into(),
            "-95. FS?" => "1:                     1.\r\n".into(),
            _ => EMPTY.into(),
        });
        let plan = c
            .prepare_for_xmodem(XmodemDirection::ToCalculator, "HPTXX")
            .unwrap();
        assert_eq!(
            sent(&log),
            [
                "VERSION", "DROP2", "G D", "-95. FS?", "DROP", "-95. CF", "G F"
            ]
        );
        assert_eq!(plan.model, Model::Hp49G);
        assert_eq!(plan.keys, "'HPTXX' XRECV");
        assert!(plan.exists && plan.switched_to_rpn);
        let text = plan.instructions();
        assert!(
            text.starts_with("On the calculator, type 'HPTXX' XRECV"),
            "{text}"
        );
        assert!(text.contains("HPTXX.1"), "{text}");
        assert!(text.contains("-95 SF"), "{text}");
        assert!(
            text.ends_with("type SERVER on it after the transfer."),
            "{text}"
        );
    }

    #[test]
    fn prepare_xsend_on_48gx() {
        let (mut c, log) = calc(|cmd| match cmd {
            "VERSION" => GX_VERSION.into(),
            "G D" => "{ HOME } 2000\r\nHPTXX 10.5 String 1234\r\n".into(),
            _ => EMPTY.into(),
        });
        let plan = c
            .prepare_for_xmodem(XmodemDirection::FromCalculator, "HPTXX")
            .unwrap();
        assert_eq!(sent(&log), ["VERSION", "DROP2", "G D", "G F"]);
        assert_eq!(plan.keys, "'HPTXX' XSEND");
        assert!(!plan.switched_to_rpn);
        assert_eq!(plan.notes.len(), 1);
    }

    #[test]
    fn prepare_refuses_existing_name_on_48gx() {
        let (mut c, log) = calc(|cmd| match cmd {
            "VERSION" => GX_VERSION.into(),
            "G D" => "{ HOME } 2000\r\nHPTXX 10.5 String 1234\r\n".into(),
            _ => EMPTY.into(),
        });
        let err = c
            .prepare_for_xmodem(XmodemDirection::ToCalculator, "HPTXX")
            .unwrap_err();
        assert!(matches!(&err, Error::Reply(m) if m == "HPTXX: already exists"));
        assert_eq!(sent(&log), ["VERSION", "DROP2", "G D"]);
    }

    #[test]
    fn prepare_refuses_before_finishing() {
        // XSEND of a missing variable, and any XModem on a 48SX, fail
        // without ending server mode.
        let (mut c, log) = calc(|cmd| match cmd {
            "VERSION" => GX_VERSION.into(),
            "G D" => "{ HOME } 2000\r\n".into(),
            _ => EMPTY.into(),
        });
        let err = c
            .prepare_for_xmodem(XmodemDirection::FromCalculator, "NOSUCH")
            .unwrap_err();
        assert!(matches!(&err, Error::Reply(m) if m == "NOSUCH: no such variable"));
        assert!(!sent(&log).contains(&"G F".to_string()));
        assert!(matches!(
            c.prepare_for_xmodem(XmodemDirection::ToCalculator, "A B"),
            Err(Error::Name(_))
        ));

        let (mut c, log) = calc(|cmd| match cmd {
            "VERSION" => "1:  'VERSION'\r\n".into(),
            _ => EMPTY.into(),
        });
        let err = c
            .prepare_for_xmodem(XmodemDirection::ToCalculator, "X")
            .unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "{err:?}");
        assert_eq!(sent(&log), ["VERSION", "DROP"]);
    }

    #[test]
    fn cd_checks_each_component() {
        let (mut c, log) = calc(|cmd| match cmd {
            "G D" => "D1 5.5 Directory 4321\r\nX 10.5 Real Number 1234\r\n".into(),
            _ => EMPTY.into(),
        });
        c.cd(&["HOME", "D1"]).unwrap();
        assert_eq!(sent(&log), ["HOME", "G D", "D1"]);

        log.lock().unwrap().clear();
        let err = c.cd(&["NOSUCH"]).unwrap_err();
        assert!(matches!(&err, Error::Reply(m) if m.contains("no such directory")));
        assert_eq!(sent(&log), ["HOME", "G D"]);

        log.lock().unwrap().clear();
        let err = c.cd(&["HOME", "X"]).unwrap_err();
        assert!(matches!(&err, Error::Reply(m) if m.contains("no such directory")));
        assert_eq!(sent(&log), ["HOME", "G D"]);

        log.lock().unwrap().clear();
        assert!(matches!(c.cd(&["A B"]), Err(Error::Name(_))));
        assert!(sent(&log).is_empty());
    }

    #[test]
    fn rename_joined_split_and_directory() {
        let (mut c, log) = calc(|cmd| match cmd {
            "G D" => format!(
                "X 10.5 Real Number 1234\r\nSUB 5.5 Directory 4321\r\n{} 10.5 Real Number 1\r\n",
                "L".repeat(40)
            ),
            _ => EMPTY.into(),
        });
        c.rename("X", "Y").unwrap();
        assert_eq!(sent(&log), ["G D", "'X' RCL 'Y' STO 'X' PURGE"]);

        log.lock().unwrap().clear();
        let long = "L".repeat(40);
        let to = "M".repeat(40);
        c.rename(&long, &to).unwrap();
        assert_eq!(
            sent(&log),
            [
                "G D".to_string(),
                format!("'{long}' RCL"),
                format!("'{to}' STO"),
                format!("'{long}' PURGE"),
            ]
        );

        log.lock().unwrap().clear();
        c.rename("SUB", "SUB2").unwrap();
        assert_eq!(sent(&log), ["G D", "'SUB' RCL 'SUB2' STO 'SUB' PGDIR"]);

        log.lock().unwrap().clear();
        assert!(matches!(c.rename("NOSUCH", "Z"), Err(Error::Reply(_))));
        assert!(matches!(c.rename("X", "SUB"), Err(Error::Reply(_))));
        assert_eq!(sent(&log), ["G D", "G D"]);
    }

    #[test]
    fn remove_checks_listing() {
        let (mut c, log) = calc(|cmd| match cmd {
            "G D" => SX_DIR.into(),
            _ => EMPTY.into(),
        });
        let err = c.remove("NOSUCH").unwrap_err();
        assert!(matches!(&err, Error::Reply(m) if m.contains("no such variable")));
        assert_eq!(sent(&log), ["G D"]);
        c.remove("SUB").unwrap();
        c.remove("X").unwrap();
        assert_eq!(
            sent(&log),
            ["G D", "G D", "'SUB' PGDIR", "G D", "'X' PURGE"]
        );
    }

    #[test]
    fn calculator_error() {
        let (mut c, _) = calc(|_| "Error: Undefined Name\r\n1:          'NOSUCH'\r\n".into());
        let reply = c.run("'NOSUCH' RCL").unwrap();
        assert_eq!(reply.error.as_deref(), Some("Undefined Name"));
        let err = c.mkdir("NOSUCH").unwrap_err();
        assert!(
            matches!(&err, Error::Calculator { message, stack }
                if message == "Undefined Name" && stack == &["'NOSUCH'"]),
            "{err:?}"
        );
    }

    #[test]
    fn transfer_mode_is_cached() {
        let (mut c, log) = calc(|_| EMPTY.into());
        c.set_transfer_mode(TransferMode::Binary).unwrap();
        c.set_transfer_mode(TransferMode::Binary).unwrap();
        assert_eq!(sent(&log), ["-35 SF"]);
        c.set_transfer_mode(TransferMode::Ascii).unwrap();
        assert_eq!(sent(&log), ["-35 SF", "-35 CF"]);
        // An arbitrary command may change the flag.
        c.run("1").unwrap();
        c.set_transfer_mode(TransferMode::Ascii).unwrap();
        assert_eq!(sent(&log), ["-35 SF", "-35 CF", "1", "-35 CF"]);
    }

    #[test]
    fn transfer_mode_query() {
        let (mut c, log) = calc(|cmd| match cmd {
            "-35 FS?" => "1: 1.\r\n".into(),
            _ => EMPTY.into(),
        });
        assert_eq!(c.transfer_mode().unwrap(), TransferMode::Binary);
        c.set_transfer_mode(TransferMode::Binary).unwrap();
        assert_eq!(sent(&log), ["-35 FS?", "DROP"]);
    }

    #[test]
    fn set_iopar_stores_reals() {
        // Recorded on the emulated 49G: `{ 9600 0 0 0 3 3 }` stored as exact
        // integers, the server answered nothing and stopped with "Invalid
        // IOPAR". The list must hold reals, in one command.
        let (mut c, log) = calc(|_| EMPTY.into());
        let iopar = Iopar {
            translate: 3,
            ..Iopar::default()
        };
        c.set_iopar(&iopar).unwrap();
        assert_eq!(
            sent(&log),
            ["PATH HOME { 9600. 0. 0. 0. 3. 3. } 'IOPAR' STO EVAL"]
        );
        // The widest list still fits one `C` packet, so it is never split.
        log.lock().unwrap().clear();
        let wide = Iopar {
            baud: 115_200,
            parity: -4,
            receive_pacing: true,
            transmit_pacing: true,
            checksum: 3,
            translate: 255,
        };
        c.set_iopar(&wide).unwrap();
        assert_eq!(
            sent(&log),
            ["PATH HOME { 115200. -4. 1. 1. 3. 255. } 'IOPAR' STO EVAL"]
        );
    }

    #[test]
    fn command_too_long() {
        let (mut c, log) = calc(|_| EMPTY.into());
        let err = c.run(&"1".repeat(200)).unwrap_err();
        assert!(
            matches!(err, Error::CommandTooLong { len: 200, .. }),
            "{err:?}"
        );
        assert!(sent(&log).is_empty());
        c.run("1").unwrap();
    }

    #[test]
    fn names() {
        for ok in [
            "A",
            "HPTXE2E",
            "IOPAR",
            "x\u{0304}",
            "Σ1",
            "a.b",
            "CONS",
            "COM0",
            "COM10",
            "LPT",
            "XNUL",
            &"N".repeat(127),
        ] {
            assert!(validate_name(ok).is_ok(), "{ok:?}");
        }
        for bad in [
            "",
            "1A",
            ".A",
            "A B",
            "A\tB",
            "A'",
            "A\"",
            "«A",
            "{A}",
            "A[1]",
            "F(X)",
            "#A",
            ":0:A",
            "A,B",
            "A;B",
            "A+B",
            "A-B",
            "A*B",
            "A/B",
            "A^B",
            "A=B",
            "A<B",
            "A>B",
            "A\u{1}",
            "A\u{263A}",
            // security.md "Windows file names": backslash and device names.
            "A\\B",
            "CON",
            "nul",
            "Aux",
            "PRN",
            "COM1",
            "lpt9",
            "NUL.X",
            "con.txt",
            &"N".repeat(128),
        ] {
            assert!(matches!(validate_name(bad), Err(Error::Name(_))), "{bad:?}");
        }
    }
}
