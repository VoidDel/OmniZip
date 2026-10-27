//! Archive format identifiers, capabilities and naming rules (`docs/03-formats.md`).

use std::fmt;

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

/// Every format OmniZip can recognize. Recognizing a format does not mean this build has an
/// engine for it; see [`crate::FormatRegistry::formats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[non_exhaustive]
pub enum FormatId {
    #[serde(rename = "zip")]
    Zip,
    #[serde(rename = "7z")]
    SevenZ,
    #[serde(rename = "rar")]
    Rar,
    #[serde(rename = "tar")]
    Tar,
    #[serde(rename = "tar.gz")]
    TarGz,
    #[serde(rename = "tar.bz2")]
    TarBz2,
    #[serde(rename = "tar.xz")]
    TarXz,
    #[serde(rename = "tar.zst")]
    TarZst,
    #[serde(rename = "tar.lz4")]
    TarLz4,
    #[serde(rename = "tar.br")]
    TarBr,
    #[serde(rename = "gz")]
    Gz,
    #[serde(rename = "bz2")]
    Bz2,
    #[serde(rename = "xz")]
    Xz,
    #[serde(rename = "lzma")]
    Lzma,
    #[serde(rename = "zst")]
    Zst,
    #[serde(rename = "lz4")]
    Lz4,
    #[serde(rename = "br")]
    Br,
    #[serde(rename = "iso")]
    Iso,
    #[serde(rename = "cab")]
    Cab,
    #[serde(rename = "cpio")]
    Cpio,
    #[serde(rename = "ar")]
    Ar,
    #[serde(rename = "deb")]
    Deb,
    #[serde(rename = "rpm")]
    Rpm,
    #[serde(rename = "xar")]
    Xar,
    #[serde(rename = "arj")]
    Arj,
    #[serde(rename = "lzh")]
    Lzh,
    #[serde(rename = "wim")]
    Wim,
}

/// Single-stream codecs used by `tar.*` and standalone compressed files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Gzip,
    Bzip2,
    Xz,
    Lzma,
    Zstd,
    Lz4,
    Brotli,
}

impl Codec {
    /// Display name of the method.
    pub const fn method_name(self) -> &'static str {
        match self {
            Codec::Gzip => "Deflate",
            Codec::Bzip2 => "BZip2",
            Codec::Xz => "XZ",
            Codec::Lzma => "LZMA",
            Codec::Zstd => "Zstandard",
            Codec::Lz4 => "LZ4",
            Codec::Brotli => "Brotli",
        }
    }
}

/// How well a format supports random access when mounted (`docs/03-formats.md` §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MountClass {
    /// Stored data, native seeking.
    A,
    /// Per-entry streams: forward reads are cheap, rewinds need re-decoding or a disk cache.
    B,
    /// Solid blocks must be decoded from the block start.
    C,
    /// One stream for the whole archive; needs a seek index.
    D,
}

bitflags! {
    /// What an engine can do with a format.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct Capabilities: u32 {
        const READ            = 1 << 0;
        const WRITE           = 1 << 1;
        const UPDATE          = 1 << 2;
        const ENCRYPT         = 1 << 3;
        const ENCRYPT_NAMES   = 1 << 4;
        const MULTI_VOLUME    = 1 << 5;
        const SOLID           = 1 << 6;
        const COMMENT         = 1 << 7;
        const STREAMING       = 1 << 8;
        const MOUNT           = 1 << 9;
        /// Holds exactly one file (gz, xz, …).
        const SINGLE_FILE     = 1 << 10;
        /// Can decrypt encrypted content.
        const DECRYPT         = 1 << 11;
    }
}

impl Capabilities {
    /// Human-readable flag names, for `omnizip formats`.
    pub fn names(self) -> Vec<&'static str> {
        self.iter_names().map(|(n, _)| n).collect()
    }
}

/// Support tier promised by the project (`docs/03-formats.md` §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tier {
    One = 1,
    Two = 2,
    Three = 3,
}

const ALL: &[FormatId] = &[
    FormatId::Zip,
    FormatId::SevenZ,
    FormatId::Rar,
    FormatId::Tar,
    FormatId::TarGz,
    FormatId::TarBz2,
    FormatId::TarXz,
    FormatId::TarZst,
    FormatId::TarLz4,
    FormatId::TarBr,
    FormatId::Gz,
    FormatId::Bz2,
    FormatId::Xz,
    FormatId::Lzma,
    FormatId::Zst,
    FormatId::Lz4,
    FormatId::Br,
    FormatId::Iso,
    FormatId::Cab,
    FormatId::Cpio,
    FormatId::Ar,
    FormatId::Deb,
    FormatId::Rpm,
    FormatId::Xar,
    FormatId::Arj,
    FormatId::Lzh,
    FormatId::Wim,
];

