//! Binary <-> ASCII conversion for the objects that have a fixed text form
//! and need no RPL decompiler: Real Number, Complex Number, String, Binary
//! Integer, Integer (49G), Graphic (GROB) and lists of these. Anything else
//! (programs, algebraics, names, ROM pointers such as the built-in small
//! numbers the 48 puts in lists) is refused with [`ConvertError::Unsupported`]:
//! the calculator's own `get --ascii` decompiles those.
//!
//! The text forms were checked against what the emulated 48SX and 49G write
//! for `get --ascii` (2026-10-05):
//! - reals: `1.5`, `.0001`, `1.E12`, `-1.23E-15`; the 48SX writes `100`
//!   where the 49G writes `100.` (on the 49G `100` is an exact Integer);
//! - binary integers in the current base: `# 255d` (48SX), `# FFh` (49G);
//! - strings: the 49G escapes `\"` and `\\`; the 48SX writes a string
//!   holding `"` as `C$ n <n characters>`; `T(2)` and `T(3)` double every
//!   backslash on top of that (transfer layer), so in `T(3)` text a 49G
//!   string's backslash is four backslashes (checked by a 49G round trip);
//! - a LF in a string goes out as CR LF with `T(1)` or higher;
//! - GROBs: `GROB w h <body nibbles in memory order, as hex>`.
//!
//! Written text always uses `%%HP: T(3)A(D)F(.);`: pure ASCII, characters
//! 128-255 as trigraphs, reals with a decimal point so the 49G reads them
//! back as reals.

use std::fmt::Write as _;

use hptx_core::charset;
use hptx_core::object::{
    self, AsciiHeader, BinaryHeader, Family, HEADER_LEN, ObjectType, read_field, unpack,
};

/// Composite end marker.
const SEMI: u32 = 0x0312B;
/// Deepest list nesting converted.
const MAX_DEPTH: usize = 64;

/// Why a conversion did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvertError {
    /// A well-formed object hptx cannot convert (the text names what).
    Unsupported(String),
    /// Malformed input.
    Invalid(String),
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertError::Unsupported(m) | ConvertError::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ConvertError {}

type Result<T> = std::result::Result<T, ConvertError>;

fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(ConvertError::Invalid(msg.into()))
}

/// The types [`to_ascii`] and [`to_binary`] handle, for help and messages.
pub const SUPPORTED: &str = "Real Number, Complex Number, String, Binary Integer, \
                             Integer (49G), Graphic (GROB) and lists of these";

/// The header line hptx writes.
pub fn ascii_header() -> AsciiHeader {
    AsciiHeader {
        translate: 3,
        angle: b'D',
        fraction: b'.',
    }
}

/// The binary header hptx writes for `family` (ROM letters as the emulated
/// 48GX and 49G write them).
pub fn binary_header(family: Family) -> BinaryHeader {
    BinaryHeader {
        family,
        rom: match family {
            Family::Hp48 => b'R',
            Family::Hp49 => b'C',
        },
    }
}

// ---------------------------------------------------------------- binary -> ASCII

/// Converts a binary transfer file (`HPHP48-x` / `HPHP49-x`) to `%%HP:` text.
/// Bytes after the walked object (Kermit padding) are ignored.
pub fn to_ascii(data: &[u8]) -> Result<Vec<u8>> {
    let header = BinaryHeader::parse(data)
        .ok_or_else(|| ConvertError::Invalid("no HPHP48-x / HPHP49-x header".into()))?;
    let nibbles = unpack(&data[HEADER_LEN..]);
    object::object_size(&nibbles, 0).map_err(|e| ConvertError::Invalid(e.to_string()))?;
    let mut text = String::new();
    decompile(&nibbles, 0, header.family, 0, &mut text)?;
    let mut out = ascii_header().to_line().into_bytes();
    out.extend_from_slice(b"\r\n");
    out.extend(text.into_bytes());
    out.extend_from_slice(b"\r\n");
    Ok(out)
}

