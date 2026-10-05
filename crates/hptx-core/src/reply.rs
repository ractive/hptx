//! Parsers for the text a calculator in Kermit server mode returns: the
//! stack display of a host command (`C`) and the directory listing (`G D`).
//!
//! Input is already translated from the HP character set
//! ([`crate::charset::decode`]). The 48SX, 48GX and 49G differ in details:
//! the 49G prints reals with a trailing dot (`10777.`), shows lists in
//! algebraic form (`{9600.,0.,0.,0.,3.,1.}`), shows names without quotes and
//! truncates long values at the display width.

use crate::{Error, Result};

/// The stack text returned for a host command.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StackReply {
    /// The text after `Error: ` if the command failed.
    pub error: Option<String>,
    /// Stack levels as display text; `levels[0]` is level 1.
    pub levels: Vec<String>,
}

impl StackReply {
    /// Level `n` (1-based); `None` if absent or `n == 0`.
    pub fn level(&self, n: usize) -> Option<&str> {
        n.checked_sub(1)
            .and_then(|i| self.levels.get(i))
            .map(String::as_str)
    }

    /// True if the stack has no levels.
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }
}

/// Split `line` into the value after an `N:` prefix, if it starts with one.
fn level_prefix(line: &str) -> Option<(usize, &str)> {
    let (num, rest) = line.split_once(':')?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = num.parse().ok()?;
    (n >= 1).then_some((n, rest))
}

/// Parse the stack display returned for a host command.
///
/// An optional first line `Error: X` sets `error`. `Empty Stack` means no
/// levels. Each level starts with `N:`, counting down to `1:`; any other line
/// is a continuation of the current value, joined with `\n`. Never fails:
/// text before the first level line (other than the error and `Empty Stack`
/// lines) is ignored, and lines after `1:` are appended to level 1.
pub fn parse_stack(text: &str) -> StackReply {
    let text = text.trim_end_matches(['\r', '\n']);
    let mut reply = StackReply::default();
    // Highest level first; reversed at the end.
    let mut values: Vec<String> = Vec::new();
    // The level the next level line must carry; 0 before the first one.
    let mut expected = 0usize;
    let mut first = true;
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if first {
            first = false;
            if let Some(msg) = line.strip_prefix("Error:") {
                reply.error = Some(msg.trim().to_string());
                continue;
            }
        }
        if values.is_empty() {
            if line.trim() == "Empty Stack" {
                break;
            }
            if let Some((n, rest)) = level_prefix(line) {
                values.push(rest.trim_start_matches(' ').to_string());
                expected = n - 1;
            }
            continue;
        }
        if expected >= 1 {
            let prefix = format!("{expected}:");
            if let Some(rest) = line.strip_prefix(prefix.as_str()) {
                values.push(rest.trim_start_matches(' ').to_string());
                expected -= 1;
                continue;
            }
        }
        if let Some(last) = values.last_mut() {
            last.push('\n');
            last.push_str(line);
        }
    }
    values.reverse();
    reply.levels = values;
    reply
}

/// A directory listing returned for `G D`.
#[derive(Clone, Debug, PartialEq)]
pub struct Listing {
    /// The current directory path, e.g. `["HOME", "D1"]` (48GX and 49G only).
    pub path: Option<Vec<String>>,
    /// Free memory in bytes (48GX and 49G only).
    pub free: Option<f64>,
    /// The variables of the current directory, in calculator order.
    pub entries: Vec<Entry>,
}

/// One variable in a [`Listing`].
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The variable name.
    pub name: String,
    /// Size in bytes; can be a half (`29.5`).
    pub size: f64,
    /// The English type name, e.g. `Real Number`, `Directory`.
    pub kind: String,
    /// The calculator's 16-bit CRC of the object.
    pub checksum: u16,
}

impl Entry {
    /// True if the variable is a directory.
    pub fn is_directory(&self) -> bool {
        self.kind == "Directory"
    }
}

