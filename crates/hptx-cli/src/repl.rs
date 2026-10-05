//! `hptx repl`: one link for a whole session. Each line is RPL sent as a
//! host command (as `hptx run`), or a colon command over the same
//! `Calculator`. On a terminal with line editing and history; from a pipe
//! line by line, without prompt.

use std::ffi::OsString;
use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hptx_core::calc::validate_name;
use hptx_core::{Calculator, Error};
use rustyline::error::ReadlineError;

use crate::commands::{
    Ctx, GetArgs, Protocol, PutArgs, XmodemArgs, cd, pict_default_file, pict_on, put_mode,
};
use crate::error::{Hinted, describe};
use crate::output::{self, Failure, Format, Outcome};
use crate::util;

/// The colon commands, for `:help` and `repl --help`.
macro_rules! repl_commands {
    () => {
        "\
Commands (everything else is RPL for the calculator):
  :ls [PATH]                      list a directory; PATH (from HOME) becomes current
  :cd PATH                        change directory, from HOME: HOME/GAMES or GAMES
  :get NAME [FILE] [--force]      download NAME in binary to FILE [default: NAME]
  :put FILE [NAME] [--overwrite]  upload FILE as NAME [default: the file name]
  :rm NAME...                     delete variables (directories with their contents)
  :pict [FILE] [--force]          graphics screen as PNG [default: hptx-pict-<time>.png]
  :info                           model, version, free memory, path, IOPAR
  :help                           this list
  :quit, :q                       leave (Ctrl-D too)
  ::RPL                           send RPL that starts with a colon: ::a:1 sends :a:1"
    };
}

/// `repl --help` text after the options.
pub const AFTER_HELP: &str = concat!(
    repl_commands!(),
    "

Each line goes to the calculator as one host command, like `hptx run`, and
the stack comes back as the calculator displays it, deepest level first: the
49G truncates long values and shows lists with commas. An empty stack prints
nothing. After an error the calculator's message and the stack it left are
printed (to stderr) and the session goes on. One line must fit in one Kermit
packet (77 encoded bytes). Files that exist are kept unless --force, and
:put replaces a variable only with --overwrite.

Ctrl-C clears the line, Ctrl-D or :quit leaves. History is kept in
  Linux    $XDG_DATA_HOME/hptx/history (default ~/.local/share/hptx/history)
  macOS    ~/Library/Application Support/hptx/history
  Windows  %APPDATA%\\hptx\\history.txt

From a pipe hptx reads the lines without prompt or editing and exits at the
end of the input: 0, or 1 if the link failed. --json and --jq do not apply.

Examples:
  hptx repl
  hptx --port tcp://localhost:4848 repl
  printf '6 7 *\\n:ls\\n' | hptx repl"
);

/// What a line of input is.
#[derive(Debug, PartialEq, Eq)]
pub enum Line<'a> {
    /// Blank: nothing to do.
    Empty,
    /// RPL for the calculator.
    Rpl(&'a str),
    /// A colon command, the text after the colon.
    Meta(&'a str),
}

/// Classify `line`: blank, `:command`, or RPL (`::x` is the RPL `:x`).
pub fn classify(line: &str) -> Line<'_> {
    let line = line.trim();
    if line.is_empty() {
        Line::Empty
    } else if line.starts_with("::") {
        Line::Rpl(&line[1..])
    } else if let Some(rest) = line.strip_prefix(':') {
        Line::Meta(rest)
    } else {
        Line::Rpl(line)
    }
}

/// A parsed colon command.
#[derive(Debug, PartialEq, Eq)]
pub enum Meta {
    Ls(Option<String>),
    Cd(String),
    Get {
        name: String,
        file: Option<PathBuf>,
        force: bool,
    },
    Put {
        file: PathBuf,
        name: Option<String>,
        overwrite: bool,
    },
    Rm(Vec<String>),
    Pict {
        file: Option<PathBuf>,
        force: bool,
    },
    Info,
    Help,
    Quit,
}

const META_LIST: &str = "commands: :ls [PATH], :cd PATH, :get NAME [FILE], :put FILE [NAME] [--overwrite], :rm NAME..., \
     :pict [FILE], :info, :help, :quit; ::RPL sends RPL that starts with a colon";

