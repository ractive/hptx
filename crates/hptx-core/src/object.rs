//! The HP object format: `HPHP48-x` / `HPHP49-x` binary transfer files,
//! nibble packing, prologs, the object length walk and `%%HP:` ASCII headers.
//!
//! wiki: protocols/hp-object-format

use crate::{Error, Result};

/// Length of the binary transfer header (`HPHP48-x`).
pub const HEADER_LEN: usize = 8;

/// Composite objects end with this 5-nibble SEMI marker.
const SEMI: u32 = 0x0312B;

/// Directory attached-library field value meaning "no library".
const NO_LIBRARY: u32 = 0x7FF;

/// Deepest object nesting the walk accepts.
const MAX_DEPTH: usize = 64;

/// Calculator family named in the binary header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// HP 48SX/GX: `HPHP48-x`.
    Hp48,
    /// HP 49G: `HPHP49-x`.
    Hp49,
}

/// The 8-byte header of a binary transfer file, e.g. `HPHP48-R`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BinaryHeader {
    /// 48 or 49 series.
    pub family: Family,
    /// ROM revision letter byte, e.g. `b'J'`.
    pub rom: u8,
}

impl BinaryHeader {
    /// Parses the header at the start of `data`; `None` if there is none.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let head = data.get(..HEADER_LEN)?;
        let family = match &head[..6] {
            b"HPHP48" => Family::Hp48,
            b"HPHP49" => Family::Hp49,
            _ => return None,
        };
        if head[6] != b'-' || !head[7].is_ascii_graphic() {
            return None;
        }
        Some(Self {
            family,
            rom: head[7],
        })
    }

    /// The 8 header bytes.
    pub fn to_bytes(self) -> [u8; HEADER_LEN] {
        let digit = match self.family {
            Family::Hp48 => b'8',
            Family::Hp49 => b'9',
        };
        [b'H', b'P', b'H', b'P', b'4', digit, b'-', self.rom]
    }
}

/// The `%%HP: T(3)A(R)F(.);` header of an ASCII transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AsciiHeader {
    /// Translation mode 0-3.
    pub translate: u8,
    /// Angle mode `b'D'`, `b'R'` or `b'G'`.
    pub angle: u8,
    /// Fraction mark `b'.'` or `b','`.
    pub fraction: u8,
}

impl Default for AsciiHeader {
    /// `T(3)A(D)F(.)`.
    fn default() -> Self {
        Self {
            translate: 3,
            angle: b'D',
            fraction: b'.',
        }
    }
}

impl AsciiHeader {
    /// Parses the header at the start of `data`. Returns it with its length
    /// in bytes, including the CR LF or LF that ends the line (if present).
    pub fn parse(data: &[u8]) -> Option<(Self, usize)> {
        let mut p = Parser { data, pos: 0 };
        p.literal(b"%%HP:")?;
        p.spaces();
        let translate = p.field(b'T', |c| (b'0'..=b'3').contains(&c))? - b'0';
        p.spaces();
        let angle = p.field(b'A', |c| matches!(c, b'D' | b'R' | b'G'))?;
        p.spaces();
        let fraction = p.field(b'F', |c| matches!(c, b'.' | b','))?;
        p.spaces();
        p.literal(b";")?;
        if p.literal(b"\r\n").is_none() {
            let _ = p.literal(b"\n");
        }
        Some((
            Self {
                translate,
                angle,
                fraction,
            },
            p.pos,
        ))
    }

    /// The header line without line end, e.g. `%%HP: T(3)A(R)F(.);`.
    pub fn to_line(self) -> String {
        format!(
            "%%HP: T({})A({})F({});",
            self.translate,
            char::from(self.angle),
            char::from(self.fraction)
        )
    }
}

