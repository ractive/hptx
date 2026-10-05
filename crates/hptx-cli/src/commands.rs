//! The commands: connect, act, build an [`Outcome`].

use std::fmt::Write as _;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::parser::ValueSource;
use clap::{Args, CommandFactory, FromArgMatches, ValueEnum};
use hptx_core::calc::validate_name;
use hptx_core::object::{self, ObjectType};
use hptx_core::reply::{Entry, Iopar, Listing, parse_real};
use hptx_core::{Calculator, Error, Options, Session, TransferMode, transport};
use serde_json::{Value, json};

use crate::error::{Hinted, LinkInfo, describe};
use crate::offline::{self, GrobCommand, ObjectCommand, PartialFailure};
use crate::output::{self, Format, Hint, Outcome, shell_quote};
use crate::util;
use crate::{Cli, Command, Global};

/// The temporary variable `restore` leaves in port 0.
const RESTORE_LEFTOVER: &str = ":0:HPTXRS";

/// The temporary name `put --overwrite` sends under before it replaces the
/// old variable, so a failed transfer leaves the old one in place.
const PUT_TEMP: &str = "HPTXPT";

/// `get` options.
#[derive(Args, Debug)]
pub struct GetArgs {
    /// Variable in the current directory.
    pub name: String,
    /// Output file [default: NAME]; - for stdout.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Transfer as %%HP: text instead of binary.
    #[arg(long)]
    pub ascii: bool,
    /// Replace an existing file.
    #[arg(long)]
    pub force: bool,
    #[command(flatten)]
    pub xmodem: XmodemArgs,
    /// Check the variable and show what would happen, transfer nothing (with
    /// xmodem: the server keeps running).
    #[arg(long)]
    pub dry_run: bool,
}

/// How `get` and `put` move the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum Protocol {
    /// The calculator's Kermit server (SERVER): no typing on the calculator.
    #[default]
    Kermit,
    /// XRECV/XSEND, typed on the calculator when hptx says so (48G/GX, 49G).
    Xmodem,
}

/// `--protocol` and `--start-timeout`, shared by `get` and `put`.
#[derive(Args, Debug, Clone, Copy)]
pub struct XmodemArgs {
    /// Transfer protocol.
    #[arg(long, value_enum, default_value_t = Protocol::Kermit)]
    pub protocol: Protocol,
    /// xmodem: seconds to wait for XRECV/XSEND to start on the calculator
    /// (1-600) [default: 60].
    #[arg(
        long,
        value_parser = clap::value_parser!(u64).range(1..=600),
        value_name = "SECS"
    )]
    pub start_timeout: Option<u64>,
}

impl XmodemArgs {
    /// Refuse options that do not fit the protocol: `kermit_only` flags
    /// (set, name) with xmodem, `--start-timeout` with kermit.
    pub fn check(&self, kermit_only: &[(bool, &str)]) -> Result<()> {
        match self.protocol {
            Protocol::Xmodem => {
                if let Some((_, flag)) = kermit_only.iter().find(|(set, _)| *set) {
                    return Err(Hinted::new(
                        format!("{flag} works with Kermit only"),
                        xmodem_conflict_hint(flag),
                    )
                    .into());
                }
            }
            Protocol::Kermit => {
                if self.start_timeout.is_some() {
                    return Err(Hinted::new(
                        "--start-timeout applies to --protocol xmodem only",
                        "add --protocol xmodem, or drop --start-timeout (Kermit uses --timeout)",
                    )
                    .into());
                }
            }
        }
        Ok(())
    }

    /// The XModem start window.
    pub fn start_timeout(&self) -> Duration {
        Duration::from_secs(self.start_timeout.unwrap_or(DEFAULT_START_TIMEOUT))
    }
}

/// `--start-timeout` default in seconds.
const DEFAULT_START_TIMEOUT: u64 = 60;

fn xmodem_conflict_hint(flag: &str) -> &'static str {
    match flag {
        "--overwrite" => {
            "XModem cannot replace a variable (the 49G stores NAME.1, the 48G/GX refuses):              drop --protocol xmodem, or `hptx rm NAME` first"
        }
        _ => "XModem moves the file byte for byte; drop the flag or use --protocol kermit",
    }
}

/// `put` options.
#[derive(Args, Debug)]
pub struct PutArgs {
    /// File to upload; - for stdin (then --as is required).
    pub file: PathBuf,
    /// Variable name [default: the file name without extension].
    #[arg(long = "as", value_name = "NAME")]
    pub name: Option<String>,
    /// Transfer as text (%%HP: files are sent this way by default).
    #[arg(long, conflicts_with = "binary")]
    pub ascii: bool,
    /// Transfer in binary even for a %%HP: text file.
    #[arg(long)]
    pub binary: bool,
    /// Replace an existing variable (sends as HPTXPT, then swaps it in; Kermit only).
    #[arg(long)]
    pub overwrite: bool,
    /// Show what would happen, send nothing (with xmodem: the server keeps running).
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub xmodem: XmodemArgs,
}

/// `restore` options.
#[derive(Args, Debug)]
pub struct RestoreArgs {
    /// Backup file from `hptx backup`.
    #[arg(required_unless_present = "cleanup", conflicts_with = "cleanup")]
    pub file: Option<PathBuf>,
    /// Delete :0:HPTXRS left in port 0 by an earlier restore.
    #[arg(long)]
    pub cleanup: bool,
    /// Do not ask for confirmation.
    #[arg(long, short)]
    pub yes: bool,
    /// Check the file and the calculator, change nothing.
    #[arg(long)]
    pub dry_run: bool,
}

/// Transfer mode for `settings --mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ModeArg {
    /// Flag -35 set: objects travel as HPHP48/49 binary.
    Binary,
    /// Flag -35 clear: objects travel as %%HP: text.
    Ascii,
}

