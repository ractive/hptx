//! The HP 48/49 character set and the ASCII trigraphs of the transfer
//! translation modes.
//!
//! Bytes 0-127 are ASCII, 160-255 are ISO 8859-1, and 128-159 are HP's own
//! math and Greek characters. Names, file data and server text cross the
//! `kermit-proto` boundary as raw bytes in this character set; this module
//! translates them to and from UTF-8.
//!
//! In ASCII transfers with translation mode 2 or 3 (`TRANSIO`) the calculator
//! writes characters 128-255 as backslash trigraphs: `\->` for `→`, `\<<` for
//! `«`, `\160` for a no-break space. The table below was checked against a
//! 48SX GET with `T(3)` of a string holding every character from 128 to 255
//! (2026-10-05). The calculator does not read trigraphs in a host command
//! (`C` packet) itself: `\->LIST` there is a syntax error, so
//! [`encode_command`] translates them before sending.

use crate::{Error, Result};

/// Characters 128-159: (Unicode text, trigraph).
const HIGH: [(&str, &str); 32] = [
    ("∡", "\\<)"),
    ("x\u{0304}", "\\x-"),
    ("∇", "\\.V"),
    ("√", "\\v/"),
    ("∫", "\\.S"),
    ("Σ", "\\GS"),
    ("▶", "\\|>"),
    ("π", "\\pi"),
    ("∂", "\\.d"),
    ("≤", "\\<="),
    ("≥", "\\>="),
    ("≠", "\\=/"),
    ("α", "\\Ga"),
    ("→", "\\->"),
    ("←", "\\<-"),
    ("↓", "\\|v"),
    ("↑", "\\|^"),
    ("γ", "\\Gg"),
    ("δ", "\\Gd"),
    ("ε", "\\Ge"),
    ("η", "\\Gn"),
    ("θ", "\\Gh"),
    ("λ", "\\Gl"),
    ("ρ", "\\Gr"),
    ("σ", "\\Gs"),
    ("τ", "\\Gt"),
    ("ω", "\\Gw"),
    ("Δ", "\\GD"),
    ("Π", "\\PI"),
    ("Ω", "\\GW"),
    ("■", "\\[]"),
    ("∞", "\\oo"),
];

/// Characters 160-255 that have a mnemonic trigraph; the rest are `\nnn`.
const LATIN_TRIGRAPHS: [(u8, &str); 8] = [
    (171, "\\<<"),
    (176, "\\^o"),
    (181, "\\Gm"),
    (187, "\\>>"),
    (215, "\\.x"),
    (216, "\\O/"),
    (223, "\\Gb"),
    (247, "\\:-"),
];

/// Translate HP bytes to a string. Never fails: every byte has a meaning.
pub fn decode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            0x80..=0x9F => s.push_str(HIGH[usize::from(b - 0x80)].0),
            _ => s.push(char::from(b)),
        }
    }
    s
}

/// Translate a string to HP bytes. `x̄` is `x` followed by U+0304
/// (combining macron); everything else is one character per byte.
pub fn encode(text: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == 'x' && chars.peek() == Some(&'\u{0304}') {
            chars.next();
            out.push(0x81);
            continue;
        }
        out.push(encode_char(c)?);
    }
    Ok(out)
}

fn encode_char(c: char) -> Result<u8> {
    let code = u32::from(c);
    if code < 0x80 || (0xA0..=0xFF).contains(&code) {
        return u8::try_from(code).map_err(|_| Error::Charset(c));
    }
    let mut buf = [0u8; 4];
    let s: &str = c.encode_utf8(&mut buf);
    HIGH.iter()
        .position(|(text, _)| *text == s)
        .and_then(|i| u8::try_from(0x80 + i).ok())
        .ok_or(Error::Charset(c))
}

/// Like [`encode`], but also translates the ASCII trigraphs (`\->`, `\<<`,
/// `\GS`, `\160`, ...) to their bytes. A backslash that starts no known
/// trigraph is kept as is. Use it for host command text typed in ASCII.
pub fn encode_command(text: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('\\') {
        out.extend(encode(&rest[..pos])?);
        let tail = &rest[pos..];
        match trigraph_at(tail) {
            Some((byte, len)) => {
                out.push(byte);
                rest = &tail[len..];
            }
            None => {
                out.push(b'\\');
                rest = &tail[1..];
            }
        }
    }
    out.extend(encode(rest)?);
    Ok(out)
}

/// The byte and length of the trigraph at the start of `s` (which starts
/// with a backslash), if any.
pub fn trigraph_at(s: &str) -> Option<(u8, usize)> {
    // Mnemonics are a backslash and two characters; numeric codes `\nnn`.
    if let Some(head) = s.get(..3) {
        if let Some(i) = HIGH.iter().position(|(_, t)| *t == head) {
            return u8::try_from(0x80 + i).ok().map(|b| (b, 3));
        }
        if let Some((b, _)) = LATIN_TRIGRAPHS.iter().find(|(_, t)| *t == head) {
            return Some((*b, 3));
        }
    }
    let digits = s.get(1..4)?;
    if digits.bytes().all(|d| d.is_ascii_digit()) {
        let n: u16 = digits.parse().ok()?;
        if (128..=255).contains(&n) {
            return u8::try_from(n).ok().map(|b| (b, 4));
        }
    }
    None
}

