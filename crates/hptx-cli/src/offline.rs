//! Commands that need no calculator: `object inspect`, `object convert`,
//! `grob to-png`, `completions`.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use hptx_core::grob::Grob;
use hptx_core::object::{self, AsciiHeader, BinaryHeader, Family, HEADER_LEN, ObjectType};
use serde_json::{Value, json};

use crate::convert::{self, ConvertError};
use crate::error::Hinted;
use crate::output::{Hint, Outcome, shell_quote};

/// `object` subcommands.
#[derive(Subcommand, Debug)]
pub enum ObjectCommand {
    /// Show what a transfer file holds: header, type, walked size, padding.
    #[command(after_help = "\
Reads binary files (HPHP48-x / HPHP49-x header) and ASCII files (%%HP:
header). For a binary file the object is walked nibble by nibble: the size is
what the object says, and padding is the bytes after it. An unknown prolog is
an error, never a guess. For an ASCII file the header fields are shown, and the
type when `object convert` can compile the text. Exit code 1 if any file fails.

Examples:
  hptx object inspect prg.hp
  hptx object inspect *.hp --json | jq '.results[] | {file, type, padding_bytes}'")]
    Inspect {
        /// Files to inspect.
        #[arg(required = true, value_name = "FILE")]
        files: Vec<PathBuf>,
    },
    /// Convert binary <-> ASCII for numbers, strings, binary integers, GROBs, lists.
    #[command(after_help = "\
Supported: Real Number, Complex Number, String, Binary Integer, Integer (49G),
Graphic (GROB) and lists of these. hptx has no RPL decompiler: programs,
algebraics, names, directories, units and lists holding built-in objects
(the 48 stores 1 or 2 in a list as a ROM pointer) are refused. For those, let
the calculator decompile: `hptx get NAME --ascii`.

--to ascii writes %%HP: T(3)A(D)F(.); text: pure ASCII, characters 128-255
as trigraphs, reals always with a point. Strings follow the file's header: an
HPHP48 file writes a string holding a quote as C$ n ..., an HPHP49 file
escapes quotes and backslashes. A string holding a carriage return is
refused (ASCII transfers translate CR LF).

--to binary also reads the calculator's own ASCII files (48 and 49G style,
any T and F in the header). --model sets the header (HPHP48-R or HPHP49-C)
and what `5` means: a Real on the 48, an exact Integer on the 49G.

Examples:
  hptx object convert x.hp --to ascii              # writes x.txt
  hptx object convert notes.txt --to binary -o notes.hp
  hptx object convert big.txt --to binary --model 49
  hptx object convert x.hp --to ascii -o -         # print it")]
    Convert(ConvertArgs),
}

/// `grob` subcommands.
#[derive(Subcommand, Debug)]
pub enum GrobCommand {
    /// Decode a GROB file (binary, or ASCII `GROB w h hex`) to a PNG.
    #[command(after_help = "\
Examples:
  hptx get PIC -o pic.hp && hptx grob to-png pic.hp     # writes pic.png
  hptx grob to-png pic.txt -o picture.png
  hptx grob to-png pic.hp -o - | open -f -a Preview")]
    ToPng {
        /// GROB file from `hptx get` (binary or --ascii).
        file: PathBuf,
        /// Output file [default: FILE with .png]; - for stdout.
        #[arg(short, long, value_name = "OUT.png")]
        output: Option<PathBuf>,
        /// Replace an existing file.
        #[arg(long)]
        force: bool,
    },
}

/// Target of `object convert`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Target {
    /// HPHP48-x / HPHP49-x binary file.
    Binary,
    /// %%HP: text file.
    Ascii,
}

/// Calculator family for `object convert --to binary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ModelArg {
    /// HP 48S/SX/G/GX: header HPHP48-R, `5` is a Real.
    #[value(name = "48")]
    Hp48,
    /// HP 49G: header HPHP49-C, `5` is an exact Integer.
    #[value(name = "49")]
    Hp49,
}