/// `settings` options.
#[derive(Args, Debug)]
pub struct SettingsArgs {
    /// IOPAR baud rate.
    #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(["1200", "2400", "4800", "9600"]))]
    pub baud: Option<String>,
    /// IOPAR parity: 0 none, 1 odd, 2 even, 3 mark, 4 space (negative: transmit only).
    #[arg(long, allow_negative_numbers = true, value_parser = clap::value_parser!(i8).range(-4..=4))]
    pub parity: Option<i8>,
    /// IOPAR Kermit block check type: 1 or 2 (checksums), 3 (16-bit CRC).
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=3))]
    pub checksum: Option<u8>,
    /// IOPAR character translation for ASCII transfers: 0 none, 1 LF as CR LF,
    /// 2 also characters 128-159 as trigraphs, 3 also 160-255.
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=3))]
    pub translate: Option<u8>,
    /// Transfer mode, flag -35.
    #[arg(long, value_enum)]
    pub mode: Option<ModeArg>,
}

/// Parse the command line, run it, print the results or the error.
pub fn main_entry() -> ExitCode {
    let matches = Cli::command().get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };
    let port_on_command_line = matches.value_source("port") == Some(ValueSource::CommandLine);
    let format = Format::resolve(cli.global.format, cli.global.json);
    let mut ctx = Ctx {
        format,
        link: LinkInfo {
            addr: None,
            timeout: Duration::from_secs(cli.global.timeout),
            retries: cli.global.retries,
        },
        port_on_command_line,
        global: cli.global,
    };
    let result = ctx.dispatch(cli.command);
    match result {
        Ok(Some(outcome)) => match output::render(&outcome, ctx.format, ctx.global.jq.as_deref()) {
            Ok(text) => match output::print_stdout(&text) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: cannot write output: {e}");
                    ExitCode::FAILURE
                }
            },
            Err(e) => {
                let failure = output::Failure {
                    error: e,
                    hint: Some("--jq takes a jq filter over {results, total, hints}".into()),
                    stack: None,
                };
                eprintln!("{}", failure.render(ctx.format));
                ExitCode::FAILURE
            }
        },
        Ok(None) => ExitCode::SUCCESS,
        Err(err) if err.downcast_ref::<PartialFailure>().is_some() => {
            // Some of several files failed: print every result, then fail.
            if let Some(partial) = err.downcast_ref::<PartialFailure>()
                && let Ok(text) =
                    output::render(&partial.outcome, ctx.format, ctx.global.jq.as_deref())
            {
                let _ = output::print_stdout(&text);
            }
            eprintln!("{}", describe(&err, &ctx.link).render(ctx.format));
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("{}", describe(&err, &ctx.link).render(ctx.format));
            ExitCode::FAILURE
        }
    }
}

pub(crate) struct Ctx {
    pub(crate) global: Global,
    pub(crate) format: Format,
    pub(crate) link: LinkInfo,
    pub(crate) port_on_command_line: bool,
}

impl Ctx {
    /// `Ok(None)`: output already written (raw bytes to stdout).
    fn dispatch(&mut self, command: Command) -> Result<Option<Outcome>> {
        Ok(Some(match command {
            Command::Ports => self.ports()?,
            Command::Info => self.info()?,
            Command::Ls { path } => self.ls(path.as_deref())?,
            Command::Get(args) => return self.get(&args),
            Command::Put(args) => self.put(&args)?,
            Command::Rm { names, dry_run } => self.rm(&names, dry_run)?,
            Command::Mkdir { name } => self.mkdir(&name)?,
            Command::Mv { from, to } => self.mv(&from, &to)?,
            Command::Run { words } => self.run_rpl(&words.join(" "))?,
            Command::Screenshot { output, force } => {
                return self.screenshot(output.as_deref(), force);
            }
            Command::Backup { output, force } => self.backup(output.as_deref(), force)?,
            Command::Restore(args) => self.restore(&args)?,
            Command::Settings(args) => self.settings(&args)?,
            Command::Finish => self.finish()?,
            Command::Xserv { command } => return self.xserv(command),
            Command::Object {
                command: ObjectCommand::Inspect { files },
            } => offline::inspect(&files)?,
            Command::Object {
                command: ObjectCommand::Convert(args),
            } => return offline::convert(&args),
            Command::Grob {
                command:
                    GrobCommand::ToPng {
                        file,
                        output,
                        force,
                    },
            } => return offline::grob_to_png(&file, output.as_deref(), force),
            Command::Completions { shell } => return offline::completions(shell),
        }))
    }

    /// A command line for a hint: `hptx [--port P] ARGS`.
    pub(crate) fn cmd(&self, args: &str) -> String {
        match (&self.global.port, self.port_on_command_line) {
            (Some(port), true) => format!("hptx --port {} {args}", shell_quote(port)),
            _ => format!("hptx {args}"),
        }
    }

    /// Progress note on stderr, only for a person watching a terminal.
    pub(crate) fn status(&self, text: &str) {
        if self.format == Format::Text && std::io::stderr().is_terminal() {
            eprintln!("{text}");
        }
    }

    /// Open the link, finish a pending restore cleanup, change to `--dir`.
    pub(crate) fn connect(&mut self) -> Result<Calculator> {
        let mut calc = self.open()?;
        let marker = self.restore_marker();
        if marker.exists() {
            if purge_leftover(&mut calc).context("deleting :0:HPTXRS left by restore")? {
                eprintln!("note: deleted {RESTORE_LEFTOVER}, left in port 0 by hptx restore");
            }
            let _ = std::fs::remove_file(&marker);
        }
        if let Some(dir) = self.global.dir.clone() {
            cd(&mut calc, &dir)?;
        }
        Ok(calc)
    }

    fn open(&mut self) -> Result<Calculator> {
        let addr = crate::port::resolve(self.global.port.as_deref())?;
        self.link.addr = Some(addr.clone());
        let link = transport::open(&addr).with_context(|| format!("cannot open {addr}"))?;
        let options = Options {
            kermit: kermit_proto::Config {
                timeout: self.link.timeout,
                retries: self.link.retries,
                ..kermit_proto::Config::default()
            },
            ..Options::default()
        };
        let session = Session::new(link, options).with_context(|| format!("cannot open {addr}"))?;
        Ok(Calculator::new(session))
    }

