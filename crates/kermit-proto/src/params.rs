//! Send-Init parameters and their negotiation.

use crate::codec::{BlockCheck, CR, Framing, ctl, tochar, unchar};
use crate::prefix::Quoting;

/// Send-Init fields with decoded (semantic) values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitParams {
    /// Longest packet (LEN value) the sender of these params can receive.
    pub maxl: u8,
    /// Timeout in seconds.
    pub time: u8,
    /// Number of pad bytes wanted before each packet.
    pub npad: u8,
    /// The actual pad byte (wire carries ctl(padc)).
    pub padc: u8,
    /// The actual EOL byte (wire carries tochar(eol)).
    pub eol: u8,
    /// Control prefix, literal.
    pub qctl: u8,
    /// 8th-bit prefix, literal: `b'Y'`, `b'N'`, `b' '` (none) or a prefix char.
    pub qbin: u8,
    /// Check type, literal `b'1'`, `b'2'` or `b'3'`.
    pub chkt: u8,
    /// Repeat prefix, literal; `b' '` = none.
    pub rept: u8,
}

impl Default for InitParams {
    /// Kermit defaults for absent fields.
    fn default() -> Self {
        InitParams {
            maxl: 80,
            time: 5,
            npad: 0,
            padc: 0,
            eol: CR,
            qctl: b'#',
            qbin: b' ',
            chkt: b'1',
            rept: b' ',
        }
    }
}

impl InitParams {
    /// 9 fields: tochar(maxl) tochar(time) tochar(npad) ctl(padc) tochar(eol) qctl qbin chkt rept
    pub fn encode(&self) -> Vec<u8> {
        vec![
            tochar(self.maxl),
            tochar(self.time),
            tochar(self.npad),
            ctl(self.padc),
            tochar(self.eol),
            self.qctl,
            self.qbin,
            self.chkt,
            self.rept,
        ]
    }

    /// Missing fields, and blank (b' ') maxl/time/npad/padc/eol/qctl/chkt
    /// fields, get the defaults. maxl 0 -> 80. Never fails; anything beyond
    /// field 9 is ignored.
    ///
    /// A blank PADC is the default NUL, not ctl(b' ') = 0x60: the Kermit
    /// Protocol Manual (6th ed., Send-Init) lets any field be "left blank,
    /// i.e. contain a space, to accept or specify default values", and PADC
    /// is "the control character I need for padding", which 0x60 is not. For
    /// the same reason a PADC that does not decode to a control character
    /// (0-31 or 127) also gets the default.
    pub fn decode(data: &[u8]) -> Self {
        let mut p = InitParams::default();
        let field = |i: usize| data.get(i).copied().filter(|&c| c != b' ');
        if let Some(c) = field(0) {
            let v = unchar(c);
            if v != 0 {
                p.maxl = v;
            }
        }
        if let Some(c) = field(1) {
            p.time = unchar(c);
        }
        if let Some(c) = field(2) {
            p.npad = unchar(c);
        }
        if let Some(c) = field(3).map(ctl)
            && (c < 32 || c == 127)
        {
            p.padc = c;
        }
        if let Some(c) = field(4) {
            p.eol = unchar(c);
        }
        if let Some(c) = field(5) {
            p.qctl = c;
        }
        if let Some(&c) = data.get(6) {
            p.qbin = c;
        }
        if let Some(c) = field(7) {
            p.chkt = c;
        }
        if let Some(&c) = data.get(8) {
            p.rept = c;
        }
        p
    }
}

/// Result of one S/I exchange, from our point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Negotiated {
    /// For encoding what we send: qctl = ours.qctl.
    pub send: Quoting,
    /// For decoding what we receive: qctl = theirs.qctl.
    pub recv: Quoting,
    /// Agreed check type.
    pub check: BlockCheck,
    /// theirs.maxl clamped to 10..=94.
    pub peer_maxl: u8,
    /// Their npad/padc/eol (we must send these).
    pub framing: Framing,
}

/// Valid prefix character: 33..=62 or 96..=126.
fn is_prefix(c: u8) -> bool {
    (33..=62).contains(&c) || (96..=126).contains(&c)
}