/// Appends the text of the object at `at`; returns its size in nibbles.
fn decompile(
    nib: &[u8],
    at: usize,
    family: Family,
    depth: usize,
    out: &mut String,
) -> Result<usize> {
    let field = |off: usize, width: usize| -> Result<u32> {
        at.checked_add(off)
            .and_then(|p| read_field(nib, p, width))
            .ok_or_else(|| ConvertError::Invalid(format!("truncated object at nibble {at}")))
    };
    let prolog = field(0, 5)?;
    let Some(ty) = ObjectType::from_prolog(prolog) else {
        return Err(ConvertError::Unsupported(format!(
            "a ROM pointer #{prolog:05X} (a built-in object such as a small number or a \
             command, which only the calculator can name)"
        )));
    };
    let body = at + 5;
    match ty {
        ObjectType::Real => {
            out.push_str(&real_text(slice(nib, body, 16)?)?);
            Ok(21)
        }
        ObjectType::Complex => {
            let re = real_text(slice(nib, body, 16)?)?;
            let im = real_text(slice(nib, body + 16, 16)?)?;
            let _ = write!(out, "({re},{im})");
            Ok(37)
        }
        ObjectType::BinaryInteger => {
            let len = field(5, 5)? as usize;
            if len != 21 {
                return Err(ConvertError::Unsupported(format!(
                    "a Binary Integer with {} nibbles (only 16 have a text form)",
                    len.saturating_sub(5)
                )));
            }
            let lo = u64::from(field(10, 8)?);
            let hi = u64::from(field(18, 8)?);
            let _ = write!(out, "# {:X}h", hi << 32 | lo);
            Ok(26)
        }
        ObjectType::String => {
            let len = field(5, 5)? as usize;
            let bytes = object::pack(slice(nib, body + 5, len.saturating_sub(5))?);
            out.push_str(&string_text(&bytes, family)?);
            Ok(5 + len)
        }
        ObjectType::Integer => {
            let len = field(5, 5)? as usize;
            let digits = slice(nib, body + 5, len.saturating_sub(5))?;
            out.push_str(&integer_text(digits)?);
            Ok(5 + len)
        }
        ObjectType::Graphic => {
            let (len, height, width) = (field(5, 5)?, field(10, 5)?, field(15, 5)?);
            let rows = slice(nib, at + 20, (len as usize).saturating_sub(15))?;
            let _ = write!(out, "GROB {width} {height} ");
            for &n in rows {
                out.push(
                    char::from_digit(u32::from(n), 16).map_or('0', |c| c.to_ascii_uppercase()),
                );
            }
            Ok(5 + len as usize)
        }
        ObjectType::List => {
            if depth >= MAX_DEPTH {
                return invalid(format!("lists nested deeper than {MAX_DEPTH} levels"));
            }
            out.push('{');
            let mut pos = body;
            loop {
                let p = read_field(nib, pos, 5)
                    .ok_or_else(|| ConvertError::Invalid("list without its end".into()))?;
                if p == SEMI {
                    break;
                }
                out.push(' ');
                pos += decompile(nib, pos, family, depth + 1, out)?;
            }
            out.push_str(" }");
            Ok(pos + 5 - at)
        }
        other => Err(ConvertError::Unsupported(format!("a {}", other.name()))),
    }
}

fn slice(nib: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    at.checked_add(len)
        .and_then(|end| nib.get(at..end))
        .ok_or_else(|| ConvertError::Invalid(format!("truncated object at nibble {at}")))
}

fn bcd(n: u8) -> Result<char> {
    if n > 9 {
        return invalid(format!("#{n:X} is not a decimal digit"));
    }
    Ok(char::from(b'0' + n))
}

/// The 16 nibbles of a real (3 exponent, 12 mantissa, 1 sign; each low
/// nibble first) in the calculator's STD form, always with a decimal point.
fn real_text(n: &[u8]) -> Result<String> {
    let exp_bcd = n[2..3]
        .iter()
        .chain(&n[1..2])
        .chain(&n[0..1])
        .map(|&d| bcd(d))
        .collect::<Result<String>>()?;
    let exp_raw: i32 = exp_bcd.parse().unwrap_or(0);
    let exp = if exp_raw >= 500 {
        exp_raw - 1000
    } else {
        exp_raw
    };
    let digits: String = n[3..15]
        .iter()
        .rev()
        .map(|&d| bcd(d))
        .collect::<Result<_>>()?;
    let negative = match n[15] {
        0 => false,
        9 => true,
        s => return invalid(format!("real sign nibble #{s:X}")),
    };
    let digits = digits.trim_end_matches('0');
    if digits.is_empty() {
        return Ok("0.".into());
    }
    let sig = digits.len() as i32;
    let mut s = String::from(if negative { "-" } else { "" });
    if (0..12).contains(&exp) {
        let int_len = (exp + 1) as usize;
        if digits.len() <= int_len {
            s.push_str(digits);
            s.push_str(&"0".repeat(int_len - digits.len()));
            s.push('.');
        } else {
            s.push_str(&digits[..int_len]);
            s.push('.');
            s.push_str(&digits[int_len..]);
        }
    } else if exp < 0 && (-exp - 1) + sig <= 12 {
        s.push('.');
        s.push_str(&"0".repeat((-exp - 1) as usize));
        s.push_str(digits);
    } else {
        s.push_str(&digits[..1]);
        s.push('.');
        s.push_str(&digits[1..]);
        let _ = write!(s, "E{exp}");
    }
    Ok(s)
}

/// 49G Integer body: BCD digits, least significant first, then the sign
/// nibble (0 or 9); zero is the single nibble 0.
fn integer_text(body: &[u8]) -> Result<String> {
    match body {
        [] => invalid("empty Integer"),
        [0] => Ok("0".into()),
        [digits @ .., sign] => {
            let negative = match sign {
                0 => false,
                9 => true,
                s => return invalid(format!("Integer sign nibble #{s:X}")),
            };
            let text: String = digits
                .iter()
                .rev()
                .map(|&d| bcd(d))
                .collect::<Result<_>>()?;
            let text = text.trim_start_matches('0');
            let text = if text.is_empty() { "0" } else { text };
            Ok(format!("{}{text}", if negative { "-" } else { "" }))
        }
    }
}