/// `object convert` options.
#[derive(Args, Debug)]
pub struct ConvertArgs {
    /// Binary (HPHP48/49) or ASCII (%%HP:) object file; - for stdin.
    pub file: PathBuf,
    /// Format to write.
    #[arg(long, value_enum)]
    pub to: Target,
    /// Output file [default: FILE with .txt for ascii, .hp for binary]; - for stdout.
    #[arg(short, long, value_name = "OUT")]
    pub output: Option<PathBuf>,
    /// Calculator the binary file is for (--to binary only).
    #[arg(long, value_enum, default_value = "48")]
    pub model: ModelArg,
    /// Replace an existing file.
    #[arg(long)]
    pub force: bool,
}

/// An outcome that is shown as usual but ends with exit code 1 (some of
/// several files failed).
#[derive(Debug)]
pub struct PartialFailure {
    /// What to print.
    pub outcome: Outcome,
    /// The summary for stderr.
    pub message: String,
}

impl std::fmt::Display for PartialFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PartialFailure {}

fn read_input(file: &Path) -> Result<Vec<u8>> {
    if file == Path::new("-") {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .context("reading stdin")?;
        return Ok(buf);
    }
    std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))
}

fn family_name(f: Family) -> &'static str {
    match f {
        Family::Hp48 => "HP 48",
        Family::Hp49 => "HP 49G",
    }
}

/// What one file holds, as JSON and text.
fn inspect_one(file: &Path) -> Result<(Value, String, Vec<Hint>)> {
    let label = file.display().to_string();
    let data = read_input(file)?;
    let q = shell_quote(&label);
    if let Some(header) = BinaryHeader::parse(&data) {
        let header_text = String::from_utf8_lossy(&header.to_bytes()).into_owned();
        let info = object::inspect(&data).map_err(|e| {
            Hinted::new(
                format!("{label}: {e}"),
                "the header says binary but no object follows it; `get` the variable again",
            )
        })?;
        let nibbles = object::unpack(&data[HEADER_LEN..]);
        let size = object::object_size(&nibbles, 0).map_err(|e| {
            let what = info.object_type.map_or_else(
                || format!("prolog #{:05X}", info.prolog),
                |t| t.name().into(),
            );
            Hinted::new(
                format!("{label}: {what}: {e}"),
                "hptx cannot walk this object; the file may be truncated or hold a type hptx \
                 does not know. `hptx get NAME --ascii` gets it as text instead",
            )
        })?;
        let Some(ty) = info.object_type else {
            // object_size fails on an unknown prolog, so this does not happen.
            anyhow::bail!("{label}: unknown prolog #{:05X}", info.prolog);
        };
        let object_bytes = size.div_ceil(2);
        let padding = data.len() - HEADER_LEN - object_bytes;
        let mut text = String::new();
        let _ = writeln!(text, "{label}");
        let _ = writeln!(
            text,
            "  header   {header_text} ({}, ROM {})",
            family_name(header.family),
            char::from(header.rom)
        );
        let _ = writeln!(text, "  type     {} (#{:05X})", ty.name(), info.prolog);
        let _ = writeln!(
            text,
            "  size     {size} nibbles, {object_bytes} bytes + {HEADER_LEN} header bytes"
        );
        let _ = writeln!(
            text,
            "  padding  {padding} byte{} after the object",
            if padding == 1 { "" } else { "s" }
        );
        let mut hints = Vec::new();
        if ty == ObjectType::Graphic {
            hints.push(Hint::cmd(
                "Save the GROB as PNG",
                format!("hptx grob to-png {q}"),
            ));
        }
        if convert::to_ascii(&data).is_ok() {
            hints.push(Hint::cmd(
                "Convert it to text",
                format!("hptx object convert {q} --to ascii"),
            ));
        }
        let json = json!({
            "file": label,
            "format": "binary",
            "header": header_text,
            "family": match header.family { Family::Hp48 => "48", Family::Hp49 => "49" },
            "rom": char::from(header.rom).to_string(),
            "prolog": format!("#{:05X}", info.prolog),
            "type": ty.name(),
            "size_nibbles": size,
            "size_bytes": object_bytes,
            "file_bytes": data.len(),
            "padding_bytes": padding,
        });
        return Ok((json, text, hints));
    }
    if let Some((header, header_len)) = AsciiHeader::parse(&data) {
        let ty = convert::parse(&data, Family::Hp48)
            .ok()
            .map(|p| p.object_type.name());
        let mut text = String::new();
        let _ = writeln!(text, "{label}");
        let _ = writeln!(
            text,
            "  header   {} (translate {}, angle {}, fraction mark {})",
            header.to_line(),
            header.translate,
            char::from(header.angle),
            char::from(header.fraction)
        );
        let _ = writeln!(
            text,
            "  type     {}",
            ty.unwrap_or("not known to hptx (the calculator compiles the text on put)")
        );
        let _ = writeln!(
            text,
            "  size     {} bytes of text after the header",
            data.len() - header_len
        );
        let hints = if ty.is_some() {
            vec![Hint::cmd(
                "Compile it to a binary object",
                format!("hptx object convert {q} --to binary"),
            )]
        } else {
            vec![Hint::cmd(
                "Upload it; the calculator compiles it",
                format!("hptx put {q}"),
            )]
        };
        let json = json!({
            "file": label,
            "format": "ascii",
            "header": header.to_line(),
            "translate": header.translate,
            "angle": char::from(header.angle).to_string(),
            "fraction_mark": char::from(header.fraction).to_string(),
            "type": ty,
            "text_bytes": data.len() - header_len,
            "file_bytes": data.len(),
        });
        return Ok((json, text, hints));
    }
    Err(Hinted::new(
        format!("{label}: neither a binary object (HPHP48-x / HPHP49-x) nor %%HP: text"),
        "inspect reads files from `hptx get`, `hptx backup` or the calculator's own transfers",
    )
    .into())
}