/// Parse the text after the colon.
pub fn parse_meta(text: &str) -> std::result::Result<Meta, Hinted> {
    let words = split_words(text)?;
    let Some((command, args)) = words.split_first() else {
        return Err(Hinted::new("a colon command needs a name", META_LIST));
    };
    // Each command's flags; any other `--word` is refused.
    let allowed: &[&str] = match command.as_str() {
        "get" | "pict" => &["--force"],
        "put" => &["--overwrite"],
        _ => &[],
    };
    let (flags, plain): (Vec<&String>, Vec<&String>) =
        args.iter().partition(|a| a.starts_with("--"));
    if let Some(flag) = flags.iter().find(|f| !allowed.contains(&f.as_str())) {
        return Err(Hinted::new(
            format!(":{command}: unknown option {flag}"),
            usage(command),
        ));
    }
    let force = flags.iter().any(|f| *f == "--force");
    let overwrite = flags.iter().any(|f| *f == "--overwrite");
    let arity = |min: usize, max: usize| -> std::result::Result<(), Hinted> {
        if plain.len() < min || plain.len() > max {
            Err(Hinted::new(
                format!(":{command}: wrong number of arguments"),
                usage(command),
            ))
        } else {
            Ok(())
        }
    };
    let file = |s: &String| -> std::result::Result<PathBuf, Hinted> {
        if s == "-" {
            Err(Hinted::new(
                format!(":{command}: - (stdin/stdout) does not work in the REPL"),
                "give a file name",
            ))
        } else {
            Ok(PathBuf::from(s))
        }
    };
    let meta = match command.as_str() {
        "ls" => {
            arity(0, 1)?;
            Meta::Ls(plain.first().map(|s| (*s).clone()))
        }
        "cd" => {
            arity(1, 1)?;
            Meta::Cd(plain[0].clone())
        }
        "get" => {
            arity(1, 2)?;
            Meta::Get {
                name: plain[0].clone(),
                file: plain.get(1).map(|s| file(s)).transpose()?,
                force,
            }
        }
        "put" => {
            arity(1, 2)?;
            Meta::Put {
                file: file(plain[0])?,
                name: plain.get(1).map(|s| (*s).clone()),
                overwrite,
            }
        }
        "rm" => {
            arity(1, usize::MAX)?;
            Meta::Rm(plain.iter().map(|s| (*s).clone()).collect())
        }
        "pict" => {
            arity(0, 1)?;
            Meta::Pict {
                file: plain.first().map(|s| file(s)).transpose()?,
                force,
            }
        }
        "info" => {
            arity(0, 0)?;
            Meta::Info
        }
        "help" | "h" | "?" => Meta::Help,
        "quit" | "q" | "exit" => Meta::Quit,
        _ => {
            return Err(Hinted::new(
                format!("unknown command :{command}"),
                META_LIST,
            ));
        }
    };
    Ok(meta)
}

fn usage(command: &str) -> String {
    let form = match command {
        "ls" => ":ls [PATH]",
        "cd" => ":cd PATH",
        "get" => ":get NAME [FILE] [--force]",
        "put" => ":put FILE [NAME] [--overwrite]",
        "rm" => ":rm NAME...",
        "pict" => ":pict [FILE] [--force]",
        "info" => ":info",
        _ => return META_LIST.to_string(),
    };
    format!("usage: {form}")
}

/// Split on whitespace; '…' and "…" quote a word with spaces.
fn split_words(text: &str) -> std::result::Result<Vec<String>, Hinted> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            None => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err(Hinted::new(
            "unbalanced quote",
            "close the quote; quotes are only needed for file names with spaces",
        ));
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// How input is read.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum InputMode {
    /// A person at a terminal: prompt, line editing, history.
    Editor,
    /// A pipe or file: plain lines, no prompt.
    Plain,
}

/// The editor only when stdin is a terminal.
pub fn input_mode(stdin_is_terminal: bool) -> InputMode {
    if stdin_is_terminal {
        InputMode::Editor
    } else {
        InputMode::Plain
    }
}

/// The stack as `N: value` lines, deepest first; `levels[0]` is level 1.
/// Values are printed as the calculator displayed them. Empty for an empty
/// stack.
pub fn stack_text(levels: &[String]) -> String {
    levels
        .iter()
        .enumerate()
        .rev()
        .map(|(i, level)| format!("{}: {level}", i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Hint after a calculator error inside the REPL.
const CALC_ERROR_HINT: &str = "the calculator leaves the arguments of a failed command on its \
                               stack: DROP removes one level, CLEAR all";

/// A calculator error as `hptx run` words it (message and stack), with a
/// hint for the REPL.
pub fn calculator_failure(message: String, stack: Vec<String>) -> Failure {
    let err = anyhow::Error::new(Error::Calculator { message, stack });
    let mut failure = describe(&err, &crate::error::LinkInfo::default());
    failure.hint = Some(CALC_ERROR_HINT.into());
    failure
}

/// True if `err` means the link is gone: the session cannot go on.
pub fn is_link_failure(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<Error>(),
            Some(Error::Io(_) | Error::Serial(_) | Error::Kermit(_))
        )
    })
}