/// A string in `T(3)` text. Two layers, as the calculator reads it: the
/// transfer translation (`\\` for a backslash, trigraphs for 128-255) over
/// the string syntax. The 49G's string syntax escapes `"` and `\` with a
/// backslash, so its backslash ends up as four; the 48 has no escapes and
/// writes a string holding `"` as `C$ n chars`. A LF stays a bare LF; a CR
/// does not survive the CR LF translation reliably and is refused.
fn string_text(bytes: &[u8], family: Family) -> Result<String> {
    if bytes.contains(&b'\r') {
        return Err(ConvertError::Unsupported(
            "a String holding a carriage return (ASCII transfers translate CR LF)".into(),
        ));
    }
    if family == Family::Hp48 {
        let body = transfer_encode(bytes);
        return Ok(if bytes.contains(&b'"') {
            format!("C$ {} {body}", bytes.len())
        } else {
            format!("\"{body}\"")
        });
    }
    let mut syntax = Vec::with_capacity(bytes.len());
    for &b in bytes {
        if matches!(b, b'\\' | b'"') {
            syntax.push(b'\\');
        }
        syntax.push(b);
    }
    Ok(format!("\"{}\"", transfer_encode(&syntax)))
}

/// The `T(3)` transfer translation: `\\` for a backslash, trigraphs for
/// 128-255.
fn transfer_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b == b'\\' {
            out.push_str("\\\\");
        } else {
            out.push_str(&charset::to_trigraphs(&[b]));
        }
    }
    out
}