/// Byte cursor for [`AsciiHeader::parse`].
struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn literal(&mut self, lit: &[u8]) -> Option<()> {
        let end = self.pos.checked_add(lit.len())?;
        if self.data.get(self.pos..end)? == lit {
            self.pos = end;
            Some(())
        } else {
            None
        }
    }

    fn spaces(&mut self) {
        while matches!(self.data.get(self.pos), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    /// `K(v)` with `v` accepted by `ok`; returns `v`.
    fn field(&mut self, key: u8, ok: impl Fn(u8) -> bool) -> Option<u8> {
        self.literal(&[key, b'('])?;
        let v = *self.data.get(self.pos)?;
        if !ok(v) {
            return None;
        }
        self.pos += 1;
        self.literal(b")")?;
        Some(v)
    }
}

/// Splits bytes into nibbles (one per element, 0..=15), low nibble first.
pub fn unpack(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().flat_map(|b| [b & 0x0F, b >> 4]).collect()
}

/// Packs nibbles two per byte, low nibble first; an odd count is padded
/// with a 0 nibble. Only the low 4 bits of each element are used.
pub fn pack(nibbles: &[u8]) -> Vec<u8> {
    nibbles
        .chunks(2)
        .map(|c| (c[0] & 0x0F) | (c.get(1).copied().unwrap_or(0) & 0x0F) << 4)
        .collect()
}

/// Reads a `width`-nibble field at `at`, low nibble first. `None` past the
/// end or for fields wider than 8 nibbles.
pub fn read_field(nibbles: &[u8], at: usize, width: usize) -> Option<u32> {
    if width > 8 {
        return None;
    }
    let field = nibbles.get(at..at.checked_add(width)?)?;
    Some(
        field
            .iter()
            .rev()
            .fold(0u32, |acc, &n| (acc << 4) | u32::from(n & 0x0F)),
    )
}

/// Object types by prolog. wiki: protocols/hp-object-format
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectType {
    /// #02911.
    SystemBinary,
    /// #02933.
    Real,
    /// #02955.
    LongReal,
    /// #02977.
    Complex,
    /// #0299D.
    LongComplex,
    /// #029BF.
    Character,
    /// #029E8.
    Array,
    /// #02A0A.
    LinkedArray,
    /// #02A2C.
    String,
    /// #02A4E.
    BinaryInteger,
    /// #02A74.
    List,
    /// #02A96.
    Directory,
    /// #02AB8.
    Algebraic,
    /// #02ADA.
    Unit,
    /// #02AFC.
    Tagged,
    /// #02B1E (GROB).
    Graphic,
    /// #02B40.
    Library,
    /// #02B62.
    Backup,
    /// #02B88.
    LibraryData,
    /// #02D9D.
    Program,
    /// #02DCC.
    Code,
    /// #02E48.
    GlobalName,
    /// #02E6D.
    LocalName,
    /// #02E92.
    XlibName,
    /// #02614, 49G only.
    Integer,
    /// #02686 (DOMATRIX), 49G symbolic matrix.
    SymbolicMatrix,
    /// #0263A (DOLNGREAL), 49G long real.
    LongReal49,
    /// #02660 (DOLNGCMP), 49G long complex.
    LongComplex49,
    /// #026AC (DOFLASHP), 49G flash pointer.
    FlashPointer,
    /// #026D5 (DOAPLET), 49G.
    Aplet,
    /// #026FE (DOMINIFONT), 49G.
    MiniFont,
    /// #02BAA (DOEXT1 / DOACPTR), extended (access) pointer.
    ExtendedPointer,
    /// #02BCC (DOEXT2).
    Extended2,
    /// #02BEE (DOEXT3).
    Extended3,
    /// #02C10 (DOEXT4).
    Extended4,
}

/// How the walk finds an object's end.
#[derive(Clone, Copy)]
enum Walk {
    /// Fixed total size in nibbles.
    Fixed(usize),
    /// 5-nibble length after the prolog counting itself and the body.
    Length,
    /// `n` consecutive length-prefixed fields after the prolog, each length
    /// counting itself and its body (49G long real: 2, long complex: 4).
    Lengths(usize),
    /// Elements up to and including SEMI.
    Composite,
    /// 2-nibble char count, chars, one element.
    Tagged,
    /// 2-nibble char count, chars.
    Name,
    /// RAM-ROM pair directory.
    Directory,
}

/// Prologs live in this ROM range on the 48 and the 49G. An embedded value
/// in it that is not in [`TYPES`] is an unknown object, not a ROM pointer.
const PROLOG_RANGE: std::ops::RangeInclusive<u32> = 0x02600..=0x02FFF;

/// (type, prolog, name, walk rule). Prologs and walk rules: wiki:
/// protocols/hp-object-format (RPLMAN), wiki: sources/conn4x-ymodem-pas
/// (raw/protocols/xserv-conn4x/conn4xsource/FixObj.pas, prolog list and
/// per-type sizes, including the 49G types and DOEXT1-4) and the 48 entry
/// list raw/saturn-hardware/hp48-hw-notes/mlstarterkit/hardware/internals/
/// byaddress (DOEXT0-4 = #02B88, #02BAA, #02BCC, #02BEE, #02C10).
const TYPES: [(ObjectType, u32, &str, Walk); 35] = [
    (
        ObjectType::SystemBinary,
        0x02911,
        "System Binary",
        Walk::Fixed(10),
    ),
    (ObjectType::Real, 0x02933, "Real Number", Walk::Fixed(21)),
    (ObjectType::LongReal, 0x02955, "Long Real", Walk::Fixed(26)),
    (
        ObjectType::Complex,
        0x02977,
        "Complex Number",
        Walk::Fixed(37),
    ),
    (
        ObjectType::LongComplex,
        0x0299D,
        "Long Complex",
        Walk::Fixed(47),
    ),
    (ObjectType::Character, 0x029BF, "Character", Walk::Fixed(7)),
    (ObjectType::Array, 0x029E8, "Array", Walk::Length),
    (
        ObjectType::LinkedArray,
        0x02A0A,
        "Linked Array",
        Walk::Length,
    ),
    (ObjectType::String, 0x02A2C, "String", Walk::Length),
    (
        ObjectType::BinaryInteger,
        0x02A4E,
        "Binary Integer",
        Walk::Length,
    ),
    (ObjectType::List, 0x02A74, "List", Walk::Composite),
    (ObjectType::Directory, 0x02A96, "Directory", Walk::Directory),
    (ObjectType::Algebraic, 0x02AB8, "Algebraic", Walk::Composite),
    (ObjectType::Unit, 0x02ADA, "Unit", Walk::Composite),
    (ObjectType::Tagged, 0x02AFC, "Tagged", Walk::Tagged),
    (ObjectType::Graphic, 0x02B1E, "Graphic", Walk::Length),
    (ObjectType::Library, 0x02B40, "Library", Walk::Length),
    (ObjectType::Backup, 0x02B62, "Backup", Walk::Length),
    (
        ObjectType::LibraryData,
        0x02B88,
        "Library Data",
        Walk::Length,
    ),
    (ObjectType::Program, 0x02D9D, "Program", Walk::Composite),
    (ObjectType::Code, 0x02DCC, "Code", Walk::Length),
    (ObjectType::GlobalName, 0x02E48, "Global Name", Walk::Name),
    (ObjectType::LocalName, 0x02E6D, "Local Name", Walk::Name),
    (ObjectType::XlibName, 0x02E92, "XLIB Name", Walk::Fixed(11)),
    (ObjectType::Integer, 0x02614, "Integer", Walk::Length),
    (
        ObjectType::SymbolicMatrix,
        0x02686,
        "Symbolic Matrix",
        Walk::Composite,
    ),
    // Mantissa and exponent, each a length-prefixed integer body.
    (
        ObjectType::LongReal49,
        0x0263A,
        "Long Real (49G)",
        Walk::Lengths(2),
    ),
    // Two long reals without their prologs.
    (
        ObjectType::LongComplex49,
        0x02660,
        "Long Complex (49G)",
        Walk::Lengths(4),
    ),
    // Prolog, 3-nibble bank, 4-nibble address.
    (
        ObjectType::FlashPointer,
        0x026AC,
        "Flash Pointer",
        Walk::Fixed(12),
    ),
    // FixObj.pas marks these two as string-like (length-prefixed) without
    // walking them; not verified against a real object.
    (ObjectType::Aplet, 0x026D5, "Aplet", Walk::Length),
    (ObjectType::MiniFont, 0x026FE, "Mini Font", Walk::Length),
    // Prolog and two 5-nibble fields.
    (
        ObjectType::ExtendedPointer,
        0x02BAA,
        "Extended Pointer",
        Walk::Fixed(15),
    ),
    (ObjectType::Extended2, 0x02BCC, "Extended 2", Walk::Length),
    (ObjectType::Extended3, 0x02BEE, "Extended 3", Walk::Length),
    (ObjectType::Extended4, 0x02C10, "Extended 4", Walk::Length),
];

impl ObjectType {
    /// The type with this prolog address, if known.
    pub fn from_prolog(prolog: u32) -> Option<Self> {
        TYPES.iter().find(|t| t.1 == prolog).map(|t| t.0)
    }

    fn entry(self) -> &'static (ObjectType, u32, &'static str, Walk) {
        // TYPES lists every variant once, in declaration order.
        &TYPES[self as usize]
    }

    /// The prolog address.
    pub fn prolog(self) -> u32 {
        self.entry().1
    }

    /// Type name as the calculator's `G D` listing prints it, e.g. `Real Number`.
    pub fn name(self) -> &'static str {
        self.entry().2
    }
}

fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b)
        .ok_or_else(|| Error::Object("object size overflows".into()))
}

fn field(nibbles: &[u8], at: usize, width: usize) -> Result<usize> {
    read_field(nibbles, at, width)
        .map(|v| v as usize)
        .ok_or_else(|| Error::Object(format!("truncated object at nibble {at}")))
}

/// Size in nibbles of the object starting at nibble `at`. Fails on an
/// unknown prolog, a truncated object, an unsupported directory or nesting
/// deeper than 64 levels.
pub fn object_size(nibbles: &[u8], at: usize) -> Result<usize> {
    walk(nibbles, at, 0)
}

/// Size of an embedded object (composite element, tagged payload, directory
/// variable): an object with a known prolog, otherwise a 5-nibble ROM
/// pointer. A value in [`PROLOG_RANGE`] with no known prolog is an error:
/// guessing 5 nibbles would end the walk early and cut real data.
fn element_size(nibbles: &[u8], at: usize, depth: usize) -> Result<usize> {
    let p = field(nibbles, at, 5)? as u32;
    if ObjectType::from_prolog(p).is_some() {
        walk(nibbles, at, depth)
    } else if PROLOG_RANGE.contains(&p) {
        Err(Error::Object(format!(
            "unknown prolog #{p:05X} at nibble {at}"
        )))
    } else {
        Ok(5)
    }
}