/// Which operating system's data directory to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOs,
    Windows,
}

impl Platform {
    fn current() -> Platform {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        }
    }
}

/// The history file under the user's data directory; `None` without HOME
/// (or APPDATA on Windows).
pub fn history_path(platform: Platform, var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    match platform {
        Platform::Windows => Some(set("APPDATA")?.join("hptx").join("history.txt")),
        Platform::MacOs => Some(
            set("HOME")?
                .join("Library")
                .join("Application Support")
                .join("hptx")
                .join("history"),
        ),
        Platform::Linux => {
            let data = set("XDG_DATA_HOME")
                .filter(|p| p.is_absolute())
                .or_else(|| set("HOME").map(|h| h.join(".local").join("share")))?;
            Some(data.join("hptx").join("history"))
        }
    }
}

/// After a line: go on or leave.
enum Flow {
    Continue,
    Quit,
}

impl Ctx {
    /// `hptx repl`. Returns an error only when the session cannot start or
    /// the link fails; everything else is printed and the session goes on.
    pub(crate) fn repl(&mut self) -> Result<Option<Outcome>> {
        if self.global.json || self.global.format == Some(Format::Json) || self.global.jq.is_some()
        {
            return Err(Hinted::new(
                "repl prints text: --json, --format json and --jq do not apply",
                "for JSON run single commands, e.g. `hptx run '6 7 *' --json`; \
                 pipe lines into `hptx repl` for the text",
            )
            .into());
        }
        self.format = Format::Text;
        let mut calc = self.connect()?;
        // --dir applied once at the start; :cd changes it from then on.
        self.global.dir = None;
        match input_mode(std::io::stdin().is_terminal()) {
            InputMode::Editor => self.repl_editor(&mut calc)?,
            InputMode::Plain => self.repl_plain(&mut calc)?,
        }
        Ok(None)
    }

    fn repl_editor(&mut self, calc: &mut Calculator) -> Result<()> {
        let config = rustyline::Config::builder()
            .max_history_size(1000)
            .context("history size")?
            .build();
        let mut editor =
            rustyline::DefaultEditor::with_config(config).context("starting the line editor")?;
        let mut history = History::new(history_path(Platform::current(), |name| {
            std::env::var_os(name)
        }));
        if let Some(path) = &history.path {
            // No history yet is fine.
            let _ = editor.load_history(path);
        }
        // rustyline leaves raw mode before readline returns, also on Ctrl-C
        // and errors, so the terminal is cooked while a command runs.
        loop {
            match editor.readline("> ") {
                Ok(line) => {
                    if !line.trim().is_empty() {
                        let _ = editor.add_history_entry(line.as_str());
                        // Appended before the line runs: Ctrl-C during a
                        // command kills hptx and must not lose the session.
                        if let Some(path) = history.path.clone() {
                            let result = editor.append_history(&path);
                            if let Some(note) = history.note(result) {
                                eprintln!("{note}");
                            }
                        }
                    }
                    match self.repl_line(calc, &line)? {
                        Flow::Continue => {}
                        Flow::Quit => return Ok(()),
                    }
                }
                Err(ReadlineError::Interrupted) => {}
                Err(ReadlineError::Eof) => return Ok(()),
                Err(e) => return Err(anyhow::Error::new(e).context("reading the input")),
            }
        }
    }