/// Undoes the transfer translation of `T(t)`: CR LF to LF from 1 on, and
/// from 2 on `\\` to a backslash and trigraphs to their bytes (a backslash
/// that starts neither stays).
fn transfer_decode(text: &[u8], t: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while let Some(&c) = text.get(i) {
        if t >= 1 && c == b'\r' && text.get(i + 1) == Some(&b'\n') {
            out.push(b'\n');
            i += 2;
            continue;
        }
        if t >= 2 && c == b'\\' {
            if text.get(i + 1) == Some(&b'\\') {
                out.push(b'\\');
                i += 2;
                continue;
            }
            let head = &text[i..(i + 4).min(text.len())];
            let ascii_len = head.iter().take_while(|b| b.is_ascii()).count();
            if let Ok(tail) = std::str::from_utf8(&head[..ascii_len])
                && let Some((byte, len)) = charset::trigraph_at(tail)
            {
                out.push(byte);
                i += len;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

// ---------------------------------------------------------------- ASCII -> binary

/// What [`parse`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// The `%%HP:` header, if the text had one.
    pub header: Option<AsciiHeader>,
    /// The object's type.
    pub object_type: ObjectType,
    /// The compiled object in nibbles.
    pub nibbles: Vec<u8>,
}

/// Compiles `%%HP:` text holding one object into a binary transfer file
/// for `family`.
pub fn to_binary(text: &[u8], family: Family) -> Result<Vec<u8>> {
    let parsed = parse(text, family)?;
    let mut out = binary_header(family).to_bytes().to_vec();
    out.extend(object::pack(&parsed.nibbles));
    Ok(out)
}

/// Parses `%%HP:` text (or bare text, read as `T(3)A(D)F(.)`) holding one
/// object. `family` decides what a number without `.` or `E` is: a Real on
/// the 48, an exact Integer on the 49G (its default exact mode).
pub fn parse(text: &[u8], family: Family) -> Result<Parsed> {
    let (header, start) = match AsciiHeader::parse(text) {
        Some((h, len)) => (Some(h), len),
        None if text.starts_with(b"%%HP") => return invalid("malformed %%HP: header"),
        None => (None, 0),
    };
    let h = header.unwrap_or_else(ascii_header);
    let decoded = transfer_decode(&text[start..], h.translate);
    let mut p = TextParser {
        s: &decoded,
        pos: 0,
        decimal: h.fraction,
        family,
    };
    p.skip_ws();
    let mut nibbles = Vec::new();
    let object_type = p.object(&mut nibbles, 0)?;
    p.skip_ws();
    if p.pos < decoded.len() {
        return Err(p.unsupported_word("after the first object"));
    }
    Ok(Parsed {
        header,
        object_type,
        nibbles,
    })
}

fn push(out: &mut Vec<u8>, value: u64, width: usize) {
    out.extend((0..width).map(|i| (value >> (4 * i) & 0xF) as u8));
}

struct TextParser<'a> {
    s: &'a [u8],
    pos: usize,
    decimal: u8,
    family: Family,
}

impl TextParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }

    fn rest_starts_with(&self, lit: &[u8]) -> bool {
        self.s[self.pos..].starts_with(lit)
    }

    /// The next whitespace-delimited word, for messages.
    fn word(&self) -> String {
        let rest = &self.s[self.pos..];
        let end = rest
            .iter()
            .position(|b| b.is_ascii_whitespace())
            .unwrap_or(rest.len())
            .min(40);
        charset::decode(&rest[..end])
    }

    fn unsupported_word(&self, place: &str) -> ConvertError {
        ConvertError::Unsupported(format!(
            "`{}` {place} (only {SUPPORTED} have a text form hptx compiles)",
            self.word()
        ))
    }

    fn object(&mut self, out: &mut Vec<u8>, depth: usize) -> Result<ObjectType> {
        let Some(c) = self.peek() else {
            return invalid("no object in the text");
        };
        match c {
            b'"' => self.string(out),
            b'{' => self.list(out, depth),
            b'(' => self.complex(out),
            b'#' => self.binary_integer(out),
            _ if self.rest_starts_with(b"C$ ") => self.counted_string(out),
            _ if self.rest_starts_with(b"GROB ") => self.grob(out),
            b'-' | b'0'..=b'9' => self.number(out),
            _ if c == self.decimal => self.number(out),
            _ => Err(self.unsupported_word("is not a number, string, list or GROB")),
        }
    }

    fn list(&mut self, out: &mut Vec<u8>, depth: usize) -> Result<ObjectType> {
        if depth >= MAX_DEPTH {
            return invalid(format!("lists nested deeper than {MAX_DEPTH} levels"));
        }
        self.pos += 1;
        push(out, ObjectType::List.prolog().into(), 5);
        loop {
            self.skip_ws();
            match self.peek() {
                None => return invalid("list without `}`"),
                Some(b'}') => {
                    self.pos += 1;
                    push(out, SEMI.into(), 5);
                    return Ok(ObjectType::List);
                }
                Some(_) => {
                    self.object(out, depth + 1)?;
                    self.expect_delimiter()?;
                }
            }
        }
    }

    /// After an element: whitespace, `}` or end.
    fn expect_delimiter(&self) -> Result<()> {
        match self.peek() {
            None | Some(b' ' | b'\t' | b'\r' | b'\n' | b'}' | b'{' | b'"') => Ok(()),
            _ => Err(self.unsupported_word("is not a number, string, list or GROB")),
        }
    }

    /// One character of a quoted string at `pos`: the 49G's string syntax
    /// reads `\"` and `\\` as escapes, the 48's has none.
    fn string_char(&mut self) -> u8 {
        let c = self.s[self.pos];
        if c == b'\\'
            && self.family == Family::Hp49
            && let Some(&next @ (b'\\' | b'"')) = self.s.get(self.pos + 1)
        {
            self.pos += 2;
            return next;
        }
        self.pos += 1;
        c
    }

    fn string(&mut self, out: &mut Vec<u8>) -> Result<ObjectType> {
        self.pos += 1;
        let mut bytes = Vec::new();
        loop {
            match self.peek() {
                None => return invalid("string without its closing \""),
                Some(b'"') => {
                    self.pos += 1;
                    break;
                }
                Some(_) => bytes.push(self.string_char()),
            }
        }
        string_object(out, &bytes)
    }

    /// `C$ n chars`: the 48's form for a string holding `"`.
    fn counted_string(&mut self, out: &mut Vec<u8>) -> Result<ObjectType> {
        self.pos += 3;
        let digits_end = self.s[self.pos..]
            .iter()
            .position(|b| !b.is_ascii_digit())
            .map_or(self.s.len(), |i| self.pos + i);
        let count: usize = std::str::from_utf8(&self.s[self.pos..digits_end])
            .ok()
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| ConvertError::Invalid("C$ without a character count".into()))?;
        if self.s.get(digits_end) != Some(&b' ') {
            return invalid("C$ count not followed by a space");
        }
        self.pos = digits_end + 1;
        // Checked before anything is allocated: the count is untrusted.
        let rest = self.s.len() - self.pos;
        if count > rest {
            return invalid(format!("C$ {count}: text ends after {rest} characters"));
        }
        let bytes = self.s[self.pos..self.pos + count].to_vec();
        self.pos += count;
        string_object(out, &bytes)
    }

    fn complex(&mut self, out: &mut Vec<u8>) -> Result<ObjectType> {
        let sep = if self.decimal == b',' { b';' } else { b',' };
        self.pos += 1;
        self.skip_ws();
        let re = self.real_token(&[sep])?;
        self.skip_ws();
        if self.peek() != Some(sep) {
            return invalid(format!("complex number without `{}`", char::from(sep)));
        }
        self.pos += 1;
        self.skip_ws();
        let im = self.real_token(b")")?;
        self.skip_ws();
        if self.peek() != Some(b')') {
            return invalid("complex number without `)`");
        }
        self.pos += 1;
        push(out, ObjectType::Complex.prolog().into(), 5);
        out.extend(re);
        out.extend(im);
        Ok(ObjectType::Complex)
    }

    /// The characters of a number up to whitespace or one of `stops`.
    fn number_token(&mut self, stops: &[u8]) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || stops.contains(&c) || b"{}()\"".contains(&c) {
                break;
            }
            self.pos += 1;
        }
        String::from_utf8_lossy(&self.s[start..self.pos]).into_owned()
    }

    fn real_token(&mut self, stops: &[u8]) -> Result<Vec<u8>> {
        let start = self.pos;
        let tok = self.number_token(stops);
        match parse_number(&tok, self.decimal) {
            Some(n) => real_nibbles(&n),
            None => {
                self.pos = start;
                Err(self.unsupported_word("is not a real number"))
            }
        }
    }

    fn number(&mut self, out: &mut Vec<u8>) -> Result<ObjectType> {
        let start = self.pos;
        let tok = self.number_token(&[]);
        let Some(n) = parse_number(&tok, self.decimal) else {
            self.pos = start;
            return Err(self.unsupported_word("is not a number"));
        };
        if self.family == Family::Hp49 && n.exact {
            integer_object(out, &n)?;
            return Ok(ObjectType::Integer);
        }
        push(out, ObjectType::Real.prolog().into(), 5);
        out.extend(real_nibbles(&n)?);
        Ok(ObjectType::Real)
    }

    fn binary_integer(&mut self, out: &mut Vec<u8>) -> Result<ObjectType> {
        self.pos += 1;
        while self.peek() == Some(b' ') {
            self.pos += 1;
        }
        let tok = self.number_token(&[]);
        let Some(base) = tok.chars().last() else {
            return invalid("`#` without a number");
        };
        // `base` may be multi-byte (any non-ASCII byte reads as U+FFFD), so
        // slice by its UTF-8 length, never by one byte.
        let digits = &tok[..tok.len() - base.len_utf8()];
        let radix = match base {
            'h' | 'H' => 16,
            'd' | 'D' => 10,
            'o' | 'O' => 8,
            'b' | 'B' => 2,
            _ => {
                return invalid(format!(
                    "binary integer `# {tok}` needs a base suffix h, d, o or b"
                ));
            }
        };
        let value = u64::from_str_radix(digits, radix).map_err(|_| {
            ConvertError::Invalid(format!(
                "binary integer `# {tok}`: {digits:?} is not a 64-bit base-{radix} number"
            ))
        })?;
        push(out, ObjectType::BinaryInteger.prolog().into(), 5);
        push(out, 21, 5);
        push(out, value, 16);
        Ok(ObjectType::BinaryInteger)
    }

    fn grob(&mut self, out: &mut Vec<u8>) -> Result<ObjectType> {
        self.pos += 5;
        let mut dims = [0usize; 2];
        for d in &mut dims {
            self.skip_ws();
            let tok = self.number_token(&[]);
            *d = tok
                .parse()
                .ok()
                .filter(|&v| v < 0x10_0000)
                .ok_or_else(|| ConvertError::Invalid(format!("GROB size `{tok}`")))?;
        }
        let [width, height] = dims;
        self.skip_ws();
        let hex = self.number_token(&[]);
        let row = width.div_ceil(8) * 2;
        let body = row * height;
        if hex.len() != body {
            return invalid(format!(
                "GROB {width} {height} needs {body} hex digits, has {}",
                hex.len()
            ));
        }
        let len = 15 + body;
        if len > 0xF_FFFF {
            return invalid(format!("GROB {width} {height} is too large"));
        }
        push(out, ObjectType::Graphic.prolog().into(), 5);
        push(out, len as u64, 5);
        push(out, height as u64, 5);
        push(out, width as u64, 5);
        for c in hex.chars() {
            let n = c
                .to_digit(16)
                .ok_or_else(|| ConvertError::Invalid(format!("GROB data: {c:?} is not hex")))?;
            out.push(n as u8);
        }
        Ok(ObjectType::Graphic)
    }
}