impl FormatId {
    /// All known formats.
    pub const fn all() -> &'static [FormatId] {
        ALL
    }

    /// Short machine name, identical to the serde form (`7z`, `tar.zst`).
    pub const fn name(self) -> &'static str {
        match self {
            FormatId::Zip => "zip",
            FormatId::SevenZ => "7z",
            FormatId::Rar => "rar",
            FormatId::Tar => "tar",
            FormatId::TarGz => "tar.gz",
            FormatId::TarBz2 => "tar.bz2",
            FormatId::TarXz => "tar.xz",
            FormatId::TarZst => "tar.zst",
            FormatId::TarLz4 => "tar.lz4",
            FormatId::TarBr => "tar.br",
            FormatId::Gz => "gz",
            FormatId::Bz2 => "bz2",
            FormatId::Xz => "xz",
            FormatId::Lzma => "lzma",
            FormatId::Zst => "zst",
            FormatId::Lz4 => "lz4",
            FormatId::Br => "br",
            FormatId::Iso => "iso",
            FormatId::Cab => "cab",
            FormatId::Cpio => "cpio",
            FormatId::Ar => "ar",
            FormatId::Deb => "deb",
            FormatId::Rpm => "rpm",
            FormatId::Xar => "xar",
            FormatId::Arj => "arj",
            FormatId::Lzh => "lzh",
            FormatId::Wim => "wim",
        }
    }

    /// Upper-case badge label (`7Z`, `TAR.ZST`).
    pub fn label(self) -> String {
        self.name().to_ascii_uppercase()
    }

    /// Parses a machine name or a common alias (`tgz`, `7zip`, `gzip`).
    pub fn parse(s: &str) -> Option<FormatId> {
        let s = s.trim().trim_start_matches('.').to_ascii_lowercase();
        let alias = match s.as_str() {
            "7zip" | "7-zip" => "7z",
            "tgz" | "targz" => "tar.gz",
            "tbz" | "tbz2" | "tarbz2" => "tar.bz2",
            "txz" | "tarxz" => "tar.xz",
            "tzst" | "tarzst" | "tar.zstd" => "tar.zst",
            "tlz4" => "tar.lz4",
            "gzip" => "gz",
            "bzip2" => "bz2",
            "zstd" => "zst",
            "brotli" => "br",
            "img" => "iso",
            other => other,
        };
        ALL.iter().copied().find(|f| f.name() == alias)
    }

    /// File-name suffixes, most specific first. The first one is used when creating archives.
    pub const fn extensions(self) -> &'static [&'static str] {
        match self {
            FormatId::Zip => &[
                ".zip", ".zipx", ".jar", ".war", ".ear", ".apk", ".ipa", ".xpi", ".epub", ".cbz",
                ".nupkg", ".whl", ".vsix",
            ],
            FormatId::SevenZ => &[".7z"],
            FormatId::Rar => &[".rar", ".cbr"],
            FormatId::Tar => &[".tar"],
            FormatId::TarGz => &[".tar.gz", ".tgz", ".taz"],
            FormatId::TarBz2 => &[".tar.bz2", ".tbz2", ".tbz", ".tb2"],
            FormatId::TarXz => &[".tar.xz", ".txz"],
            FormatId::TarZst => &[".tar.zst", ".tzst", ".tar.zstd"],
            FormatId::TarLz4 => &[".tar.lz4", ".tlz4"],
            FormatId::TarBr => &[".tar.br"],
            FormatId::Gz => &[".gz", ".gzip"],
            FormatId::Bz2 => &[".bz2", ".bzip2"],
            FormatId::Xz => &[".xz"],
            FormatId::Lzma => &[".lzma"],
            FormatId::Zst => &[".zst", ".zstd"],
            FormatId::Lz4 => &[".lz4"],
            FormatId::Br => &[".br"],
            FormatId::Iso => &[".iso", ".img"],
            FormatId::Cab => &[".cab"],
            FormatId::Cpio => &[".cpio"],
            FormatId::Ar => &[".a", ".ar", ".lib"],
            FormatId::Deb => &[".deb", ".udeb"],
            FormatId::Rpm => &[".rpm"],
            FormatId::Xar => &[".xar", ".pkg"],
            FormatId::Arj => &[".arj"],
            FormatId::Lzh => &[".lzh", ".lha"],
            FormatId::Wim => &[".wim", ".swm", ".esd"],
        }
    }

    /// Primary extension used when creating (`.tar.zst`).
    pub const fn primary_extension(self) -> &'static str {
        self.extensions()[0]
    }

    /// Guesses the format from a file name using the longest matching suffix. Multi-volume
    /// suffixes such as `.7z.001` and `.part1.rar` are understood.
    pub fn from_file_name(name: &str) -> Option<FormatId> {
        let lower = name.to_ascii_lowercase();
        let base = strip_volume_suffix(&lower);
        let mut best: Option<(usize, FormatId)> = None;
        for f in ALL {
            for ext in f.extensions() {
                if base.ends_with(ext) && best.is_none_or(|(len, _)| ext.len() > len) {
                    best = Some((ext.len(), *f));
                }
            }
        }
        best.map(|(_, f)| f)
    }

    /// The codec of a `tar.*` or single-file format.
    pub const fn codec(self) -> Option<Codec> {
        match self {
            FormatId::TarGz | FormatId::Gz => Some(Codec::Gzip),
            FormatId::TarBz2 | FormatId::Bz2 => Some(Codec::Bzip2),
            FormatId::TarXz | FormatId::Xz => Some(Codec::Xz),
            FormatId::Lzma => Some(Codec::Lzma),
            FormatId::TarZst | FormatId::Zst => Some(Codec::Zstd),
            FormatId::TarLz4 | FormatId::Lz4 => Some(Codec::Lz4),
            FormatId::TarBr | FormatId::Br => Some(Codec::Brotli),
            _ => None,
        }
    }

    /// Whether this is a tar container (plain or compressed).
    pub const fn is_tar(self) -> bool {
        matches!(
            self,
            FormatId::Tar
                | FormatId::TarGz
                | FormatId::TarBz2
                | FormatId::TarXz
                | FormatId::TarZst
                | FormatId::TarLz4
                | FormatId::TarBr
        )
    }

    /// Whether this is a single compressed stream holding one file.
    pub const fn is_single_file(self) -> bool {
        matches!(
            self,
            FormatId::Gz
                | FormatId::Bz2
                | FormatId::Xz
                | FormatId::Lzma
                | FormatId::Zst
                | FormatId::Lz4
                | FormatId::Br
        )
    }

    /// The `tar.*` variant for a single-file codec format (`Gz` → `TarGz`).
    pub const fn as_tar(self) -> Option<FormatId> {
        match self {
            FormatId::Gz => Some(FormatId::TarGz),
            FormatId::Bz2 => Some(FormatId::TarBz2),
            FormatId::Xz => Some(FormatId::TarXz),
            FormatId::Zst => Some(FormatId::TarZst),
            FormatId::Lz4 => Some(FormatId::TarLz4),
            FormatId::Br => Some(FormatId::TarBr),
            _ => None,
        }
    }

    /// Support tier.
    pub const fn tier(self) -> Tier {
        match self {
            FormatId::Zip
            | FormatId::SevenZ
            | FormatId::Tar
            | FormatId::TarGz
            | FormatId::TarBz2
            | FormatId::TarXz
            | FormatId::TarZst
            | FormatId::TarLz4
            | FormatId::TarBr
            | FormatId::Gz
            | FormatId::Bz2
            | FormatId::Xz
            | FormatId::Lzma
            | FormatId::Zst
            | FormatId::Lz4
            | FormatId::Br => Tier::One,
            FormatId::Rar
            | FormatId::Iso
            | FormatId::Cab
            | FormatId::Cpio
            | FormatId::Ar
            | FormatId::Deb
            | FormatId::Rpm
            | FormatId::Xar
            | FormatId::Arj
            | FormatId::Lzh => Tier::Two,
            FormatId::Wim => Tier::Three,
        }
    }

    /// Mount class (random-access quality) of the format.
    pub const fn mount_class(self, solid: bool) -> MountClass {
        match self {
            FormatId::Tar | FormatId::Iso | FormatId::Cpio | FormatId::Cab | FormatId::Ar => {
                MountClass::A
            }
            FormatId::Zip => MountClass::B,
            FormatId::SevenZ | FormatId::Rar if solid => MountClass::C,
            FormatId::SevenZ | FormatId::Rar => MountClass::B,
            _ => MountClass::D,
        }
    }

    /// Badge color (stable across themes, `docs/05-theming.md` §7).
    pub const fn badge_color(self) -> &'static str {
        match self {
            FormatId::Zip => "#2F6FED",
            FormatId::SevenZ => "#7A5AF8",
            FormatId::Rar => "#C0392B",
            FormatId::Iso => "#0E9F8E",
            FormatId::Tar
            | FormatId::TarGz
            | FormatId::TarBz2
            | FormatId::TarXz
            | FormatId::TarZst
            | FormatId::TarLz4
            | FormatId::TarBr => "#8A6D3B",
            _ => "#64748B",
        }
    }
}

