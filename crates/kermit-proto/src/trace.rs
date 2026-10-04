//! Text trace format: one line per chunk, `> ` for bytes sent, `< ` for bytes
//! received; `#` lines and blank lines are comments.

use std::fmt::Write as _;

/// Direction of a traced chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Bytes we sent (`> `).
    Out,
    /// Bytes we received (`< `).
    In,
}

/// Escape bytes for one trace line.
pub fn escape(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len());
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\\' => s.push_str("\\\\"),
            b'\r' => s.push_str("\\r"),
            b'\n' => s.push_str("\\n"),
            b' ' if i + 1 < bytes.len() => s.push(' '),
            0x21..=0x7E => s.push(char::from(b)),
            _ => {
                let _ = write!(s, "\\x{b:02x}");
            }
        }
    }
    s
}

/// Inverse of [`escape`].
pub fn unescape(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'\\' {
            if !(0x20..=0x7E).contains(&b) {
                return Err(format!("unexpected byte 0x{b:02x} at {i}"));
            }
            out.push(b);
            i += 1;
            continue;
        }
        match bytes.get(i + 1) {
            Some(b'\\') => out.push(b'\\'),
            Some(b'r') => out.push(b'\r'),
            Some(b'n') => out.push(b'\n'),
            Some(b'x') => {
                let hex = s
                    .get(i + 2..i + 4)
                    .ok_or_else(|| format!("truncated \\x escape at {i}"))?;
                let v = u8::from_str_radix(hex, 16)
                    .map_err(|_| format!("bad \\x escape {hex:?} at {i}"))?;
                out.push(v);
                i += 2;
            }
            _ => return Err(format!("bad escape at {i}")),
        }
        i += 2;
    }
    Ok(out)
}

/// Parse a whole trace into (direction, bytes) chunks.
pub fn parse(text: &str) -> Result<Vec<(Direction, Vec<u8>)>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.trim_start().is_empty() || line.starts_with('#') {
            continue;
        }
        let (dir, rest) = if let Some(r) = line.strip_prefix('>') {
            (Direction::Out, r)
        } else if let Some(r) = line.strip_prefix('<') {
            (Direction::In, r)
        } else {
            return Err(format!("line {}: expected '>' or '<'", n + 1));
        };
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        let bytes = unescape(rest).map_err(|e| format!("line {}: {e}", n + 1))?;
        out.push((dir, bytes));
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn escape_round_trip_all_bytes() {
        let all: Vec<u8> = (0..=255u8).collect();
        let e = escape(&all);
        assert!(e.is_ascii());
        assert_eq!(unescape(&e).unwrap(), all);
        assert_eq!(escape(b"\x01# N3\r"), "\\x01# N3\\r");
        assert_eq!(escape(b"a\\b\n\xff"), "a\\\\b\\n\\xff");
    }

    #[test]
    fn trailing_space_escaped() {
        assert_eq!(escape(b"a b "), "a b\\x20");
        assert_eq!(escape(b" "), "\\x20");
        assert_eq!(unescape("a b\\x20").unwrap(), b"a b ");
    }

    #[test]
    fn parse_with_comments() {
        let text = "# header\n\n> \\x01# N3\\r\n   \n< \\x01#!Y?\\r\r\n# end\n>\n";
        let got = parse(text).unwrap();
        assert_eq!(
            got,
            vec![
                (Direction::Out, b"\x01# N3\r".to_vec()),
                (Direction::In, b"\x01#!Y?\r".to_vec()),
                (Direction::Out, vec![]),
            ]
        );
    }

    #[test]
    fn errors() {
        assert!(unescape("\\q").is_err());
        assert!(unescape("\\").is_err());
        assert!(unescape("\\x4").is_err());
        assert!(unescape("\\xzz").is_err());
        assert!(unescape("é").is_err());
        assert!(parse("> ok\n? nope\n").is_err());
        assert!(parse("> \\x0g\n").is_err());
    }
}
