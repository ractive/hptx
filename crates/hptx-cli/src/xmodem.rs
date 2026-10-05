//! `get` and `put` with `--protocol xmodem`: hptx ends the Kermit server,
//! tells the user to type `'NAME' XRECV` or `'NAME' XSEND` on the
//! calculator, waits for it to start and runs the transfer
//! ([`hptx_core::xmodem`]). Afterwards the calculator is out of server mode
//! and the user types SERVER again.

use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use hptx_core::reply::Entry;
use hptx_core::xmodem::{XmodemDirection, XmodemPlan};
use hptx_core::xmodem_proto::{Check, Event};
use hptx_core::{Calculator, Error, Model, XmodemOptions, XmodemSession};
use serde_json::{Value, json};

use crate::commands::{
    Ctx, GetArgs, PutArgs, object_type_name_binary, refuse_existing_file, write_file,
};
use crate::error::Hinted;
use crate::output::{Format, Hint, Outcome, shell_quote};
use crate::util;

/// What to type on the calculator after the transfer.
const SERVER_AGAIN: &str = "Type SERVER on the calculator to use hptx with Kermit again";

/// Display name of a block check.
pub fn check_name(check: Check) -> &'static str {
    match check {
        Check::Checksum => "checksum",
        Check::Crc16 => "CRC-16",
        Check::HpCrc => "HP CRC",
        _ => "other check",
    }
}

/// What the model's XModem profile does, for the plan text.
fn profile_text(model: Model) -> &'static str {
    match model {
        Model::Hp49G => "HP CRC, 1k blocks",
        Model::Hp48Gx => "checksum, 128-byte blocks",
        _ => "CRC or checksum, whatever the calculator asks for",
    }
}

/// The variable `name` in the current directory, if listed.
fn lookup(calc: &mut Calculator, name: &str) -> Result<Option<Entry>> {
    let listing = calc.list().context("ls")?;
    Ok(listing.entries.into_iter().find(|e| e.name == name))
}

/// The model, refusing one without XModem (the 48S/SX).
fn xmodem_model(calc: &mut Calculator) -> Result<(Model, XmodemOptions)> {
    let model = calc.model().context("VERSION")?;
    let options = XmodemOptions::for_model(model)?;
    Ok((model, options))
}

/// Watches the transfer's events: whether the calculator started, and a
/// progress line on a terminal.
struct Progress {
    started: Option<Check>,
    show: bool,
    shown: bool,
}

impl Progress {
    fn new(show: bool) -> Self {
        Progress {
            started: None,
            show,
            shown: false,
        }
    }

    fn on_event(&mut self, event: &Event) {
        match event {
            Event::Started { check } => {
                self.started = Some(*check);
                if self.show {
                    eprintln!("Started ({}).", check_name(*check));
                }
            }
            Event::Progress { bytes, total } if self.show => {
                match total {
                    Some(t) => eprint!("\r{bytes} of {t} bytes"),
                    None => eprint!("\r{bytes} bytes"),
                }
                let _ = std::io::stderr().flush();
                self.shown = true;
            }
            _ => {}
        }
    }

    /// End the progress line.
    fn finish(&self) {
        if self.shown {
            eprintln!();
        }
    }
}

impl Ctx {
    /// Whether to talk to a person on stderr: text mode, or a terminal.
    fn tell_user(&self) -> bool {
        self.format == Format::Text || std::io::stderr().is_terminal()
    }

    /// Show what to type, then wait for the calculator.
    fn announce(&self, plan: &XmodemPlan, start_timeout: Duration) {
        if self.tell_user() {
            eprintln!(
                "{} hptx waits {} s.",
                plan.instructions(),
                start_timeout.as_secs()
            );
        }
    }

    /// The error for a transfer that failed after the server ended.
    fn xmodem_failure(
        &self,
        err: Error,
        progress: &Progress,
        plan: &XmodemPlan,
        start_timeout: Duration,
        what: &str,
    ) -> anyhow::Error {
        let command = plan.direction.calculator_command();
        if progress.started.is_none()
            && matches!(err, Error::Xmodem(hptx_core::xmodem_proto::Error::Timeout))
        {
            return Hinted::new(
                format!(
                    "{what}: the calculator did not start {command} within {} s",
                    start_timeout.as_secs()
                ),
                format!(
                    "type {} on the calculator and press ENTER while hptx waits \
                     (--start-timeout SECS gives more time). The calculator has left server \
                     mode: if {command} is running, press ON to cancel it, then type SERVER \
                     before running hptx again",
                    plan.keys
                ),
            )
            .into();
        }
        let mut hint = format!(
            "the calculator has left server mode: press ON if {command} still runs, type \
             SERVER, then run the command again; check the cable if it keeps failing"
        );
        if plan.direction == XmodemDirection::ToCalculator && plan.model == Model::Hp48Gx {
            let _ = write!(
                hint,
                ". {} may hold an empty string now: `{}`",
                plan.name,
                self.cmd(&format!("rm {}", shell_quote(&plan.name)))
            );
        }
        anyhow::Error::new(err).context(Hinted::new(format!("{what} failed"), hint))
    }