/// Combine our Send-Init parameters with the peer's.
pub fn negotiate(ours: &InitParams, theirs: &InitParams) -> Negotiated {
    let qbin = if is_prefix(ours.qbin) && (theirs.qbin == b'Y' || theirs.qbin == ours.qbin) {
        Some(ours.qbin)
    } else if is_prefix(theirs.qbin) && (ours.qbin == b'Y' || ours.qbin == theirs.qbin) {
        Some(theirs.qbin)
    } else {
        None
    };
    let rept = (ours.rept == theirs.rept && is_prefix(ours.rept)).then_some(ours.rept);
    // QCTL (either side's), QBIN and REPT must be distinct. On a collision the
    // optional feature is turned off: QBIN against QCTL first, then REPT
    // against QCTL or the surviving QBIN.
    let is_qctl = |c: u8| c == ours.qctl || c == theirs.qctl;
    let qbin = qbin.filter(|&c| !is_qctl(c));
    let rept = rept.filter(|&c| !is_qctl(c) && Some(c) != qbin);
    let check = if ours.chkt == theirs.chkt {
        BlockCheck::from_char(ours.chkt).unwrap_or(BlockCheck::Type1)
    } else {
        BlockCheck::Type1
    };
    Negotiated {
        send: Quoting {
            qctl: ours.qctl,
            qbin,
            rept,
        },
        recv: Quoting {
            qctl: theirs.qctl,
            qbin,
            rept,
        },
        check,
        peer_maxl: theirs.maxl.clamp(10, 94),
        framing: Framing {
            npad: theirs.npad,
            padc: theirs.padc,
            eol: theirs.eol,
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn ours() -> InitParams {
        InitParams {
            maxl: 94,
            time: 20,
            npad: 0,
            padc: 0,
            eol: CR,
            qctl: b'#',
            qbin: b'Y',
            chkt: b'3',
            rept: b'~',
        }
    }

    #[test]
    fn decode_hp_ack_to_i() {
        let p = InitParams::decode(b"~& @-# 3");
        assert_eq!(
            p,
            InitParams {
                maxl: 94,
                time: 6,
                npad: 0,
                padc: 0,
                eol: 13,
                qctl: b'#',
                qbin: b' ',
                chkt: b'3',
                rept: b' ',
            }
        );
    }

    #[test]
    fn decode_hp_send_init() {
        let p = InitParams::decode(b"~* @-#Y3");
        assert_eq!(p.maxl, 94);
        assert_eq!(p.time, 10);
        assert_eq!(p.eol, CR);
        assert_eq!(p.qbin, b'Y');
        assert_eq!(p.chkt, b'3');
        assert_eq!(p.rept, b' ');
    }

    #[test]
    fn encode_ours() {
        assert_eq!(ours().encode(), b"~4 @-#Y3~");
        assert_eq!(InitParams::decode(&ours().encode()), ours());
    }

    #[test]
    fn decode_defaults() {
        assert_eq!(InitParams::decode(b""), InitParams::default());
        assert_eq!(InitParams::decode(b"         "), InitParams::default());
        assert_eq!(InitParams::decode(b"\x20").maxl, 80);
        assert_eq!(InitParams::decode(b"~4 @-#Y3~extra"), ours());
    }

    #[test]
    fn decode_padc() {
        // Blank PADC: the default NUL, as for every other blank field.
        let p = InitParams::decode(b"~*\"   ");
        assert_eq!((p.npad, p.padc), (2, 0));
        assert_eq!(InitParams::decode(b"~*\"@").padc, 0);
        assert_eq!(InitParams::decode(b"~*\"?").padc, 0x7F);
        assert_eq!(InitParams::decode(b"~*\"A").padc, 0x01);
        // Not a control character: default.
        assert_eq!(InitParams::decode(b"~*\"`").padc, 0);
        assert_eq!(InitParams::decode(b"~*\"!").padc, 0);
        // Round trip of every control character.
        for padc in (0..32).chain([127]) {
            let mut o = ours();
            o.padc = padc;
            assert_eq!(InitParams::decode(&o.encode()).padc, padc);
        }
    }

    #[test]
    fn negotiate_qbin() {
        let mut theirs = InitParams::default();
        let mut o = ours();
        theirs.qbin = b'&';
        assert_eq!(negotiate(&o, &theirs).send.qbin, Some(b'&'));
        assert_eq!(negotiate(&o, &theirs).recv.qbin, Some(b'&'));
        theirs.qbin = b'Y';
        assert_eq!(negotiate(&o, &theirs).send.qbin, None);
        o.qbin = b' ';
        theirs.qbin = b'&';
        assert_eq!(negotiate(&o, &theirs).send.qbin, None);
        o.qbin = b'&';
        theirs.qbin = b'Y';
        assert_eq!(negotiate(&o, &theirs).recv.qbin, Some(b'&'));
        theirs.qbin = b'N';
        assert_eq!(negotiate(&o, &theirs).recv.qbin, None);
    }

    #[test]
    fn negotiate_rept_check_maxl_framing() {
        let o = ours();
        let mut theirs = o;
        theirs.qctl = b'!';
        theirs.npad = 2;
        theirs.padc = 0x7F;
        theirs.eol = b'\n';
        let n = negotiate(&o, &theirs);
        assert_eq!(n.send.rept, Some(b'~'));
        assert_eq!(n.recv.rept, Some(b'~'));
        assert_eq!(n.send.qctl, b'#');
        assert_eq!(n.recv.qctl, b'!');
        assert_eq!(n.check, BlockCheck::Type3);
        assert_eq!(n.peer_maxl, 94);
        assert_eq!(
            n.framing,
            Framing {
                npad: 2,
                padc: 0x7F,
                eol: b'\n'
            }
        );

        let hp = InitParams::decode(b"~* @-#Y2");
        let n = negotiate(&o, &hp);
        assert_eq!(n.send.rept, None);
        assert_eq!(n.check, BlockCheck::Type1);

        theirs.chkt = b'9';
        let mut o9 = o;
        o9.chkt = b'9';
        assert_eq!(negotiate(&o9, &theirs).check, BlockCheck::Type1);

        theirs.maxl = 5;
        assert_eq!(negotiate(&o, &theirs).peer_maxl, 10);
        theirs.maxl = 200;
        assert_eq!(negotiate(&o, &theirs).peer_maxl, 94);
    }

    #[test]
    fn negotiate_prefixes_distinct() {
        // REPT equal to QBIN: repeat compression is dropped, QBIN kept.
        let mut o = ours();
        o.qbin = b'&';
        o.rept = b'&';
        let mut theirs = o;
        theirs.qbin = b'Y';
        let n = negotiate(&o, &theirs);
        assert_eq!((n.send.qbin, n.send.rept), (Some(b'&'), None));
        assert_eq!((n.recv.qbin, n.recv.rept), (Some(b'&'), None));

        // REPT equal to the peer's QCTL: dropped in both directions.
        let o = ours();
        let mut theirs = o;
        theirs.qctl = b'~';
        let n = negotiate(&o, &theirs);
        assert_eq!((n.send.rept, n.recv.rept), (None, None));
        // ... and equal to our QCTL.
        let mut o2 = o;
        o2.qctl = b'~';
        let n = negotiate(&o2, &o);
        assert_eq!((n.send.rept, n.recv.rept), (None, None));

        // QBIN equal to the peer's QCTL: 8th-bit prefixing off, REPT kept.
        let mut o = ours();
        o.qbin = b'&';
        let mut theirs = o;
        theirs.qbin = b'Y';
        theirs.qctl = b'&';
        let n = negotiate(&o, &theirs);
        assert_eq!((n.send.qbin, n.recv.qbin), (None, None));
        assert_eq!(n.send.rept, Some(b'~'));

        // Distinct prefixes are untouched.
        let mut o = ours();
        o.qbin = b'&';
        let n = negotiate(&o, &o);
        assert_eq!((n.send.qbin, n.send.rept), (Some(b'&'), Some(b'~')));
    }
}
