//! XSERV packet framing and commands: the command server on top of XModem
//! (49g+/50g, the 49G after ROM 1.10, and the XSrvr48 library on the 48).
//!
//! **Unverified on hardware.** Everything here follows the wiki's
//! description of HP's own client code (wiki: protocols/xserv); no
//! calculator has been seen answering it. The emulated 49G (ROM 2.15) used
//! for the e2e tests has no XSERV, so this module is covered by unit tests
//! with hand-built vectors only. It is sans-IO: no transport, no clock.
//!
//! # Framing
//!
//! A packet is a 2-byte length (high byte first), the bytes, and a 1-byte
//! checksum: the sum of the bytes mod 256. The receiving side answers ACK,
//! or NAK to get the packet again (the client retries a command packet up
//! to 5 times and a reply up to 4).
//!
//! # Commands
//!
//! The host sends one command byte once the line is quiet, then, depending
//! on the command, a command packet ([`XservCommand::packet`]) and an XModem
//! transfer or a reply packet:
//!
//! | Byte | Command | Then |
//! | --- | --- | --- |
//! | `P` | [`XservCommand::Put`] | packet with the name, XModem send by the host |
//! | `G` | [`XservCommand::Get`] | packet with the name, XModem receive by the host |
//! | `E` | [`XservCommand::Execute`] | packet with RPL text |
//! | `M` | [`XservCommand::Memory`] | reply packet |
//! | `L` | [`XservCommand::List`] | reply packet, one [`DirRecord`] per variable |

use std::fmt;

use crate::Result;
use crate::calc::validate_name;
use crate::charset::{decode, encode, encode_command};

/// Most data bytes a packet can carry (16-bit length).
pub const MAX_DATA: usize = 0xFFFF;

/// Why a packet or a directory listing could not be built or read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// More than [`MAX_DATA`] bytes to send.
    TooLong(usize),
    /// The frame or record ends early.
    Truncated,
    /// Bytes after the checksum of a single frame.
    TrailingBytes(usize),
    /// The checksum byte does not match the data.
    BadChecksum {
        /// Sum of the data bytes mod 256.
        expected: u8,
        /// The checksum byte received.
        got: u8,
    },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::TooLong(n) => write!(f, "{n} bytes do not fit one XSERV packet"),
            FrameError::Truncated => f.write_str("XSERV packet or record ends early"),
            FrameError::TrailingBytes(n) => write!(f, "{n} bytes after the XSERV packet"),
            FrameError::BadChecksum { expected, got } => {
                write!(f, "XSERV checksum {got:#04x}, expected {expected:#04x}")
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// Sum of `data` mod 256.
pub fn checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b))
}

/// Frame `data` as one packet: length (high byte first), data, checksum.
pub fn encode_packet(data: &[u8]) -> std::result::Result<Vec<u8>, FrameError> {
    let len = u16::try_from(data.len()).map_err(|_| FrameError::TooLong(data.len()))?;
    let mut out = Vec::with_capacity(data.len() + 3);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(data);
    out.push(checksum(data));
    Ok(out)
}

/// Read exactly one complete packet and return its data.
pub fn decode_packet(frame: &[u8]) -> std::result::Result<Vec<u8>, FrameError> {
    let mut deframer = Deframer::new();
    deframer.push(frame);
    let data = deframer.next_packet().ok_or(FrameError::Truncated)??;
    match deframer.pending() {
        0 => Ok(data),
        n => Err(FrameError::TrailingBytes(n)),
    }
}

/// Splits a byte stream into packets, in any chunking.
#[derive(Clone, Debug, Default)]
pub struct Deframer {
    buf: Vec<u8>,
}

impl Deframer {
    /// Empty deframer.
    pub fn new() -> Self {
        Deframer::default()
    }

    /// Append bytes read from the link.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes buffered but not yet returned as a packet.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    /// Drop everything buffered (after a NAK, the sender starts over).
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// The next complete packet's data, or why it is bad (answer NAK); `None`
    /// until a whole packet is buffered. A bad packet is consumed.
    pub fn next_packet(&mut self) -> Option<std::result::Result<Vec<u8>, FrameError>> {
        let (&hi, &lo) = (self.buf.first()?, self.buf.get(1)?);
        let len = usize::from(u16::from_be_bytes([hi, lo]));
        let got = *self.buf.get(2 + len)?;
        let frame: Vec<u8> = self.buf.drain(..3 + len).collect();
        let data = frame.get(2..2 + len).unwrap_or_default().to_vec();
        let expected = checksum(&data);
        Some(if got == expected {
            Ok(data)
        } else {
            Err(FrameError::BadChecksum { expected, got })
        })
    }
}

