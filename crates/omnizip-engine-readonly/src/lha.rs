//! Decoding shared by LZH and ARJ: the LHA methods (`delharc`, which ARJ methods 1–3 reuse as
//! `-lh6-`) and ARJ method 4, both as streams of a file's declared size.

use std::{io, path::Path};

use omnizip_core::{Error, ErrorCode, Result};

/// An LHA decoder as a [`Read`](io::Read) of exactly `size` bytes.
pub(crate) struct LhaStream<R: io::Read> {
    dec: delharc::decode::DecoderAny<R>,
    left: u64,
}

impl<R: io::Read> LhaStream<R> {
    pub fn new(
        method: delharc::CompressionMethod,
        input: R,
        size: u64,
        path: &Path,
    ) -> Result<Self> {
        let dec = delharc::decode::DecoderAny::new_from_compression(method, input);
        if !dec.is_supported() {
            return Err(Error::new(ErrorCode::UnsupportedMethod)
                .with("method", format!("{method:?}"))
                .with("path", path.display()));
        }
        Ok(Self { dec, left: size })
    }
}

impl<R: io::Read> io::Read for LhaStream<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use delharc::decode::Decoder;
        let n = buf
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        if n == 0 {
            return Ok(0);
        }
        self.dec.fill_buffer(&mut buf[..n]).map_err(|e| match e {
            delharc::LhaError::Io(io) => io,
            other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
        })?;
        self.left -= n as u64;
        Ok(n)
    }
}

/// Reads bits most significant first.
struct Bits<R> {
    inner: R,
    acc: u32,
    count: u32,
}

impl<R: io::Read> Bits<R> {
    fn bits(&mut self, n: u32) -> io::Result<u32> {
        while self.count < n {
            let mut b = [0u8];
            if self.inner.read(&mut b)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "ARJ data ends early",
                ));
            }
            self.acc = (self.acc << 8) | u32::from(b[0]);
            self.count += 8;
        }
        self.count -= n;
        Ok((self.acc >> self.count) & ((1 << n) - 1))
    }

    /// ARJ's variable-length value: a unary prefix of up to `to - from` one bits selects how
    /// many bits follow, offset by the ranges skipped.
    fn value(&mut self, from: u32, to: u32) -> io::Result<u32> {
        let (mut add, mut exp, mut bit) = (0, 1 << from, from);
        while bit < to {
            if self.bits(1)? == 0 {
                break;
            }
            add += exp;
            exp <<= 1;
            bit += 1;
        }
        let v = if bit != 0 { self.bits(bit)? } else { 0 };
        Ok(v + add)
    }
}

/// History kept for ARJ method 4: back references reach at most 15,871 bytes.
const WINDOW: usize = 1 << 15;
const THRESHOLD: usize = 3;

/// ARJ method 4 ("fastest"): literals and back references with unary-coded lengths.
pub(crate) struct ArjFastest<R> {
    bits: Bits<R>,
    left: u64,
    window: Box<[u8]>,
    /// Bytes written so far (the window position is this modulo its size).
    produced: usize,
    /// A back reference being copied: bytes left, and where it reads from.
    copy_left: usize,
    copy_from: usize,
}

impl<R: io::Read> ArjFastest<R> {
    pub fn new(input: R, size: u64) -> Self {
        Self {
            bits: Bits {
                inner: input,
                acc: 0,
                count: 0,
            },
            left: size,
            window: vec![0; WINDOW].into_boxed_slice(),
            produced: 0,
            copy_left: 0,
            copy_from: 0,
        }
    }
}

impl<R: io::Read> io::Read for ArjFastest<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut n = 0;
        while n < buf.len() && self.left > 0 {
            let byte = if self.copy_left > 0 {
                let b = self.window[self.copy_from % WINDOW];
                self.copy_from += 1;
                self.copy_left -= 1;
                b
            } else {
                let len = self.bits.value(0, 7)? as usize;
                if len == 0 {
                    self.bits.bits(8)? as u8
                } else {
                    let back = self.bits.value(9, 13)? as usize;
                    if back >= self.produced {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "ARJ back reference before the start",
                        ));
                    }
                    self.copy_left = len + THRESHOLD - 1;
                    self.copy_from = self.produced - 1 - back;
                    continue;
                }
            };
            buf[n] = byte;
            self.window[self.produced % WINDOW] = byte;
            self.produced += 1;
            self.left -= 1;
            n += 1;
        }
        Ok(n)
    }
}

/// A DOS date and time (local time) as packed in LHA and ARJ headers: the date in the high
/// 16 bits, the time in the low.
pub(crate) fn dos_time(v: u32) -> Option<std::time::SystemTime> {
    let (date, time) = ((v >> 16) as u16, v as u16);
    omnizip_core::timeutil::local_to_system(
        1980 + i32::from(date >> 9),
        ((date >> 5) & 0xF) as u8,
        (date & 0x1F) as u8,
        (time >> 11) as u8,
        ((time >> 5) & 0x3F) as u8,
        ((time & 0x1F) * 2) as u8,
    )
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    #[test]
    fn arj_fastest_literals_and_copies() {
        // value(0,7) = 0 then 8 literal bits: 'a'; again 'b'; then a copy: value(0,7) = 1
        // (bits "1" "0" + 1 bit) → len 1 → 3 bytes, value(9,13) = 1 ("0" + 9 bits) → back 1.
        let mut bits = String::new();
        bits += "0";
        bits += &format!("{:08b}", b'a');
        bits += "0";
        bits += &format!("{:08b}", b'b');
        bits += "10";
        bits += "0";
        bits += "0";
        bits += &format!("{:09b}", 1);
        while !bits.len().is_multiple_of(8) {
            bits.push('0');
        }
        let bytes: Vec<u8> = bits
            .as_bytes()
            .chunks(8)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap())
            .collect();
        let mut out = Vec::new();
        ArjFastest::new(bytes.as_slice(), 5)
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, b"ababa");
        // A reference before the start is damage.
        let mut out = Vec::new();
        let bad = [0b1000_0000u8, 0, 0, 0];
        assert!(ArjFastest::new(&bad[..], 4).read_to_end(&mut out).is_err());
    }
}