fn string_object(out: &mut Vec<u8>, bytes: &[u8]) -> Result<ObjectType> {
    let len = 5 + 2 * bytes.len();
    if len > 0xF_FFFF {
        return invalid("string longer than a String object holds");
    }
    push(out, ObjectType::String.prolog().into(), 5);
    push(out, len as u64, 5);
    out.extend(object::unpack(bytes));
    Ok(ObjectType::String)
}

/// A decimal number: significant digits (no leading zeros) and the
/// exponent of the first one.
#[derive(Debug, PartialEq, Eq)]
struct Number {
    negative: bool,
    digits: Vec<u8>,
    exp: i64,
    /// No decimal point and no exponent.
    exact: bool,
}

fn parse_number(tok: &str, decimal: u8) -> Option<Number> {
    let b = tok.as_bytes();
    let mut i = 0;
    let negative = b.first() == Some(&b'-');
    if negative {
        i += 1;
    }
    let mut digits = Vec::new();
    let mut point: Option<usize> = None;
    let mut seen_digit = false;
    while let Some(&c) = b.get(i) {
        if c.is_ascii_digit() {
            digits.push(c - b'0');
            seen_digit = true;
        } else if c == decimal && point.is_none() {
            point = Some(digits.len());
        } else {
            break;
        }
        i += 1;
    }
    if !seen_digit {
        return None;
    }
    let mut exp10: i64 = 0;
    let mut has_exp = false;
    if matches!(b.get(i), Some(b'E' | b'e')) {
        has_exp = true;
        i += 1;
        let sign = match b.get(i) {
            Some(b'-') => {
                i += 1;
                -1
            }
            Some(b'+') => {
                i += 1;
                1
            }
            _ => 1,
        };
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        exp10 = sign * tok.get(start..i)?.parse::<i64>().ok()?;
        // Far outside the calculator's range (-499..499) whatever the
        // mantissa; bounding it keeps the arithmetic below from overflowing.
        if exp10.abs() > 10_000 {
            return None;
        }
    }
    if i != b.len() {
        return None;
    }
    let int_len = i64::try_from(point.unwrap_or(digits.len())).ok()?;
    let lead = digits.iter().take_while(|&&d| d == 0).count();
    let sig: Vec<u8> = digits[lead..].to_vec();
    let exp = int_len
        .checked_sub(i64::try_from(lead).ok()?)?
        .checked_sub(1)?
        .checked_add(exp10)?;
    Some(Number {
        negative,
        exp,
        digits: sig,
        exact: point.is_none() && !has_exp,
    })
}

