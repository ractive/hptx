//! Small helpers: paths, names, numbers, times, bounded input.

use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::error::Hinted;

/// Most bytes hptx reads from a local file or stdin: Kermit's receive
/// limit (`kermit_proto::Config::max_size`, 4 MiB), far above the largest
/// HP object or a whole backup.
pub const MAX_INPUT: u64 = 4 << 20;

/// Read `file` (`-` for stdin) whole, at most [`MAX_INPUT`] bytes: a file's
/// size is checked before it is read, and the read stops one byte past the
/// limit (stdin, a file that grows), so nothing larger is ever buffered.
pub fn read_input(file: &Path) -> Result<Vec<u8>> {
    read_input_limited(file, MAX_INPUT)
}

fn read_input_limited(file: &Path, limit: u64) -> Result<Vec<u8>> {
    let label = file.display().to_string();
    let too_large = || {
        anyhow::Error::new(Hinted::new(
            format!("{label} is larger than {} bytes", limit),
            "hptx reads at most 4 MiB, more than any calculator holds; is it the right file?",
        ))
    };
    let mut data = Vec::new();
    if file == Path::new("-") {
        std::io::stdin()
            .lock()
            .take(limit + 1)
            .read_to_end(&mut data)
            .context("reading stdin")?;
    } else {
        let f = std::fs::File::open(file).with_context(|| format!("cannot read {label}"))?;
        let len = f
            .metadata()
            .with_context(|| format!("cannot read {label}"))?
            .len();
        if len > limit {
            return Err(too_large());
        }
        f.take(limit + 1)
            .read_to_end(&mut data)
            .with_context(|| format!("cannot read {label}"))?;
    }
    if data.len() as u64 > limit {
        return Err(too_large());
    }
    Ok(data)
}

/// Split a directory path into names below HOME: `HOME/A/B`, `/A/B`,
/// `A/B`, `{ HOME A B }` and `HOME A B` all give `["A", "B"]`.
pub fn parse_dir(path: &str) -> Vec<String> {
    let trimmed = path.trim().trim_start_matches('{').trim_end_matches('}');
    let mut parts: Vec<String> = trimmed
        .split(|c: char| c == '/' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if parts.first().is_some_and(|p| p == "HOME") {
        parts.remove(0);
    }
    parts
}

/// `HOME/A/B` for `["A", "B"]`.
pub fn dir_string(components: &[String]) -> String {
    std::iter::once("HOME")
        .chain(components.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("/")
}

/// A calculator path such as `["HOME", "A"]` as `HOME/A`.
pub fn path_string(path: &[String]) -> String {
    path.join("/")
}

/// Variable name for a file: its name without the last extension.
pub fn name_from_file(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
}

/// A size in bytes as the listing has it: `29.5`, `12`.
pub fn number(value: f64) -> serde_json::Value {
    if value.fract() == 0.0 && value.abs() < 9.0e15 {
        #[allow(clippy::cast_possible_truncation)]
        serde_json::Value::from(value as i64)
    } else {
        serde_json::Value::from(value)
    }
}

/// [`number`] as text.
pub fn number_text(value: f64) -> String {
    number(value).to_string()
}

/// UTC time as `YYYYMMDD-HHMMSS`, for default file names.
pub fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format_timestamp(secs)
}

fn format_timestamp(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or_default();
    let rest = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn dir_forms() {
        for p in [
            "HOME/A/B",
            "/A/B",
            "A/B",
            "{ HOME A B }",
            "HOME A B",
            "A/B/",
        ] {
            assert_eq!(parse_dir(p), ["A", "B"], "{p}");
        }
        assert!(parse_dir("HOME").is_empty());
        assert!(parse_dir("/").is_empty());
        assert_eq!(dir_string(&parse_dir("A/B")), "HOME/A/B");
    }

    #[test]
    fn names_from_files() {
        assert_eq!(name_from_file(Path::new("dir/prg.hp")).unwrap(), "prg");
        assert_eq!(name_from_file(Path::new("PRG")).unwrap(), "PRG");
    }

    #[test]
    fn numbers() {
        assert_eq!(number_text(12.0), "12");
        assert_eq!(number_text(29.5), "29.5");
    }

    /// security.md "Local input files are read whole": an oversized file
    /// is refused on its size, before it is read.
    #[test]
    fn oversized_input_is_refused() {
        let path = std::env::temp_dir().join(format!("hptx-input-{}", std::process::id()));
        let f = std::fs::File::create(&path).unwrap();
        // Sparse: the length says 4 MiB + 1 without writing it.
        f.set_len(MAX_INPUT + 1).unwrap();
        drop(f);
        let err = read_input(&path).unwrap_err();
        assert!(
            err.to_string().ends_with("is larger than 4194304 bytes"),
            "{err}"
        );
        std::fs::write(&path, b"12345").unwrap();
        assert_eq!(read_input_limited(&path, 5).unwrap(), b"12345");
        assert!(read_input_limited(&path, 4).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn timestamps() {
        assert_eq!(format_timestamp(0), "19700101-000000");
        assert_eq!(format_timestamp(1_791_201_845), "20261005-120405");
        assert_eq!(format_timestamp(951_782_400), "20000229-000000");
    }
}