/// Parse a `G D` directory listing.
///
/// An optional first line starting with `{` holds the path and the free
/// memory. Every other non-blank line is `NAME SIZE TYPE... CHECKSUM`.
/// Returns [`Error::Reply`] for a line that does not fit.
pub fn parse_listing(text: &str) -> Result<Listing> {
    let mut listing = Listing {
        path: None,
        free: None,
        entries: Vec::new(),
    };
    let mut first = true;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if std::mem::take(&mut first) && line.starts_with('{') {
            let bad = || Error::Reply(format!("bad directory header: {line:?}"));
            let end = line.find('}').ok_or_else(bad)?;
            let (list, rest) = line.split_at(end + 1);
            listing.path = Some(parse_list(list).ok_or_else(bad)?);
            if !rest.trim().is_empty() {
                listing.free = Some(parse_real(rest).ok_or_else(bad)?);
            }
            continue;
        }
        listing.entries.push(parse_entry(line)?);
    }
    Ok(listing)
}

fn parse_entry(line: &str) -> Result<Entry> {
    let bad = || Error::Reply(format!("bad directory line: {line:?}"));
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let [name, size, kind @ .., checksum] = tokens.as_slice() else {
        return Err(bad());
    };
    if kind.is_empty() {
        return Err(bad());
    }
    let size = parse_real(size).filter(|s| *s >= 0.0).ok_or_else(bad)?;
    let checksum = parse_real(checksum)
        .and_then(integer)
        .and_then(|c| u16::try_from(c).ok())
        .ok_or_else(bad)?;
    Ok(Entry {
        name: (*name).to_string(),
        size,
        kind: kind.join(" "),
        checksum,
    })
}

/// The integer value of `x`, if it has no fractional part.
fn integer(x: f64) -> Option<i64> {
    // The bounds keep the cast exact.
    (x.fract() == 0.0 && x.abs() < 9.0e15).then_some(x as i64)
}

/// Parse a displayed real such as `127828.`, `1.5`, `-3` or `1.E-3`.
/// Leading and trailing whitespace is ignored.
pub fn parse_real(s: &str) -> Option<f64> {
    let s = s.trim();
    // Rust also accepts `inf` and `NaN`; the calculator never prints those.
    if s.is_empty()
        || !s
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'E' | 'e' | '+' | '-'))
    {
        return None;
    }
    s.parse().ok()
}