/// `object inspect FILE...`.
pub fn inspect(files: &[PathBuf]) -> Result<Outcome> {
    if let [file] = files {
        let (json, text, hints) = inspect_one(file)?;
        return Ok(Outcome {
            results: Value::from(vec![json]),
            total: Some(1),
            dir: None,
            hints,
            text,
        });
    }
    let mut results = Vec::new();
    let mut text = String::new();
    let mut hints = Vec::new();
    let mut failed = Vec::new();
    for file in files {
        let label = file.display().to_string();
        if !text.is_empty() {
            text.push('\n');
        }
        match inspect_one(file) {
            Ok((json, t, h)) => {
                results.push(json);
                text.push_str(&t);
                if hints.is_empty() {
                    hints = h;
                }
            }
            Err(e) => {
                let failure = crate::error::describe(&e, &crate::error::LinkInfo::default());
                let _ = writeln!(text, "{label}\n  error    {}", failure.error);
                results.push(json!({"file": label, "error": failure.error, "hint": failure.hint}));
                failed.push(label);
            }
        }
    }
    let outcome = Outcome {
        total: Some(results.len() as u64),
        results: Value::from(results),
        dir: None,
        hints,
        text,
    };
    if failed.is_empty() {
        Ok(outcome)
    } else {
        Err(PartialFailure {
            message: format!("cannot inspect {}", failed.join(", ")),
            outcome,
        }
        .into())
    }
}

/// `FILE` with `ext`, or `FILE.ext` when that would be `FILE` itself.
fn sibling(file: &Path, ext: &str) -> PathBuf {
    let out = file.with_extension(ext);
    if out == file {
        let mut s = file.as_os_str().to_owned();
        s.push(".");
        s.push(ext);
        PathBuf::from(s)
    } else {
        out
    }
}

fn convert_error(label: &str, e: ConvertError, target: Target) -> anyhow::Error {
    match e {
        ConvertError::Unsupported(what) => Hinted::new(
            format!("{label}: cannot convert {what} to {}", target_name(target)),
            format!(
                "hptx converts only {}. Let the calculator do it: `hptx put` the file, \
                 then `hptx get NAME{}`",
                convert::SUPPORTED,
                if target == Target::Ascii {
                    " --ascii"
                } else {
                    ""
                }
            ),
        )
        .into(),
        ConvertError::Invalid(why) => Hinted::new(
            format!("{label}: {why}"),
            "`hptx object inspect FILE` shows what the file holds",
        )
        .into(),
    }
}

fn target_name(t: Target) -> &'static str {
    match t {
        Target::Binary => "binary",
        Target::Ascii => "ASCII",
    }
}