impl fmt::Display for FormatId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Removes multi-volume suffixes: `x.7z.001` → `x.7z`, `x.part01.rar` → `x.rar`,
/// `x.r00` → `x.rar`, `x.z01` → `x.zip`. The input must already be lower-case.
pub fn strip_volume_suffix(lower: &str) -> String {
    // .001 style
    if let Some(dot) = lower.rfind('.') {
        let ext = &lower[dot + 1..];
        if ext.len() == 3 && ext.bytes().all(|b| b.is_ascii_digit()) {
            return lower[..dot].to_owned();
        }
        // .r00 – .r99 (old RAR volumes) and .z01 – .z99 (zip spans)
        if ext.len() == 3 && ext.as_bytes()[1..].iter().all(u8::is_ascii_digit) {
            match ext.as_bytes()[0] {
                b'r' => return format!("{}.rar", &lower[..dot]),
                b'z' => return format!("{}.zip", &lower[..dot]),
                _ => {}
            }
        }
    }
    // .partN.rar
    if let Some(stem) = lower.strip_suffix(".rar")
        && let Some(pos) = stem.rfind(".part")
        && stem[pos + 5..].bytes().all(|b| b.is_ascii_digit())
        && stem.len() > pos + 5
    {
        return format!("{}.rar", &stem[..pos]);
    }
    lower.to_owned()
}

