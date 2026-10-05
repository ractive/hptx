//! Turning errors into a message and a hint.

use std::io::ErrorKind;
use std::time::Duration;

use hptx_core::Error;

use crate::output::{Failure, shell_quote};

/// An error that already knows what the user should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hinted {
    /// What went wrong.
    pub message: String,
    /// What to do about it.
    pub hint: String,
}

impl Hinted {
    pub fn new(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Hinted {
            message: message.into(),
            hint: hint.into(),
        }
    }
}

impl std::fmt::Display for Hinted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Hinted {}

/// The link settings, for messages.
#[derive(Debug, Clone, Default)]
pub struct LinkInfo {
    /// The address in use, once known.
    pub addr: Option<String>,
    /// Kermit per-packet timeout.
    pub timeout: Duration,
    /// Kermit retries per packet.
    pub retries: u32,
}

/// The temporary variables hptx creates on the calculator.
const TEMP_VARS: &[&str] = &["HPTXTMP", "HPTXBK", "HPTXRS", "HPTXPT"];

/// Describe `err` (with its context chain) for the user.
pub fn describe(err: &anyhow::Error, link: &LinkInfo) -> Failure {
    let message = chain_text(err);
    // anyhow's own downcast also finds a `Hinted` given as context, which
    // the chain's causes do not show as one.
    if let Some(h) = err.downcast_ref::<Hinted>() {
        return Failure {
            error: message,
            hint: Some(h.hint.clone()),
            stack: None,
        };
    }
    for cause in err.chain() {
        if let Some(e) = cause.downcast_ref::<Error>() {
            return core_failure(message, e, link);
        }
    }
    Failure {
        error: message,
        hint: None,
        stack: None,
    }
}

/// `context: cause: cause`, skipping a cause whose text the previous one
/// already ends with (hptx-core errors embed their source, e.g.
/// `I/O error: Connection refused` with source `Connection refused`).
fn chain_text(err: &anyhow::Error) -> String {
    let mut text = String::new();
    for cause in err.chain() {
        let part = cause.to_string();
        if text.ends_with(&part) {
            continue;
        }
        if !text.is_empty() {
            text.push_str(": ");
        }
        text.push_str(&part);
    }
    text
}

fn core_failure(message: String, err: &Error, link: &LinkInfo) -> Failure {
    let addr = link.addr.as_deref().unwrap_or("the port");
    let mut stack = None;
    let hint: Option<String> = match err {
        Error::Kermit(kermit_proto::Error::Timeout) => {
            let what = format!(
                "no answer from the calculator on {addr} ({} tries, {} s each)",
                link.retries + 1,
                link.timeout.as_secs()
            );
            let timeout = Error::Kermit(kermit_proto::Error::Timeout).to_string();
            return Failure {
                error: message.replace(&timeout, &what),
                hint: Some(
                    "check the cable and the port, that the calculator shows \
                     \"Awaiting Server Cmd.\" (run SERVER on it), and that IOPAR is 9600 baud"
                        .into(),
                ),
                stack: None,
            };
        }
        Error::NoReply { .. } => Some(
            "check on the calculator whether the command ran before running it again (hptx \
             never sends a command twice); if it is still busy, wait or press ON (ATTN) to \
             stop it. If it shows \"Awaiting Server Cmd.\", check the cable"
                .into(),
        ),
        Error::Kermit(_) => Some(
            "the transfer broke off; run the command again. If it keeps failing, check the \
             cable and that the calculator runs SERVER at 9600 baud"
                .into(),
        ),
        Error::Remote(text) if text.contains("Undefined Name") => {
            Some("no such variable in the current directory; `hptx ls` lists them".into())
        }
        Error::Remote(_) => Some("the calculator refused the request".into()),
        Error::Calculator {
            stack: levels,
            message: m,
        } => {
            stack = Some(levels.clone());
            Some(if m.contains("Undefined Name") {
                "no such variable; `hptx ls` lists them. The calculator leaves the \
                 arguments of a failed command on its stack: `hptx run DROP` removes one level"
                    .into()
            } else {
                "the calculator leaves the arguments of a failed command on its stack: \
                 `hptx run DROP` removes one level, `hptx run CLEAR` all"
                    .into()
            })
        }
        Error::CommandTooLong { max, .. } => Some(format!(
            "a host command must fit in one Kermit packet ({max} encoded bytes; \
             « » → count as 3 or 4). Split it into several `hptx run` calls (the stack \
             stays between calls), or `hptx put` a program and run it by name"
        )),
        Error::Name(_) => Some(
            "calculator names start with a letter and have no spaces, quotes, brackets, \
             # : , ; \\ or + - * / ^ = < >, and are not a Windows device name (CON, NUL, \
             COM1, ...); choose one with --as NAME"
                .into(),
        ),
        Error::Charset(_) => {
            Some("use characters the calculator has, or its ASCII trigraphs such as \\->".into())
        }
        Error::Address(_) => Some(
            "--port takes a device path (/dev/ttyUSB0, /dev/cu.usbserial-X, COM3) or \
             tcp://host:port"
                .into(),
        ),
        Error::Emulator(_) => Some(
            "saturnus://[MODEL@]ROM addresses (MODEL 48sx, the default, 48gx or 49g) need hptx \
             built with the `saturnus` feature of hptx-core and a readable ROM file of that \
             model; use tcp://host:port for the Docker emulator"
                .into(),
        ),
        Error::Serial(_) => {
            Some("`hptx ports` lists the serial ports; is another program using it?".into())
        }
        Error::Io(e) if e.kind() == ErrorKind::ConnectionRefused => Some(format!(
            "nothing listens on {addr}; is the emulator running?"
        )),
        Error::Io(e) if e.kind() == ErrorKind::UnexpectedEof => {
            Some("the link closed; check the cable or the emulator".into())
        }
        Error::Io(_) => None,
        Error::Reply(text) => reply_hint(text),
        Error::Object(_) => {
            Some("the file is not an HP binary object; `get` it again in binary mode".into())
        }
        Error::Xmodem(_) => Some(
            "the XModem transfer failed; the calculator is out of server mode: press ON if \
             XRECV/XSEND still runs, type SERVER, check the cable and run the command again"
                .into(),
        ),
        Error::Unsupported(_) => Some(
            "use Kermit, the default: drop --protocol xmodem (the 48S/SX has no XRECV/XSEND)"
                .into(),
        ),
    };
    Failure {
        error: message,
        hint,
        stack,
    }
}