/// `object convert`. `Ok(None)` when written to stdout.
pub fn convert(args: &ConvertArgs) -> Result<Option<Outcome>> {
    let label = args.file.display().to_string();
    let data = read_input(&args.file)?;
    let is_binary = BinaryHeader::parse(&data).is_some();
    let (out, ty) = match (args.to, is_binary) {
        (Target::Ascii, true) => {
            let ty = object::inspect(&data).ok().and_then(|i| i.object_type);
            let out = convert::to_ascii(&data).map_err(|e| convert_error(&label, e, args.to))?;
            (out, ty)
        }
        (Target::Binary, false) => {
            let family = match args.model {
                ModelArg::Hp48 => Family::Hp48,
                ModelArg::Hp49 => Family::Hp49,
            };
            let parsed =
                convert::parse(&data, family).map_err(|e| convert_error(&label, e, args.to))?;
            (
                convert::to_binary(&data, family).map_err(|e| convert_error(&label, e, args.to))?,
                Some(parsed.object_type),
            )
        }
        (Target::Ascii, false) => {
            return Err(Hinted::new(
                format!("{label} is not a binary object (no HPHP48-x / HPHP49-x header)"),
                format!(
                    "it may be text already: `hptx object inspect {}`",
                    shell_quote(&label)
                ),
            )
            .into());
        }
        (Target::Binary, true) => {
            return Err(Hinted::new(
                format!("{label} is binary already"),
                format!(
                    "--to ascii converts it to text: `hptx object convert {} --to ascii`",
                    shell_quote(&label)
                ),
            )
            .into());
        }
    };
    let ext = match args.to {
        Target::Ascii => "txt",
        Target::Binary => "hp",
    };
    let to_stdout = args.output.as_deref() == Some(Path::new("-"));
    if to_stdout || (args.output.is_none() && args.file == Path::new("-")) {
        write_stdout(&out)?;
        return Ok(None);
    }
    let file = args
        .output
        .clone()
        .unwrap_or_else(|| sibling(&args.file, ext));
    write_new(&file, &out, args.force)?;
    let type_name = ty.map(ObjectType::name);
    let file_label = file.display().to_string();
    let mut hints = vec![Hint::cmd(
        "Check the result",
        format!("hptx object inspect {}", shell_quote(&file_label)),
    )];
    hints.push(Hint::cmd(
        "Upload it",
        format!("hptx put {}", shell_quote(&file_label)),
    ));
    Ok(Some(Outcome {
        results: json!({
            "file": label,
            "output": file_label,
            "to": target_name(args.to).to_ascii_lowercase(),
            "type": type_name,
            "bytes": out.len(),
        }),
        total: None,
        dir: None,
        hints,
        text: format!(
            "{label} -> {file_label} ({}, {}, {} bytes)",
            type_name.unwrap_or("object"),
            target_name(args.to),
            out.len()
        ),
    }))
}

/// `grob to-png`. `Ok(None)` when written to stdout.
pub fn grob_to_png(file: &Path, output: Option<&Path>, force: bool) -> Result<Option<Outcome>> {
    let label = file.display().to_string();
    let data = read_input(file)?;
    let grob = if BinaryHeader::parse(&data).is_some() {
        Grob::from_file(&data).map_err(|e| not_a_grob(&label, &data, &e.to_string()))?
    } else {
        let family = Family::Hp48;
        let parsed =
            convert::parse(&data, family).map_err(|e| not_a_grob(&label, &data, &e.to_string()))?;
        if parsed.object_type != ObjectType::Graphic {
            return Err(not_a_grob(
                &label,
                &data,
                &format!("it holds a {}", parsed.object_type.name()),
            ));
        }
        Grob::from_nibbles(&parsed.nibbles, 0)
            .map_err(|e| not_a_grob(&label, &data, &e.to_string()))?
    };
    let png = grob.to_png().context("PNG encoding")?;
    if output == Some(Path::new("-")) {
        write_stdout(&png)?;
        return Ok(None);
    }
    let out = output.map_or_else(|| sibling(file, "png"), Path::to_path_buf);
    write_new(&out, &png, force)?;
    let out_label = out.display().to_string();
    Ok(Some(Outcome {
        results: json!({
            "file": label,
            "output": out_label,
            "width": grob.width,
            "height": grob.height,
            "bytes": png.len(),
        }),
        total: None,
        dir: None,
        hints: Vec::new(),
        text: format!(
            "{label} -> {out_label} ({}x{} GROB, {} bytes)",
            grob.width,
            grob.height,
            png.len()
        ),
    }))
}