    fn repl_plain(&mut self, calc: &mut Calculator) -> Result<()> {
        let mut input = std::io::stdin().lock();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            if input.read_until(b'\n', &mut buf).context("reading stdin")? == 0 {
                return Ok(());
            }
            let line = String::from_utf8_lossy(&buf);
            if let Flow::Quit = self.repl_line(calc, &line)? {
                return Ok(());
            }
        }
    }

    /// Run one line. `Err` only for a link failure.
    fn repl_line(&mut self, calc: &mut Calculator, line: &str) -> Result<Flow> {
        let result = match classify(line) {
            Line::Empty => return Ok(Flow::Continue),
            Line::Rpl(rpl) => self.repl_rpl(calc, rpl),
            Line::Meta(text) => match parse_meta(text) {
                Ok(Meta::Quit) => return Ok(Flow::Quit),
                Ok(meta) => self.repl_meta(calc, meta),
                Err(e) => Err(e.into()),
            },
        };
        match result {
            Ok(()) => Ok(Flow::Continue),
            Err(e) if is_link_failure(&e) => Err(e),
            Err(e) => {
                eprintln!("{}", repl_failure(&e, &self.link).render(Format::Text));
                Ok(Flow::Continue)
            }
        }
    }

    fn repl_rpl(&mut self, calc: &mut Calculator, rpl: &str) -> Result<()> {
        let reply = calc.run(rpl)?;
        if let Some(message) = reply.error {
            eprintln!(
                "{}",
                calculator_failure(message, reply.levels).render(Format::Text)
            );
            return Ok(());
        }
        print_text(&stack_text(&reply.levels))
    }

    fn repl_meta(&mut self, calc: &mut Calculator, meta: Meta) -> Result<()> {
        let outcome = match meta {
            Meta::Ls(path) => self.ls_on(calc, path.as_deref())?,
            Meta::Cd(path) => {
                cd(calc, &path)?;
                return print_text(&util::dir_string(&util::parse_dir(&path)));
            }
            Meta::Get { name, file, force } => {
                validate_name(&name).with_context(|| format!("get {name}"))?;
                let file = file.unwrap_or_else(|| PathBuf::from(&name));
                refuse_existing(&file, force, &format!(":get {name} FILE"))?;
                let args = GetArgs {
                    name,
                    output: Some(file.clone()),
                    ascii: false,
                    force,
                    xmodem: kermit(),
                    dry_run: false,
                };
                match self.get_on(calc, &args, &file, false)? {
                    Some(outcome) => outcome,
                    None => return Ok(()),
                }
            }
            Meta::Put {
                file,
                name,
                overwrite,
            } => {
                let args = PutArgs {
                    file,
                    name,
                    ascii: false,
                    binary: false,
                    overwrite,
                    dry_run: false,
                    xmodem: kermit(),
                };
                let (name, data) = self.put_input(&args)?;
                let label = args.file.display().to_string();
                let mode = put_mode(&args, &data);
                self.put_on(calc, &args, &name, &data, &label, mode)?
            }
            Meta::Rm(names) => {
                for name in &names {
                    validate_name(name).with_context(|| format!("rm {name}"))?;
                }
                self.rm_on(calc, &names, false)?
            }
            Meta::Pict { file, force } => {
                let file = file.unwrap_or_else(pict_default_file);
                refuse_existing(&file, force, ":pict FILE")?;
                match pict_on(calc, &file, false, force)? {
                    Some(outcome) => outcome,
                    None => return Ok(()),
                }
            }
            Meta::Info => self.info_on(calc)?,
            Meta::Help => return print_text(repl_commands!()),
            Meta::Quit => return Ok(()),
        };
        // Hints name shell commands; the REPL shows the result only.
        print_text(&outcome.text)
    }
}

fn kermit() -> XmodemArgs {
    XmodemArgs {
        protocol: Protocol::Kermit,
        start_timeout: None,
    }
}

/// Refuse an existing file unless `force`, with a REPL hint.
fn refuse_existing(file: &Path, force: bool, other: &str) -> Result<()> {
    if !force && file.exists() {
        return Err(Hinted::new(
            format!("{} exists", file.display()),
            format!("add --force to replace it, or name another file: {other}"),
        )
        .into());
    }
    Ok(())
}

/// Print `text` and a newline, nothing for empty text.
fn print_text(text: &str) -> Result<()> {
    let text = text.trim_end_matches('\n');
    if text.is_empty() {
        return Ok(());
    }
    output::print_stdout(text).context("writing to stdout")
}

/// The history file and whether its failure was reported.
struct History {
    path: Option<PathBuf>,
    noted: bool,
}

impl History {
    /// Creates the file's directory once; a failure shows at the first write.
    fn new(path: Option<PathBuf>) -> History {
        if let Some(dir) = path.as_deref().and_then(Path::parent) {
            let _ = std::fs::create_dir_all(dir);
        }
        History { path, noted: false }
    }

    /// The note for a failed write, only the first time.
    fn note<E: std::fmt::Display>(&mut self, result: std::result::Result<(), E>) -> Option<String> {
        let e = result.err()?;
        if self.noted {
            return None;
        }
        self.noted = true;
        let path = self
            .path
            .as_deref()
            .map_or(String::new(), |p| p.display().to_string());
        Some(format!("note: cannot save the history to {path}: {e}"))
    }
}

/// Hint after a too long line inside the REPL.
const TOO_LONG_HINT: &str = "a line must fit in one Kermit packet (77 encoded bytes; « » → \
                             count as 3 or 4): split it over several lines (the stack stays \
                             between lines), or :put a program and run it by name";