    /// A file in the temp directory that says a restore on this port still
    /// needs its cleanup.
    fn restore_marker(&self) -> PathBuf {
        let addr = self.link.addr.as_deref().unwrap_or_default();
        let safe: String = addr
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        std::env::temp_dir().join(format!("hptx-restore-pending-{safe}"))
    }

    fn ports(&self) -> Result<Outcome> {
        let ports = crate::port::list()?;
        let mut text = String::new();
        if ports.is_empty() {
            text.push_str("No serial ports.");
        }
        for p in &ports {
            let _ = write!(text, "{:<32} {:<9}", p.name, p.kind);
            if let Some(id) = &p.usb_id {
                let _ = write!(text, " {id}");
            }
            if let Some(product) = &p.product {
                let _ = write!(text, " {product}");
            }
            if p.candidate {
                text.push_str("  (USB serial)");
            }
            text.push('\n');
        }
        let candidates: Vec<_> = ports.iter().filter(|p| p.candidate).collect();
        let hints = match candidates.as_slice() {
            [one] => vec![Hint::cmd(
                format!(
                    "Talk to the calculator on {} (picked automatically)",
                    one.name
                ),
                "hptx info",
            )],
            [] => vec![Hint::advice(
                "No USB serial port: connect the cable, or use --port tcp://localhost:4848 for the emulator",
            )],
            many => many
                .iter()
                .map(|p| {
                    Hint::cmd(
                        format!("Talk to the calculator on {}", p.name),
                        format!("hptx --port {} info", shell_quote(&p.name)),
                    )
                })
                .collect(),
        };
        Ok(Outcome {
            results: serde_json::to_value(&ports)?,
            total: Some(ports.len() as u64),
            dir: None,
            hints,
            text,
        })
    }

    fn info(&mut self) -> Result<Outcome> {
        let mut calc = self.connect()?;
        let version = calc.version().context("VERSION")?;
        let free = calc.mem().context("MEM")?;
        let path = calc.path().context("PATH")?;
        let iopar = calc.iopar().context("IOPAR")?;
        let mode = calc.transfer_mode().context("flag -35")?;
        let model = hptx_core::Model::from_version(version.as_deref()).name();
        let addr = self.link.addr.clone().unwrap_or_default();
        let mut text = String::new();
        let _ = writeln!(text, "model     {model}");
        let _ = writeln!(
            text,
            "version   {}",
            version
                .as_deref()
                .unwrap_or("(none: the 48S/SX has no VERSION)")
        );
        let _ = writeln!(text, "path      {}", util::path_string(&path));
        let _ = writeln!(text, "free      {} bytes", util::number_text(free));
        let _ = writeln!(text, "iopar     {}", iopar_text(&iopar));
        let _ = writeln!(text, "transfer  {}", mode_text(mode));
        let _ = writeln!(text, "port      {addr}");
        Ok(Outcome {
            results: json!({
                "model": model,
                "version": version,
                "path": path,
                "free_bytes": util::number(free),
                "iopar": iopar_json(&iopar),
                "transfer_mode": mode_name(mode),
                "port": addr,
            }),
            total: None,
            dir: None,
            hints: vec![Hint::cmd("List the current directory", self.cmd("ls"))],
            text,
        })
    }

    fn ls(&mut self, path: Option<&str>) -> Result<Outcome> {
        let mut calc = match path {
            Some(p) => {
                let mut calc = self.open_with_cleanup()?;
                cd(&mut calc, p)?;
                calc
            }
            None => self.connect()?,
        };
        let listing = calc.list().context("ls")?;
        // The 48SX listing has no path; ask only when a hint needs it.
        let path: Option<Vec<String>> = match (&listing.path, path, self.global.dir.as_deref()) {
            (Some(p), _, _) => Some(p.clone()),
            (None, Some(p), _) | (None, None, Some(p)) => Some(
                std::iter::once("HOME".to_string())
                    .chain(util::parse_dir(p))
                    .collect(),
            ),
            (None, None, None) if listing.entries.iter().any(Entry::is_directory) => {
                Some(calc.path().context("PATH")?)
            }
            _ => None,
        };
        let outcome = self.listing_outcome(&listing, path.as_deref());
        Ok(outcome)
    }

    /// `connect` without `--dir` (`ls PATH` changes directory itself).
    fn open_with_cleanup(&mut self) -> Result<Calculator> {
        let dir = self.global.dir.take();
        let calc = self.connect();
        self.global.dir = dir;
        calc
    }

    fn listing_outcome(&self, listing: &Listing, path: Option<&[String]>) -> Outcome {
        let mut text = String::new();
        let mut header = Vec::new();
        if let Some(p) = path {
            header.push(util::path_string(p));
        }
        let count = listing.entries.len();
        header.push(format!(
            "{count} variable{}",
            if count == 1 { "" } else { "s" }
        ));
        if let Some(free) = listing.free {
            header.push(format!("{} bytes free", util::number_text(free)));
        }
        let _ = writeln!(text, "{}", header.join(", "));
        let width = listing
            .entries
            .iter()
            .map(|e| e.name.chars().count())
            .max()
            .unwrap_or(0)
            .max(4);
        for e in &listing.entries {
            let pad = width - e.name.chars().count();
            let _ = writeln!(
                text,
                "  {}{} {:>8}  {:<16} #{:04X}h",
                e.name,
                " ".repeat(pad),
                util::number_text(e.size),
                e.kind,
                e.checksum
            );
        }
        let results: Vec<Value> = listing
            .entries
            .iter()
            .map(|e| {
                json!({
                    "name": e.name,
                    "size": util::number(e.size),
                    "type": e.kind,
                    "checksum": e.checksum,
                    "directory": e.is_directory(),
                })
            })
            .collect();
        let mut hints = Vec::new();
        if let Some(e) = listing.entries.iter().find(|e| !e.is_directory()) {
            hints.push(Hint::cmd(
                format!("Download {}", e.name),
                self.cmd(&format!("get {}", shell_quote(&e.name))),
            ));
        }
        if let (Some(e), Some(p)) = (listing.entries.iter().find(|e| e.is_directory()), path) {
            let mut sub: Vec<String> = p.to_vec();
            sub.push(e.name.clone());
            hints.push(Hint::cmd(
                format!("List directory {}", e.name),
                self.cmd(&format!("ls {}", shell_quote(&util::path_string(&sub)))),
            ));
        }
        if listing.entries.is_empty() {
            hints.push(Hint::cmd("Upload a file", self.cmd("put FILE --as NAME")));
        }
        Outcome {
            results: Value::from(results),
            total: Some(count as u64),
            dir: path.map(util::path_string),
            hints,
            text,
        }
    }

