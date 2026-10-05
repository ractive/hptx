//! Packet framing: character transforms, block checks, packet encoding and the
//! byte-stream deframer.

/// Start-of-header byte that begins every packet.
pub const SOH: u8 = 0x01;
/// Carriage return, the default end-of-line byte.
pub const CR: u8 = 0x0D;
/// Largest LEN value a (short) packet can carry: `tochar(94)` is `~`.
pub const MAX_LEN: usize = 94;

/// Turn a small number (0..=94) into a printable character.
pub const fn tochar(x: u8) -> u8 {
    x.wrapping_add(32)
}

/// Inverse of [`tochar`].
pub const fn unchar(x: u8) -> u8 {
    x.wrapping_sub(32)
}

/// Toggle bit 6, mapping control characters to printable ones and back.
pub const fn ctl(x: u8) -> u8 {
    x ^ 64
}

/// Block check type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BlockCheck {
    /// Single-character arithmetic checksum.
    Type1,
    /// Two-character 12-bit checksum.
    Type2,
    /// Three-character CRC-16/KERMIT.
    #[default]
    Type3,
}

impl BlockCheck {
    /// Number of check characters on the wire (1, 2 or 3).
    #[allow(clippy::len_without_is_empty)]
    pub fn len(self) -> usize {
        match self {
            BlockCheck::Type1 => 1,
            BlockCheck::Type2 => 2,
            BlockCheck::Type3 => 3,
        }
    }

    /// Parse the CHKT Send-Init field (`b'1'`, `b'2'`, `b'3'`).
    pub fn from_char(c: u8) -> Option<Self> {
        match c {
            b'1' => Some(BlockCheck::Type1),
            b'2' => Some(BlockCheck::Type2),
            b'3' => Some(BlockCheck::Type3),
            _ => None,
        }
    }

    /// The CHKT Send-Init field for this check type.
    pub fn to_char(self) -> u8 {
        match self {
            BlockCheck::Type1 => b'1',
            BlockCheck::Type2 => b'2',
            BlockCheck::Type3 => b'3',
        }
    }

    /// Compute the check characters over `body` (LEN through the end of DATA).
    pub fn compute(self, body: &[u8]) -> Vec<u8> {
        match self {
            BlockCheck::Type1 => {
                let s: u32 = body.iter().map(|&b| u32::from(b)).sum();
                vec![tochar(((s + ((s & 192) >> 6)) & 63) as u8)]
            }
            BlockCheck::Type2 => {
                let s: u32 = body.iter().map(|&b| u32::from(b)).sum::<u32>() & 0xFFF;
                vec![tochar(((s >> 6) & 63) as u8), tochar((s & 63) as u8)]
            }
            BlockCheck::Type3 => {
                let crc = crc16(body);
                vec![
                    tochar(((crc >> 12) & 15) as u8),
                    tochar(((crc >> 6) & 63) as u8),
                    tochar((crc & 63) as u8),
                ]
            }
        }
    }
}

/// CRC-16/KERMIT (init 0) as used by the type 3 block check.
pub fn crc16(body: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in body {
        let b = u16::from(b);
        crc = (crc >> 4) ^ (((crc ^ b) & 15) * 0x1081);
        crc = (crc >> 4) ^ (((crc ^ (b >> 4)) & 15) * 0x1081);
    }
    crc
}

/// One packet; `data` is the already prefix-encoded data field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    /// Sequence number, 0..=63.
    pub seq: u8,
    /// Packet type, e.g. `b'D'`.
    pub kind: u8,
    /// Encoded data field.
    pub data: Vec<u8>,
}

impl Packet {
    /// Build a packet; `seq` is stored modulo 64.
    pub fn new(seq: u8, kind: u8, data: Vec<u8>) -> Self {
        Packet {
            seq: seq % 64,
            kind,
            data,
        }
    }

    /// Longest data field that fits in a packet whose LEN may be at most
    /// `maxl` (capped at [`MAX_LEN`]) with block check `check`.
    pub fn max_data(maxl: usize, check: BlockCheck) -> usize {
        maxl.min(MAX_LEN).saturating_sub(2 + check.len())
    }