fn walk(nibbles: &[u8], at: usize, depth: usize) -> Result<usize> {
    if depth > MAX_DEPTH {
        return Err(Error::Object(format!(
            "objects nested deeper than {MAX_DEPTH} levels"
        )));
    }
    let prolog = field(nibbles, at, 5)? as u32;
    let ty = ObjectType::from_prolog(prolog)
        .ok_or_else(|| Error::Object(format!("unknown prolog #{prolog:05X} at nibble {at}")))?;
    let body = add(at, 5)?;
    let size = match ty.entry().3 {
        Walk::Fixed(n) => n,
        Walk::Length => add(5, lengths_size(nibbles, body, 1)?)?,
        Walk::Lengths(n) => add(5, lengths_size(nibbles, body, n)?)?,
        Walk::Composite => {
            let mut pos = body;
            loop {
                if field(nibbles, pos, 5)? == SEMI as usize {
                    break add(pos, 5)? - at;
                }
                pos = add(pos, element_size(nibbles, pos, depth + 1)?)?;
            }
        }
        Walk::Tagged => {
            let n = field(nibbles, body, 2)?;
            let inner = add(body, 2 + 2 * n)?;
            add(inner, element_size(nibbles, inner, depth + 1)?)? - at
        }
        Walk::Name => 7 + 2 * field(nibbles, body, 2)?,
        Walk::Directory => directory_size(nibbles, at, depth)?,
    };
    if add(at, size)? > nibbles.len() {
        return Err(Error::Object(format!(
            "{} at nibble {at} needs {size} nibbles, only {} left",
            ty.name(),
            nibbles.len().saturating_sub(at)
        )));
    }
    Ok(size)
}

/// Total size of `count` consecutive length-prefixed fields starting at
/// nibble `at`; each 5-nibble length counts itself and its body.
fn lengths_size(nibbles: &[u8], at: usize, count: usize) -> Result<usize> {
    let mut pos = at;
    for _ in 0..count {
        let len = field(nibbles, pos, 5)?;
        if len < 5 {
            return Err(Error::Object(format!(
                "length field #{len:05X} at nibble {pos} is shorter than itself"
            )));
        }
        pos = add(pos, len)?;
    }
    Ok(pos - at)
}