    fn get(&mut self, args: &GetArgs) -> Result<Option<Outcome>> {
        validate_name(&args.name).with_context(|| format!("get {}", args.name))?;
        args.xmodem
            .check(&[(args.ascii, "--ascii")])
            .with_context(|| format!("get {}", args.name))?;
        if args.xmodem.protocol == Protocol::Xmodem {
            return self.get_xmodem(args);
        }
        let to_stdout = args.output.as_deref() == Some(Path::new("-"));
        let file = args
            .output
            .clone()
            .unwrap_or_else(|| PathBuf::from(&args.name));
        if !to_stdout {
            refuse_existing_file(&file, args.force)?;
        }
        let mode = if args.ascii {
            TransferMode::Ascii
        } else {
            TransferMode::Binary
        };
        let mut calc = self.connect()?;
        if args.dry_run {
            return self
                .get_dry_run(&mut calc, args, &file, to_stdout)
                .map(Some);
        }
        let data = calc
            .get(&args.name, mode)
            .with_context(|| format!("get {}", args.name))?;
        if to_stdout {
            let mut out = std::io::stdout().lock();
            out.write_all(&data).context("writing to stdout")?;
            out.flush().context("writing to stdout")?;
            return Ok(None);
        }
        write_file(&file, &data, args.force)?;
        let kind = object_type_name(&data, mode);
        let mut text = format!(
            "{} -> {} ({} bytes, {}",
            args.name,
            file.display(),
            data.len(),
            mode_name(mode)
        );
        if let Some(k) = &kind {
            let _ = write!(text, ", {k}");
        }
        text.push(')');
        Ok(Some(Outcome {
            results: json!({
                "name": args.name,
                "file": file.display().to_string(),
                "bytes": data.len(),
                "mode": mode_name(mode),
                "type": kind,
            }),
            total: None,
            dir: None,
            hints: Vec::new(),
            text,
        }))
    }

    /// `get --dry-run` over Kermit: the variable exists, nothing is written.
    fn get_dry_run(
        &self,
        calc: &mut Calculator,
        args: &GetArgs,
        file: &Path,
        to_stdout: bool,
    ) -> Result<Outcome> {
        let listing = calc.list().context("ls")?;
        let Some(e) = listing.entries.iter().find(|e| e.name == args.name) else {
            return Err(Hinted::new(
                format!(
                    "get {}: no such variable in the current directory",
                    args.name
                ),
                format!("`{}` lists the names", self.cmd("ls")),
            )
            .into());
        };
        let target = if to_stdout {
            "stdout".to_string()
        } else {
            file.display().to_string()
        };
        let mode = if args.ascii { "ascii" } else { "binary" };
        let mut again = format!("get {}", shell_quote(&args.name));
        if let Some(o) = &args.output {
            let _ = write!(again, " -o {}", shell_quote(&o.display().to_string()));
        }
        if args.ascii {
            again.push_str(" --ascii");
        }
        if args.force {
            again.push_str(" --force");
        }
        Ok(Outcome {
            results: json!({
                "name": args.name,
                "file": target,
                "type": e.kind,
                "size": util::number(e.size),
                "mode": mode,
                "dry_run": true,
            }),
            total: None,
            dir: None,
            hints: vec![Hint::cmd("Download it", self.cmd(&again))],
            text: format!(
                "Would download {} ({}, {} bytes) to {target} ({mode}).",
                args.name,
                e.kind,
                util::number_text(e.size)
            ),
        })
    }

