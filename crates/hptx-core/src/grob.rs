//! GROB (graphic object) decoding, encoding and PNG export.
//!
//! wiki: protocols/hp-object-format "GROB layout": prolog #02B1E, 5-nibble
//! length, 5-nibble height, 5-nibble width, then the rows. Each row is padded
//! to a whole number of bytes (an even nibble count); within a nibble the
//! least significant bit is the leftmost pixel. The screenshot is `LCD→`,
//! 131x64 on the 48SX, 48GX and 49G.

use crate::object::{BinaryHeader, HEADER_LEN, ObjectType, read_field, unpack};
use crate::{Error, Result};

/// A monochrome bitmap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grob {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Row-major, `true` = pixel on (dark).
    pixels: Vec<bool>,
}

/// Nibbles per GROB row: ceil(width / 4) rounded up to even.
fn row_nibbles(width: usize) -> usize {
    width.div_ceil(8) * 2
}

impl Grob {
    /// An all-off bitmap.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![false; width.saturating_mul(height)],
        }
    }

    /// Whether the pixel is on; `false` outside the bitmap.
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        x < self.width && y < self.height && self.pixels[y * self.width + x]
    }

    /// Sets a pixel; ignored outside the bitmap.
    pub fn set_pixel(&mut self, x: usize, y: usize, on: bool) {
        if x < self.width && y < self.height {
            self.pixels[y * self.width + x] = on;
        }
    }

    /// Decodes the GROB object starting at nibble `at`. Fails if it is not a
    /// GROB, is truncated, or its length field does not match its size.
    pub fn from_nibbles(nibbles: &[u8], at: usize) -> Result<Self> {
        let field = |off: usize| {
            at.checked_add(off)
                .and_then(|p| read_field(nibbles, p, 5))
                .map(|v| v as usize)
                .ok_or_else(|| Error::Object("truncated GROB".into()))
        };
        let prolog = field(0)?;
        if prolog != ObjectType::Graphic.prolog() as usize {
            return Err(Error::Object(format!("not a GROB: prolog #{prolog:05X}")));
        }
        let (len, height, width) = (field(5)?, field(10)?, field(15)?);
        let row = row_nibbles(width);
        let expected = height
            .checked_mul(row)
            .and_then(|b| b.checked_add(15))
            .ok_or_else(|| Error::Object("GROB size overflows".into()))?;
        if len != expected {
            return Err(Error::Object(format!(
                "GROB {width}x{height} needs length #{expected:05X}, has #{len:05X}"
            )));
        }
        let body_at = at + 20;
        let body = body_at
            .checked_add(height * row)
            .and_then(|end| nibbles.get(body_at..end))
            .ok_or_else(|| Error::Object("truncated GROB".into()))?;
        let mut grob = Self::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let nib = body[y * row + x / 4];
                grob.pixels[y * width + x] = nib >> (x % 4) & 1 != 0;
            }
        }
        Ok(grob)
    }

    /// Decodes a GROB file: an `HPHP48-x` / `HPHP49-x` binary transfer file
    /// or bare object bytes.
    pub fn from_file(data: &[u8]) -> Result<Self> {
        let object = if BinaryHeader::parse(data).is_some() {
            &data[HEADER_LEN..]
        } else {
            data
        };
        Self::from_nibbles(&unpack(object), 0)
    }

    /// Nibbles of the GROB object: prolog, length, height, width, rows.
    pub fn to_object(&self) -> Vec<u8> {
        let row = row_nibbles(self.width);
        let body_len = self.height * row;
        let mut out = Vec::with_capacity(20 + body_len);
        let len = 15 + body_len;
        for value in [
            ObjectType::Graphic.prolog() as usize,
            len,
            self.height,
            self.width,
        ] {
            out.extend((0..5).map(|i| ((value >> (4 * i)) & 0xF) as u8));
        }
        for y in 0..self.height {
            let mut nibs = vec![0u8; row];
            for x in 0..self.width {
                if self.pixels[y * self.width + x] {
                    nibs[x / 4] |= 1 << (x % 4);
                }
            }
            out.extend(nibs);
        }
        out
    }

    /// Encodes a 1-bit grayscale PNG: pixel on = black, off = white.
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let png_err = |e: png::EncodingError| Error::Object(format!("PNG: {e}"));
        let dim = |v: usize| {
            u32::try_from(v).map_err(|_| Error::Object(format!("GROB too large for PNG: {v}")))
        };
        let stride = self.width.div_ceil(8);
        let mut data = vec![0u8; stride * self.height];
        for y in 0..self.height {
            for x in 0..self.width {
                // PNG grayscale: bit 1 = white, MSB = leftmost pixel.
                if !self.pixels[y * self.width + x] {
                    data[y * stride + x / 8] |= 0x80 >> (x % 8);
                }
            }
        }
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, dim(self.width)?, dim(self.height)?);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::One);
        let mut writer = encoder.write_header().map_err(png_err)?;
        writer.write_image_data(&data).map_err(png_err)?;
        writer.finish().map_err(png_err)?;
        Ok(out)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::object::pack;

    #[test]
    fn lcd_fixtures() {
        for model in ["48sx", "48gx", "49g"] {
            let path = format!("{}/fixtures/{model}-G.hp", env!("CARGO_MANIFEST_DIR"));
            let data = std::fs::read(&path).unwrap();
            let g = Grob::from_file(&data).unwrap();
            assert_eq!((g.width, g.height), (131, 64), "{model}");
            assert!(g.pixels.iter().any(|&p| p), "{model}");
            assert_eq!(pack(&g.to_object()), &data[HEADER_LEN..], "{model}");
        }
    }

    #[test]
    fn small_grob_layout() {
        // 3x2: row 0 = on off on, row 1 = off on off; each row padded to 2 nibbles.
        let nibbles = [
            0xE, 0x1, 0xB, 0x2, 0x0, // prolog
            0x3, 0x1, 0x0, 0x0, 0x0, // length 15 + 2*2 = #00013
            0x2, 0x0, 0x0, 0x0, 0x0, // height
            0x3, 0x0, 0x0, 0x0, 0x0, // width
            0b0101, 0x0, 0b0010, 0x0,
        ];
        let g = Grob::from_nibbles(&nibbles, 0).unwrap();
        assert_eq!((g.width, g.height), (3, 2));
        let on: Vec<bool> = (0..2)
            .flat_map(|y| (0..3).map(move |x| (x, y)))
            .map(|(x, y)| g.pixel(x, y))
            .collect();
        assert_eq!(on, [true, false, true, false, true, false]);
        assert!(!g.pixel(3, 0) && !g.pixel(0, 2));
        assert_eq!(g.to_object(), nibbles);

        let mut bad = nibbles;
        bad[5] = 0x4;
        assert!(Grob::from_nibbles(&bad, 0).is_err());
        assert!(Grob::from_nibbles(&nibbles[..22], 0).is_err());
        let mut real = nibbles;
        real[..5].copy_from_slice(&[3, 3, 9, 2, 0]);
        assert!(Grob::from_nibbles(&real, 0).is_err());
        assert!(Grob::from_file(b"HPHP48-R").is_err());
    }

    #[test]
    fn object_round_trip() {
        let mut g = Grob::new(13, 5);
        for (x, y) in [(0, 0), (12, 4), (7, 2), (8, 3)] {
            g.set_pixel(x, y, true);
        }
        g.set_pixel(99, 99, true);
        let nib = g.to_object();
        assert_eq!(nib.len(), 20 + 5 * 4);
        assert_eq!(crate::object::object_size(&nib, 0).unwrap(), nib.len());
        let mut shifted = vec![0u8; 3];
        shifted.extend(&nib);
        assert_eq!(Grob::from_nibbles(&shifted, 3).unwrap(), g);
    }

    #[test]
    fn png_export() {
        let mut g = Grob::new(131, 64);
        g.set_pixel(0, 0, true);
        let bytes = g.to_png().unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
        let mut reader = decoder.read_info().unwrap();
        let info = reader.info();
        assert_eq!((info.width, info.height), (131, 64));
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut buf).unwrap();
        // First pixel on = black (bit 0), second off = white (bit 1).
        assert_eq!(buf[0] & 0xC0, 0x40);
    }
}