/// Strips every known archive suffix (including volume suffixes) from a file name:
/// `backup-2026.tar.zst` → `backup-2026`, `movie.part1.rar` → `movie`.
pub fn archive_stem(file_name: &str) -> String {
    let lower = file_name.to_ascii_lowercase();
    let base = strip_volume_suffix(&lower);
    let mut best = 0;
    for f in ALL {
        for ext in f.extensions() {
            if base.ends_with(ext) && ext.len() > best {
                best = ext.len();
            }
        }
    }
    // Map the stripped length back onto the original string (volume suffix removal only ever
    // removes ASCII, so byte positions line up for the leading part).
    let keep = base.len().saturating_sub(best);
    let stem = &file_name[..keep.min(file_name.len())];
    if stem.is_empty() {
        file_name.to_owned()
    } else {
        stem.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_suffix_wins() {
        assert_eq!(FormatId::from_file_name("a.tar.gz"), Some(FormatId::TarGz));
        assert_eq!(FormatId::from_file_name("a.gz"), Some(FormatId::Gz));
        assert_eq!(FormatId::from_file_name("A.TGZ"), Some(FormatId::TarGz));
        assert_eq!(FormatId::from_file_name("x.7z.001"), Some(FormatId::SevenZ));
        assert_eq!(FormatId::from_file_name("x.part1.rar"), Some(FormatId::Rar));
        assert_eq!(FormatId::from_file_name("x.r00"), Some(FormatId::Rar));
        assert_eq!(FormatId::from_file_name("readme.txt"), None);
    }

    #[test]
    fn stems() {
        assert_eq!(archive_stem("backup-2026.tar.zst"), "backup-2026");
        assert_eq!(archive_stem("movie.part1.rar"), "movie");
        assert_eq!(archive_stem("photos.7z.001"), "photos");
        assert_eq!(archive_stem("中文.zip"), "中文");
        assert_eq!(archive_stem("noext"), "noext");
        assert_eq!(archive_stem(".zip"), ".zip");
    }

    #[test]
    fn parse_aliases() {
        assert_eq!(FormatId::parse("7zip"), Some(FormatId::SevenZ));
        assert_eq!(FormatId::parse("tgz"), Some(FormatId::TarGz));
        assert_eq!(FormatId::parse(".tar.zst"), Some(FormatId::TarZst));
        assert_eq!(FormatId::parse("nope"), None);
        for f in FormatId::all() {
            assert_eq!(FormatId::parse(f.name()), Some(*f));
            let json = serde_json::to_string(f).unwrap();
            assert_eq!(json, format!("\"{}\"", f.name()));
        }
    }
}