    pub(crate) fn put_xmodem(
        &mut self,
        args: &PutArgs,
        name: &str,
        data: &[u8],
        file_label: &str,
    ) -> Result<Outcome> {
        let start_timeout = args.xmodem.start_timeout();
        let mut calc = self.connect()?;
        let (model, mut options) =
            xmodem_model(&mut calc).with_context(|| format!("put {name}"))?;
        if let Some(e) = lookup(&mut calc, name)? {
            return Err(Hinted::new(
                format!(
                    "{name} exists ({}, {} bytes)",
                    e.kind,
                    util::number_text(e.size)
                ),
                format!(
                    "XRECV does not replace a variable (the 49G stores the object as {name}.1, \
                     the 48G/GX refuses): `{}` first, or replace it over Kermit with `{}`",
                    self.cmd(&format!("rm {}", shell_quote(name))),
                    self.cmd(&format!(
                        "put {} --as {} --overwrite",
                        shell_quote(file_label),
                        shell_quote(name)
                    ))
                ),
            )
            .into());
        }
        let keys = format!("'{name}' XRECV");
        if args.dry_run {
            let mut again = format!(
                "put {} --as {} --protocol xmodem",
                shell_quote(file_label),
                shell_quote(name)
            );
            if let Some(t) = args.xmodem.start_timeout {
                let _ = write!(again, " --start-timeout {t}");
            }
            return Ok(self.dry_run_outcome(
                json!({
                    "file": file_label,
                    "name": name,
                    "bytes": data.len(),
                    "protocol": "xmodem",
                }),
                format!(
                    "Would store {file_label} ({} bytes) as {name} over XModem ({}, {}).",
                    data.len(),
                    model.name(),
                    profile_text(model)
                ),
                model,
                &keys,
                start_timeout,
                Hint::cmd("Store it", self.cmd(&again)),
            ));
        }
        let plan = calc
            .prepare_for_xmodem(XmodemDirection::ToCalculator, name)
            .with_context(|| format!("put {name}"))?;
        self.announce(&plan, start_timeout);
        options.start_timeout = start_timeout;
        let mut session = XmodemSession::new(calc.into_transport(), options);
        let mut progress = Progress::new(self.tell_user() && std::io::stderr().is_terminal());
        let sent = session.send_with(data, &mut |e| progress.on_event(e));
        progress.finish();
        let report = sent.map_err(|e| {
            self.xmodem_failure(e, &progress, &plan, start_timeout, &format!("put {name}"))
        })?;
        let text = format!(
            "{file_label} -> {name} ({} bytes, xmodem, {})\n{}.",
            report.bytes,
            check_name(report.check),
            SERVER_AGAIN
        );
        Ok(Outcome {
            results: json!({
                "file": file_label,
                "name": name,
                "bytes": report.bytes,
                "protocol": "xmodem",
                "check": check_name(report.check),
                "model": plan.model.name(),
                "keys": plan.keys,
                "instructions": plan.instructions(),
                "switched_to_rpn": plan.switched_to_rpn,
                "server_mode": false,
            }),
            total: None,
            dir: None,
            hints: self.after_hints(&plan),
            text,
        })
    }

