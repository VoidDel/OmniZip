//! Single-stream codecs: decoders for reading and finishing encoders for writing.

use std::io::{self, Read, Write};

use omnizip_core::{Codec, Level, deflate::ParDeflate};

/// A boxed decoder.
pub type Decoder<'a> = Box<dyn Read + Send + 'a>;

/// Wraps `input` in the decoder for `codec`. Multi-member / multi-frame streams are read fully.
pub fn decoder<'a, R: Read + Send + 'a>(codec: Codec, input: R) -> io::Result<Decoder<'a>> {
    let input = io::BufReader::with_capacity(256 * 1024, input);
    Ok(match codec {
        Codec::Gzip => Box::new(flate2::bufread::MultiGzDecoder::new(input)),
        Codec::Bzip2 => Box::new(bzip2::bufread::MultiBzDecoder::new(input)),
        Codec::Xz => Box::new(liblzma::bufread::XzDecoder::new_multi_decoder(input)),
        Codec::Lzma => {
            let stream =
                liblzma::stream::Stream::new_lzma_decoder(u64::MAX).map_err(io::Error::other)?;
            Box::new(liblzma::bufread::XzDecoder::new_stream(input, stream))
        }
        Codec::Zstd => {
            let mut d = zstd::stream::read::Decoder::with_buffer(input)?;
            // Archives made with --long need a large window.
            d.window_log_max(31)?;
            Box::new(d)
        }
        Codec::Lz4 => Box::new(lz4_flex::frame::FrameDecoder::new(input)),
        Codec::Brotli => Box::new(brotli::Decompressor::new(input, 64 * 1024)),
    })
}

/// An encoder that must be finished explicitly to write its trailer.
pub enum Encoder<W: Write> {
    Plain(W),
    Gzip(flate2::write::GzEncoder<W>),
    /// Gzip deflated on several threads.
    GzipMt(ParGzip<W>),
    Bzip2(bzip2::write::BzEncoder<W>),
    Xz(liblzma::write::XzEncoder<W>),
    Zstd(zstd::stream::write::Encoder<'static, W>),
    Lz4(lz4_flex::frame::FrameEncoder<W>),
    Brotli(Box<brotli::CompressorWriter<W>>),
}

/// Native level of a codec for the 7-step UI level (`docs/03-formats.md` §6.1).
pub fn native_level(codec: Codec, level: Level) -> u32 {
    let i = level.index();
    match codec {
        Codec::Gzip | Codec::Bzip2 => [0, 1, 3, 6, 7, 9, 9][i],
        Codec::Xz | Codec::Lzma => [0, 0, 2, 6, 7, 9, 9][i],
        Codec::Zstd => [1, 1, 3, 6, 12, 19, 22][i],
        Codec::Brotli => [0, 1, 4, 6, 9, 11, 11][i],
        Codec::Lz4 => 0,
    }
}

impl<W: Write + Send> Encoder<W> {
    /// Creates the encoder. `gzip_name` and `gzip_mtime` fill the gzip header (single files).
    pub fn new(
        codec: Option<Codec>,
        out: W,
        level: Level,
        threads: u32,
        gzip_name: Option<&str>,
        gzip_mtime: u32,
    ) -> io::Result<Self> {
        let Some(codec) = codec else {
            return Ok(Encoder::Plain(out));
        };
        let n = native_level(codec, level);
        Ok(match codec {
            Codec::Gzip if threads > 1 && n > 0 => {
                Encoder::GzipMt(ParGzip::new(out, n, threads, gzip_name, gzip_mtime)?)
            }
            Codec::Gzip => {
                let mut b = flate2::GzBuilder::new().mtime(gzip_mtime);
                if let Some(name) = gzip_name {
                    b = b.filename(name.as_bytes());
                }
                Encoder::Gzip(b.write(out, flate2::Compression::new(n)))
            }
            Codec::Bzip2 => Encoder::Bzip2(bzip2::write::BzEncoder::new(
                out,
                bzip2::Compression::new(n.max(1)),
            )),
            Codec::Xz => {
                let mut preset = n;
                if level == Level::Ultra {
                    preset |= liblzma::stream::PRESET_EXTREME;
                }
                let stream = if threads > 1 {
                    liblzma::stream::MtStreamBuilder::new()
                        .threads(threads)
                        .preset(preset)
                        .check(liblzma::stream::Check::Crc64)
                        .encoder()
                } else {
                    liblzma::stream::Stream::new_easy_encoder(preset, liblzma::stream::Check::Crc64)
                }
                .map_err(io::Error::other)?;
                Encoder::Xz(liblzma::write::XzEncoder::new_stream(out, stream))
            }
            Codec::Lzma => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "use .xz to create LZMA streams",
                ));
            }
            Codec::Zstd => {
                let mut e = zstd::stream::write::Encoder::new(out, n as i32)?;
                if threads > 1 {
                    e.multithread(threads)?;
                }
                if level >= Level::Best {
                    e.long_distance_matching(true)?;
                }
                e.include_checksum(true)?;
                Encoder::Zstd(e)
            }
            Codec::Lz4 => Encoder::Lz4(lz4_flex::frame::FrameEncoder::new(out)),
            Codec::Brotli => {
                let lgwin = if level == Level::Ultra { 24 } else { 22 };
                Encoder::Brotli(Box::new(brotli::CompressorWriter::new(
                    out,
                    64 * 1024,
                    n,
                    lgwin,
                )))
            }
        })
    }

    /// Writes the trailer and returns the inner writer.
    pub fn finish(self) -> io::Result<W> {
        match self {
            Encoder::Plain(w) => Ok(w),
            Encoder::Gzip(e) => e.finish(),
            Encoder::GzipMt(e) => e.finish(),
            Encoder::Bzip2(e) => e.finish(),
            Encoder::Xz(e) => e.finish(),
            Encoder::Zstd(e) => e.finish(),
            Encoder::Lz4(e) => e.finish().map_err(io::Error::other),
            Encoder::Brotli(mut e) => {
                e.flush()?;
                Ok(e.into_inner())
            }
        }
    }
}