/// Directory layout (verified against the fixtures): prolog, 3-nibble
/// attached library (#7FF = none), 5-nibble offset counted from the start of
/// that field to the last record's name length field (0 = empty). Records:
/// 5-nibble back-offset, name length n, 2n name nibbles, n again (absent when
/// n = 0), object (or ROM pointer). wiki: protocols/hp-object-format
fn directory_size(nibbles: &[u8], at: usize, depth: usize) -> Result<usize> {
    let lib_at = add(at, 5)?;
    let lib = field(nibbles, lib_at, 3)?;
    if lib != NO_LIBRARY as usize {
        return Err(Error::Object(format!(
            "attached library not supported (#{lib:03X} at nibble {lib_at})"
        )));
    }
    let off_at = add(lib_at, 3)?;
    let offset = field(nibbles, off_at, 5)?;
    let mut pos = add(off_at, 5)?;
    if offset == 0 {
        return Ok(pos - at);
    }
    let last = add(off_at, offset)?;
    loop {
        let name_at = add(pos, 5)?;
        if name_at > last {
            return Err(Error::Object(format!(
                "directory at nibble {at}: last-variable offset does not match its records"
            )));
        }
        let n = field(nibbles, name_at, 2)?;
        pos = add(name_at, 2 + 2 * n)?;
        if n != 0 {
            field(nibbles, pos, 2)?;
            pos = add(pos, 2)?;
        }
        pos = add(pos, element_size(nibbles, pos, depth + 1)?)?;
        if name_at == last {
            return Ok(pos - at);
        }
    }
}

/// What [`inspect`] finds in a binary transfer file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectInfo {
    /// The `HPHP4x-x` header.
    pub header: BinaryHeader,
    /// The object's prolog address.
    pub prolog: u32,
    /// The type, if the prolog is known.
    pub object_type: Option<ObjectType>,
    /// Object size in nibbles; `None` when the walk fails.
    pub size_nibbles: Option<usize>,
}

/// Reads the header and walks the object of a binary transfer file. Fails
/// if there is no binary header or fewer than 5 object nibbles.
pub fn inspect(data: &[u8]) -> Result<ObjectInfo> {
    let header = BinaryHeader::parse(data)
        .ok_or_else(|| Error::Object("no HPHP48-x / HPHP49-x header".into()))?;
    let nibbles = unpack(&data[HEADER_LEN..]);
    let prolog = read_field(&nibbles, 0, 5)
        .ok_or_else(|| Error::Object("no object after the header".into()))?;
    Ok(ObjectInfo {
        header,
        prolog,
        object_type: ObjectType::from_prolog(prolog),
        size_nibbles: object_size(&nibbles, 0).ok(),
    })
}

/// Padding allowance for [`strip_padding`] on Kermit transfers: the HP adds
/// at most the spare half byte of an odd nibble count, so anything beyond a
/// few bytes means the walk was wrong, not that the file was padded.
/// wiki: protocols/hp-object-format (Observed on the saturnng emulator)
pub const KERMIT_PADDING_ALLOWANCE: usize = 4;