    /// NPAD x PADC, SOH, LEN, SEQ, TYPE, DATA, CHECK, EOL.
    ///
    /// The data field must fit: `data.len() <= Packet::max_data(MAX_LEN, check)`.
    /// Callers guarantee this; violating it is a bug, caught by a debug
    /// assertion. In release builds the LEN byte saturates at `~` instead of
    /// wrapping into a different, valid-looking length.
    pub fn encode(&self, check: BlockCheck, framing: &Framing) -> Vec<u8> {
        let len = 2 + self.data.len() + check.len();
        debug_assert!(
            len <= MAX_LEN,
            "packet data too long: LEN {len} exceeds {MAX_LEN}"
        );
        let len_char = u8::try_from(len.min(MAX_LEN)).map_or(tochar(94), tochar);
        let mut out = Vec::with_capacity(usize::from(framing.npad) + len + 3);
        out.extend(std::iter::repeat_n(framing.padc, usize::from(framing.npad)));
        out.push(SOH);
        let body_start = out.len();
        out.push(len_char);
        out.push(tochar(self.seq % 64));
        out.push(self.kind);
        out.extend_from_slice(&self.data);
        let chk = check.compute(&out[body_start..]);
        out.extend_from_slice(&chk);
        out.push(framing.eol);
        out
    }
}

/// What the peer asked us to put around each packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framing {
    /// Number of pad bytes before each packet.
    pub npad: u8,
    /// The pad byte.
    pub padc: u8,
    /// End-of-line byte after each packet.
    pub eol: u8,
}

impl Default for Framing {
    fn default() -> Self {
        Framing {
            npad: 0,
            padc: 0,
            eol: CR,
        }
    }
}

/// Reasons a frame is rejected by [`parse_frame`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// LEN out of range or inconsistent with the frame size.
    BadLength,
    /// The block check does not match.
    BadCheck,
}

/// Accumulates raw input bytes and splits out complete frames.
#[derive(Debug, Default)]
pub struct Deframer {
    buf: Vec<u8>,
}

impl Deframer {
    /// Empty deframer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append received bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Next complete frame: from SOH through the last CHECK byte (EOL and anything
    /// between frames is dropped). Rules: discard bytes before the first SOH; if
    /// unchar(LEN) is < 3 or > 94, drop that SOH and rescan; if another SOH appears
    /// before the frame is complete, restart at the new SOH (all control chars are
    /// prefixed, so a bare SOH always starts a packet). Returns None if incomplete.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        loop {
            match self.buf.iter().position(|&b| b == SOH) {
                Some(i) => {
                    self.buf.drain(..i);
                }
                None => {
                    self.buf.clear();
                    return None;
                }
            }
            if self.buf.len() < 2 {
                return None;
            }
            let len = usize::from(unchar(self.buf[1]));
            if !(3..=94).contains(&len) {
                self.buf.remove(0);
                continue;
            }
            let total = 2 + len;
            let end = total.min(self.buf.len());
            if let Some(j) = self.buf[1..end].iter().position(|&b| b == SOH) {
                self.buf.drain(..=j);
                continue;
            }
            if self.buf.len() < total {
                return None;
            }
            return Some(self.buf.drain(..total).collect());
        }
    }

    /// Drop all buffered input.
    pub fn clear(&mut self) {
        self.buf.clear();
    }
}

