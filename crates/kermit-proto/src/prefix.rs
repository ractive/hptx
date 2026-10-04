//! Data-field prefix encoding: control quoting, 8th-bit quoting and repeat counts.

use crate::codec::{ctl, tochar, unchar};

/// Prefix characters in effect for one direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quoting {
    /// Control prefix (QCTL), always active.
    pub qctl: u8,
    /// 8th-bit prefix (QBIN), if negotiated.
    pub qbin: Option<u8>,
    /// Repeat prefix (REPT), if negotiated.
    pub rept: Option<u8>,
}

impl Default for Quoting {
    fn default() -> Self {
        Quoting {
            qctl: b'#',
            qbin: None,
            rept: None,
        }
    }
}

/// Longest run a single repeat sequence can describe.
const MAX_RUN: usize = 94;

/// Append the encoding of one byte (without repeat prefix).
fn encode_byte(b: u8, q: &Quoting, out: &mut Vec<u8>) {
    let mut c = b;
    if let Some(p) = q.qbin
        && c & 0x80 != 0
    {
        out.push(p);
        c &= 0x7F;
    }
    let a7 = c & 0x7F;
    if a7 < 32 || a7 == 127 {
        out.push(q.qctl);
        out.push(ctl(c));
    } else if a7 == q.qctl || Some(a7) == q.qbin || Some(a7) == q.rept {
        out.push(q.qctl);
        out.push(c);
    } else {
        out.push(c);
    }
}

/// Encode as much of `input` as fits in `max` bytes without splitting a prefixed
/// sequence. Returns (encoded, consumed_input_bytes). Consumes >= 1 byte when input is
/// non-empty and max >= 5.
pub fn encode(input: &[u8], q: &Quoting, max: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut seq = Vec::with_capacity(5);
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        seq.clear();
        let mut n = 1;
        if let Some(r) = q.rept {
            let run = input[i..]
                .iter()
                .take(MAX_RUN)
                .take_while(|&&x| x == b)
                .count();
            if run >= 3 {
                n = run;
                seq.push(r);
                seq.push(tochar(run as u8));
            }
        }
        encode_byte(b, q, &mut seq);
        if out.len() + seq.len() > max {
            break;
        }
        out.extend_from_slice(&seq);
        i += n;
    }
    (out, i)
}

/// Encode everything, no size limit (command packet data).
pub fn encode_all(input: &[u8], q: &Quoting) -> Vec<u8> {
    encode(input, q, usize::MAX).0
}

/// Truncated prefix sequence at end of data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrefixError;

impl std::fmt::Display for PrefixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("truncated prefix sequence")
    }
}

impl std::error::Error for PrefixError {}