/// Parse a displayed flat list into its items as text: `{ 9600 0 0 0 3 1 }`,
/// `{9600.,0.,0.,0.,3.,1.}`, `{ HOME D1 }`, `{ }`.
///
/// Items are separated by whitespace, `,` or `;`. `None` if the text is not
/// wrapped in `{ }` (a 49G truncated list lacks the closing brace) or holds
/// nested braces or quotes.
pub fn parse_list(s: &str) -> Option<Vec<String>> {
    let inner = s.trim().strip_prefix('{')?.strip_suffix('}')?;
    if inner.contains(['{', '}', '"', '\'']) {
        return None;
    }
    Some(
        inner
            .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Parse a displayed string: `"Version HP48-R"` gives `Version HP48-R`.
///
/// A missing closing quote (the 49G truncates long values) is accepted and
/// the rest is returned as is. `None` if the text does not start with `"`.
pub fn parse_string(s: &str) -> Option<String> {
    let s = s.trim();
    let body = s.strip_prefix('"')?;
    Some(body.strip_suffix('"').unwrap_or(body).to_string())
}

/// Parse a quoted global name: `'NOSUCH'` gives `NOSUCH`.
///
/// `None` for anything else, including an unquoted name (the 49G in
/// algebraic mode shows names without quotes) and quoted expressions.
pub fn parse_name(s: &str) -> Option<String> {
    let inner = s.trim().strip_prefix('\'')?.strip_suffix('\'')?;
    let first = inner.chars().next()?;
    let valid = !first.is_ascii_digit()
        && first != '.'
        && inner.chars().all(|c| {
            !c.is_whitespace()
                && !matches!(
                    c,
                    '+' | '-'
                        | '*'
                        | '/'
                        | '^'
                        | '='
                        | '<'
                        | '>'
                        | '('
                        | ')'
                        | '{'
                        | '}'
                        | '['
                        | ']'
                        | '#'
                        | ','
                        | ';'
                        | '\''
                        | '"'
                        | ':'
                        | '«'
                        | '»'
                )
        });
    valid.then(|| inner.to_string())
}

/// The IOPAR list (wiki: protocols/iopar): baud, parity, receive pacing,
/// transmit pacing, Kermit checksum type, translation code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Iopar {
    /// Baud rate.
    pub baud: u32,
    /// Parity: 0 none, 1 odd, 2 even, 3 mark, 4 space; negative applies to
    /// transmit only.
    pub parity: i8,
    /// XON/XOFF pacing when receiving.
    pub receive_pacing: bool,
    /// XON/XOFF pacing when transmitting.
    pub transmit_pacing: bool,
    /// Kermit block check type (1, 2 or 3).
    pub checksum: u8,
    /// Character translation code.
    pub translate: u8,
}

impl Default for Iopar {
    /// `{ 9600 0 0 0 3 1 }`, the settings of a fresh calculator.
    fn default() -> Self {
        Self {
            baud: 9600,
            parity: 0,
            receive_pacing: false,
            transmit_pacing: false,
            checksum: 3,
            translate: 1,
        }
    }
}

impl Iopar {
    /// Parse a displayed IOPAR list in 48 (`{ 9600 0 0 0 3 1 }`) or 49G
    /// (`{9600.,0.,0.,0.,3.,1.}`) form. `None` unless it has exactly six
    /// integer items in range. Nonzero pacing values mean on.
    pub fn parse(s: &str) -> Option<Self> {
        let items = parse_list(s)?;
        let values = items
            .iter()
            .map(|item| parse_real(item).and_then(integer))
            .collect::<Option<Vec<i64>>>()?;
        let [baud, parity, rx, tx, checksum, translate] = values.as_slice() else {
            return None;
        };
        Some(Self {
            baud: u32::try_from(*baud).ok()?,
            parity: i8::try_from(*parity).ok()?,
            receive_pacing: *rx != 0,
            transmit_pacing: *tx != 0,
            checksum: u8::try_from(*checksum).ok()?,
            translate: u8::try_from(*translate).ok()?,
        })
    }

    /// The list as RPL source, e.g. `{ 9600 0 0 0 3 1 }`.
    pub fn to_rpl(&self) -> String {
        format!(
            "{{ {} {} {} {} {} {} }}",
            self.baud,
            self.parity,
            u8::from(self.receive_pacing),
            u8::from(self.transmit_pacing),
            self.checksum,
            self.translate
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::charset::decode;

    macro_rules! fixture {
        ($model:literal, $name:literal) => {
            decode(include_bytes!(concat!(
                "../fixtures/",
                $model,
                "-",
                $name,
                ".txt"
            )))
        };
    }

    macro_rules! all_models {
        ($name:literal) => {
            [
                ("48sx", fixture!("48sx", $name)),
                ("48gx", fixture!("48gx", $name)),
                ("49g", fixture!("49g", $name)),
            ]
        };
    }

    fn entry<'a>(l: &'a Listing, name: &str) -> &'a Entry {
        l.entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("no entry {name}"))
    }

    #[test]
    fn dir() {
        for (model, text) in all_models!("dir") {
            let l = parse_listing(&text).unwrap();
            if model == "48sx" {
                assert_eq!(l.path, None);
                assert_eq!(l.free, None);
            } else {
                assert_eq!(l.path, Some(vec!["HOME".to_string()]), "{model}");
                assert!(l.free.unwrap() > 0.0, "{model}");
            }
            let d1 = entry(&l, "D1");
            assert!(d1.is_directory(), "{model}");
            let g = entry(&l, "G");
            assert_eq!(
                (g.size, g.kind.as_str(), g.checksum),
                (1103.5, "Graphic", 3894)
            );
            let r = entry(&l, "R");
            assert_eq!(
                (r.size, r.kind.as_str(), r.checksum),
                (16.0, "Real Number", 52870)
            );
            let b = entry(&l, "B");
            assert_eq!(b.kind, "Binary Integer");
            let iopar = entry(&l, "IOPAR");
            assert_eq!((iopar.size, iopar.kind.as_str()), (29.5, "List"));
            assert!(!iopar.is_directory());
        }
        let l = parse_listing(&fixture!("48gx", "dir")).unwrap();
        assert_eq!(l.free, Some(126562.0));
        assert_eq!(l.entries.len(), 11);
        assert_eq!(entry(&l, "D1").checksum, 51467);
        let l = parse_listing(&fixture!("49g", "dir")).unwrap();
        assert_eq!(l.free, Some(244692.0));
        assert_eq!(l.entries.len(), 12);
        assert_eq!(entry(&l, "IOPAR").checksum, 10777);
        assert_eq!(entry(&l, "D1").checksum, 65022);
        assert!(entry(&l, "CASDIR").is_directory());
        assert_eq!(entry(&l, "C").size, 24.0);
    }

    #[test]
    fn dir_sub() {
        for (model, text) in all_models!("dir-sub") {
            let l = parse_listing(&text).unwrap();
            assert_eq!(l.entries.len(), 1, "{model}");
            assert_eq!(l.entries[0].name, "Z");
            if model != "48sx" {
                assert_eq!(l.path, Some(vec!["HOME".to_string(), "D1".to_string()]));
            }
        }
        let l = parse_listing(&fixture!("49g", "dir-sub")).unwrap();
        assert_eq!(
            l.entries[0],
            Entry {
                name: "Z".into(),
                size: 11.5,
                kind: "Integer".into(),
                checksum: 25035
            }
        );
        assert_eq!(l.free, Some(244679.0));
    }

    #[test]
    fn path() {
        for (model, text) in all_models!("path") {
            let s = parse_stack(&text);
            assert_eq!(
                parse_list(s.level(1).unwrap()).unwrap(),
                ["HOME"],
                "{model}"
            );
        }
        for (model, text) in all_models!("path-sub") {
            let s = parse_stack(&text);
            assert_eq!(
                parse_list(s.level(1).unwrap()).unwrap(),
                ["HOME", "D1"],
                "{model}"
            );
        }
    }

    #[test]
    fn mem() {
        for (model, text) in all_models!("mem") {
            let s = parse_stack(&text);
            assert!(parse_real(s.level(1).unwrap()).unwrap() > 0.0, "{model}");
        }
        let s = parse_stack(&fixture!("48sx", "mem"));
        assert_eq!(parse_real(s.level(1).unwrap()), Some(28859.5));
    }

    #[test]
    fn version() {
        let s = parse_stack(&fixture!("48sx", "version"));
        assert_eq!(s.levels.len(), 1);
        assert_eq!(parse_name(s.level(1).unwrap()).as_deref(), Some("VERSION"));

        let s = parse_stack(&fixture!("48gx", "version"));
        assert_eq!(s.levels.len(), 2);
        assert_eq!(
            parse_string(s.level(2).unwrap()).as_deref(),
            Some("Version HP48-R")
        );
        assert_eq!(
            parse_string(s.level(1).unwrap()).as_deref(),
            Some("Copyright HP 1993")
        );

        // The 49G cuts the two-line version string at the display width.
        let s = parse_stack(&fixture!("49g", "version"));
        assert_eq!(s.levels.len(), 2);
        assert_eq!(
            parse_string(s.level(2).unwrap()).as_deref(),
            Some("Version HP48-C\nRevisi")
        );
        assert_eq!(
            parse_string(s.level(1).unwrap()).as_deref(),
            Some("Copyright HP 2009")
        );
    }

    #[test]
    fn iopar() {
        for (model, text) in all_models!("iopar") {
            let s = parse_stack(&text);
            let iopar = Iopar::parse(s.level(1).unwrap());
            assert_eq!(iopar, Some(Iopar::default()), "{model}");
        }
        let d = Iopar::default();
        assert_eq!(d.to_rpl(), "{ 9600 0 0 0 3 1 }");
        let other = Iopar {
            baud: 2400,
            parity: -2,
            receive_pacing: true,
            transmit_pacing: false,
            checksum: 1,
            translate: 3,
        };
        assert_eq!(Iopar::parse(&other.to_rpl()), Some(other));
        assert_eq!(Iopar::parse("{ 9600 0 0 0 3 }"), None);
        assert_eq!(Iopar::parse("{ 9600 0 0 0 3 1.5 }"), None);
        assert_eq!(Iopar::parse("{ 9600 0 0 0 300 1 }"), None);
    }

    #[test]
    fn vars() {
        for model in ["48sx", "48gx"] {
            let text = if model == "48sx" {
                fixture!("48sx", "vars")
            } else {
                fixture!("48gx", "vars")
            };
            let items = parse_list(parse_stack(&text).level(1).unwrap()).unwrap();
            assert!(items.iter().any(|i| i == "D1"), "{model}");
            assert!(items.iter().any(|i| i == "IOPAR"), "{model}");
        }
        // The 49G truncates the list at the display width: no closing brace.
        let s = parse_stack(&fixture!("49g", "vars"));
        assert_eq!(s.level(1), Some("{D1,G,TG,B,C,A,P,L,S,R"));
        assert_eq!(parse_list(s.level(1).unwrap()), None);
    }

    #[test]
    fn error() {
        for model in ["48sx", "48gx"] {
            let text = if model == "48sx" {
                fixture!("48sx", "error")
            } else {
                fixture!("48gx", "error")
            };
            let s = parse_stack(&text);
            assert_eq!(s.error.as_deref(), Some("Infinite Result"), "{model}");
            assert_eq!(s.levels, ["0", "1"]);
        }
        // 1/0 is not an error on the 49G: it returns infinity.
        let s = parse_stack(&fixture!("49g", "error"));
        assert_eq!(s.error, None);
        assert_eq!(s.level(1), Some("∞"));
    }

    #[test]
    fn error_undefined() {
        for (model, text) in all_models!("error-undefined") {
            let s = parse_stack(&text);
            assert_eq!(s.error.as_deref(), Some("Undefined Name"), "{model}");
            assert_eq!(s.levels.len(), 1);
            if model == "49g" {
                // Shown unquoted in algebraic mode.
                assert_eq!(s.level(1), Some("NOSUCH"));
                assert_eq!(parse_name(s.level(1).unwrap()), None);
            } else {
                assert_eq!(parse_name(s.level(1).unwrap()).as_deref(), Some("NOSUCH"));
            }
        }
    }

    #[test]
    fn error_syntax() {
        for (model, text) in all_models!("error-syntax") {
            let s = parse_stack(&text);
            assert_eq!(s.error.as_deref(), Some("Invalid Syntax"), "{model}");
            assert_eq!(s.levels.len(), 2);
            assert_eq!(
                parse_string(s.level(1).unwrap()).as_deref(),
                Some("CLEAR \\->LIST")
            );
        }
    }

    #[test]
    fn stack() {
        for (model, text) in all_models!("stack") {
            let s = parse_stack(&text);
            assert_eq!(s.error, None);
            assert_eq!(s.levels.len(), 3, "{model}");
            assert_eq!(s.level(3), Some("\"a\nb\""));
            assert_eq!(s.level(2), Some("1.5"));
            assert_eq!(parse_list(s.level(1).unwrap()).unwrap(), ["1", "2"]);
            assert_eq!(s.level(0), None);
            assert_eq!(s.level(4), None);
        }
        let s = parse_stack(&fixture!("49g", "stack"));
        assert_eq!(s.level(1), Some("{1,2}"));
    }

    #[test]
    fn empty() {
        for (model, text) in all_models!("empty") {
            let s = parse_stack(&text);
            assert!(s.is_empty(), "{model}");
            assert_eq!(s.error, None);
        }
    }

    #[test]
    fn flag() {
        for (model, text) in all_models!("flag") {
            let s = parse_stack(&text);
            assert_eq!(parse_real(s.level(1).unwrap()), Some(1.0), "{model}");
        }
    }

    #[test]
    fn stack_edge_cases() {
        let s = parse_stack("Error: X\r\nEmpty Stack");
        assert_eq!(s.error.as_deref(), Some("X"));
        assert!(s.is_empty());

        // "2:" inside a continuation line is not the expected next level.
        let s = parse_stack("2:  5\r\n1:  \"x\r\n2: y\"\r\n");
        assert_eq!(s.levels, ["\"x\n2: y\"", "5"]);

        // Only the next lower level number starts a new level.
        let s = parse_stack("2: \"a\r\n3: b\"\r\n1: 7");
        assert_eq!(s.levels, ["7", "\"a\n3: b\""]);

        // Trailing spaces inside a value are kept.
        let s = parse_stack("1:   \"a \r\n b \"\r\n");
        assert_eq!(s.level(1), Some("\"a \n b \""));

        // Junk before the first level is ignored, never an error.
        let s = parse_stack("garbage\r\n1: 3");
        assert_eq!(s.levels, ["3"]);
        assert_eq!(parse_stack(""), StackReply::default());
        assert_eq!(parse_stack("noise"), StackReply::default());
    }

    #[test]
    fn listing_edge_cases() {
        let l = parse_listing("").unwrap();
        assert_eq!(l.entries, []);
        assert_eq!(l.path, None);
        assert!(parse_listing("\r\n\r\n").unwrap().entries.is_empty());
        assert!(matches!(parse_listing("X 16 Real"), Err(Error::Reply(_))));
        assert!(matches!(
            parse_listing("X big Real Number 12"),
            Err(Error::Reply(_))
        ));
        assert!(matches!(
            parse_listing("X 16 Real Number 70000"),
            Err(Error::Reply(_))
        ));
        assert!(matches!(
            parse_listing("X 16 Real Number 12.5"),
            Err(Error::Reply(_))
        ));
        assert!(matches!(parse_listing("{ HOME 12"), Err(Error::Reply(_))));
        let l = parse_listing("{ HOME }\r\n").unwrap();
        assert_eq!((l.path.unwrap().len(), l.free), (1, None));
    }

    #[test]
    fn scalars() {
        for (s, v) in [
            ("127828.", 127828.0),
            ("1.5", 1.5),
            ("-3", -3.0),
            ("30001", 30001.0),
            (" 245966.5 ", 245966.5),
            ("1.E-3", 0.001),
            ("-1.5E12", -1.5e12),
        ] {
            assert_eq!(parse_real(s), Some(v), "{s}");
        }
        for s in ["", "inf", "NaN", "1,5", "abc", "."] {
            assert_eq!(parse_real(s), None, "{s}");
        }

        assert_eq!(parse_list("{ }").unwrap(), Vec::<String>::new());
        assert_eq!(parse_list("{}").unwrap(), Vec::<String>::new());
        assert_eq!(parse_list("{1;2}").unwrap(), ["1", "2"]);
        assert_eq!(parse_list("1 2"), None);
        assert_eq!(parse_list("{ { 1 } }"), None);
        assert_eq!(parse_list("{ \"a\" }"), None);

        assert_eq!(parse_string("\"\"").as_deref(), Some(""));
        assert_eq!(parse_string("\"abc").as_deref(), Some("abc"));
        assert_eq!(parse_string("abc"), None);

        assert_eq!(parse_name("'X1'").as_deref(), Some("X1"));
        assert_eq!(parse_name("'→A'").as_deref(), Some("→A"));
        assert_eq!(parse_name("NOSUCH"), None);
        assert_eq!(parse_name("''"), None);
        assert_eq!(parse_name("'X+1'"), None);
        assert_eq!(parse_name("'1X'"), None);
    }
}