/// Hints for the checks hptx-core makes before acting. Names in commands
/// are shell-quoted: they come from the calculator or the user.
fn reply_hint(text: &str) -> Option<String> {
    let name = text.split([' ', ':']).next().unwrap_or_default();
    let q = shell_quote(name);
    if text.contains("exists in the current directory") && TEMP_VARS.contains(&name) {
        return Some(format!(
            "{name} is hptx's temporary variable, probably left by an interrupted run: \
             `hptx get {q}` to keep it, then `hptx rm {q}` (or `hptx mv {q} OTHER`)"
        ));
    }
    if text.ends_with("no such variable") || text.ends_with("no such directory") {
        return Some("`hptx ls` lists the current directory".into());
    }
    if text.ends_with("already exists") {
        return Some(format!(
            "choose another name, or `hptx rm {q}` first (`--dry-run` shows what goes)"
        ));
    }
    if text.starts_with("not in step with the calculator") {
        return Some(
            "another program or an aborted command may still be talking to the calculator: \
             press ON (ATTN) on it if it is busy, check that it shows \"Awaiting Server Cmd.\", \
             and run the command again"
                .into(),
        );
    }
    if text.starts_with("RESTORE's result is unknown") {
        return Some(
            "check HOME (`hptx ls`) before running the restore again; `hptx restore \
             --cleanup` deletes the copy in port 0"
                .into(),
        );
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use anyhow::Context;

    fn link() -> LinkInfo {
        LinkInfo {
            addr: Some("/dev/ttyUSB0".into()),
            timeout: Duration::from_secs(20),
            retries: 5,
        }
    }

    fn fail(e: Error) -> Failure {
        let err = Err::<(), _>(e).context("ls").unwrap_err();
        describe(&err, &link())
    }

    #[test]
    fn timeout_says_what_was_tried() {
        let f = fail(Error::Kermit(kermit_proto::Error::Timeout));
        assert!(f.error.contains("/dev/ttyUSB0"), "{}", f.error);
        assert_eq!(
            f.error,
            "ls: no answer from the calculator on /dev/ttyUSB0 (6 tries, 20 s each)"
        );
        assert!(f.hint.unwrap().contains("Awaiting Server Cmd."));
    }

    #[test]
    fn temp_var_exists() {
        let f = fail(Error::Reply(
            "HPTXBK exists in the current directory; remove it first".into(),
        ));
        assert!(f.hint.unwrap().contains("hptx rm HPTXBK"));
    }

    #[test]
    fn too_long_suggests_splitting() {
        let f = fail(Error::CommandTooLong {
            command: "1".repeat(80),
            len: 80,
            max: 77,
        });
        assert!(f.hint.unwrap().contains("77 encoded bytes"));
        assert!(f.error.starts_with("ls: host command too long"));
    }

    #[test]
    fn calculator_error_keeps_the_stack() {
        let f = fail(Error::Calculator {
            message: "Undefined Name".into(),
            stack: vec!["'X'".into()],
        });
        assert_eq!(f.stack, Some(vec!["'X'".to_string()]));
        assert!(f.hint.unwrap().contains("hptx run DROP"));
    }

    #[test]
    fn remote_text_is_shown() {
        let f = fail(Error::Remote("Undefined Name".into()));
        assert_eq!(f.error, "ls: calculator: Undefined Name");
        assert!(f.hint.is_some());
    }

    #[test]
    fn io_cause_is_not_repeated() {
        let io = std::io::Error::from(ErrorKind::ConnectionRefused);
        let text = io.to_string();
        let err = Err::<(), _>(Error::Io(io))
            .context("cannot open tcp://h:1")
            .unwrap_err();
        let f = describe(&err, &link());
        assert_eq!(f.error, format!("cannot open tcp://h:1: I/O error: {text}"));
        assert!(f.hint.unwrap().contains("emulator"));
    }

    #[test]
    fn xmodem_errors_have_hints() {
        let f = fail(Error::Unsupported(
            "the HP 48S/SX has no XModem; use Kermit".into(),
        ));
        assert_eq!(
            f.error,
            "ls: unsupported: the HP 48S/SX has no XModem; use Kermit"
        );
        assert!(f.hint.unwrap().contains("drop --protocol xmodem"));
        let f = fail(Error::Xmodem(
            hptx_core::xmodem_proto::Error::RemoteCancelled,
        ));
        assert!(f.hint.unwrap().contains("SERVER"));
    }

    #[test]
    fn hinted_wins() {
        let err = anyhow::Error::new(Hinted::new("m", "h"));
        let f = describe(&err, &link());
        assert_eq!((f.error.as_str(), f.hint.as_deref()), ("m", Some("h")));
    }
}