/// An XSERV command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XservCommand {
    /// `P`: store the file that follows by XModem as variable NAME.
    Put(String),
    /// `G`: send variable NAME by XModem.
    Get(String),
    /// `E`: execute RPL text, e.g. `HOME DIR1` to change directory.
    Execute(String),
    /// `M`: free memory, as a reply packet (format unknown).
    Memory,
    /// `L`: list the current directory, as a reply packet of
    /// [`DirRecord`]s.
    List,
}

impl XservCommand {
    /// The command byte.
    pub fn byte(&self) -> u8 {
        match self {
            XservCommand::Put(_) => b'P',
            XservCommand::Get(_) => b'G',
            XservCommand::Execute(_) => b'E',
            XservCommand::Memory => b'M',
            XservCommand::List => b'L',
        }
    }

    /// The command packet that follows the command byte, framed; `None` for
    /// `M` and `L`. Names must be valid variable names; names and RPL text
    /// are encoded in the HP character set (RPL text may use ASCII
    /// trigraphs such as `\->`).
    pub fn packet(&self) -> Result<Option<Vec<u8>>> {
        let data = match self {
            XservCommand::Put(name) | XservCommand::Get(name) => {
                validate_name(name)?;
                encode(name)?
            }
            XservCommand::Execute(text) => encode_command(text)?,
            XservCommand::Memory | XservCommand::List => return Ok(None),
        };
        encode_packet(&data)
            .map(Some)
            .map_err(|e| crate::Error::Reply(e.to_string()))
    }

    /// Whether the calculator answers with a reply packet.
    pub fn has_reply(&self) -> bool {
        matches!(self, XservCommand::Memory | XservCommand::List)
    }
}

/// One variable in an `L` reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirRecord {
    /// Name, decoded from the HP character set.
    pub name: String,
    /// Low 16 bits of the object's prolog, e.g. `0x2A96` for a directory,
    /// `0x2A2C` for a string.
    pub prolog: u16,
    /// Size in nibbles.
    pub size_nibbles: u32,
    /// The object's CRC as the calculator reports it.
    pub crc: u16,
}

impl DirRecord {
    /// Whether the prolog is a directory's (#2A96).
    pub fn is_directory(&self) -> bool {
        self.prolog == 0x2A96
    }
}