/// Verify the check and split a frame from [`Deframer::next_frame`].
/// `check` is used except for kind b'N': a NAK has no data, so its check length is
/// unchar(LEN) - 2 (Kermit manual p. 30) when that is 1..=3.
pub fn parse_frame(frame: &[u8], check: BlockCheck) -> Result<Packet, FrameError> {
    if frame.len() < 5 || frame[0] != SOH {
        return Err(FrameError::BadLength);
    }
    let len = usize::from(unchar(frame[1]));
    if !(3..=94).contains(&len) || frame.len() != 2 + len {
        return Err(FrameError::BadLength);
    }
    let kind = frame[3];
    let check = if kind == b'N' {
        match len - 2 {
            1 => BlockCheck::Type1,
            2 => BlockCheck::Type2,
            3 => BlockCheck::Type3,
            _ => check,
        }
    } else {
        check
    };
    let cl = check.len();
    if len < 2 + cl {
        return Err(FrameError::BadLength);
    }
    let split = frame.len() - cl;
    if check.compute(&frame[1..split]) != frame[split..] {
        return Err(FrameError::BadCheck);
    }
    Ok(Packet::new(
        unchar(frame[2]),
        kind,
        frame[4..split].to_vec(),
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// (wire bytes incl. CR, seq, kind, data)
    const VECTORS: &[(&[u8], u8, u8, &[u8])] = &[
        (b"\x01, I~* @-#Y3~Z\r", 0, b'I', b"~* @-#Y3~"),
        (b"\x01+ Y~& @-# 3,\r", 0, b'Y', b"~& @-# 3"),
        (b"\x01( C6 7 *C\r", 0, b'C', b"6 7 *"),
        (b"\x01+ S~* @-#Y3$\r", 0, b'S', b"~* @-#Y3"),
        (b"\x01# N3\r", 0, b'N', b""),
        (b"\x01#!X>\r", 1, b'X', b""),
        (b"\x01#!Y?\r", 1, b'Y', b""),
        (
            b"\x01?\"D1:                    42#M#J6\r",
            2,
            b'D',
            b"1:                    42#M#J",
        ),
    ];

    #[test]
    fn char_transforms() {
        assert_eq!(tochar(0), b' ');
        assert_eq!(tochar(94), b'~');
        assert_eq!(unchar(b'~'), 94);
        assert_eq!(unchar(tochar(17)), 17);
        assert_eq!(ctl(0), b'@');
        assert_eq!(ctl(b'M'), CR);
        assert_eq!(ctl(0x7F), b'?');
    }

    #[test]
    fn crc_vector() {
        assert_eq!(crc16(b"123456789"), 0x2189);
        assert_eq!(crc16(b""), 0);
    }

    #[test]
    fn check_types() {
        assert_eq!(BlockCheck::Type1.compute(b"# N"), b"3");
        let s: u32 = b"abc".iter().map(|&b| u32::from(b)).sum();
        assert_eq!(
            BlockCheck::Type2.compute(b"abc"),
            vec![tochar(((s >> 6) & 63) as u8), tochar((s & 63) as u8)]
        );
        let c = crc16(b"123456789");
        assert_eq!(
            BlockCheck::Type3.compute(b"123456789"),
            vec![
                tochar((c >> 12) as u8 & 15),
                tochar((c >> 6) as u8 & 63),
                tochar(c as u8 & 63)
            ]
        );
        for t in [BlockCheck::Type1, BlockCheck::Type2, BlockCheck::Type3] {
            assert_eq!(BlockCheck::from_char(t.to_char()), Some(t));
            assert_eq!(t.compute(b"xyz").len(), t.len());
        }
        assert_eq!(BlockCheck::from_char(b'4'), None);
        assert_eq!(BlockCheck::default(), BlockCheck::Type3);
    }

    #[test]
    fn wire_vectors_round_trip() {
        for &(wire, seq, kind, data) in VECTORS {
            let frame = &wire[..wire.len() - 1];
            let p = parse_frame(frame, BlockCheck::Type1).unwrap();
            assert_eq!(p, Packet::new(seq, kind, data.to_vec()), "{wire:?}");
            assert_eq!(p.encode(BlockCheck::Type1, &Framing::default()), wire);
        }
    }

    #[test]
    fn type2_type3_round_trip() {
        let p = Packet::new(70, b'D', b"hello #M#J world".to_vec());
        assert_eq!(p.seq, 6);
        for t in [BlockCheck::Type1, BlockCheck::Type2, BlockCheck::Type3] {
            let wire = p.encode(t, &Framing::default());
            let frame = &wire[..wire.len() - 1];
            assert_eq!(parse_frame(frame, t).unwrap(), p);
        }
    }

    #[test]
    fn max_data_and_longest_packet() {
        assert_eq!(Packet::max_data(80, BlockCheck::Type1), 77);
        assert_eq!(Packet::max_data(94, BlockCheck::Type3), 89);
        assert_eq!(Packet::max_data(200, BlockCheck::Type3), 89);
        assert_eq!(Packet::max_data(3, BlockCheck::Type3), 0);
        let p = Packet::new(0, b'D', vec![b'x'; 89]);
        let wire = p.encode(BlockCheck::Type3, &Framing::default());
        assert_eq!(wire[1], b'~');
        let frame = &wire[..wire.len() - 1];
        assert_eq!(parse_frame(frame, BlockCheck::Type3).unwrap(), p);
    }

    #[test]
    #[should_panic(expected = "packet data too long")]
    #[cfg(debug_assertions)]
    fn oversized_packet_is_caught() {
        let p = Packet::new(0, b'D', vec![b'x'; 90]);
        let _ = p.encode(BlockCheck::Type3, &Framing::default());
    }

    #[test]
    fn padding_and_eol() {
        let f = Framing {
            npad: 2,
            padc: 0,
            eol: b'\n',
        };
        let wire = Packet::new(1, b'Y', vec![]).encode(BlockCheck::Type1, &f);
        assert_eq!(wire, b"\0\0\x01#!Y?\n");
    }

    #[test]
    fn bad_check_detected() {
        let wire =
            Packet::new(3, b'D', b"abcdef".to_vec()).encode(BlockCheck::Type3, &Framing::default());
        let mut frame = wire[..wire.len() - 1].to_vec();
        frame[6] ^= 1;
        assert_eq!(
            parse_frame(&frame, BlockCheck::Type3),
            Err(FrameError::BadCheck)
        );
    }

    #[test]
    fn bad_length() {
        assert_eq!(
            parse_frame(b"\x01$ Y?", BlockCheck::Type1),
            Err(FrameError::BadLength)
        );
        assert_eq!(
            parse_frame(b"\x01\" Y?", BlockCheck::Type1),
            Err(FrameError::BadLength)
        );
        // LEN 3 with a type 3 check leaves no room.
        assert_eq!(
            parse_frame(b"\x01# Y?", BlockCheck::Type3),
            Err(FrameError::BadLength)
        );
    }

    #[test]
    fn nak_check_length_heuristic() {
        let nak = Packet::new(5, b'N', vec![]);
        for t in [BlockCheck::Type1, BlockCheck::Type2, BlockCheck::Type3] {
            let wire = nak.encode(t, &Framing::default());
            let frame = &wire[..wire.len() - 1];
            assert_eq!(parse_frame(frame, BlockCheck::Type1).unwrap(), nak);
            assert_eq!(parse_frame(frame, BlockCheck::Type3).unwrap(), nak);
        }
    }

    fn drain(d: &mut Deframer) -> Vec<Vec<u8>> {
        let mut v = Vec::new();
        while let Some(f) = d.next_frame() {
            v.push(f);
        }
        v
    }

    #[test]
    fn deframer_byte_by_byte() {
        let mut d = Deframer::new();
        let mut frames = Vec::new();
        let input: Vec<u8> = VECTORS.iter().flat_map(|v| v.0.iter().copied()).collect();
        for b in input {
            d.push(&[b]);
            frames.extend(drain(&mut d));
        }
        assert_eq!(frames.len(), VECTORS.len());
        for (f, v) in frames.iter().zip(VECTORS) {
            assert_eq!(f.as_slice(), &v.0[..v.0.len() - 1]);
        }
    }

    #[test]
    fn deframer_garbage_and_two_packets() {
        let mut d = Deframer::new();
        d.push(b"noise\r\n\x01# N3\r\x01#!Y?\r");
        assert_eq!(
            drain(&mut d),
            vec![b"\x01# N3".to_vec(), b"\x01#!Y?".to_vec()]
        );
        d.push(b"only garbage");
        assert_eq!(d.next_frame(), None);
    }

    #[test]
    fn deframer_invalid_len_skipped() {
        let mut d = Deframer::new();
        d.push(b"\x01\x7f\x01\"x\x01#!Y?\r");
        assert_eq!(drain(&mut d), vec![b"\x01#!Y?".to_vec()]);
    }

    #[test]
    fn deframer_resync_on_truncated() {
        let mut d = Deframer::new();
        d.push(b"\x01?\"D1:   ");
        assert_eq!(d.next_frame(), None);
        d.push(b"\x01#!Y?\r");
        assert_eq!(drain(&mut d), vec![b"\x01#!Y?".to_vec()]);
    }

    #[test]
    fn deframer_clear() {
        let mut d = Deframer::new();
        d.push(b"\x01#!Y");
        d.clear();
        d.push(b"?\r");
        assert_eq!(d.next_frame(), None);
    }
}
