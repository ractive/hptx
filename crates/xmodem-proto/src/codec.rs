//! XModem block framing and block checks.
//!
//! A block is `SOH|STX, blk, 255-blk, data, check`: 128 data bytes after SOH,
//! 1024 after STX; the check is one byte (sum of the data mod 256) or two
//! bytes (CRC-16, high byte first; standard or HP's own, see [`Check`]).
//! wiki: protocols/xmodem, protocols/xmodem-hp.

/// Start of a 128-byte block.
pub const SOH: u8 = 0x01;
/// Start of a 1024-byte block.
pub const STX: u8 = 0x02;
/// End of transmission.
pub const EOT: u8 = 0x04;
/// Positive acknowledgement.
pub const ACK: u8 = 0x06;
/// Negative acknowledgement; as a receiver's start character it asks for
/// checksum mode.
pub const NAK: u8 = 0x15;
/// Cancel; two in a row abort the transfer.
pub const CAN: u8 = 0x18;
/// Receiver start character asking for CRC-16 mode.
pub const CRC_START: u8 = b'C';
/// Receiver start character asking for HP's CRC mode ([`Check::HpCrc`]).
pub const HP_CRC_START: u8 = b'D';
/// End of a Kermit packet (the calculator's server may still send one).
pub(crate) const CR_KERMIT: u8 = b'\r';
/// CP/M end-of-file, the classic XModem padding byte.
pub const SUB: u8 = 0x1A;

/// The block check a transfer uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// One byte: the sum of the data bytes mod 256. The only mode the 48G
    /// series knows (wiki: protocols/xmodem-hp).
    Checksum,
    /// Two bytes, high byte first: CRC-16 with polynomial #1021, MSB first,
    /// initial value 0 ("CRC-16/XMODEM"). Not the Kermit type-3 check, which
    /// is LSB first (#8408 reflected, i.e. #1081 per nibble).
    Crc16,
    /// HP's own mode, requested with `D`: two bytes, high byte first, of the
    /// CRC-16 the Saturn hardware and Kermit's type-3 check use (polynomial
    /// #1081 per nibble, LSB first, initial value 0, i.e. "CRC-16/KERMIT").
    /// The 49G's `XRECV` asks for it first (three `D`s, then NAK) and
    /// accepts 1k and 128-byte blocks with it; verified on the emulated 49G
    /// (wiki: questions/xmodem-hp-crc-mode).
    HpCrc,
}

impl Check {
    /// Number of check bytes after the data.
    pub fn len(self) -> usize {
        match self {
            Check::Checksum => 1,
            Check::Crc16 | Check::HpCrc => 2,
        }
    }

    /// Always false; a check has at least one byte (clippy's `len` pairing).
    pub fn is_empty(self) -> bool {
        false
    }

    /// The check bytes for `data`, in wire order.
    pub fn compute(self, data: &[u8]) -> Vec<u8> {
        match self {
            Check::Checksum => vec![checksum(data)],
            Check::Crc16 => crc16(data).to_be_bytes().to_vec(),
            Check::HpCrc => hp_crc(data).to_be_bytes().to_vec(),
        }
    }

    /// The receiver start character that requests this check.
    pub fn start_char(self) -> u8 {
        match self {
            Check::Checksum => NAK,
            Check::Crc16 => CRC_START,
            Check::HpCrc => HP_CRC_START,
        }
    }

    /// The check a receiver's start character asks for.
    pub fn from_start_char(byte: u8) -> Option<Check> {
        match byte {
            NAK => Some(Check::Checksum),
            CRC_START => Some(Check::Crc16),
            HP_CRC_START => Some(Check::HpCrc),
            _ => None,
        }
    }
}

/// Data size of a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockSize {
    /// 128 bytes, header SOH.
    B128,
    /// 1024 bytes, header STX (XModem-1K).
    B1k,
}

impl BlockSize {
    /// Number of data bytes.
    pub fn len(self) -> usize {
        match self {
            BlockSize::B128 => 128,
            BlockSize::B1k => 1024,
        }
    }

    /// Always false (clippy's `len` pairing).
    pub fn is_empty(self) -> bool {
        false
    }

    /// The header byte (SOH or STX).
    pub fn header(self) -> u8 {
        match self {
            BlockSize::B128 => SOH,
            BlockSize::B1k => STX,
        }
    }

    /// The block size a header byte announces, if it is SOH or STX.
    pub fn from_header(byte: u8) -> Option<BlockSize> {
        match byte {
            SOH => Some(BlockSize::B128),
            STX => Some(BlockSize::B1k),
            _ => None,
        }
    }
}

/// Sum of `data` mod 256.
pub fn checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b))
}

/// CRC-16/XMODEM: polynomial #1021, MSB first, initial value 0, no final XOR.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// CRC-16/KERMIT: polynomial #1081 applied per nibble, LSB first, initial
/// value 0. Same value as the Saturn CRC circuit and Kermit's type-3 check.
pub fn hp_crc(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        for nibble in [byte & 0x0F, byte >> 4] {
            let q = (crc ^ u16::from(nibble)) & 0x0F;
            crc = (crc >> 4) ^ (q * 0x1081);
        }
    }
    crc
}

/// Total length on the wire of a block of `size` with `check`.
pub fn frame_len(size: BlockSize, check: Check) -> usize {
    3 + size.len() + check.len()
}