    fn put(&mut self, args: &PutArgs) -> Result<Outcome> {
        let from_stdin = args.file.as_path() == Path::new("-");
        let name = match (&args.name, from_stdin) {
            (Some(n), _) => n.clone(),
            (None, true) => {
                return Err(
                    Hinted::new("put -: no variable name", "give one with --as NAME").into(),
                );
            }
            (None, false) => util::name_from_file(&args.file).unwrap_or_default(),
        };
        let data = if from_stdin {
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .context("reading stdin")?;
            buf
        } else {
            std::fs::read(&args.file)
                .with_context(|| format!("cannot read {}", args.file.display()))?
        };
        if validate_name(&name).is_err() {
            return Err(Hinted::new(
                format!("{name:?} is not a valid calculator name"),
                format!(
                    "choose one with --as NAME, e.g. `{}`; names start with a letter and have \
                     no spaces, quotes, brackets or operators",
                    self.cmd(&format!(
                        "put {} --as PRG",
                        shell_quote(&args.file.display().to_string())
                    ))
                ),
            )
            .into());
        }
        args.xmodem
            .check(&[
                (args.ascii, "--ascii"),
                (args.binary, "--binary"),
                (args.overwrite, "--overwrite"),
            ])
            .with_context(|| format!("put {name}"))?;
        let file_label = args.file.display().to_string();
        if args.xmodem.protocol == Protocol::Xmodem {
            return self.put_xmodem(args, &name, &data, &file_label);
        }
        let mode = if args.ascii || (!args.binary && data.starts_with(b"%%HP:")) {
            TransferMode::Ascii
        } else {
            TransferMode::Binary
        };
        let mut calc = self.connect()?;
        let listing = calc.list().context("ls")?;
        let existing = listing.entries.iter().find(|e| e.name == name).cloned();
        if let Some(e) = &existing {
            if e.is_directory() {
                return Err(Hinted::new(
                    format!("{name} is a directory"),
                    "choose another name with --as NAME; put never replaces a directory",
                )
                .into());
            }
            if !args.overwrite {
                return Err(Hinted::new(
                    format!(
                        "{name} exists ({}, {} bytes)",
                        e.kind,
                        util::number_text(e.size)
                    ),
                    format!(
                        "`{}` replaces it, or choose another name with --as NAME",
                        self.cmd(&format!(
                            "put {} --as {} --overwrite",
                            shell_quote(&file_label),
                            shell_quote(&name)
                        ))
                    ),
                )
                .into());
            }
        }
        if existing.is_some() && listing.entries.iter().any(|e| e.name == PUT_TEMP) {
            return Err(Hinted::new(
                format!("{PUT_TEMP} exists in the current directory"),
                format!(
                    "it is hptx's temporary variable for put --overwrite, probably left by an \
                     interrupted run: `{}` to keep it, then `{}`",
                    self.cmd(&format!("get {PUT_TEMP}")),
                    self.cmd(&format!("rm {PUT_TEMP}"))
                ),
            )
            .into());
        }
        let replaces = existing
            .as_ref()
            .map(|e| json!({"type": e.kind, "size": util::number(e.size)}));
        if args.dry_run {
            let mut text = format!(
                "Would store {file_label} ({} bytes, {}) as {name}",
                data.len(),
                mode_name(mode)
            );
            if let Some(e) = &existing {
                let _ = write!(
                    text,
                    ", sending it as {PUT_TEMP} first and then replacing the existing {name} \
                     ({}, {} bytes)",
                    e.kind,
                    util::number_text(e.size)
                );
            }
            text.push('.');
            let mut again = format!(
                "put {} --as {}",
                shell_quote(&file_label),
                shell_quote(&name)
            );
            if args.overwrite {
                again.push_str(" --overwrite");
            }
            if args.ascii {
                again.push_str(" --ascii");
            }
            if args.binary {
                again.push_str(" --binary");
            }
            return Ok(Outcome {
                results: json!({
                    "file": file_label,
                    "name": name,
                    "bytes": data.len(),
                    "mode": mode_name(mode),
                    "replaces": replaces,
                    "dry_run": true,
                }),
                total: None,
                dir: None,
                hints: vec![Hint::cmd("Store it", self.cmd(&again))],
                text,
            });
        }
        let stored = if existing.is_some() {
            self.put_replacing(&mut calc, &name, &data, mode)?
        } else {
            calc.put(&name, &data, mode)
                .with_context(|| format!("put {name}"))?
        };
        let mut text = format!(
            "{file_label} -> {stored} ({} bytes, {})",
            data.len(),
            mode_name(mode)
        );
        if existing.is_some() {
            text.push_str(", replaced the old variable");
        }
        if stored != name {
            let _ = write!(
                text,
                "\nnote: the calculator stored it as {stored}, not {name}"
            );
        }
        Ok(Outcome {
            results: json!({
                "file": file_label,
                "name": stored,
                "bytes": data.len(),
                "mode": mode_name(mode),
                "replaced": replaces,
            }),
            total: None,
            dir: None,
            hints: vec![Hint::cmd("List the directory", self.cmd("ls"))],
            text,
        })
    }

    /// Replace the existing variable `name`: send `data` as [`PUT_TEMP`],
    /// then delete `name` and rename the temporary to it. A failed transfer
    /// leaves the old variable untouched. Returns the final name.
    fn put_replacing(
        &self,
        calc: &mut Calculator,
        name: &str,
        data: &[u8],
        mode: TransferMode,
    ) -> Result<String> {
        let temp = match calc.put(PUT_TEMP, data, mode) {
            Ok(stored) => stored,
            Err(e) => {
                // Best effort: a partial transfer may have left the
                // temporary; the transfer error matters more.
                let _ = calc.remove(PUT_TEMP);
                return Err(anyhow::Error::new(e).context(Hinted::new(
                    format!("put {name} failed; the old {name} is unchanged"),
                    format!(
                        "check the link and try again; if {PUT_TEMP} is left over, `{}`",
                        self.cmd(&format!("rm {PUT_TEMP}"))
                    ),
                )));
            }
        };
        if let Err(e) = calc.remove(name) {
            return Err(anyhow::Error::new(e).context(Hinted::new(
                format!("deleting the old {name} failed; the new one is stored as {temp}"),
                format!(
                    "`{}` then `{}`",
                    self.cmd(&format!("rm {name}")),
                    self.cmd(&format!("mv {temp} {name}"))
                ),
            )));
        }
        if let Err(e) = calc.rename(&temp, name) {
            return Err(anyhow::Error::new(e).context(Hinted::new(
                format!("the old {name} is deleted but renaming {temp} to {name} failed"),
                format!(
                    "the new object is in {temp}: `{}`",
                    self.cmd(&format!("mv {temp} {name}"))
                ),
            )));
        }
        Ok(name.to_string())
    }