/// The 16 real nibbles (exponent, mantissa, sign) for `n`, rounded half up
/// to 12 digits as the calculator does.
fn real_nibbles(n: &Number) -> Result<Vec<u8>> {
    let mut digits: Vec<u8> = n.digits.iter().copied().take(12).collect();
    let mut exp = n.exp;
    if n.digits.get(12).is_some_and(|&d| d >= 5) {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, 1);
                digits.truncate(12);
                exp = exp.saturating_add(1);
                break;
            }
            i -= 1;
            if digits[i] == 9 {
                digits[i] = 0;
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    digits.resize(12, 0);
    let mut out = Vec::with_capacity(16);
    if digits.iter().all(|&d| d == 0) {
        return Ok(vec![0; 16]);
    }
    if !(-499..=499).contains(&exp) {
        return Err(ConvertError::Invalid(format!(
            "exponent {exp} is outside the calculator's range (-499 to 499)"
        )));
    }
    let e = if exp < 0 { 1000 + exp } else { exp } as u64;
    out.extend([(e % 10) as u8, (e / 10 % 10) as u8, (e / 100) as u8]);
    out.extend(digits.iter().rev());
    out.push(if n.negative { 9 } else { 0 });
    Ok(out)
}

fn integer_object(out: &mut Vec<u8>, n: &Number) -> Result<ObjectType> {
    // exact: no point, no exponent, so exp + 1 is the digit count.
    let mut body: Vec<u8> = n.digits.iter().rev().copied().collect();
    if body.is_empty() {
        body.push(0);
    } else {
        body.push(if n.negative { 9 } else { 0 });
    }
    let len = 5 + body.len();
    if len > 0xF_FFFF {
        return invalid("integer too long");
    }
    push(out, ObjectType::Integer.prolog().into(), 5);
    push(out, len as u64, 5);
    out.extend(body);
    Ok(ObjectType::Integer)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/../hptx-core/fixtures/{name}.hp",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn text(data: &[u8]) -> String {
        let out = to_ascii(data).unwrap();
        let s = String::from_utf8(out).unwrap();
        let body = s.strip_prefix("%%HP: T(3)A(D)F(.);\r\n").unwrap();
        body.strip_suffix("\r\n").unwrap().to_string()
    }

    /// Binary file of `family` holding `nibbles`.
    fn file(family: Family, nibbles: &[u8]) -> Vec<u8> {
        let mut f = binary_header(family).to_bytes().to_vec();
        f.extend(object::pack(nibbles));
        f
    }

    fn object_nibbles(data: &[u8]) -> Vec<u8> {
        let nib = unpack(&data[HEADER_LEN..]);
        let size = object::object_size(&nib, 0).unwrap();
        nib[..size].to_vec()
    }

    #[test]
    fn fixtures_to_ascii_and_back() {
        let cases = [
            ("R", "1.5"),
            ("S", "\"AB\""),
            ("C", "(1.,2.)"),
            ("B", "# FFh"),
        ];
        for model in ["48sx", "48gx", "49g"] {
            let family = if model == "49g" {
                Family::Hp49
            } else {
                Family::Hp48
            };
            for (kind, want) in cases {
                let data = fixture(&format!("{model}-{kind}"));
                assert_eq!(text(&data), want, "{model}-{kind}");
                let back = to_binary(&to_ascii(&data).unwrap(), family).unwrap();
                assert_eq!(
                    object_nibbles(&back),
                    object_nibbles(&data),
                    "{model}-{kind}"
                );
            }
            // The LCD GROB round-trips through its hex text.
            let g = fixture(&format!("{model}-G"));
            let t = text(&g);
            assert!(t.starts_with("GROB 131 64 "), "{model}");
            let back = to_binary(&to_ascii(&g).unwrap(), family).unwrap();
            assert_eq!(&back[HEADER_LEN..], &g[HEADER_LEN..], "{model}-G");
        }
    }

    #[test]
    fn unsupported_fixtures_name_the_type() {
        for model in ["48sx", "48gx", "49g"] {
            for (kind, what) in [
                ("P", "Program"),
                ("A", "Algebraic"),
                ("D1", "Directory"),
                // { 1 2 }: the calculator stores the small numbers as ROM pointers.
                ("L", "ROM pointer"),
            ] {
                let err = to_ascii(&fixture(&format!("{model}-{kind}"))).unwrap_err();
                assert!(
                    matches!(&err, ConvertError::Unsupported(m) if m.contains(what)),
                    "{model}-{kind}: {err:?}"
                );
            }
        }
    }

    #[test]
    fn reals_in_std_form() {
        let real = |s: &str| {
            let n = parse_number(s, b'.').unwrap();
            real_text(&real_nibbles(&n).unwrap()).unwrap()
        };
        // Bytes recorded from the emulated 48SX and 49G (2026-10-05).
        for (input, want) in [
            ("1.5", "1.5"),
            ("100", "100."),
            ("100.", "100."),
            (".0001", ".0001"),
            ("0.0001", ".0001"),
            ("1E12", "1.E12"),
            ("1.E12", "1.E12"),
            ("123456789012", "123456789012."),
            ("0", "0."),
            ("-0", "0."),
            ("1E499", "1.E499"),
            ("-1.23E-15", "-1.23E-15"),
            ("-2", "-2."),
            ("1234567890123", "1.23456789012E12"),
            ("9999999999995", "1.E13"),
            (".00123456789012", "1.23456789012E-3"),
            (".0012345678901", "1.2345678901E-3"),
            (".001234567891", ".001234567891"),
            ("12.5E2", "1250."),
        ] {
            assert_eq!(real(input), want, "{input}");
        }
        assert!(real_nibbles(&parse_number("1E500", b'.').unwrap()).is_err());
        assert_eq!(parse_number("1,5", b','), parse_number("1.5", b'.'));
        for bad in ["", "-", ".", "1..2", "1E", "1x", "E5"] {
            assert_eq!(parse_number(bad, b'.'), None, "{bad}");
        }
    }

    #[test]
    fn recorded_real_and_integer_bytes() {
        // 48SX: -1.23E-15 is 33 29 50 98 00 00 00 00 30 12 09 after the header.
        let t = parse(b"-1.23E-15", Family::Hp48).unwrap();
        assert_eq!(
            object::pack(&t.nibbles),
            [0x33, 0x29, 0x50, 0x98, 0, 0, 0, 0, 0x30, 0x12, 0x09]
        );
        // 49G Integers: 123, -45, 0.
        for (input, bytes) in [
            ("123", &[0x14, 0x26, 0x90, 0x00, 0x00, 0x23, 0x01][..]),
            ("-45", &[0x14, 0x26, 0x80, 0x00, 0x00, 0x45, 0x09][..]),
            ("0", &[0x14, 0x26, 0x60, 0x00, 0x00, 0x00][..]),
        ] {
            let p = parse(input.as_bytes(), Family::Hp49).unwrap();
            assert_eq!(p.object_type, ObjectType::Integer);
            assert_eq!(object::pack(&p.nibbles), bytes, "{input}");
            let back = text(&file(Family::Hp49, &p.nibbles));
            assert_eq!(back, input);
        }
        // On the 48 the same text is a real.
        assert_eq!(
            parse(b"123", Family::Hp48).unwrap().object_type,
            ObjectType::Real
        );
    }

    #[test]
    fn calculator_text_parses() {
        // `get --ascii` output of { 1.5 "x" { #5h } } from both models.
        let sx = b"%%HP: T(1)A(D)F(.);\r\n{ 1.5 \"x\" { # 5d }\r\n}\r\n";
        let g49 = b"%%HP: T(1)A(R)F(.);\r\n{ 1.5 \"x\" { # 5h } }";
        let a = parse(sx, Family::Hp48).unwrap();
        let b = parse(g49, Family::Hp49).unwrap();
        assert_eq!(a.nibbles, b.nibbles);
        assert_eq!(a.object_type, ObjectType::List);
        assert_eq!(
            text(&file(Family::Hp48, &a.nibbles)),
            "{ 1.5 \"x\" { # 5h } }"
        );
        // A string q"\ LF → (141): the 48SX writes `C$ 5` and sends the LF as
        // CR LF (counted as one character); the 49G escapes " and \.
        let want: &[u8] = b"q\"\\\n\x8d";
        let sx_t1 = b"%%HP: T(1)A(D)F(.);\r\n{ C$ 5 q\"\\\r\n\x8d }\r\n";
        let sx_t3 = b"%%HP: T(3)A(D)F(.);\r\nC$ 5 q\"\\\\\r\n\\->";
        let g49_t1 = b"%%HP: T(1)A(R)F(.);\r\n\"q\\\"\\\\\r\n\x8d\"";
        for (input, family) in [
            (&sx_t1[..], Family::Hp48),
            (&sx_t3[..], Family::Hp48),
            (&g49_t1[..], Family::Hp49),
        ] {
            let p = parse(input, family).unwrap();
            let at = if p.object_type == ObjectType::List {
                15
            } else {
                10
            };
            let len = 2 * want.len();
            assert_eq!(
                object::pack(&p.nibbles[at..at + len]),
                want,
                "{}",
                String::from_utf8_lossy(input)
            );
        }
        // A CR LF the calculator wrote for CR LF in a string is ambiguous:
        // it reads back as one LF, and the count of `C$ 4` runs past it.
        assert!(parse(b"%%HP: T(1)A(D)F(.);\r\nC$ 4 a\r\nb", Family::Hp48).is_err());
    }

    /// Audit PR #18, #4: a huge `C$` count is refused before anything is
    /// allocated (it used to reserve the declared count first).
    #[test]
    fn counted_string_count_beyond_the_input() {
        let err = parse(
            b"%%HP: T(3)A(D)F(.);\r\nC$ 18446744073709551615 ab",
            Family::Hp48,
        )
        .unwrap_err();
        assert!(
            matches!(&err, ConvertError::Invalid(m) if m.contains("text ends after 2 characters")),
            "{err:?}"
        );
    }

    #[test]
    fn strings_per_family() {
        let s = |bytes: &[u8], family| {
            let mut n = Vec::new();
            string_object(&mut n, bytes).unwrap();
            let t = text(&file(family, &n));
            let back = parse(t.as_bytes(), family).unwrap().nibbles;
            assert_eq!(back, n, "{t}");
            t
        };
        assert_eq!(
            s(b"a\"b\\c\x8d\n", Family::Hp49),
            concat!(r#""a\\"b\\\\c\->"#, "\n\"")
        );
        assert_eq!(s(b"a\"b\\c\x8d\n", Family::Hp48), "C$ 7 a\"b\\\\c\\->\n");
        assert_eq!(s(b"x\xa0\xff", Family::Hp48), "\"x\\160\\255\"");
        assert_eq!(s(b"", Family::Hp48), "\"\"");
        let mut n = Vec::new();
        string_object(&mut n, b"a\rb").unwrap();
        assert!(matches!(
            to_ascii(&file(Family::Hp48, &n)),
            Err(ConvertError::Unsupported(m)) if m.contains("carriage return")
        ));
    }

    #[test]
    fn binary_integers_and_complex() {
        for (input, want) in [
            ("# 255d", "# FFh"),
            ("#FFh", "# FFh"),
            ("# 777o", "# 1FFh"),
            ("# 101b", "# 5h"),
            ("# FFFFFFFFFFFFFFFFh", "# FFFFFFFFFFFFFFFFh"),
            ("(1.5,-2)", "(1.5,-2.)"),
            ("( 0 , 1E-3 )", "(0.,.001)"),
            ("{ }", "{ }"),
            ("{{}{ 1.5 }}", "{ { } { 1.5 } }"),
            ("GROB 3 2 5020", "GROB 3 2 5020"),
        ] {
            let p = parse(input.as_bytes(), Family::Hp48).unwrap();
            assert_eq!(text(&file(Family::Hp48, &p.nibbles)), want, "{input}");
        }
        assert_eq!(
            parse(b"%%HP: T(3)A(D)F(,);\n(1,5;-2)", Family::Hp48)
                .unwrap()
                .nibbles,
            parse(b"(1.5,-2)", Family::Hp48).unwrap().nibbles
        );
    }

    #[test]
    fn parse_errors() {
        for (input, unsupported) in [
            ("\u{ab} 1 + \u{bb}", true),
            ("'X+Y'", true),
            ("ABC", true),
            ("{ 1.5 DUP }", true),
            ("1.5 2.5", true),
            ("# 12", false),
            ("# 1Gh", false),
            ("\"abc", false),
            ("{ 1", false),
            ("(1,2", false),
            ("GROB 3 2 50", false),
            ("C$ 5 ab", false),
            ("", false),
            ("%%HP: T(9);\n1", false),
            ("10E9223372036854775807", true),
            ("1E-9223372036854775808", true),
            ("1E10001", true),
        ] {
            let err = parse(input.as_bytes(), Family::Hp48).unwrap_err();
            assert_eq!(
                matches!(err, ConvertError::Unsupported(_)),
                unsupported,
                "{input}: {err:?}"
            );
        }
        // Non-ASCII base suffixes: a clean error, not a char-boundary panic.
        for input in [
            &b"# 12\xff"[..],
            "# 12\u{e9}".as_bytes(),
            b"%%HP: T(3)A(D)F(.);\n# 12\\160",
        ] {
            let err = parse(input, Family::Hp48).unwrap_err();
            assert!(
                matches!(err, ConvertError::Invalid(_)),
                "{:?}: {err:?}",
                String::from_utf8_lossy(input)
            );
        }
        assert!(to_ascii(b"no header").is_err());
        assert!(to_ascii(b"HPHP48-R\x2c\x2a").is_err());
    }
}
