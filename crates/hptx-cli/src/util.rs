//! Small pure helpers: paths, names, numbers, times.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// The model, as far as the ROM version text tells: `VERSION` is missing on
/// the 48S/SX and says `HP48-R, Copyright HP 1993` on the 48G/GX. The 49G
/// also says `HP48-C Revision ...` (cut by its display width), so it is told
/// apart by its copyright year (1999 or later) or an `HP49`.
pub fn model(version: Option<&str>) -> &'static str {
    let Some(v) = version else {
        return "HP 48S/SX";
    };
    let year = v
        .rsplit(|c: char| !c.is_ascii_digit())
        .find(|w| w.len() == 4)
        .and_then(|w| w.parse::<u32>().ok());
    if v.contains("HP49") || year.is_some_and(|y| y >= 1999) {
        "HP 49G"
    } else if v.contains("HP48") {
        "HP 48G/GX"
    } else {
        "unknown"
    }
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

    #[test]
    fn models() {
        assert_eq!(model(None), "HP 48S/SX");
        assert_eq!(
            model(Some("Version HP48-R, Copyright HP 1993")),
            "HP 48G/GX"
        );
        assert_eq!(model(Some("Version HP49-C, Copyright HP 2000")), "HP 49G");
        // Recorded on the emulated 49G (fixtures/49g-version.txt).
        assert_eq!(
            model(Some("Version HP48-C Revisi, Copyright HP 2009")),
            "HP 49G"
        );
    }

    #[test]
    fn timestamps() {
        assert_eq!(format_timestamp(0), "19700101-000000");
        assert_eq!(format_timestamp(1_791_201_845), "20261005-120405");
        assert_eq!(format_timestamp(951_782_400), "20000229-000000");
    }
}