    fn rm(&mut self, names: &[String], dry_run: bool) -> Result<Outcome> {
        for name in names {
            validate_name(name).with_context(|| format!("rm {name}"))?;
        }
        let mut calc = self.connect()?;
        let listing = calc.list().context("ls")?;
        let mut targets = Vec::new();
        let mut missing = Vec::new();
        for name in names {
            match listing.entries.iter().find(|e| &e.name == name) {
                Some(e) => targets.push(e.clone()),
                None => missing.push(name.as_str()),
            }
        }
        if !missing.is_empty() {
            return Err(Hinted::new(
                format!(
                    "no such variable in the current directory: {}",
                    missing.join(", ")
                ),
                format!("nothing was deleted; `{}` lists the names", self.cmd("ls")),
            )
            .into());
        }
        let mut text = String::new();
        let verb = if dry_run { "Would delete" } else { "Deleted" };
        let mut results = Vec::new();
        for e in &targets {
            if !dry_run {
                calc.remove(&e.name)
                    .with_context(|| format!("rm {}", e.name))?;
            }
            let what = if e.is_directory() {
                "directory, with its contents"
            } else {
                e.kind.as_str()
            };
            let _ = writeln!(
                text,
                "{verb} {} ({what}, {} bytes)",
                e.name,
                util::number_text(e.size)
            );
            results.push(json!({
                "name": e.name,
                "type": e.kind,
                "size": util::number(e.size),
                "directory": e.is_directory(),
                "deleted": !dry_run,
            }));
        }
        let hints = if dry_run {
            let quoted: Vec<String> = names.iter().map(|n| shell_quote(n)).collect();
            vec![Hint::cmd(
                "Delete them",
                self.cmd(&format!("rm {}", quoted.join(" "))),
            )]
        } else {
            vec![Hint::cmd("List the directory", self.cmd("ls"))]
        };
        Ok(Outcome {
            total: Some(results.len() as u64),
            dir: None,
            results: Value::from(results),
            hints,
            text,
        })
    }

    fn mkdir(&mut self, name: &str) -> Result<Outcome> {
        let mut calc = self.connect()?;
        calc.mkdir(name).with_context(|| format!("mkdir {name}"))?;
        Ok(Outcome {
            results: json!({"name": name}),
            total: None,
            dir: None,
            hints: vec![Hint::cmd("List the directory", self.cmd("ls"))],
            text: format!("Created directory {name}"),
        })
    }

    fn mv(&mut self, from: &str, to: &str) -> Result<Outcome> {
        let mut calc = self.connect()?;
        calc.rename(from, to)
            .with_context(|| format!("mv {from} {to}"))?;
        Ok(Outcome {
            results: json!({"from": from, "to": to}),
            total: None,
            dir: None,
            hints: vec![Hint::cmd("List the directory", self.cmd("ls"))],
            text: format!("{from} -> {to}"),
        })
    }

    fn run_rpl(&mut self, command: &str) -> Result<Outcome> {
        let mut calc = self.connect()?;
        let reply = calc
            .run(command)
            .with_context(|| format!("run {command}"))?;
        if let Some(message) = reply.error {
            return Err(anyhow::Error::new(Error::Calculator {
                message,
                stack: reply.levels,
            })
            .context(format!("run {command}")));
        }
        let mut text = String::new();
        if reply.levels.is_empty() {
            text.push_str("Empty Stack");
        }
        for (i, level) in reply.levels.iter().enumerate().rev() {
            let _ = writeln!(text, "{}: {level}", i + 1);
        }
        Ok(Outcome {
            results: json!({"command": command, "stack": reply.levels}),
            total: Some(reply.levels.len() as u64),
            dir: None,
            hints: Vec::new(),
            text,
        })
    }

