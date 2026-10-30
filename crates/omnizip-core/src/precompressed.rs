//! Files whose content is compressed already (`docs/03-formats.md` §6.1, "fast archiving"):
//! photos, audio, video, archives and documents that are ZIP files inside. Compressing them
//! again costs much time for next to nothing, so writers store them as they are when
//! [`WriteOptions::skip_compressed`](crate::WriteOptions::skip_compressed) is on.
//!
//! A known extension decides at once. A larger file with another extension is judged by
//! samples of its content compressed at the fastest level.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use flate2::{Compress, Compression, FlushCompress};

use crate::{Source, SourceKind};

/// Extensions (lower case) of formats whose content is compressed, by kind.
const EXTENSIONS: &[&str] = &[
    // Images.
    "jpg jpeg jpe jfif png gif webp heic heif avif jxl jp2 j2k",
    // Audio.
    "mp3 aac m4a m4b ogg oga opus flac wma ape wv mpc spx amr",
    // Video.
    "mp4 m4v mkv webm avi mov wmv asf flv f4v 3gp 3g2 mts m2ts mpg mpeg vob rm rmvb ogv divx",
    // Archives.
    "zip 7z rar gz tgz bz2 tbz tbz2 xz txz lzma tlz lz zst tzst lz4 br z cab arj lzh lha zpaq dmg deb rpm sit sitx",
    // Packages and documents that are ZIP files.
    "jar war ear apk aab ipa xpi crx nupkg vsix appx msix appxbundle msixbundle whl",
    "docx docm xlsx xlsm pptx pptm odt ods odp odg epub cbz cbr cb7 kmz 3mf xps oxps",
    // Fonts.
    "woff woff2",
];

/// Files with other extensions below this size are compressed without a look at their
/// content: compressing them takes little time either way.
const SAMPLE_FROM: u64 = 1 << 20;
/// Bytes compressed from each of the start, the middle and the end of a larger file.
const SAMPLE: usize = 64 << 10;
/// Samples the fastest deflate shrinks by less than this fraction mark compressed content.
const MIN_SAVING: f64 = 0.02;

/// Whether a file name has the extension of a compressed format.
pub fn by_name(name: &str) -> bool {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    name.rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .is_some_and(|(_, ext)| {
            EXTENSIONS
                .iter()
                .flat_map(|kind| kind.split(' '))
                .any(|e| e.eq_ignore_ascii_case(ext))
        })
}

/// Whether `s` is a file to store rather than compress: one with the extension of a compressed
/// format, or a large one whose samples hardly compress. Directories, links and empty files
/// never are.
pub fn is_precompressed(s: &Source) -> bool {
    if s.kind != SourceKind::File || s.size == 0 {
        return false;
    }
    by_name(&s.archive_path) || s.size >= SAMPLE_FROM && samples_incompressible(&s.fs_path, s.size)
}

/// Compresses up to three samples of the file; a file that cannot be read is compressed as
/// usual (reading it then reports the error).
fn samples_incompressible(path: &Path, size: u64) -> bool {
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let sample = SAMPLE as u64;
    let mut at = vec![
        0,
        size.saturating_sub(sample) / 2,
        size.saturating_sub(sample),
    ];
    at.dedup();
    let mut buf = vec![0; SAMPLE];
    let mut out = vec![0; SAMPLE + SAMPLE / 8 + 1024];
    let mut z = Compress::new(Compression::fast(), false);
    let (mut read, mut written) = (0u64, 0u64);
    for pos in at {
        let n = match f
            .seek(SeekFrom::Start(pos))
            .and_then(|_| read_up_to(&mut f, &mut buf))
        {
            Ok(n) => n,
            Err(_) => return false,
        };
        z.reset();
        if z.compress(&buf[..n], &mut out, FlushCompress::Finish)
            .is_err()
        {
            return false;
        }
        read += n as u64;
        written += z.total_out();
    }
    read > 0 && (written as f64) > (read as f64) * (1.0 - MIN_SAVING)
}

fn read_up_to(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for n in [
            "a.jpg",
            "dir/b.MP4",
            "c.tar.gz",
            "d.docx",
            r"e\f.7Z",
            "photo.HEIC",
        ] {
            assert!(by_name(n), "{n}");
        }
        for n in ["a.txt", "b.tar", "jpg", ".png", "c.jpg.txt", "d.iso", "e"] {
            assert!(!by_name(n), "{n}");
        }
    }

    #[test]
    fn content() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, data: &[u8]| {
            let p = dir.path().join(name);
            std::fs::write(&p, data).unwrap();
            Source::file(&p, name).unwrap()
        };
        // Pseudo-random bytes (xorshift) stand for compressed content.
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let noise: Vec<u8> = (0..2 << 20)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let text = b"the quick brown fox jumps over the lazy dog. ".repeat(50_000);
        assert!(is_precompressed(&file("noise.bin", &noise)));
        assert!(!is_precompressed(&file("text.bin", &text)));
        // Small files are judged by name only.
        assert!(!is_precompressed(&file("small.bin", &noise[..4096])));
        assert!(is_precompressed(&file("small.jpg", b"not really a jpeg")));
        assert!(!is_precompressed(&file("empty.jpg", b"")));
    }
}