/// Cuts trailing bytes after the object: header + ceil(size / 2) bytes when
/// the walk succeeds, fits in `data` and leaves at most `allowance` excess
/// bytes; otherwise returns `data` unchanged. The allowance bounds the
/// damage of a wrong walk: a too-short walk cannot silently cut real data.
/// Use [`KERMIT_PADDING_ALLOWANCE`] for Kermit; XModem pads whole blocks and
/// needs a larger allowance.
pub fn strip_padding(data: &[u8], allowance: usize) -> &[u8] {
    let Some(size) = inspect(data).ok().and_then(|i| i.size_nibbles) else {
        return data;
    };
    let want = HEADER_LEN + size.div_ceil(2);
    match data.len().checked_sub(want) {
        Some(excess) if excess <= allowance => data.get(..want).unwrap_or(data),
        _ => data,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const MODELS: [(&str, Family, u8); 3] = [
        ("48sx", Family::Hp48, b'J'),
        ("48gx", Family::Hp48, b'R'),
        ("49g", Family::Hp49, b'C'),
    ];

    fn fixture(model: &str, kind: &str) -> Vec<u8> {
        let path = format!("{}/fixtures/{model}-{kind}.hp", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    /// Nibbles of a little-endian field.
    fn f(value: u32, width: usize) -> Vec<u8> {
        (0..width)
            .map(|i| ((value >> (4 * i)) & 0xF) as u8)
            .collect()
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    fn real() -> Vec<u8> {
        cat(&[f(0x02933, 5), vec![0; 16]])
    }

    #[test]
    fn fixtures_walk() {
        use ObjectType as T;
        for (model, family, rom) in MODELS {
            let kinds = [
                ("R", Some(T::Real)),
                ("S", Some(T::String)),
                ("L", Some(T::List)),
                ("P", Some(T::Program)),
                ("A", Some(T::Algebraic)),
                ("C", Some(T::Complex)),
                ("B", Some(T::BinaryInteger)),
                ("TG", Some(T::List)),
                ("G", Some(T::Graphic)),
                ("D1", Some(T::Directory)),
            ];
            for (kind, ty) in kinds {
                let data = fixture(model, kind);
                let info = inspect(&data).unwrap();
                assert_eq!(info.header, BinaryHeader { family, rom }, "{model}-{kind}");
                assert_eq!(info.object_type, ty, "{model}-{kind}");
                let size = info.size_nibbles.expect("walk");
                let full = 2 * (data.len() - HEADER_LEN);
                assert!(
                    size == full || size + 1 == full,
                    "{model}-{kind}: {size} vs {full}"
                );
                let allow = KERMIT_PADDING_ALLOWANCE;
                assert_eq!(strip_padding(&data, allow), &data[..], "{model}-{kind}");
                let mut padded = data.clone();
                padded.extend_from_slice(&[0; KERMIT_PADDING_ALLOWANCE]);
                assert_eq!(strip_padding(&padded, allow), &data[..], "{model}-{kind}");
                padded.push(0);
                assert_eq!(strip_padding(&padded, allow), &padded[..], "{model}-{kind}");
            }
        }
    }

    #[test]
    fn tagged_fixture_holds_tag() {
        for (model, _, _) in MODELS {
            let data = fixture(model, "TG");
            let nib = unpack(&data[HEADER_LEN..]);
            assert_eq!(read_field(&nib, 5, 5), Some(ObjectType::Tagged.prolog()));
        }
    }

    #[test]
    fn binary_header_round_trip() {
        for (_, family, rom) in MODELS {
            let h = BinaryHeader { family, rom };
            assert_eq!(BinaryHeader::parse(&h.to_bytes()), Some(h));
        }
        assert_eq!(
            &BinaryHeader::parse(b"HPHP48-R").unwrap().to_bytes(),
            b"HPHP48-R"
        );
        assert_eq!(BinaryHeader::parse(b"HPHP47-R"), None);
        assert_eq!(BinaryHeader::parse(b"HPHP48-"), None);
        assert_eq!(BinaryHeader::parse(b"%%HP: T(1)"), None);
        assert!(inspect(b"HPHP48-R\x33").is_err());
        assert!(inspect(b"not a file").is_err());
    }

    #[test]
    fn ascii_header() {
        let (h, len) = AsciiHeader::parse(b"%%HP: T(1)A(D)F(.);\r\n\"AB\"").unwrap();
        assert_eq!(len, 21);
        assert_eq!(
            h,
            AsciiHeader {
                translate: 1,
                angle: b'D',
                fraction: b'.'
            }
        );
        assert_eq!(h.to_line(), "%%HP: T(1)A(D)F(.);");
        let (h2, len2) = AsciiHeader::parse(b"%%HP: T(3)A(R)F(,);\n").unwrap();
        assert_eq!(len2, 20);
        assert_eq!(AsciiHeader::parse(h2.to_line().as_bytes()).unwrap().0, h2);
        assert_eq!(AsciiHeader::default().to_line(), "%%HP: T(3)A(D)F(.);");
        assert_eq!(AsciiHeader::parse(b"%%HP: T(4)A(D)F(.);"), None);
        assert_eq!(AsciiHeader::parse(b"HPHP48-R"), None);
        assert_eq!(AsciiHeader::parse(b"%%HP: T(1)A(D)"), None);
    }

    #[test]
    fn nibble_packing() {
        assert_eq!(unpack(&[0x2A, 0xF0]), vec![0xA, 0x2, 0x0, 0xF]);
        for n in [vec![], vec![7], vec![1, 2, 3], vec![0xF, 0, 0xA, 5]] {
            let packed = pack(&n);
            assert_eq!(packed.len(), n.len().div_ceil(2));
            assert_eq!(unpack(&packed)[..n.len()], n[..]);
        }
        assert_eq!(pack(&[1, 2, 3]), vec![0x21, 0x03]);
        assert_eq!(read_field(&[0xE, 0x1, 0xB, 0x2, 0x0], 0, 5), Some(0x02B1E));
        assert_eq!(read_field(&[1, 2], 1, 2), None);
        assert_eq!(read_field(&[1, 2], 0, 0), Some(0));
    }

    #[test]
    fn type_table() {
        for t in TYPES {
            assert_eq!(ObjectType::from_prolog(t.1), Some(t.0));
            assert_eq!(t.0.prolog(), t.1);
            assert_eq!(t.0.name(), t.2);
        }
        assert_eq!(ObjectType::from_prolog(0x12345), None);
    }

    #[test]
    fn empty_directory() {
        let d = cat(&[f(0x02A96, 5), f(0x7FF, 3), f(0, 5)]);
        assert_eq!(object_size(&d, 0).unwrap(), 13);
        let lib = cat(&[f(0x02A96, 5), f(0x123, 3), f(0, 5)]);
        assert!(object_size(&lib, 0).is_err());
    }

    #[test]
    fn directory_two_variables() {
        let rec1 = cat(&[f(0, 5), f(1, 2), f(b'A'.into(), 2), f(1, 2), real()]);
        let rec2 = cat(&[f(rec1.len() as u32, 5), f(0, 2), real()]);
        let off = 5 + rec1.len() as u32 + 5;
        let d = cat(&[
            f(0x02A96, 5),
            f(0x7FF, 3),
            f(off, 5),
            rec1,
            rec2,
            vec![9; 3],
        ]);
        assert_eq!(object_size(&d, 0).unwrap(), d.len() - 3);
        let bad = cat(&[f(0x02A96, 5), f(0x7FF, 3), f(3, 5), real()]);
        assert!(object_size(&bad, 0).is_err());
    }

    #[test]
    fn nested_lists_and_tags() {
        let inner = cat(&[f(0x02A74, 5), real(), f(SEMI, 5)]);
        let outer = cat(&[
            f(0x02A74, 5),
            inner.clone(),
            f(0x1ABCD, 5),
            inner,
            f(SEMI, 5),
        ]);
        assert_eq!(object_size(&outer, 0).unwrap(), outer.len());
        let tag = cat(&[f(0x02AFC, 5), f(2, 2), f(0x4241, 4), real()]);
        assert_eq!(object_size(&tag, 0).unwrap(), tag.len());
        let tag_ptr = cat(&[f(0x02AFC, 5), f(1, 2), f(0x54, 2), f(0x2A31D, 5)]);
        assert_eq!(object_size(&tag_ptr, 0).unwrap(), tag_ptr.len());
    }

    #[test]
    fn program_with_rom_pointers_and_integer() {
        let prog = cat(&[
            f(0x02D9D, 5),
            f(0x1E061, 5),
            real(),
            f(0x1A2B3, 5),
            f(SEMI, 5),
        ]);
        assert_eq!(object_size(&prog, 0).unwrap(), prog.len());
        let int = cat(&[f(0x02614, 5), f(7, 5), vec![3, 0]]);
        assert_eq!(object_size(&int, 0).unwrap(), 12);
        let name = cat(&[f(0x02E48, 5), f(1, 2), f(0x5A, 2)]);
        assert_eq!(object_size(&name, 0).unwrap(), 9);
    }

    #[test]
    fn malformed_input_errors() {
        let prog = cat(&[f(0x02D9D, 5), f(0x1E061, 5), real(), f(SEMI, 5)]);
        for cut in 0..prog.len() {
            assert!(object_size(&prog[..cut], 0).is_err(), "cut {cut}");
        }
        let err = object_size(&f(0x12345, 5), 0).unwrap_err().to_string();
        assert!(err.contains("#12345"), "{err}");
        let short = cat(&[f(0x02A2C, 5), f(3, 5)]);
        assert!(object_size(&short, 0).is_err());
        let huge = cat(&[f(0x02A2C, 5), f(0xFFFFF, 5)]);
        assert!(object_size(&huge, 0).is_err());
        assert!(object_size(&real(), 100).is_err());
    }

    /// A 49G list holding a symbolic matrix `[[ 1. ]]`. Before #02686 was in
    /// TYPES the walk took the matrix prolog for a 5-nibble ROM pointer,
    /// stopped at the matrix's SEMI and came out 5 nibbles short, so
    /// strip_padding cut the list's real SEMI off.
    #[test]
    fn list_with_symbolic_matrix_walks_full_length_regression() {
        let list = |matrix_prolog: u32| {
            cat(&[
                f(0x02A74, 5),
                f(matrix_prolog, 5),
                real(),
                f(SEMI, 5),
                f(SEMI, 5),
            ])
        };
        let fixed = list(0x02686);
        assert_eq!(object_size(&fixed, 0).unwrap(), fixed.len());
        // What the old walk saw: the matrix prolog as an opaque pointer.
        let as_pointer = list(0x1ABCD);
        assert_eq!(object_size(&as_pointer, 0).unwrap(), as_pointer.len() - 5);

        let mut file = BinaryHeader {
            family: Family::Hp49,
            rom: b'C',
        }
        .to_bytes()
        .to_vec();
        file.extend(pack(&fixed));
        assert_eq!(strip_padding(&file, KERMIT_PADDING_ALLOWANCE), &file[..]);
    }

    #[test]
    fn unknown_value_in_prolog_range_errors() {
        for p in [0x02600, 0x02700, 0x02FFF] {
            let list = cat(&[f(0x02A74, 5), f(p, 5), real(), f(SEMI, 5)]);
            let err = object_size(&list, 0).unwrap_err().to_string();
            assert!(err.contains(&format!("unknown prolog #{p:05X}")), "{err}");
        }
        // Just outside the range: still a ROM pointer.
        for p in [0x025FF, 0x03000] {
            let list = cat(&[f(0x02A74, 5), f(p, 5), f(SEMI, 5)]);
            assert_eq!(object_size(&list, 0).unwrap(), 15);
        }
    }

    #[test]
    fn strip_padding_keeps_data_when_excess_exceeds_allowance() {
        let mut file = BinaryHeader {
            family: Family::Hp48,
            rom: b'R',
        }
        .to_bytes()
        .to_vec();
        file.extend(pack(&real()));
        let object_len = file.len();
        file.extend([0xAA; 10]);
        assert_eq!(strip_padding(&file, 4), &file[..]);
        assert_eq!(strip_padding(&file, 9), &file[..]);
        assert_eq!(strip_padding(&file, 10), &file[..object_len]);
        assert_eq!(strip_padding(&file, 64), &file[..object_len]);
        assert_eq!(strip_padding(b"not a file", 64), b"not a file");
    }

    #[test]
    fn hp49_and_extended_types_walk() {
        let long_real = cat(&[f(0x0263A, 5), f(7, 5), vec![1, 2], f(6, 5), vec![3]]);
        assert_eq!(object_size(&long_real, 0).unwrap(), long_real.len());
        let long_cmp = cat(&[
            f(0x02660, 5),
            f(6, 5),
            vec![1],
            f(5, 5),
            f(8, 5),
            vec![1, 2, 3],
            f(6, 5),
            vec![4],
        ]);
        assert_eq!(object_size(&long_cmp, 0).unwrap(), long_cmp.len());
        assert!(object_size(&long_cmp[..long_cmp.len() - 1], 0).is_err());
        let flash = cat(&[f(0x026AC, 5), f(0x123, 3), f(0x4567, 4)]);
        assert_eq!(object_size(&flash, 0).unwrap(), 12);
        let acptr = cat(&[f(0x02BAA, 5), f(0x12345, 5), f(0x6789A, 5)]);
        assert_eq!(object_size(&acptr, 0).unwrap(), 15);
        for p in [0x026D5, 0x026FE, 0x02BCC, 0x02BEE, 0x02C10] {
            let obj = cat(&[f(p, 5), f(8, 5), vec![1, 2, 3]]);
            assert_eq!(object_size(&obj, 0).unwrap(), 13, "#{p:05X}");
        }
        let in_list = cat(&[f(0x02A74, 5), flash, acptr, long_real, f(SEMI, 5)]);
        assert_eq!(object_size(&in_list, 0).unwrap(), in_list.len());
    }

    #[test]
    fn depth_limit() {
        let deep = |levels: usize| {
            let mut v = Vec::new();
            for _ in 0..levels {
                v.extend(f(0x02A74, 5));
            }
            for _ in 0..levels {
                v.extend(f(SEMI, 5));
            }
            v
        };
        assert!(object_size(&deep(MAX_DEPTH), 0).is_ok());
        assert!(object_size(&deep(MAX_DEPTH + 2), 0).is_err());
        assert!(object_size(&deep(10_000), 0).is_err());
    }
}