    fn screenshot(&mut self, output: Option<&Path>, force: bool) -> Result<Option<Outcome>> {
        let to_stdout = output == Some(Path::new("-"));
        let file = output
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(format!("hptx-screen-{}.png", util::timestamp())));
        if !to_stdout {
            refuse_existing_file(&file, force)?;
        }
        let mut calc = self.connect()?;
        let grob = calc.screenshot().context("screenshot")?;
        let png = grob.to_png().context("PNG encoding")?;
        if to_stdout {
            let mut out = std::io::stdout().lock();
            out.write_all(&png).context("writing to stdout")?;
            out.flush().context("writing to stdout")?;
            return Ok(None);
        }
        write_file(&file, &png, force)?;
        Ok(Some(Outcome {
            results: json!({
                "file": file.display().to_string(),
                "width": grob.width,
                "height": grob.height,
                "bytes": png.len(),
            }),
            total: None,
            dir: None,
            hints: Vec::new(),
            text: format!(
                "Screen ({}x{}) -> {}",
                grob.width,
                grob.height,
                file.display()
            ),
        }))
    }

    fn backup(&mut self, output: Option<&Path>, force: bool) -> Result<Outcome> {
        let file = output
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(format!("hptx-backup-{}.hp", util::timestamp())));
        refuse_existing_file(&file, force)?;
        let mut calc = self.connect()?;
        self.status("Archiving HOME and downloading it at 9600 baud (about 1 KB/s)...");
        let data = calc.backup().context("backup")?;
        write_file(&file, &data, force)?;
        let label = file.display().to_string();
        Ok(Outcome {
            results: json!({"file": label, "bytes": data.len()}),
            total: None,
            dir: None,
            hints: vec![Hint::cmd(
                "Check that the backup can be restored",
                self.cmd(&format!("restore {} --dry-run", shell_quote(&label))),
            )],
            text: format!("HOME -> {label} ({} bytes)", data.len()),
        })
    }

    fn restore(&mut self, args: &RestoreArgs) -> Result<Outcome> {
        if args.cleanup {
            return self.restore_cleanup(args.dry_run);
        }
        let Some(file) = &args.file else {
            bail!("restore needs a FILE or --cleanup");
        };
        let label = file.display().to_string();
        let data = std::fs::read(file).with_context(|| format!("cannot read {label}"))?;
        let info = object::inspect(&data).map_err(|e| {
            Hinted::new(
                format!("{label} is not a backup: {e}"),
                "restore takes a file written by `hptx backup`",
            )
        })?;
        if info.object_type != Some(ObjectType::Directory) {
            return Err(Hinted::new(
                format!(
                    "{label} is not a backup: it holds a {}, not a Directory",
                    info.object_type.map_or("unknown object", ObjectType::name)
                ),
                format!(
                    "to upload a single object use `{}`",
                    self.cmd(&format!("put {}", shell_quote(&label)))
                ),
            )
            .into());
        }
        if args.dry_run {
            let mut calc = self.connect()?;
            let path = calc.path().context("PATH")?;
            if calc
                .list()
                .context("ls")?
                .entries
                .iter()
                .any(|e| e.name == "HPTXRS")
            {
                return Err(Hinted::new(
                    format!("HPTXRS exists in {}", util::path_string(&path)),
                    "it is hptx's temporary variable: `hptx get HPTXRS` to keep it, then `hptx rm HPTXRS`",
                )
                .into());
            }
            return Ok(Outcome {
                results: json!({"file": label, "bytes": data.len(), "dry_run": true}),
                total: None,
                dir: None,
                hints: vec![Hint::cmd(
                    "Replace HOME with the backup",
                    self.cmd(&format!("restore {}", shell_quote(&label))),
                )],
                text: format!(
                    "{label} is a backup ({} bytes). restore would replace everything in HOME;\n\
                     the calculator would warm-start and leave server mode.",
                    data.len()
                ),
            });
        }
        if !args.yes {
            confirm(&format!(
                "Replace EVERYTHING in HOME on the calculator with {label}?"
            ))?;
        }
        let mut calc = self.connect()?;
        // The restore runs in HOME's backup; --dir does not matter.
        self.status("Uploading the backup at 9600 baud (about 1 KB/s)...");
        calc.restore(&data).context("restore")?;
        let marker = self.restore_marker();
        let _ = std::fs::write(&marker, b"hptx restore cleanup pending\n");
        Ok(Outcome {
            results: json!({
                "file": label,
                "bytes": data.len(),
                "leftover": RESTORE_LEFTOVER,
                "server_mode": false,
            }),
            total: None,
            dir: None,
            hints: vec![
                Hint::advice("Run SERVER on the calculator again"),
                Hint::cmd(
                    format!("Then delete {RESTORE_LEFTOVER} (the next hptx command does it too)"),
                    self.cmd("restore --cleanup"),
                ),
            ],
            text: format!(
                "Restored HOME from {label}.\n\
                 The calculator warm-starts and has left server mode. {RESTORE_LEFTOVER} (a copy\n\
                 of the backup) stays in port 0 until `hptx restore --cleanup` or the next\n\
                 hptx command on this port from this computer deletes it."
            ),
        })
    }

    fn restore_cleanup(&mut self, dry_run: bool) -> Result<Outcome> {
        let mut calc = self.open()?;
        let marker = self.restore_marker();
        if dry_run {
            return Ok(Outcome {
                results: json!({"leftover": RESTORE_LEFTOVER, "dry_run": true}),
                total: None,
                dir: None,
                hints: vec![Hint::cmd("Delete it", self.cmd("restore --cleanup"))],
                text: format!("Would delete {RESTORE_LEFTOVER} if it exists."),
            });
        }
        let deleted = purge_leftover(&mut calc).context("restore --cleanup")?;
        let _ = std::fs::remove_file(&marker);
        Ok(Outcome {
            results: json!({"leftover": RESTORE_LEFTOVER, "deleted": deleted}),
            total: None,
            dir: None,
            hints: Vec::new(),
            text: if deleted {
                format!("Deleted {RESTORE_LEFTOVER}.")
            } else {
                format!("No {RESTORE_LEFTOVER}: nothing to clean up.")
            },
        })
    }

    fn settings(&mut self, args: &SettingsArgs) -> Result<Outcome> {
        let mut calc = self.connect()?;
        let changes_iopar = args.baud.is_some()
            || args.parity.is_some()
            || args.checksum.is_some()
            || args.translate.is_some();
        let mut changed = Vec::new();
        if changes_iopar {
            let old = calc.iopar().context("IOPAR")?;
            let mut iopar = old;
            if let Some(baud) = &args.baud {
                iopar.baud = baud.parse().context("--baud")?;
            }
            if let Some(parity) = args.parity {
                iopar.parity = parity;
            }
            if let Some(checksum) = args.checksum {
                iopar.checksum = checksum;
            }
            if let Some(translate) = args.translate {
                iopar.translate = translate;
            }
            if iopar != old {
                calc.set_iopar(&iopar).context("storing IOPAR")?;
                changed.push("iopar");
            }
        }
        if let Some(mode) = args.mode {
            let mode = match mode {
                ModeArg::Binary => TransferMode::Binary,
                ModeArg::Ascii => TransferMode::Ascii,
            };
            calc.set_transfer_mode(mode).context("flag -35")?;
            changed.push("transfer_mode");
        }
        let iopar = calc.iopar().context("IOPAR")?;
        let mode = calc.transfer_mode().context("flag -35")?;
        let mut text = format!(
            "iopar     {}\ntransfer  {}",
            iopar_text(&iopar),
            mode_text(mode)
        );
        let mut hints = Vec::new();
        if changed.contains(&"iopar") {
            text.push_str("\nIOPAR takes effect when SERVER starts again.");
            hints.push(Hint::advice(
                "Leave and restart SERVER on the calculator for the new IOPAR",
            ));
        }
        if iopar.baud != 9600 {
            hints.push(Hint::advice(
                "hptx talks at 9600 baud only; set --baud 9600 before restarting SERVER",
            ));
        }
        Ok(Outcome {
            results: json!({
                "iopar": iopar_json(&iopar),
                "transfer_mode": mode_name(mode),
                "changed": changed,
            }),
            total: None,
            dir: None,
            hints,
            text,
        })
    }

    fn finish(&mut self) -> Result<Outcome> {
        let mut calc = self.open()?;
        calc.finish().context("finish")?;
        Ok(Outcome {
            results: json!({"server_mode": false}),
            total: None,
            dir: None,
            hints: vec![Hint::advice(
                "Run SERVER on the calculator to connect again",
            )],
            text: "Server mode ended.".into(),
        })
    }
}

/// Change to `dir` (absolute from HOME).
fn cd(calc: &mut Calculator, dir: &str) -> Result<()> {
    let components = util::parse_dir(dir);
    let mut path: Vec<&str> = vec!["HOME"];
    path.extend(components.iter().map(String::as_str));
    calc.cd(&path)
        .with_context(|| format!("cd {}", util::dir_string(&components)))
}