/// A character with a mnemonic trigraph: its code, its Unicode text and the
/// ASCII trigraph that types it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedChar {
    /// The byte in the HP character set (128-255).
    pub code: u8,
    /// The character as Unicode text (`x̄` is two code points).
    pub text: String,
    /// The trigraph, e.g. `\->`.
    pub trigraph: &'static str,
}

/// Every character with a mnemonic trigraph (all of 128-159 and some of
/// 160-255), by code. The other codes 160-255 are typed as `\nnn`, three
/// decimal digits.
pub fn named_chars() -> Vec<NamedChar> {
    let high = (0x80u8..)
        .zip(HIGH.iter())
        .map(|(code, (text, trigraph))| NamedChar {
            code,
            text: (*text).to_string(),
            trigraph,
        });
    let latin = LATIN_TRIGRAPHS.iter().map(|&(code, trigraph)| NamedChar {
        code,
        text: char::from(code).to_string(),
        trigraph,
    });
    high.chain(latin).collect()
}

/// HP bytes to 7-bit ASCII the way the calculator writes them with
/// translation mode 3: bytes 128-255 become trigraphs, the rest is unchanged.
pub fn to_trigraphs(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            0x80..=0x9F => s.push_str(HIGH[usize::from(b - 0x80)].1),
            0xA0..=0xFF => match LATIN_TRIGRAPHS.iter().find(|(l, _)| *l == b) {
                Some((_, t)) => s.push_str(t),
                None => s.push_str(&format!("\\{b}")),
            },
            _ => s.push(char::from(b)),
        }
    }
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// What a 48SX sends for `"" 128 255 FOR I I CHR + NEXT` with
    /// `IOPAR` translation 3 (recorded 2026-10-05).
    const SX_T3: &str = concat!(
        "\\<)\\x-\\.V\\v/\\.S\\GS\\|>\\pi\\.d\\<=\\>=\\=/\\Ga\\->\\<-\\|v\\|^\\Gg\\Gd",
        "\\Ge\\Gn\\Gh\\Gl\\Gr\\Gs\\Gt\\Gw\\GD\\PI\\GW\\[]\\oo\\160\\161\\162\\163",
        "\\164\\165\\166\\167\\168\\169\\170\\<<\\172\\173\\174\\175\\^o\\177\\178",
        "\\179\\180\\Gm\\182\\183\\184\\185\\186\\>>\\188\\189\\190\\191\\192\\193",
        "\\194\\195\\196\\197\\198\\199\\200\\201\\202\\203\\204\\205\\206\\207",
        "\\208\\209\\210\\211\\212\\213\\214\\.x\\O/\\217\\218\\219\\220\\221\\222",
        "\\Gb\\224\\225\\226\\227\\228\\229\\230\\231\\232\\233\\234\\235\\236",
        "\\237\\238\\239\\240\\241\\242\\243\\244\\245\\246\\:-\\248\\249\\250",
        "\\251\\252\\253\\254\\255",
    );

    #[test]
    fn trigraphs_match_the_calculator() {
        let high: Vec<u8> = (128..=255).collect();
        assert_eq!(to_trigraphs(&high), SX_T3);
        assert_eq!(encode_command(SX_T3).unwrap(), high);
    }

    #[test]
    fn every_byte_round_trips_through_utf8() {
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(encode(&decode(&all)).unwrap(), all);
    }

    #[test]
    fn decode_known_characters() {
        assert_eq!(decode(b"\x8dSTR"), "→STR");
        assert_eq!(decode(b"'\x9f'"), "'∞'");
        assert_eq!(decode(b"\xab 1 \xbb"), "« 1 »");
        assert_eq!(decode(b"\x81"), "x̄");
    }

    #[test]
    fn encode_rejects_unknown() {
        assert!(matches!(encode("€"), Err(Error::Charset('€'))));
        assert_eq!(encode("x").unwrap(), b"x");
        assert_eq!(encode("x\u{0304}y").unwrap(), b"\x81y");
    }

    #[test]
    fn named_chars_cover_both_tables_and_round_trip() {
        let named = named_chars();
        assert_eq!(named.len(), HIGH.len() + LATIN_TRIGRAPHS.len());
        assert!(named.windows(2).all(|w| w[0].code < w[1].code));
        for c in &named {
            assert_eq!(
                encode_command(c.trigraph).unwrap(),
                [c.code],
                "{}",
                c.trigraph
            );
            assert_eq!(encode(&c.text).unwrap(), [c.code], "{}", c.text);
            assert_eq!(decode(&[c.code]), c.text);
            assert_eq!(to_trigraphs(&[c.code]), c.trigraph);
        }
        assert_eq!(named[3].trigraph, "\\v/");
        assert_eq!(named[3].text, "√");
    }

    #[test]
    fn command_trigraphs() {
        assert_eq!(
            encode_command("{ 1 2 } \\->STR").unwrap(),
            b"{ 1 2 } \x8dSTR"
        );
        assert_eq!(encode_command("→STR").unwrap(), b"\x8dSTR");
        assert_eq!(encode_command("\\<< 1 \\>>").unwrap(), b"\xab 1 \xbb");
        // Not a trigraph: kept.
        assert_eq!(encode_command("\"a\\b\" \\1").unwrap(), b"\"a\\b\" \\1");
        assert_eq!(encode_command("\\127 \\256").unwrap(), b"\\127 \\256");
        assert_eq!(encode_command("\\").unwrap(), b"\\");
    }
}