/// Decode a prefix-encoded data field.
pub fn decode(data: &[u8], q: &Quoting) -> Result<Vec<u8>, PrefixError> {
    let mut out = Vec::with_capacity(data.len());
    let mut it = data.iter().copied();
    while let Some(mut b) = it.next() {
        let mut n = 1;
        if let Some(r) = q.rept
            && b == r
        {
            n = usize::from(unchar(it.next().ok_or(PrefixError)?));
            b = it.next().ok_or(PrefixError)?;
        }
        let mut hi = 0;
        if let Some(p) = q.qbin
            && b == p
        {
            hi = 0x80;
            b = it.next().ok_or(PrefixError)?;
        }
        if b == q.qctl {
            b = it.next().ok_or(PrefixError)?;
            if (63..=95).contains(&(b & 0x7F)) {
                b = ctl(b);
            }
        }
        out.extend(std::iter::repeat_n(b | hi, n));
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn q(qbin: Option<u8>, rept: Option<u8>) -> Quoting {
        Quoting {
            qctl: b'#',
            qbin,
            rept,
        }
    }

    #[test]
    fn nul_run() {
        let q = q(None, Some(b'~'));
        assert_eq!(encode_all(&[0u8; 120], &q), b"~~#@~:#@");
        assert_eq!(decode(b"~~#@~:#@", &q).unwrap(), vec![0u8; 120]);
    }

    #[test]
    fn run_rests() {
        let q = q(None, Some(b'~'));
        assert_eq!(encode_all(b"aab", &q), b"aab");
        assert_eq!(encode_all(b"aaab", &q), b"~#ab");
        assert_eq!(encode_all(&[b'x'; 95], &q), b"~~xx");
        assert_eq!(encode_all(&[b'x'; 96], &q), b"~~xxx");
    }

    #[test]
    fn control_and_specials() {
        let d = Quoting::default();
        assert_eq!(encode_all(b"\r\n\0\x1f", &d), b"#M#J#@#_");
        assert_eq!(encode_all(b"\x7f", &d), b"#?");
        assert_eq!(encode_all(b"#", &d), b"##");
        assert_eq!(encode_all(b"&~", &d), b"&~");
        assert_eq!(encode_all(b"&~", &q(Some(b'&'), Some(b'~'))), b"#&#~");
    }

    #[test]
    fn eight_bit() {
        let d = Quoting::default();
        assert_eq!(encode_all(&[0xC1], &d), &[0xC1]);
        assert_eq!(encode_all(&[0x81], &d), &[b'#', 0xC1]);
        assert_eq!(encode_all(&[0xFF], &d), &[b'#', 0xBF]);
        assert_eq!(encode_all(&[0xA3], &d), &[b'#', 0xA3]);
        let qb = q(Some(b'&'), None);
        assert_eq!(encode_all(&[0xC1], &qb), b"&A");
        assert_eq!(encode_all(&[0x81], &qb), b"&#A");
        assert_eq!(encode_all(&[0xFF], &qb), b"&#?");
        assert_eq!(encode_all(&[0xA6], &qb), b"&#&");
        assert_eq!(encode_all(&[0xA3], &qb), b"&##");
    }

    #[test]
    fn all_bytes_round_trip() {
        let all: Vec<u8> = (0..=255u8).collect();
        let mut runs = Vec::new();
        for b in 0..=255u8 {
            runs.extend(std::iter::repeat_n(b, usize::from(b % 7) + 1));
        }
        runs.extend(std::iter::repeat_n(0xFFu8, 300));
        for qbin in [None, Some(b'&')] {
            for rept in [None, Some(b'~')] {
                let qq = q(qbin, rept);
                for input in [&all, &runs] {
                    let enc = encode_all(input, &qq);
                    assert!(enc.iter().all(|&c| (32..127).contains(&(c & 0x7F))));
                    if qbin.is_some() {
                        assert!(enc.iter().all(|&c| (32..127).contains(&c)));
                    }
                    assert_eq!(&decode(&enc, &qq).unwrap(), input, "{qq:?}");
                }
            }
        }
    }

    #[test]
    fn limited_encode_never_splits() {
        let mut input = Vec::new();
        for b in 0..=255u8 {
            input.extend(std::iter::repeat_n(b, usize::from(b % 5) + 1));
        }
        for qbin in [None, Some(b'&')] {
            for rept in [None, Some(b'~')] {
                let qq = q(qbin, rept);
                for max in 5..40 {
                    let mut rest = &input[..];
                    let mut joined = Vec::new();
                    while !rest.is_empty() {
                        let (enc, used) = encode(rest, &qq, max);
                        assert!(used >= 1);
                        assert!(enc.len() <= max);
                        assert_eq!(decode(&enc, &qq).unwrap(), &rest[..used]);
                        joined.extend_from_slice(&rest[..used]);
                        rest = &rest[used..];
                    }
                    assert_eq!(joined, input);
                }
            }
        }
        assert_eq!(encode(b"abc", &Quoting::default(), 0), (vec![], 0));
        assert_eq!(encode(b"", &Quoting::default(), 10), (vec![], 0));
    }

    #[test]
    fn truncated_decode() {
        let qq = q(Some(b'&'), Some(b'~'));
        for bad in [&b"#"[..], b"ab&", b"&#", b"~", b"~%", b"~%#"] {
            assert_eq!(decode(bad, &qq), Err(PrefixError), "{bad:?}");
        }
    }

    #[test]
    fn real_d_packet() {
        let got = decode(b"1:                    42#M#J", &Quoting::default()).unwrap();
        let mut want = b"1:".to_vec();
        want.extend_from_slice(&[b' '; 20]);
        want.extend_from_slice(b"42\r\n");
        assert_eq!(got, want);
    }
}