/// `err` as the REPL shows it: hints name REPL commands, not `hptx ...`.
pub fn repl_failure(err: &anyhow::Error, link: &crate::error::LinkInfo) -> Failure {
    let mut failure = describe(err, link);
    let core = err.chain().find_map(|c| c.downcast_ref::<Error>());
    failure.hint = match core {
        Some(Error::Calculator { .. }) if failure.hint.is_some() => Some(CALC_ERROR_HINT.into()),
        Some(Error::CommandTooLong { .. }) => Some(TOO_LONG_HINT.into()),
        _ => failure.hint.map(|h| repl_hint(&h)),
    };
    failure
}

/// Rewrite a CLI hint for the REPL: each `` `hptx ...` `` becomes the REPL
/// line that does the same (`:ls`, `:put FILE NAME --overwrite`, plain RPL
/// for `hptx run`); a command the REPL lacks keeps its form, marked "after
/// :quit". CLI-only phrases (`--as NAME`, `-o FILE`, `--dry-run`) follow.
pub fn repl_hint(hint: &str) -> String {
    let mut out = String::new();
    let mut rest = hint;
    while let Some(start) = rest.find('`') {
        let Some(len) = rest[start + 1..].find('`') else {
            break;
        };
        let inner = &rest[start + 1..start + 1 + len];
        out.push_str(&rest[..start]);
        match inner.strip_prefix("hptx ") {
            Some(cmd) => match repl_command(cmd) {
                Some(line) => {
                    out.push('`');
                    out.push_str(&line);
                    out.push('`');
                }
                None => {
                    out.push('`');
                    out.push_str(inner);
                    out.push_str("` (after :quit)");
                }
            },
            None => {
                out.push('`');
                out.push_str(inner);
                out.push('`');
            }
        }
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    out.replace(" (`--dry-run` shows what goes)", "")
        .replace("with --as NAME", "with :put FILE NAME")
        .replace("with -o FILE", "as the FILE argument")
}

/// The REPL line for the CLI command `cmd` (after `hptx `), if any.
fn repl_command(cmd: &str) -> Option<String> {
    let words = split_words(cmd).ok()?;
    let mut words = words.as_slice();
    // Global options a hint may carry; the REPL already has its link.
    while let [opt, _, tail @ ..] = words {
        if matches!(opt.as_str(), "--port" | "-p" | "--dir") {
            words = tail;
        } else {
            break;
        }
    }
    let (command, args) = words.split_first()?;
    // Option values and positional words.
    let mut output = None;
    let mut name = None;
    let mut flags = Vec::new();
    let mut plain = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--output" => output = it.next().cloned(),
            "--as" => name = it.next().cloned(),
            f if f.starts_with('-')
                && f.len() > 1
                && !f[1..].starts_with(|c: char| c.is_ascii_digit()) =>
            {
                flags.push(f)
            }
            _ => plain.push(a.clone()),
        }
    }
    let mut line: Vec<String> = match command.as_str() {
        "run" if !args.is_empty() => return Some(args.join(" ")),
        "ls" | "rm" | "info" => {
            let mut l = vec![format!(":{command}")];
            l.extend(plain.iter().map(|w| quote(w)));
            l
        }
        "get" => {
            let mut l = vec![":get".to_string()];
            l.extend(plain.iter().map(|w| quote(w)));
            l.extend(output.iter().map(|w| quote(w)));
            l
        }
        "put" => {
            let mut l = vec![":put".to_string()];
            l.extend(plain.iter().map(|w| quote(w)));
            l.extend(name.iter().map(|w| quote(w)));
            l
        }
        "pict" => {
            let mut l = vec![":pict".to_string()];
            l.extend(output.iter().map(|w| quote(w)));
            l
        }
        _ => return None,
    };
    let keep: &[&str] = match command.as_str() {
        "get" | "pict" => &["--force"],
        "put" => &["--overwrite"],
        _ => &[],
    };
    line.extend(
        flags
            .into_iter()
            .filter(|f| keep.contains(f))
            .map(str::to_string),
    );
    Some(line.join(" "))
}