/// Purge `:0:HPTXRS`; `false` if it was not there. `PURGE` of a missing
/// port object reports no error (48SX and 49G emulators, 2026-10-05), so
/// `VTYPE` (-1 = no such object) tells first.
fn purge_leftover(calc: &mut Calculator) -> Result<bool> {
    let query = format!("{RESTORE_LEFTOVER} VTYPE");
    let reply = calc.run(&query).with_context(|| query.clone())?;
    if let Some(message) = reply.error {
        return Err(Error::Calculator {
            message,
            stack: reply.levels,
        })
        .with_context(|| query.clone());
    }
    let vtype = reply.level(1).and_then(parse_real);
    let dropped = calc.run("DROP").context("DROP")?;
    if let Some(message) = dropped.error {
        return Err(Error::Calculator {
            message,
            stack: dropped.levels,
        })
        .context("DROP");
    }
    match vtype {
        Some(t) if t < 0.0 => Ok(false),
        Some(_) => {
            calc.purge_restore_leftover()
                .with_context(|| format!("{RESTORE_LEFTOVER} PURGE"))?;
            Ok(true)
        }
        None => bail!("{query}: not a number: {:?}", reply.levels),
    }
}

/// Ask on the terminal; fail without one.
fn confirm(question: &str) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        return Err(Hinted::new(
            "restore replaces everything in HOME and needs confirmation",
            "pass --yes (stdin is not a terminal); --dry-run checks the file first",
        )
        .into());
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("reading the answer")?;
    if matches!(answer.trim(), "y" | "Y" | "yes" | "YES" | "Yes") {
        Ok(())
    } else {
        Err(Hinted::new("restore cancelled", "nothing was changed").into())
    }
}

pub(crate) fn refuse_existing_file(file: &Path, force: bool) -> Result<()> {
    if !force && file.exists() {
        return Err(Hinted::new(
            format!("{} exists", file.display()),
            "--force replaces it, or name another file with -o FILE",
        )
        .into());
    }
    Ok(())
}

pub(crate) fn write_file(file: &Path, data: &[u8], force: bool) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut f = options
        .open(file)
        .with_context(|| format!("cannot write {}", file.display()))?;
    f.write_all(data)
        .with_context(|| format!("cannot write {}", file.display()))
}

fn object_type_name(data: &[u8], mode: TransferMode) -> Option<String> {
    if mode != TransferMode::Binary {
        return None;
    }
    object_type_name_binary(data)
}

/// The object type of a binary file, or its prolog when unknown.
pub(crate) fn object_type_name_binary(data: &[u8]) -> Option<String> {
    let info = object::inspect(data).ok()?;
    Some(info.object_type.map_or_else(
        || format!("prolog {:05X}", info.prolog),
        |t| t.name().to_string(),
    ))
}

fn mode_name(mode: TransferMode) -> &'static str {
    match mode {
        TransferMode::Binary => "binary",
        TransferMode::Ascii => "ascii",
    }
}

fn mode_text(mode: TransferMode) -> &'static str {
    match mode {
        TransferMode::Binary => "binary (flag -35 set)",
        TransferMode::Ascii => "ascii (flag -35 clear)",
    }
}

fn iopar_text(iopar: &Iopar) -> String {
    format!(
        "{iopar}  ({} baud, parity {}, checksum type {}, translate {})",
        iopar.baud, iopar.parity, iopar.checksum, iopar.translate
    )
}

fn iopar_json(iopar: &Iopar) -> Value {
    json!({
        "baud": iopar.baud,
        "parity": iopar.parity,
        "receive_pacing": iopar.receive_pacing,
        "transmit_pacing": iopar.transmit_pacing,
        "checksum": iopar.checksum,
        "translate": iopar.translate,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn iopar_shapes() {
        let i = Iopar::default();
        assert_eq!(
            iopar_text(&i),
            "{ 9600 0 0 0 3 1 }  (9600 baud, parity 0, checksum type 3, translate 1)"
        );
        assert_eq!(iopar_json(&i)["checksum"], 3);
    }

    #[test]
    fn hint_commands_carry_an_explicit_port() {
        let mut global = Global {
            port: Some("tcp://localhost:4848".into()),
            dir: None,
            timeout: 20,
            retries: 5,
            format: None,
            json: false,
            jq: None,
        };
        let ctx = |global: Global, on_cli| Ctx {
            global,
            format: Format::Text,
            link: LinkInfo::default(),
            port_on_command_line: on_cli,
        };
        assert_eq!(
            ctx(global.clone(), true).cmd("ls"),
            "hptx --port tcp://localhost:4848 ls"
        );
        assert_eq!(ctx(global.clone(), false).cmd("ls"), "hptx ls");
        global.port = None;
        assert_eq!(ctx(global, true).cmd("ls"), "hptx ls");
    }

    #[test]
    fn listing_text_and_json() {
        let ctx = Ctx {
            global: Global {
                port: None,
                dir: None,
                timeout: 20,
                retries: 5,
                format: None,
                json: false,
                jq: None,
            },
            format: Format::Text,
            link: LinkInfo::default(),
            port_on_command_line: false,
        };
        let listing = Listing {
            path: Some(vec!["HOME".into()]),
            free: Some(1000.0),
            entries: vec![
                Entry {
                    name: "GAMES".into(),
                    size: 120.0,
                    kind: "Directory".into(),
                    checksum: 0x1A2B,
                },
                Entry {
                    name: "X".into(),
                    size: 10.5,
                    kind: "Real Number".into(),
                    checksum: 7,
                },
            ],
        };
        let o = ctx.listing_outcome(&listing, listing.path.as_deref());
        assert_eq!(o.total, Some(2));
        assert_eq!(o.results[1]["size"], 10.5);
        assert_eq!(o.results[0]["directory"], true);
        assert!(o.text.starts_with("HOME, 2 variables, 1000 bytes free\n"));
        assert!(o.text.contains("  GAMES      120  Directory        #1A2Bh"));
        let cmds: Vec<&str> = o.hints.iter().map(|h| h.cmd.as_str()).collect();
        assert_eq!(cmds, ["hptx get X", "hptx ls HOME/GAMES"]);
    }
}
