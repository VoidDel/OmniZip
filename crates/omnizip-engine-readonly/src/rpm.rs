//! RPM packages (`docs/03-formats.md` §3): the lead, the signature and main headers (read for
//! the payload's compressor and the package name), then the payload, a compressed `newc` cpio
//! stream decoded from the start for every listing and extraction.

use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};

use omnizip_core::{
    ArchiveInfo, Codec, Error, ErrorCode, FormatId, JobCtx, OpenOptions, Result,
    volume::SharedInput,
};

use crate::{
    common::{Streamed, Walk, corrupt, read_at},
    cpio::{self, Cpio},
};

const LEAD_MAGIC: [u8; 4] = [0xED, 0xAB, 0xEE, 0xDB];
const HEADER_MAGIC: [u8; 3] = [0x8E, 0xAD, 0xE8];
/// Most index entries / header bytes accepted (rpm's own limits are 64 Ki / 256 MiB).
const MAX_INDEX: u32 = 1 << 16;
const MAX_STORE: u32 = 256 << 20;

const TAG_NAME: u32 = 1000;
const TAG_VERSION: u32 = 1001;
const TAG_RELEASE: u32 = 1002;
const TAG_PAYLOADFORMAT: u32 = 1124;
const TAG_PAYLOADCOMPRESSOR: u32 = 1125;
const TYPE_STRING: u32 = 6;

/// A header structure at `pos`: its string tags, and where it ends.
fn header(input: &mut SharedInput, pos: u64, path: &Path) -> Result<(Vec<(u32, String)>, u64)> {
    let h = read_at(input, pos, 16, path)?;
    if h[..3] != HEADER_MAGIC {
        return Err(corrupt("bad RPM header", path));
    }
    let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let count = be(&h[8..12]);
    let store_len = be(&h[12..16]);
    if count > MAX_INDEX || store_len > MAX_STORE {
        return Err(corrupt("RPM header too large", path));
    }
    let index = read_at(input, pos + 16, count as usize * 16, path)?;
    let store_at = pos + 16 + u64::from(count) * 16;
    let store = read_at(input, store_at, store_len as usize, path)?;
    let mut tags = Vec::new();
    for e in index.as_chunks::<16>().0 {
        let (tag, ty, offset) = (be(&e[0..4]), be(&e[4..8]), be(&e[8..12]) as usize);
        if ty == TYPE_STRING
            && let Some(rest) = store.get(offset..)
        {
            let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
            tags.push((tag, String::from_utf8_lossy(&rest[..end]).into_owned()));
        }
    }
    Ok((tags, store_at + u64::from(store_len)))
}

struct RpmWalk {
    input: SharedInput,
    payload: u64,
    codec: Codec,
    path: PathBuf,
}

impl RpmWalk {
    fn cpio(&self) -> Result<Cpio<Box<dyn Read + Send>>> {
        let mut input = self.input.clone();
        input
            .seek(SeekFrom::Start(self.payload))
            .map_err(|e| Error::io(e, &self.path))?;
        let stream = omnizip_engine_tar::codec::decoder(self.codec, input)
            .map_err(|e| corrupt(e, &self.path))?;
        Ok(Cpio::new(stream))
    }
}

impl Walk for RpmWalk {
    fn walk(
        &self,
        ctx: &JobCtx,
        f: &mut dyn FnMut(usize, &mut dyn Read) -> Result<bool>,
    ) -> Result<()> {
        let mut cpio = self.cpio()?;
        let mut ord = 0;
        while cpio.next().map_err(|e| corrupt(e, &self.path))?.is_some() {
            ctx.check()?;
            let mut data = cpio.data();
            let go = f(ord, &mut data).map_err(|e| {
                // A payload that ends early fails inside the sink's copy.
                if e.code() == ErrorCode::Io {
                    corrupt(e, &self.path)
                } else {
                    e
                }
            })?;
            if !go {
                break;
            }
            ord += 1;
        }
        Ok(())
    }
}

pub(crate) fn open(path: &Path, opts: &OpenOptions, ctx: &JobCtx) -> Result<Streamed> {
    let mut input = SharedInput::open(path)?;
    let size = input.len();
    if read_at(&mut input, 0, 4, path)? != LEAD_MAGIC {
        return Err(corrupt("not an RPM package", path));
    }
    // The signature header follows the 96-byte lead and is padded to 8 bytes.
    let (_, sig_end) = header(&mut input, 96, path)?;
    let main = sig_end.div_ceil(8) * 8;
    let (tags, payload) = header(&mut input, main, path)?;
    let tag = |t: u32| tags.iter().find(|(k, _)| *k == t).map(|(_, v)| v.as_str());
    if let Some(f) = tag(TAG_PAYLOADFORMAT)
        && f != "cpio"
    {
        return Err(Error::new(ErrorCode::UnsupportedMethod)
            .with("method", f)
            .with("path", path.display()));
    }
    let codec = match tag(TAG_PAYLOADCOMPRESSOR).unwrap_or("gzip") {
        "gzip" => Codec::Gzip,
        "bzip2" => Codec::Bzip2,
        "xz" => Codec::Xz,
        "lzma" => Codec::Lzma,
        "zstd" => Codec::Zstd,
        other => {
            return Err(Error::new(ErrorCode::UnsupportedMethod)
                .with("method", other)
                .with("path", path.display()));
        }
    };
    let mut info = ArchiveInfo::new(path, FormatId::Rpm, size);
    info.add_method(codec.method_name());
    if let (Some(n), Some(v), Some(r)) = (tag(TAG_NAME), tag(TAG_VERSION), tag(TAG_RELEASE)) {
        info.comment = Some(format!("{n}-{v}-{r}"));
    }
    let walker = RpmWalk {
        input,
        payload,
        codec,
        path: path.to_owned(),
    };
    let listed = cpio::list(&mut walker.cpio()?, path, ctx)?;
    let (entries, ids, enc) = cpio::entries(&listed, opts);
    info.encoding = enc;
    Ok(Streamed {
        info,
        entries: entries.into(),
        ordinals: ids.into(),
        walker: Arc::new(walker),
    })
}