    pub(crate) fn get_xmodem(&mut self, args: &GetArgs) -> Result<Option<Outcome>> {
        let name = args.name.as_str();
        let start_timeout = args.xmodem.start_timeout();
        let to_stdout = args.output.as_deref() == Some(Path::new("-"));
        let file = args.output.clone().unwrap_or_else(|| PathBuf::from(name));
        if !to_stdout {
            refuse_existing_file(&file, args.force)?;
        }
        let mut calc = self.connect()?;
        let (model, mut options) =
            xmodem_model(&mut calc).with_context(|| format!("get {name}"))?;
        let Some(entry) = lookup(&mut calc, name)? else {
            return Err(Hinted::new(
                format!("get {name}: no such variable in the current directory"),
                format!("`{}` lists the names", self.cmd("ls")),
            )
            .into());
        };
        let keys = format!("'{name}' XSEND");
        if args.dry_run {
            let target = if to_stdout {
                "stdout".to_string()
            } else {
                file.display().to_string()
            };
            let mut again = format!("get {} --protocol xmodem", shell_quote(name));
            if let Some(o) = &args.output {
                let _ = write!(again, " -o {}", shell_quote(&o.display().to_string()));
            }
            if args.force {
                again.push_str(" --force");
            }
            if let Some(t) = args.xmodem.start_timeout {
                let _ = write!(again, " --start-timeout {t}");
            }
            return Ok(Some(self.dry_run_outcome(
                json!({
                    "name": name,
                    "file": target,
                    "type": entry.kind,
                    "size": util::number(entry.size),
                    "protocol": "xmodem",
                }),
                format!(
                    "Would download {name} ({}, {} bytes) to {target} over XModem ({}, {}).",
                    entry.kind,
                    util::number_text(entry.size),
                    model.name(),
                    profile_text(model)
                ),
                model,
                &keys,
                start_timeout,
                Hint::cmd("Download it", self.cmd(&again)),
            )));
        }
        let plan = calc
            .prepare_for_xmodem(XmodemDirection::FromCalculator, name)
            .with_context(|| format!("get {name}"))?;
        self.announce(&plan, start_timeout);
        options.start_timeout = start_timeout;
        let mut session = XmodemSession::new(calc.into_transport(), options);
        let mut progress = Progress::new(self.tell_user() && std::io::stderr().is_terminal());
        let received = session.receive_with(&mut |e| progress.on_event(e));
        progress.finish();
        let got = received.map_err(|e| {
            self.xmodem_failure(e, &progress, &plan, start_timeout, &format!("get {name}"))
        })?;
        let padding_note = match got.stripped {
            Some(_) => None,
            None => Some(format!(
                "the object's end was not found (not an HP binary object, or an unknown \
                 type): the file keeps all {} bytes received, up to {} of them padding",
                got.received, got.last_block
            )),
        };
        if to_stdout {
            if let Some(note) = &padding_note
                && self.tell_user()
            {
                eprintln!("note: {note}");
            }
            if self.tell_user() {
                eprintln!("{SERVER_AGAIN}.");
            }
            let mut out = std::io::stdout().lock();
            out.write_all(&got.data).context("writing to stdout")?;
            out.flush().context("writing to stdout")?;
            return Ok(None);
        }
        write_file(&file, &got.data, args.force)?;
        let kind = object_type_name_binary(&got.data);
        let mut text = format!(
            "{name} -> {} ({} bytes, xmodem, {}",
            file.display(),
            got.data.len(),
            check_name(got.check)
        );
        if let Some(k) = &kind {
            let _ = write!(text, ", {k}");
        }
        text.push(')');
        if let Some(note) = &padding_note {
            let _ = write!(text, "\nnote: {note}");
        }
        let _ = write!(text, "\n{SERVER_AGAIN}.");
        Ok(Some(Outcome {
            results: json!({
                "name": name,
                "file": file.display().to_string(),
                "bytes": got.data.len(),
                "received": got.received,
                "stripped": got.stripped,
                "protocol": "xmodem",
                "check": check_name(got.check),
                "type": kind,
                "model": plan.model.name(),
                "keys": plan.keys,
                "instructions": plan.instructions(),
                "switched_to_rpn": plan.switched_to_rpn,
                "server_mode": false,
            }),
            total: None,
            dir: None,
            hints: self.after_hints(&plan),
            text,
        }))
    }

    /// `--dry-run` with xmodem: nothing changed, the server still runs.
    fn dry_run_outcome(
        &self,
        mut results: Value,
        first: String,
        model: Model,
        keys: &str,
        start_timeout: Duration,
        run: Hint,
    ) -> Outcome {
        let steps = format!(
            "hptx would end server mode (Kermit FINISH){}, then wait {} s for you to type \
             {keys} on the calculator and press ENTER. After the transfer, type SERVER on the \
             calculator again.",
            if model == Model::Hp49G {
                ", switching an algebraic-mode 49G to RPN first"
            } else {
                ""
            },
            start_timeout.as_secs()
        );
        if let Value::Object(map) = &mut results {
            map.insert("model".into(), json!(model.name()));
            map.insert("keys".into(), json!(keys));
            map.insert("start_timeout".into(), json!(start_timeout.as_secs()));
            map.insert("dry_run".into(), json!(true));
        }
        Outcome {
            results,
            total: None,
            dir: None,
            hints: vec![run],
            text: format!("{first}\n{steps}\nNothing was changed; the server is still running."),
        }
    }

    /// Hints after a transfer: the plan's notes, SERVER, then `ls`.
    fn after_hints(&self, plan: &XmodemPlan) -> Vec<Hint> {
        let mut hints: Vec<Hint> = plan
            .notes
            .iter()
            .filter(|n| !n.contains("type SERVER"))
            .map(Hint::advice)
            .collect();
        hints.push(Hint::advice(SERVER_AGAIN));
        hints.push(Hint::cmd("Then list the directory", self.cmd("ls")));
        hints
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn check_names() {
        assert_eq!(check_name(Check::HpCrc), "HP CRC");
        assert_eq!(check_name(Check::Checksum), "checksum");
    }

    #[test]
    fn progress_tracks_the_start() {
        let mut p = Progress::new(false);
        p.on_event(&Event::Progress {
            bytes: 1,
            total: None,
        });
        assert_eq!(p.started, None);
        p.on_event(&Event::Started {
            check: Check::HpCrc,
        });
        assert_eq!(p.started, Some(Check::HpCrc));
        assert!(!p.shown);
    }
}
