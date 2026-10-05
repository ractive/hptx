//! Output conventions borrowed from hyalo: text on a terminal, JSON when
//! piped, `--format` overrides; the JSON envelope `{results, total, hints}`;
//! `--jq` reshapes the envelope; hints suggest the next commands. Errors go
//! to stderr, as `{error, hint}` JSON in JSON mode.

use std::fmt::Write as _;
use std::io::{IsTerminal, Write};

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, Vars, data};
use jaq_json::Val;
use serde::Serialize;

/// Output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// Human-readable text.
    Text,
    /// The JSON envelope `{results, total, hints}`.
    Json,
}

impl Format {
    /// `explicit` if given, else text on a terminal and JSON when piped.
    pub fn resolve(explicit: Option<Format>, json: bool) -> Format {
        match (explicit, json) {
            (Some(f), _) => f,
            (None, true) => Format::Json,
            (None, false) if std::io::stdout().is_terminal() => Format::Text,
            (None, false) => Format::Json,
        }
    }
}

/// A suggested next command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hint {
    /// What the command does.
    pub description: String,
    /// The command line; empty for advice without a command.
    pub cmd: String,
}

impl Hint {
    /// A hint suggesting `cmd`.
    pub fn cmd(description: impl Into<String>, cmd: impl Into<String>) -> Self {
        Hint {
            description: description.into(),
            cmd: cmd.into(),
        }
    }

    /// Advice without a command.
    pub fn advice(description: impl Into<String>) -> Self {
        Hint {
            description: description.into(),
            cmd: String::new(),
        }
    }
}

/// What a command produced.
#[derive(Debug, Default)]
pub struct Outcome {
    /// `results` in the JSON envelope.
    pub results: serde_json::Value,
    /// `total` for list commands.
    pub total: Option<u64>,
    /// The calculator directory the results are from, when known.
    pub dir: Option<String>,
    /// Suggested next commands.
    pub hints: Vec<Hint>,
    /// The text rendering of `results` (no trailing newline needed).
    pub text: String,
}

#[derive(Serialize)]
struct Envelope<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    dir: Option<&'a str>,
    results: &'a serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<u64>,
    hints: &'a [Hint],
}

/// Render `outcome` for `format`, or through the `jq` filter if given.
pub fn render(outcome: &Outcome, format: Format, jq: Option<&str>) -> Result<String, String> {
    let envelope = Envelope {
        dir: outcome.dir.as_deref(),
        results: &outcome.results,
        total: outcome.total,
        hints: &outcome.hints,
    };
    if let Some(filter) = jq {
        let value = serde_json::to_value(&envelope).map_err(|e| e.to_string())?;
        return run_jq(filter, &value);
    }
    match format {
        Format::Json => serde_json::to_string_pretty(&envelope).map_err(|e| e.to_string()),
        Format::Text => {
            let mut text = outcome.text.trim_end_matches('\n').to_string();
            append_hints(&mut text, &outcome.hints);
            Ok(escape_control(&text))
        }
    }
}