/// A word as the REPL's colon commands read it back.
fn quote(word: &str) -> String {
    if word.is_empty() || word.contains(char::is_whitespace) || word.contains('"') {
        format!("'{word}'")
    } else if word.contains('\'') {
        format!("\"{word}\"")
    } else {
        word.to_string()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn classify_lines() {
        assert_eq!(classify(""), Line::Empty);
        assert_eq!(classify("   \r\n"), Line::Empty);
        assert_eq!(classify("6 7 *\n"), Line::Rpl("6 7 *"));
        assert_eq!(classify("  42 'X' STO  "), Line::Rpl("42 'X' STO"));
        assert_eq!(classify(":ls"), Line::Meta("ls"));
        assert_eq!(classify(" :get X x.hp\r\n"), Line::Meta("get X x.hp"));
        assert_eq!(classify(":"), Line::Meta(""));
        // `::` escapes RPL that starts with a colon (a tagged object).
        assert_eq!(classify("::a:1"), Line::Rpl(":a:1"));
        assert_eq!(classify("::"), Line::Rpl(":"));
        assert_eq!(classify(":::x"), Line::Rpl("::x"));
        // A colon later in the line is RPL.
        assert_eq!(classify("1 :a: 2"), Line::Rpl("1 :a: 2"));
    }

    #[test]
    fn parse_meta_commands() {
        assert_eq!(parse_meta("ls").unwrap(), Meta::Ls(None));
        assert_eq!(
            parse_meta("ls HOME/GAMES").unwrap(),
            Meta::Ls(Some("HOME/GAMES".into()))
        );
        assert_eq!(parse_meta("cd GAMES").unwrap(), Meta::Cd("GAMES".into()));
        assert_eq!(
            parse_meta("get PRG").unwrap(),
            Meta::Get {
                name: "PRG".into(),
                file: None,
                force: false
            }
        );
        assert_eq!(
            parse_meta("get PRG prg.hp --force").unwrap(),
            Meta::Get {
                name: "PRG".into(),
                file: Some("prg.hp".into()),
                force: true
            }
        );
        assert_eq!(
            parse_meta("put 'my prg.hp' PRG").unwrap(),
            Meta::Put {
                file: "my prg.hp".into(),
                name: Some("PRG".into()),
                overwrite: false
            }
        );
        assert_eq!(
            parse_meta("put prg.hp").unwrap(),
            Meta::Put {
                file: "prg.hp".into(),
                name: None,
                overwrite: false
            }
        );
        assert_eq!(
            parse_meta("put prg.hp PRG --overwrite").unwrap(),
            Meta::Put {
                file: "prg.hp".into(),
                name: Some("PRG".into()),
                overwrite: true
            }
        );
        assert_eq!(
            parse_meta("rm A B").unwrap(),
            Meta::Rm(vec!["A".into(), "B".into()])
        );
        assert_eq!(
            parse_meta("pict").unwrap(),
            Meta::Pict {
                file: None,
                force: false
            }
        );
        assert_eq!(
            parse_meta("pict --force s.png").unwrap(),
            Meta::Pict {
                file: Some("s.png".into()),
                force: true
            }
        );
        assert_eq!(parse_meta("info").unwrap(), Meta::Info);
        assert_eq!(parse_meta("help").unwrap(), Meta::Help);
        assert_eq!(parse_meta("quit").unwrap(), Meta::Quit);
        assert_eq!(parse_meta("q").unwrap(), Meta::Quit);
    }

    #[test]
    fn parse_meta_errors() {
        let unknown = parse_meta("lss").unwrap_err();
        assert_eq!(unknown.message, "unknown command :lss");
        assert!(unknown.hint.contains(":ls [PATH]"));
        assert!(unknown.hint.contains("::RPL"));
        assert!(parse_meta("").unwrap_err().hint.contains(":quit"));
        let e = parse_meta("cd").unwrap_err();
        assert_eq!(e.hint, "usage: :cd PATH");
        assert!(parse_meta("cd A B").is_err());
        assert!(parse_meta("rm").is_err());
        assert!(parse_meta("get").is_err());
        assert!(parse_meta("get A b c").is_err());
        assert!(parse_meta("info x").is_err());
        assert!(parse_meta("rm X --force").is_err());
        let e = parse_meta("put a.hp --force").unwrap_err();
        assert_eq!(e.message, ":put: unknown option --force");
        assert_eq!(e.hint, "usage: :put FILE [NAME] [--overwrite]");
        assert!(parse_meta("get X --overwrite").is_err());
        assert!(parse_meta("pict --overwrite").is_err());
        assert!(parse_meta("ls --json").is_err());
        let e = parse_meta("get X -").unwrap_err();
        assert!(e.message.contains("does not work in the REPL"));
        assert!(parse_meta("put -").is_err());
        assert!(
            parse_meta("put 'a.hp")
                .unwrap_err()
                .message
                .contains("quote")
        );
    }

    #[test]
    fn stack_printer() {
        assert_eq!(stack_text(&[]), "");
        assert_eq!(stack_text(&["42".into()]), "1: 42");
        // Level 1 first in, deepest first out; values verbatim.
        let levels = vec!["{9600.,0.,0.}".into(), "'X'".into(), "42.".into()];
        assert_eq!(stack_text(&levels), "3: 42.\n2: 'X'\n1: {9600.,0.,0.}");
        let multi = vec!["\u{ab} 1 +\n\u{bb}".into()];
        assert_eq!(stack_text(&multi), "1: \u{ab} 1 +\n\u{bb}");
    }

    #[test]
    fn calculator_error_printer() {
        let f = calculator_failure("Infinite Result".into(), vec!["0".into(), "1".into()]);
        assert_eq!(
            f.render(Format::Text),
            "error: calculator error: Infinite Result\n  stack:\n    2: 1\n    1: 0\n  \
             hint: the calculator leaves the arguments of a failed command on its stack: \
             DROP removes one level, CLEAR all"
        );
        let empty = calculator_failure("Too Few Arguments".into(), Vec::new());
        assert!(!empty.render(Format::Text).contains("\n  stack:"));
    }

    #[test]
    fn mode_choice() {
        assert_eq!(input_mode(true), InputMode::Editor);
        assert_eq!(input_mode(false), InputMode::Plain);
    }

    #[test]
    fn link_failures_end_the_session() {
        let timeout = anyhow::Error::new(Error::Kermit(kermit_proto::Error::Timeout)).context("ls");
        assert!(is_link_failure(&timeout));
        let io = anyhow::Error::new(Error::Io(std::io::ErrorKind::UnexpectedEof.into()));
        assert!(is_link_failure(&io));
        let calc = anyhow::Error::new(Error::Calculator {
            message: "Undefined Name".into(),
            stack: vec![],
        });
        assert!(!is_link_failure(&calc));
        let too_long = anyhow::Error::new(Error::CommandTooLong {
            command: String::new(),
            len: 80,
            max: 77,
        });
        assert!(!is_link_failure(&too_long));
        // A local file error is not the link.
        let file = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::NotFound))
            .context("cannot read x.hp");
        assert!(!is_link_failure(&file));
        assert!(!is_link_failure(&Hinted::new("m", "h").into()));
    }

    #[test]
    fn history_locations() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        assert_eq!(
            history_path(Platform::Linux, env(&[("HOME", "/home/u")])),
            Some(PathBuf::from("/home/u/.local/share/hptx/history"))
        );
        assert_eq!(
            history_path(
                Platform::Linux,
                env(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "/data")])
            ),
            Some(PathBuf::from("/data/hptx/history"))
        );
        // A relative XDG_DATA_HOME is ignored, as the spec says.
        assert_eq!(
            history_path(
                Platform::Linux,
                env(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "rel")])
            ),
            Some(PathBuf::from("/home/u/.local/share/hptx/history"))
        );
        assert_eq!(
            history_path(Platform::MacOs, env(&[("HOME", "/Users/u")])),
            Some(PathBuf::from(
                "/Users/u/Library/Application Support/hptx/history"
            ))
        );
        assert_eq!(
            history_path(
                Platform::Windows,
                env(&[("APPDATA", r"C:\Users\u\AppData\Roaming")])
            ),
            Some(
                PathBuf::from(r"C:\Users\u\AppData\Roaming")
                    .join("hptx")
                    .join("history.txt")
            )
        );
        assert_eq!(history_path(Platform::Linux, env(&[])), None);
        assert_eq!(history_path(Platform::MacOs, env(&[("HOME", "")])), None);
        assert_eq!(
            history_path(Platform::Windows, env(&[("HOME", "/h")])),
            None
        );
    }

    #[test]
    fn hints_name_repl_commands() {
        // put over an existing name (put_on), with and without --port.
        assert_eq!(
            repl_hint(
                "`hptx put prg.hp --as PRG --overwrite` replaces it, or choose another \
                 name with --as NAME"
            ),
            "`:put prg.hp PRG --overwrite` replaces it, or choose another name with \
             :put FILE NAME"
        );
        assert_eq!(
            repl_hint("`hptx --port tcp://localhost:4848 put 'my prg.hp' --as PRG --overwrite`"),
            "`:put 'my prg.hp' PRG --overwrite`"
        );
        // rm of a missing name (rm_on), Undefined Name (describe).
        assert_eq!(
            repl_hint("nothing was deleted; `hptx ls` lists the names"),
            "nothing was deleted; `:ls` lists the names"
        );
        // An invalid name from :put (put_input).
        assert_eq!(
            repl_hint(
                "choose one with --as NAME, e.g. `hptx put 'a b.hp' --as PRG`; names start \
                 with a letter"
            ),
            "choose one with :put FILE NAME, e.g. `:put 'a b.hp' PRG`; names start with a letter"
        );
        // Temporary variables (put_on, reply_hint): get, rm, mv.
        assert_eq!(
            repl_hint(
                "`hptx get HPTXPT` to keep it, then `hptx rm HPTXPT` (or `hptx mv HPTXPT OTHER`)"
            ),
            "`:get HPTXPT` to keep it, then `:rm HPTXPT` (or `hptx mv HPTXPT OTHER` (after \
             :quit))"
        );
        assert_eq!(
            repl_hint("choose another name, or `hptx rm X` first (`--dry-run` shows what goes)"),
            "choose another name, or `:rm X` first"
        );
        // run becomes plain RPL (pict_on).
        assert_eq!(
            repl_hint("draw over the link, e.g. `hptx run 'ERASE { # 10d # 10d } PIXON'`"),
            "draw over the link, e.g. `ERASE { # 10d # 10d } PIXON`"
        );
        assert_eq!(
            repl_hint("`hptx run DROP` removes one level"),
            "`DROP` removes one level"
        );
        // get/pict keep their file and --force; CLI-only flags go.
        assert_eq!(
            repl_hint("`hptx get PRG -o prg.hp --ascii --force`"),
            "`:get PRG prg.hp --force`"
        );
        assert_eq!(repl_hint("`hptx pict -o p.png`"), "`:pict p.png`");
        assert_eq!(repl_hint("`hptx rm A B --dry-run`"), "`:rm A B`");
        assert_eq!(
            repl_hint("--force replaces it, or name another file with -o FILE"),
            "--force replaces it, or name another file as the FILE argument"
        );
        // No REPL command: the CLI form stays, marked.
        assert_eq!(
            repl_hint("`hptx ports` lists the serial ports"),
            "`hptx ports` (after :quit) lists the serial ports"
        );
        // Text without commands is unchanged; an unpaired backtick too.
        assert_eq!(repl_hint("check the cable"), "check the cable");
        assert_eq!(repl_hint("a ` b"), "a ` b");
    }

    #[test]
    fn repl_failures_get_repl_hints() {
        let link = crate::error::LinkInfo::default();
        let missing = anyhow::Error::new(Hinted::new(
            "no such variable in the current directory: X",
            "nothing was deleted; `hptx ls` lists the names",
        ));
        assert_eq!(
            repl_failure(&missing, &link).hint.as_deref(),
            Some("nothing was deleted; `:ls` lists the names")
        );
        let undefined = anyhow::Error::new(Error::Remote("Undefined Name".into())).context("get X");
        assert_eq!(
            repl_failure(&undefined, &link).hint.as_deref(),
            Some("no such variable in the current directory; `:ls` lists them")
        );
        let calc = anyhow::Error::new(Error::Calculator {
            message: "Undefined Name".into(),
            stack: vec!["'X'".into()],
        })
        .context("rm X");
        assert_eq!(
            repl_failure(&calc, &link).hint.as_deref(),
            Some(CALC_ERROR_HINT)
        );
        let long = anyhow::Error::new(Error::CommandTooLong {
            command: String::new(),
            len: 80,
            max: 77,
        });
        let hint = repl_failure(&long, &link).hint.unwrap();
        assert!(
            hint.contains("several lines") && !hint.contains("hptx"),
            "{hint}"
        );
        let name = anyhow::Error::new(Error::Name("1X".into()));
        let hint = repl_failure(&name, &link).hint.unwrap();
        assert!(hint.ends_with("choose one with :put FILE NAME"), "{hint}");
    }

    #[test]
    fn history_failure_noted_once() {
        let mut h = History {
            path: Some(PathBuf::from("/nonexistent/h")),
            noted: false,
        };
        assert_eq!(h.note::<&str>(Ok(())), None);
        assert_eq!(
            h.note(Err("denied")).as_deref(),
            Some("note: cannot save the history to /nonexistent/h: denied")
        );
        assert_eq!(h.note(Err("denied")), None);
        assert_eq!(h.note::<&str>(Ok(())), None);
    }

    #[test]
    fn history_appends_each_line() {
        let dir = std::env::temp_dir().join(format!("hptx-history-test-{}", std::process::id()));
        let path = dir.join("sub").join("history");
        let _ = std::fs::remove_dir_all(&dir);
        let history = History::new(Some(path.clone()));
        assert!(path.parent().unwrap().is_dir());
        let mut editor = rustyline::DefaultEditor::new().unwrap();
        for line in ["42 'X' STO", "X"] {
            editor.add_history_entry(line).unwrap();
            editor
                .append_history(history.path.as_ref().unwrap())
                .unwrap();
        }
        // Each line is on disk before the next one runs; no final save.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("42 'X' STO\nX\n"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