fn not_a_grob(label: &str, data: &[u8], why: &str) -> anyhow::Error {
    let ty = object::inspect(data)
        .ok()
        .and_then(|i| i.object_type)
        .filter(|t| *t != ObjectType::Graphic);
    let message = match ty {
        Some(t) => format!("{label} holds a {}, not a GROB", t.name()),
        None => format!("{label} is not a GROB: {why}"),
    };
    Hinted::new(
        message,
        "to-png takes a GROB from `hptx get` (binary or --ascii); `hptx screenshot` saves the \
         display directly",
    )
    .into()
}

/// `completions SHELL`.
pub fn completions(shell: clap_complete::Shell) -> Result<Option<Outcome>> {
    let mut cmd = <crate::Cli as clap::CommandFactory>::command();
    let mut bytes = Vec::new();
    clap_complete::generate(shell, &mut cmd, "hptx", &mut bytes);
    write_stdout(&bytes)?;
    Ok(None)
}

fn write_stdout(bytes: &[u8]) -> Result<()> {
    let mut out = std::io::stdout().lock();
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other.context("writing to stdout"),
    }
}

fn write_new(file: &Path, data: &[u8], force: bool) -> Result<()> {
    crate::commands::refuse_existing_file(file, force)?;
    crate::commands::write_file(file, data, force)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(format!(
            "{}/../hptx-core/fixtures/{name}.hp",
            env!("CARGO_MANIFEST_DIR")
        ))
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hptx-offline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn inspect_fixtures() {
        for model in ["48sx", "48gx", "49g"] {
            for (kind, ty) in [
                ("R", "Real Number"),
                ("S", "String"),
                ("L", "List"),
                ("P", "Program"),
                ("G", "Graphic"),
                ("D1", "Directory"),
            ] {
                let path = fixture(&format!("{model}-{kind}"));
                let len = std::fs::metadata(&path).unwrap().len();
                let o = inspect(&[path]).unwrap();
                let r = &o.results[0];
                assert_eq!(r["type"], ty, "{model}-{kind}");
                assert_eq!(r["format"], "binary");
                assert_eq!(r["file_bytes"], len);
                assert_eq!(r["padding_bytes"], 0, "{model}-{kind}");
                let bytes = r["size_bytes"].as_u64().unwrap();
                assert_eq!(bytes + 8, len, "{model}-{kind}");
                assert!(o.text.contains(ty));
            }
        }
        let g = inspect(&[fixture("48sx-G")]).unwrap();
        assert_eq!(g.results[0]["size_nibbles"], 2196);
        assert!(g.hints.iter().any(|h| h.cmd.contains("grob to-png")));
        assert_eq!(g.results[0]["header"], "HPHP48-J");
    }

    #[test]
    fn inspect_padding_and_errors() {
        let mut data = std::fs::read(fixture("49g-R")).unwrap();
        data.extend([0, 0, 0]);
        let padded = tmp("padded.hp");
        std::fs::write(&padded, &data).unwrap();
        let o = inspect(std::slice::from_ref(&padded)).unwrap();
        assert_eq!(o.results[0]["padding_bytes"], 3);

        // A list holding an unknown prolog: an error, not a guess.
        let mut bad = b"HPHP49-C".to_vec();
        bad.extend(object::pack(&[
            4, 7, 0xA, 2, 0, // list
            0, 0, 7, 2, 0, // #02700: in the prolog range, unknown
            0xB, 2, 1, 3, 0, // SEMI
        ]));
        let bad_path = tmp("bad.hp");
        std::fs::write(&bad_path, &bad).unwrap();
        let err = inspect(std::slice::from_ref(&bad_path)).unwrap_err();
        assert!(err.to_string().contains("unknown prolog #02700"), "{err}");

        // Several files: results for each, the error inline, exit code 1.
        let err = inspect(&[padded, bad_path]).unwrap_err();
        let partial = err.downcast_ref::<PartialFailure>().unwrap();
        assert_eq!(partial.outcome.total, Some(2));
        assert!(
            partial.outcome.results[1]["error"]
                .as_str()
                .unwrap()
                .contains("unknown prolog")
        );
        let text = tmp("plain.txt");
        std::fs::write(&text, "hello").unwrap();
        assert!(inspect(&[text]).is_err());
    }

    #[test]
    fn inspect_ascii() {
        let path = tmp("x.txt");
        std::fs::write(&path, "%%HP: T(1)A(R)F(.);\r\n{ 1.5 \"x\" }").unwrap();
        let o = inspect(std::slice::from_ref(&path)).unwrap();
        assert_eq!(o.results[0]["format"], "ascii");
        assert_eq!(o.results[0]["translate"], 1);
        assert_eq!(o.results[0]["type"], "List");
        std::fs::write(&path, "%%HP: T(3)A(D)F(.);\n\\<< 1 + \\>>").unwrap();
        let o = inspect(&[path]).unwrap();
        assert_eq!(o.results[0]["type"], Value::Null);
        assert!(o.hints[0].cmd.starts_with("hptx put"));
    }

    #[test]
    fn convert_round_trip_and_refusals() {
        let src = tmp("s.hp");
        std::fs::copy(fixture("48gx-S"), &src).unwrap();
        let args = |file: &Path, to, output: Option<PathBuf>| ConvertArgs {
            file: file.to_path_buf(),
            to,
            output,
            model: ModelArg::Hp48,
            force: false,
        };
        let o = convert(&args(&src, Target::Ascii, None)).unwrap().unwrap();
        let txt = sibling(&src, "txt");
        assert_eq!(o.results["output"], txt.display().to_string());
        assert_eq!(
            std::fs::read(&txt).unwrap(),
            b"%%HP: T(3)A(D)F(.);\r\n\"AB\"\r\n"
        );
        // Existing output is kept unless --force.
        assert!(convert(&args(&src, Target::Ascii, None)).is_err());
        let back = tmp("s2.hp");
        convert(&args(&txt, Target::Binary, Some(back.clone()))).unwrap();
        assert_eq!(
            std::fs::read(&back).unwrap()[8..],
            std::fs::read(&src).unwrap()[8..]
        );
        // Wrong direction and unsupported types.
        assert!(convert(&args(&src, Target::Binary, Some(tmp("n")))).is_err());
        assert!(convert(&args(&txt, Target::Ascii, Some(tmp("n")))).is_err());
        let err =
            convert(&args(&fixture("48gx-P"), Target::Ascii, Some(tmp("p.txt")))).unwrap_err();
        let h = err.downcast_ref::<Hinted>().unwrap();
        assert!(
            h.message.contains("cannot convert a Program"),
            "{}",
            h.message
        );
        assert!(h.hint.contains("--ascii"), "{}", h.hint);
    }

    #[test]
    fn grob_png_from_binary_and_ascii() {
        let png = tmp("lcd.png");
        let o = grob_to_png(&fixture("49g-G"), Some(&png), false)
            .unwrap()
            .unwrap();
        assert_eq!(
            (o.results["width"].clone(), o.results["height"].clone()),
            (131.into(), 64.into())
        );
        assert!(std::fs::read(&png).unwrap().starts_with(b"\x89PNG"));
        let txt = tmp("g.txt");
        std::fs::write(&txt, "%%HP: T(3)A(D)F(.);\r\nGROB 3 2 5020\r\n").unwrap();
        let o = grob_to_png(&txt, None, false).unwrap().unwrap();
        assert_eq!(
            o.results["output"],
            sibling(&txt, "png").display().to_string()
        );
        let err = grob_to_png(&fixture("48sx-R"), Some(&tmp("r.png")), false).unwrap_err();
        assert!(
            err.to_string().contains("holds a Real Number, not a GROB"),
            "{err}"
        );
    }

    #[test]
    fn sibling_names() {
        assert_eq!(sibling(Path::new("a/x.hp"), "txt"), Path::new("a/x.txt"));
        assert_eq!(sibling(Path::new("PRG"), "hp"), Path::new("PRG.hp"));
        assert_eq!(sibling(Path::new("x.hp"), "hp"), Path::new("x.hp.hp"));
    }
}