/// Encode block number `num` holding `data`, padded with `pad` to the full
/// block size. `data` longer than the block is cut (callers never pass that).
pub fn encode_block(num: u8, size: BlockSize, data: &[u8], pad: u8, check: Check) -> Vec<u8> {
    let n = size.len();
    let mut out = Vec::with_capacity(frame_len(size, check));
    out.push(size.header());
    out.push(num);
    out.push(!num);
    let take = data.len().min(n);
    out.extend_from_slice(data.get(..take).unwrap_or_default());
    out.resize(3 + n, pad);
    let check_bytes = check.compute(out.get(3..).unwrap_or_default());
    out.extend_from_slice(&check_bytes);
    out
}

/// A complete block decoded from the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// Block number as sent.
    pub num: u8,
    /// The data (128 or 1024 bytes), padding included.
    pub data: Vec<u8>,
}

/// Why a frame was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockError {
    /// Not SOH or STX.
    BadHeader,
    /// The block number and its complement disagree.
    BadNumber,
    /// The frame has the wrong length for its header and check.
    BadLength,
    /// The block check does not match.
    BadCheck,
}

/// Decode one complete frame (header to check) with `check`.
pub fn decode_block(frame: &[u8], check: Check) -> Result<Block, BlockError> {
    let size = frame
        .first()
        .and_then(|&b| BlockSize::from_header(b))
        .ok_or(BlockError::BadHeader)?;
    if frame.len() != frame_len(size, check) {
        return Err(BlockError::BadLength);
    }
    let (num, inv) = match (frame.get(1), frame.get(2)) {
        (Some(&a), Some(&b)) => (a, b),
        _ => return Err(BlockError::BadLength),
    };
    if num != !inv {
        return Err(BlockError::BadNumber);
    }
    let data = frame.get(3..3 + size.len()).ok_or(BlockError::BadLength)?;
    let got = frame.get(3 + size.len()..).ok_or(BlockError::BadLength)?;
    if got != check.compute(data).as_slice() {
        return Err(BlockError::BadCheck);
    }
    Ok(Block {
        num,
        data: data.to_vec(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn crc16_known_vectors() {
        // CRC-16/XMODEM check value.
        assert_eq!(crc16(b"123456789"), 0x31C3);
        assert_eq!(crc16(b""), 0);
        assert_eq!(crc16(b"A"), 0x58E5);
        // 128 bytes of SUB (an all-padding block).
        assert_eq!(crc16(&[SUB; 128]), crc16(&[SUB; 128]));
        assert_eq!(crc16(&[0u8; 128]), 0);
    }

    #[test]
    fn crc16_is_not_the_kermit_check() {
        // Kermit type 3 (LSB first, reflected) of "123456789" is 0x2189.
        assert_ne!(crc16(b"123456789"), 0x2189);
    }

    #[test]
    fn hp_crc_known_vectors() {
        // CRC-16/KERMIT check value.
        assert_eq!(hp_crc(b"123456789"), 0x2189);
        assert_eq!(hp_crc(b""), 0);
        assert_eq!(Check::HpCrc.compute(b"123456789"), vec![0x21, 0x89]);
        assert_eq!(Check::from_start_char(b'D'), Some(Check::HpCrc));
        assert_eq!(Check::from_start_char(0x15), Some(Check::Checksum));
        assert_eq!(Check::from_start_char(b'C'), Some(Check::Crc16));
        assert_eq!(Check::from_start_char(b'X'), None);
    }

    #[test]
    fn checksum_wraps() {
        assert_eq!(checksum(b""), 0);
        assert_eq!(checksum(&[0xFF, 0x02]), 0x01);
        let all: Vec<u8> = (0..=127).collect();
        assert_eq!(checksum(&all), (127 * 128 / 2 % 256) as u8);
    }

    #[test]
    fn encode_checksum_block_bytes() {
        let b = encode_block(1, BlockSize::B128, b"AB", SUB, Check::Checksum);
        assert_eq!(b.len(), 132);
        assert_eq!(&b[..5], &[SOH, 0x01, 0xFE, b'A', b'B']);
        assert!(b[5..131].iter().all(|&x| x == SUB));
        // 0x41 + 0x42 + 126 * 0x1A = 0x83 + 0xCCC = 0xD4F -> 0x4F
        assert_eq!(b[131], 0x4F);
    }

    #[test]
    fn encode_crc_block_bytes() {
        let data: Vec<u8> = (0..128).collect();
        let b = encode_block(0xFF, BlockSize::B128, &data, 0, Check::Crc16);
        assert_eq!(b.len(), 133);
        assert_eq!(&b[..3], &[SOH, 0xFF, 0x00]);
        let crc = crc16(&data);
        assert_eq!(&b[131..], &[(crc >> 8) as u8, crc as u8]);
    }

    #[test]
    fn encode_1k_block() {
        let b = encode_block(2, BlockSize::B1k, &[7; 1000], 0, Check::Crc16);
        assert_eq!(b.len(), 1029);
        assert_eq!(&b[..3], &[STX, 2, 0xFD]);
        assert_eq!(b[1002], 7);
        assert_eq!(b[1003], 0);
    }

    #[test]
    fn decode_round_trip_and_errors() {
        let b = encode_block(3, BlockSize::B128, b"hello", SUB, Check::Crc16);
        let got = decode_block(&b, Check::Crc16).unwrap();
        assert_eq!(got.num, 3);
        assert!(got.data.starts_with(b"hello"));
        assert_eq!(
            decode_block(&b, Check::Checksum),
            Err(BlockError::BadLength)
        );
        let mut bad = b.clone();
        bad[10] ^= 1;
        assert_eq!(decode_block(&bad, Check::Crc16), Err(BlockError::BadCheck));
        let mut bad = b.clone();
        bad[2] = 0;
        assert_eq!(decode_block(&bad, Check::Crc16), Err(BlockError::BadNumber));
        assert_eq!(
            decode_block(&[EOT], Check::Crc16),
            Err(BlockError::BadHeader)
        );
    }
}