/// Parse the data of an `L` reply: per variable, 1 byte name length, the
/// name, 2 bytes prolog (low 16 bits, low byte first), 3 bytes size in
/// nibbles (low byte first), 2 bytes CRC. The CRC's byte order is not
/// documented; it is read low byte first like the other fields.
pub fn parse_dir_list(data: &[u8]) -> std::result::Result<Vec<DirRecord>, FrameError> {
    let mut records = Vec::new();
    let mut rest = data;
    while let Some((&len, tail)) = rest.split_first() {
        let len = usize::from(len);
        let record = tail.get(..len + 7).ok_or(FrameError::Truncated)?;
        let (name, fields) = record.split_at(len);
        let byte = |i: usize| fields.get(i).copied().unwrap_or_default();
        records.push(DirRecord {
            name: decode(name),
            prolog: u16::from_le_bytes([byte(0), byte(1)]),
            size_nibbles: u32::from_le_bytes([byte(2), byte(3), byte(4), 0]),
            crc: u16::from_le_bytes([byte(5), byte(6)]),
        });
        rest = tail.get(len + 7..).unwrap_or_default();
    }
    Ok(records)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn encode_by_hand() {
        // "AB": length 0x0002, 0x41 + 0x42 = 0x83.
        assert_eq!(
            encode_packet(b"AB").unwrap(),
            [0x00, 0x02, b'A', b'B', 0x83]
        );
        assert_eq!(encode_packet(b"").unwrap(), [0x00, 0x00, 0x00]);
        // 300 bytes of 0xFF: length 0x012C, sum 300 * 255 mod 256 = 0xD4.
        let big = encode_packet(&[0xFF; 300]).unwrap();
        assert_eq!(&big[..2], &[0x01, 0x2C]);
        assert_eq!(big.len(), 303);
        assert_eq!(big[302], 0xD4);
        assert_eq!(
            encode_packet(&vec![0; MAX_DATA + 1]),
            Err(FrameError::TooLong(MAX_DATA + 1))
        );
        assert_eq!(
            encode_packet(&vec![0; MAX_DATA]).unwrap().len(),
            MAX_DATA + 3
        );
    }

    #[test]
    fn decode_by_hand() {
        assert_eq!(
            decode_packet(&[0x00, 0x02, b'A', b'B', 0x83]).unwrap(),
            b"AB"
        );
        assert_eq!(
            decode_packet(&[0x00, 0x02, b'A', b'B', 0x84]),
            Err(FrameError::BadChecksum {
                expected: 0x83,
                got: 0x84
            })
        );
        assert_eq!(
            decode_packet(&[0x00, 0x02, b'A']),
            Err(FrameError::Truncated)
        );
        assert_eq!(decode_packet(&[0x00]), Err(FrameError::Truncated));
        assert_eq!(
            decode_packet(&[0x00, 0x00, 0x00, 0x07]),
            Err(FrameError::TrailingBytes(1))
        );
    }

    #[test]
    fn deframer_handles_any_chunking() {
        let mut wire = encode_packet(b"HOME").unwrap();
        wire.extend(encode_packet(&[0x10; 256]).unwrap());
        let mut d = Deframer::new();
        let mut got = Vec::new();
        for byte in &wire {
            d.push(&[*byte]);
            while let Some(p) = d.next_packet() {
                got.push(p.unwrap());
            }
        }
        assert_eq!(got, vec![b"HOME".to_vec(), vec![0x10; 256]]);
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn deframer_consumes_a_bad_packet() {
        let mut d = Deframer::new();
        d.push(&[0x00, 0x01, b'x', 0x00]);
        d.push(&encode_packet(b"y").unwrap());
        assert!(matches!(
            d.next_packet(),
            Some(Err(FrameError::BadChecksum { .. }))
        ));
        assert_eq!(d.next_packet(), Some(Ok(b"y".to_vec())));
        d.push(&[0x00]);
        d.clear();
        assert_eq!(d.next_packet(), None);
    }

    #[test]
    fn commands() {
        let put = XservCommand::Put("ABC".into());
        assert_eq!(put.byte(), b'P');
        // 0x41 + 0x42 + 0x43 = 0xC6.
        assert_eq!(
            put.packet().unwrap(),
            Some(vec![0x00, 0x03, b'A', b'B', b'C', 0xC6])
        );
        assert_eq!(XservCommand::Get("X".into()).byte(), b'G');
        assert_eq!(
            XservCommand::Get("X".into()).packet().unwrap(),
            Some(vec![0x00, 0x01, b'X', b'X'])
        );
        assert!(XservCommand::Get("1X".into()).packet().is_err());
        assert!(XservCommand::Put("A B".into()).packet().is_err());

        // `\->` becomes the HP's arrow (0x8D).
        let exec = XservCommand::Execute("\\->A".into());
        assert_eq!(exec.byte(), b'E');
        assert_eq!(
            exec.packet().unwrap(),
            Some(vec![0x00, 0x02, 0x8D, b'A', 0x8D + b'A'])
        );
        assert_eq!(XservCommand::Memory.byte(), b'M');
        assert_eq!(XservCommand::List.byte(), b'L');
        assert_eq!(XservCommand::Memory.packet().unwrap(), None);
        assert_eq!(XservCommand::List.packet().unwrap(), None);
        assert!(XservCommand::List.has_reply() && XservCommand::Memory.has_reply());
        assert!(!put.has_reply());
    }

    #[test]
    fn dir_list_by_hand() {
        let data = [
            // "DIR1": directory #2A96, 22 nibbles, CRC 0x1234.
            &[4][..],
            b"DIR1",
            &[0x96, 0x2A, 22, 0, 0, 0x34, 0x12],
            // "S": string #2A2C, 0x012345 nibbles, CRC 0xBEEF.
            &[1],
            b"S",
            &[0x2C, 0x2A, 0x45, 0x23, 0x01, 0xEF, 0xBE],
        ]
        .concat();
        let list = parse_dir_list(&data).unwrap();
        assert_eq!(
            list,
            vec![
                DirRecord {
                    name: "DIR1".into(),
                    prolog: 0x2A96,
                    size_nibbles: 22,
                    crc: 0x1234
                },
                DirRecord {
                    name: "S".into(),
                    prolog: 0x2A2C,
                    size_nibbles: 0x012345,
                    crc: 0xBEEF
                },
            ]
        );
        assert!(list[0].is_directory() && !list[1].is_directory());
        assert_eq!(parse_dir_list(&[]).unwrap(), vec![]);
        assert_eq!(
            parse_dir_list(&data[..data.len() - 1]),
            Err(FrameError::Truncated)
        );
        // HP characters in names are decoded (0x8D is the arrow).
        let arrow = parse_dir_list(&[1, 0x8D, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(arrow[0].name, "→");
    }
}