/// `text` safe for a terminal: every control character but newline and tab
/// (C0, DEL, C1, a CR not before a LF) becomes `\xHH`, so text from the
/// calculator or a crafted file (stack levels, names, error texts) cannot
/// send escape sequences. `\r\n` becomes `\n`. Applied to everything text
/// mode prints; JSON escapes control characters itself.
pub fn escape_control(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {
                let _ = write!(out, "\\x{:02X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// hyalo's layout: a blank line, then `  -> cmd  # description`.
fn append_hints(text: &mut String, hints: &[Hint]) {
    if hints.is_empty() {
        return;
    }
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    for (i, hint) in hints.iter().enumerate() {
        if i > 0 {
            text.push('\n');
        }
        if hint.cmd.is_empty() {
            let _ = write!(text, "  -> {}", hint.description);
        } else {
            let _ = write!(text, "  -> {}  # {}", hint.cmd, hint.description);
        }
    }
}

/// Write `text` and a newline to stdout; a closed pipe is not an error.
pub fn print_stdout(text: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    let result = out
        .write_all(text.as_bytes())
        .and_then(|()| out.write_all(b"\n"))
        .and_then(|()| out.flush());
    match result {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}

/// A failure as shown to the user.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Failure {
    /// What went wrong.
    pub error: String,
    /// What to do about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// The calculator's stack after a failed host command, level 1 first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<Vec<String>>,
}

impl Failure {
    /// Text (`error: ...` / `  hint: ...`) or a JSON object.
    pub fn render(&self, format: Format) -> String {
        match format {
            Format::Json => serde_json::to_string_pretty(self)
                .unwrap_or_else(|_| format!("{{\"error\": {:?}}}", self.error)),
            Format::Text => escape_control(&self.text()),
        }
    }

    /// The text form, before [`escape_control`].
    fn text(&self) -> String {
        let mut text = format!("error: {}", self.error);
        if let Some(stack) = &self.stack
            && !stack.is_empty()
        {
            text.push_str("\n  stack:");
            for (i, level) in stack.iter().enumerate().rev() {
                let _ = write!(text, "\n    {}: {}", i + 1, level);
            }
        }
        if let Some(hint) = &self.hint {
            let _ = write!(text, "\n  hint: {hint}");
        }
        text
    }
}

type D = data::JustLut<Val>;

/// Most results a `--jq` filter may produce.
const MAX_JQ_OUTPUTS: usize = 10_000;
/// Most bytes of `--jq` output.
const MAX_JQ_BYTES: usize = 64 << 20;

/// jaq-std filters left out of `--jq`: `env` reads every environment
/// variable (tokens included), `halt` exits, `debug` and `stderr` write to
/// stderr. jaq-std's `funs()` needs all its default features and the
/// feature groups are not finer than that (`std` also has `now`), so they
/// are filtered by name instead; the filter is then undefined.
const JQ_DENIED: &[&str] = &[
    "env",
    "halt",
    "halt_error",
    "debug",
    "debug_empty",
    "stderr",
    "stderr_empty",
];

/// Apply the jq filter `code` to `value`; outputs are joined with newlines,
/// strings printed raw (as `jq -r`, control characters escaped as in text
/// mode). At most [`MAX_JQ_OUTPUTS`] results and [`MAX_JQ_BYTES`] bytes.
pub fn run_jq(code: &str, value: &serde_json::Value) -> Result<String, String> {
    let program = File { code, path: () };
    let defs = jaq_core::defs()
        .chain(jaq_std::defs().filter(|d| !JQ_DENIED.contains(&d.name)))
        .chain(jaq_json::defs());
    let loader = Loader::new(defs);
    let arena = Arena::default();
    let modules = loader
        .load(&arena, program)
        .map_err(|_| format!("jq filter {code:?}: syntax error"))?;
    let funs = jaq_core::funs::<D>()
        .chain(jaq_std::funs::<D>().filter(|(name, _, _)| !JQ_DENIED.contains(name)))
        .chain(jaq_json::funs::<D>());
    let filter = Compiler::<_, D>::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|errs| {
            let undefined = errs
                .iter()
                .flat_map(|(_, undefs)| undefs.iter())
                .map(|(name, _)| *name)
                .next();
            match undefined {
                Some(name) => format!("jq filter {code:?}: undefined {name:?}"),
                None => format!("jq filter {code:?}: does not compile"),
            }
        })?;
    let input: Val = serde_json::from_value(value.clone())
        .map_err(|e| format!("jq input conversion failed: {e}"))?;
    let ctx = Ctx::<D>::new(&filter.lut, Vars::new([]));
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for result in filter.id.run((ctx, input)).map(jaq_core::unwrap_valr) {
        let val = result.map_err(|e| format!("jq filter {code:?}: {e}"))?;
        let text = match val {
            Val::TStr(ref s) | Val::BStr(ref s) => escape_control(&String::from_utf8_lossy(s)),
            other => other.to_string(),
        };
        bytes = bytes.saturating_add(text.len() + 1);
        if out.len() == MAX_JQ_OUTPUTS || bytes > MAX_JQ_BYTES {
            return Err(format!(
                "jq filter {code:?} produced too much output (more than {MAX_JQ_OUTPUTS} \
                 results or {} MiB)",
                MAX_JQ_BYTES >> 20
            ));
        }
        out.push(text);
    }
    Ok(out.join("\n"))
}

/// Quote `arg` for a POSIX shell when it is not plainly safe.
pub fn shell_quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:=@%+,".contains(c));
    if safe {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn outcome() -> Outcome {
        Outcome {
            results: json!([{"name": "A"}, {"name": "B"}]),
            total: Some(2),
            dir: None,
            hints: vec![Hint::cmd("Download A", "hptx get A")],
            text: "A\nB\n".into(),
        }
    }

    #[test]
    fn json_envelope_has_results_total_hints() {
        let out = render(&outcome(), Format::Json, None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["total"], 2);
        assert_eq!(v["results"][1]["name"], "B");
        assert_eq!(v["hints"][0]["cmd"], "hptx get A");
    }

    #[test]
    fn total_omitted_for_non_lists() {
        let o = Outcome {
            results: json!({"x": 1}),
            ..Outcome::default()
        };
        let out = render(&o, Format::Json, None).unwrap();
        assert!(!out.contains("total"));
    }

    #[test]
    fn text_appends_hints_like_hyalo() {
        let out = render(&outcome(), Format::Text, None).unwrap();
        assert_eq!(out, "A\nB\n\n  -> hptx get A  # Download A");
    }

    #[test]
    fn jq_reshapes_the_envelope() {
        assert_eq!(
            render(&outcome(), Format::Text, Some(".total")).unwrap(),
            "2"
        );
        assert_eq!(
            render(&outcome(), Format::Text, Some(".results[].name")).unwrap(),
            "A\nB"
        );
        assert!(render(&outcome(), Format::Json, Some(".[")).is_err());
        assert!(render(&outcome(), Format::Json, Some("nosuchfn")).is_err());
    }

    /// Audit PR #18, #10: `--jq` output is capped in results and bytes.
    #[test]
    fn jq_output_is_capped() {
        let err = render(&outcome(), Format::Json, Some("range(0; 1e9)")).unwrap_err();
        assert!(err.contains("produced too much output"), "{err}");
        let err = render(
            &outcome(),
            Format::Json,
            Some(r#"range(0; 100) | "x" * 1000000"#),
        )
        .unwrap_err();
        assert!(err.contains("produced too much output"), "{err}");
        let ok = render(&outcome(), Format::Json, Some("range(0; 10000)")).unwrap();
        assert_eq!(ok.lines().count(), 10_000);
    }

    /// security.md "`--jq` reads the environment": `env` and the filters
    /// that write to stderr or exit are undefined.
    #[test]
    fn jq_has_no_env_stderr_or_halt() {
        for code in [
            "env",
            "env.HOME",
            "debug",
            "debug(1)",
            "stderr",
            "halt",
            "halt_error",
        ] {
            let err = render(&outcome(), Format::Json, Some(code)).unwrap_err();
            assert!(err.contains("undefined"), "{code}: {err}");
        }
        // The rest of the standard library is there.
        assert_eq!(
            render(
                &outcome(),
                Format::Json,
                Some("[.results[].name] | join(\",\") | ascii_downcase")
            )
            .unwrap(),
            "a,b"
        );
        assert_eq!(
            render(&outcome(), Format::Json, Some("now | type")).unwrap(),
            "number"
        );
    }

    /// Audit PR #18, #11: control characters from the calculator are
    /// escaped in text mode; JSON escapes them itself.
    #[test]
    fn text_mode_escapes_control_characters() {
        assert_eq!(
            escape_control("A\u{1b}[2J\r\nB\tC\rD\u{7f}\u{9b}31m\u{0}Σ"),
            "A\\x1B[2J\nB\tC\\x0DD\\x7F\\x9B31m\\x00Σ"
        );
        let o = Outcome {
            results: json!([{"name": "X\u{1b}]0;pwned\u{7}"}]),
            text: "  X\u{1b}]0;pwned\u{7}  Real Number\n".into(),
            hints: vec![Hint::cmd("Download X\u{1b}[31m", "hptx get 'X\u{1b}'")],
            ..Outcome::default()
        };
        let text = render(&o, Format::Text, None).unwrap();
        assert!(
            !text.contains('\u{1b}') && !text.contains('\u{7}'),
            "{text:?}"
        );
        assert!(text.contains("X\\x1B]0;pwned\\x07"), "{text}");
        // JSON keeps the name as it is (escaped by serde).
        let json = render(&o, Format::Json, None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["results"][0]["name"], "X\u{1b}]0;pwned\u{7}");
        // --jq raw strings and failures too.
        let raw = render(&o, Format::Json, Some(".results[0].name")).unwrap();
        assert_eq!(raw, "X\\x1B]0;pwned\\x07");
        let f = Failure {
            error: "calculator error: \u{1b}[5m".into(),
            hint: None,
            stack: Some(vec!["\"\u{1b}c\"".into()]),
        };
        assert!(!f.render(Format::Text).contains('\u{1b}'));
    }

    #[test]
    fn failure_text_and_json() {
        let f = Failure {
            error: "boom".into(),
            hint: Some("try again".into()),
            stack: Some(vec!["'X'".into(), "2".into()]),
        };
        assert_eq!(
            f.render(Format::Text),
            "error: boom\n  stack:\n    2: 2\n    1: 'X'\n  hint: try again"
        );
        let v: serde_json::Value = serde_json::from_str(&f.render(Format::Json)).unwrap();
        assert_eq!(v["error"], "boom");
        assert_eq!(v["hint"], "try again");
        let bare = Failure {
            error: "e".into(),
            hint: None,
            stack: None,
        };
        assert_eq!(bare.render(Format::Json), "{\n  \"error\": \"e\"\n}");
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("HPTXV"), "HPTXV");
        assert_eq!(shell_quote("/dev/ttyUSB0"), "/dev/ttyUSB0");
        assert_eq!(shell_quote("Σ→X"), "'Σ→X'");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
    }
}