impl<W: Write> Write for Encoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Encoder::Plain(w) => w.write(buf),
            Encoder::Gzip(e) => e.write(buf),
            Encoder::GzipMt(e) => e.deflate.write(buf),
            Encoder::Bzip2(e) => e.write(buf),
            Encoder::Xz(e) => e.write(buf),
            Encoder::Zstd(e) => e.write(buf),
            Encoder::Lz4(e) => e.write(buf),
            Encoder::Brotli(e) => e.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Encoder::Plain(w) => w.flush(),
            Encoder::Gzip(e) => e.flush(),
            Encoder::GzipMt(e) => e.deflate.flush(),
            Encoder::Bzip2(e) => e.flush(),
            Encoder::Xz(e) => e.flush(),
            Encoder::Zstd(e) => e.flush(),
            Encoder::Lz4(e) => e.flush(),
            Encoder::Brotli(e) => e.flush(),
        }
    }
}

/// Parsed fields of a gzip member header.
/// A gzip member deflated on several threads: the header flate2 would write, a
/// [`ParDeflate`] stream and the CRC / size trailer.
pub struct ParGzip<W: Write> {
    deflate: ParDeflate<W>,
}

impl<W: Write> ParGzip<W> {
    fn new(
        mut out: W,
        level: u32,
        threads: u32,
        name: Option<&str>,
        mtime: u32,
    ) -> io::Result<Self> {
        let mut header = vec![0x1F, 0x8B, 8, if name.is_some() { 0x08 } else { 0 }];
        header.extend_from_slice(&mtime.to_le_bytes());
        // Extra flags as flate2 sets them: 2 = best compression, 4 = fastest.
        header.push(match level {
            9.. => 2,
            ..=1 => 4,
            _ => 0,
        });
        header.push(255); // OS unknown, as flate2 writes it
        if let Some(name) = name {
            header.extend_from_slice(name.as_bytes());
            header.push(0);
        }
        out.write_all(&header)?;
        Ok(Self {
            deflate: ParDeflate::new(out, level, threads as usize),
        })
    }

    fn finish(self) -> io::Result<W> {
        let (mut out, s) = self.deflate.finish()?;
        out.write_all(&s.crc32.to_le_bytes())?;
        out.write_all(&(s.input_size as u32).to_le_bytes())?;
        Ok(out)
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct GzipHeader {
    pub name: Option<Vec<u8>>,
    pub mtime: u32,
}

/// Parses the first gzip member header (RFC 1952) from the start of a file.
pub fn parse_gzip_header(head: &[u8]) -> Option<GzipHeader> {
    if head.len() < 10 || head[0] != 0x1F || head[1] != 0x8B || head[2] != 8 {
        return None;
    }
    let flags = head[3];
    let mtime = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
    let mut pos = 10usize;
    if flags & 0x04 != 0 {
        let xlen = usize::from(u16::from_le_bytes([*head.get(pos)?, *head.get(pos + 1)?]));
        pos += 2 + xlen;
    }
    let name = if flags & 0x08 != 0 {
        let rest = head.get(pos..)?;
        let end = rest.iter().position(|&b| b == 0)?;
        Some(rest[..end].to_vec())
    } else {
        None
    };
    Some(GzipHeader { name, mtime })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_codec_round_trips() {
        let data: Vec<u8> = b"OmniZip codec round trip. ".repeat(5000);
        for codec in [
            Codec::Gzip,
            Codec::Bzip2,
            Codec::Xz,
            Codec::Zstd,
            Codec::Lz4,
            Codec::Brotli,
        ] {
            for threads in [1, 4] {
                for level in [Level::Fastest, Level::Normal, Level::Ultra] {
                    let mut e =
                        Encoder::new(Some(codec), Vec::new(), level, threads, Some("x.bin"), 0)
                            .unwrap();
                    e.write_all(&data).unwrap();
                    let packed = e.finish().unwrap();
                    assert!(packed.len() < data.len() / 4, "{codec:?} compresses");
                    let mut back = Vec::new();
                    decoder(codec, packed.as_slice())
                        .unwrap()
                        .read_to_end(&mut back)
                        .unwrap();
                    assert_eq!(back, data, "{codec:?} {level:?} threads={threads}");
                }
            }
        }
    }

    #[test]
    fn gzip_header_name() {
        let gzip = |threads| {
            let mut e = Encoder::new(
                Some(Codec::Gzip),
                Vec::new(),
                Level::Normal,
                threads,
                Some("report.csv"),
                1_700_000_000,
            )
            .unwrap();
            e.write_all(b"a,b\n").unwrap();
            e.finish().unwrap()
        };
        let out = gzip(1);
        let h = parse_gzip_header(&out).unwrap();
        assert_eq!(h.name.as_deref(), Some(&b"report.csv"[..]));
        assert_eq!(h.mtime, 1_700_000_000);
        // Several threads write the same header.
        assert_eq!(gzip(4)[..21], out[..21]);
        assert_eq!(parse_gzip_header(b"nope"), None);
    }
}
