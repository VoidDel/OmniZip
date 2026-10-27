//! File-name encoding detection and decoding (`docs/02-architecture.md` §4.3).
//!
//! Old ZIP and RAR4 archives store names in the creator's legacy code page without saying which.
//! OmniZip looks at *all* names of an archive together, which gives the detector far more signal
//! than a single short name.

use std::borrow::Cow;

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;
use serde::{Deserialize, Serialize};

/// An encoding for archive entry names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameEncoding {
    /// UTF-8 (declared or detected).
    Utf8,
    /// IBM PC code page 437, the ZIP default when no UTF-8 flag is set.
    Cp437,
    /// Any encoding supported by `encoding_rs` (GBK/GB18030, Big5, Shift_JIS, EUC-KR, …).
    Other(&'static Encoding),
}

impl NameEncoding {
    /// Stable label used in settings and the CLI (`utf-8`, `cp437`, `gb18030`, `shift_jis`, …).
    pub fn label(self) -> &'static str {
        match self {
            NameEncoding::Utf8 => "utf-8",
            NameEncoding::Cp437 => "cp437",
            NameEncoding::Other(e) if e == encoding_rs::GBK || e == encoding_rs::GB18030 => {
                "gb18030"
            }
            NameEncoding::Other(e) if e == encoding_rs::IBM866 => "cp866",
            NameEncoding::Other(e) => {
                // encoding_rs names are canonical WHATWG names (e.g. "Shift_JIS").
                LABELS
                    .iter()
                    .find(|(_, enc)| *enc == e)
                    .map_or(e.name(), |(l, _)| l)
            }
        }
    }

    /// Parses a label. Accepts common aliases (`gbk`, `sjis`, `cp932`, `big5`).
    pub fn from_label(label: &str) -> Option<NameEncoding> {
        let l = label.trim().to_ascii_lowercase().replace('_', "-");
        match l.as_str() {
            "utf-8" | "utf8" => return Some(NameEncoding::Utf8),
            "cp437" | "ibm437" | "437" | "dos" => return Some(NameEncoding::Cp437),
            "auto" => return None,
            _ => {}
        }
        if let Some((_, e)) = LABELS.iter().find(|(name, _)| name.replace('_', "-") == l) {
            return Some(NameEncoding::Other(e));
        }
        Encoding::for_label(l.as_bytes()).map(|e| {
            if e == encoding_rs::UTF_8 {
                NameEncoding::Utf8
            } else {
                NameEncoding::Other(e)
            }
        })
    }

    /// Decodes raw name bytes. Invalid sequences become U+FFFD.
    pub fn decode(self, raw: &[u8]) -> String {
        match self {
            NameEncoding::Utf8 => String::from_utf8_lossy(raw).into_owned(),
            NameEncoding::Cp437 => decode_cp437(raw),
            NameEncoding::Other(e) => {
                let (s, _, _) = e.decode(raw);
                s.into_owned()
            }
        }
    }

    /// Encodes a name for writing (used only when the user forces a legacy encoding).
    pub fn encode(self, name: &str) -> Cow<'_, [u8]> {
        match self {
            NameEncoding::Utf8 | NameEncoding::Cp437 => Cow::Borrowed(name.as_bytes()),
            NameEncoding::Other(e) => {
                let (bytes, _, _) = e.encode(name);
                match bytes {
                    Cow::Borrowed(b) => Cow::Borrowed(b),
                    Cow::Owned(v) => Cow::Owned(v),
                }
            }
        }
    }
}

impl Serialize for NameEncoding {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.label())
    }
}

impl<'de> Deserialize<'de> for NameEncoding {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        NameEncoding::from_label(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown encoding {s}")))
    }
}

/// Encodings offered in the UI "filename encoding" menu, in display order.
pub const LABELS: &[(&str, &Encoding)] = &[
    ("gb18030", encoding_rs::GB18030),
    ("big5", encoding_rs::BIG5),
    ("shift_jis", encoding_rs::SHIFT_JIS),
    ("euc-jp", encoding_rs::EUC_JP),
    ("euc-kr", encoding_rs::EUC_KR),
    ("cp866", encoding_rs::IBM866),
    ("windows-1250", encoding_rs::WINDOWS_1250),
    ("windows-1251", encoding_rs::WINDOWS_1251),
    ("windows-1252", encoding_rs::WINDOWS_1252),
    ("windows-1253", encoding_rs::WINDOWS_1253),
    ("windows-1254", encoding_rs::WINDOWS_1254),
    ("windows-1255", encoding_rs::WINDOWS_1255),
    ("windows-1256", encoding_rs::WINDOWS_1256),
    ("windows-1257", encoding_rs::WINDOWS_1257),
    ("windows-1258", encoding_rs::WINDOWS_1258),
    ("windows-874", encoding_rs::WINDOWS_874),
    ("koi8-r", encoding_rs::KOI8_R),
    ("iso-8859-2", encoding_rs::ISO_8859_2),
    ("iso-8859-5", encoding_rs::ISO_8859_5),
    ("iso-8859-7", encoding_rs::ISO_8859_7),
];

/// Every selectable label, including `utf-8` and `cp437`.
pub fn all_labels() -> Vec<&'static str> {
    let mut v = vec!["utf-8", "cp437"];
    v.extend(LABELS.iter().map(|(l, _)| *l));
    v
}

/// Maps a BCP 47 locale (`zh-CN`) to the top-level domain hint chardetng understands.
fn tld_for_locale(locale: &str) -> Option<&'static [u8]> {
    let l = locale.to_ascii_lowercase();
    let (lang, region) = l.split_once(['-', '_']).unwrap_or((&l, ""));
    Some(match (lang, region) {
        ("zh", "tw" | "hk" | "mo") | ("zh", "hant") => b"tw",
        ("zh", _) => b"cn",
        ("ja", _) => b"jp",
        ("ko", _) => b"kr",
        ("ru" | "uk" | "be" | "bg" | "sr", _) => b"ru",
        ("el", _) => b"gr",
        ("tr", _) => b"tr",
        ("he", _) => b"il",
        ("ar" | "fa", _) => b"sa",
        ("th", _) => b"th",
        ("vi", _) => b"vn",
        ("pl" | "cs" | "sk" | "hu" | "ro" | "hr" | "sl", _) => b"pl",
        _ => return None,
    })
}

/// The legacy code page a system in this locale most likely used for archive names.
pub fn default_for_locale(locale: &str) -> NameEncoding {
    match tld_for_locale(locale) {
        Some(b"cn") => NameEncoding::Other(encoding_rs::GB18030),
        Some(b"tw") => NameEncoding::Other(encoding_rs::BIG5),
        Some(b"jp") => NameEncoding::Other(encoding_rs::SHIFT_JIS),
        Some(b"kr") => NameEncoding::Other(encoding_rs::EUC_KR),
        Some(b"ru") => NameEncoding::Other(encoding_rs::IBM866),
        _ => NameEncoding::Cp437,
    }
}

/// Detects the encoding of a set of raw names that are *not* declared as UTF-8.
///
/// * All names valid UTF-8 → UTF-8 (many tools omit the flag).
/// * Pure ASCII → UTF-8 (the choice does not matter).
/// * Otherwise chardetng over the concatenated names, with the system locale as tie-breaker.
pub fn detect<'a>(names: impl IntoIterator<Item = &'a [u8]>, locale: Option<&str>) -> NameEncoding {
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    let mut all_utf8 = true;
    let mut any_non_ascii = false;
    let mut fed = 0usize;
    for name in names {
        if name.is_ascii() {
            continue;
        }
        any_non_ascii = true;
        if std::str::from_utf8(name).is_err() {
            all_utf8 = false;
        }
        // Names are fed separated by a slash so byte pairs never straddle two names.
        if fed < 1 << 20 {
            detector.feed(name, false);
            detector.feed(b"/", false);
            fed += name.len() + 1;
        }
    }
    if !any_non_ascii || all_utf8 {
        return NameEncoding::Utf8;
    }
    detector.feed(b"", true);
    let tld = locale.and_then(tld_for_locale);
    let guess = detector.guess(tld, Utf8Detection::Deny);
    // chardetng never answers IBM866/CP437; when it falls back to windows-1252 for a locale with
    // a well-known DOS code page, prefer the locale default.
    if guess == encoding_rs::WINDOWS_1252 {
        let fallback = locale
            .map(default_for_locale)
            .unwrap_or(NameEncoding::Cp437);
        if fallback != NameEncoding::Cp437 {
            return fallback;
        }
        return NameEncoding::Cp437;
    }
    if guess == encoding_rs::GBK {
        return NameEncoding::Other(encoding_rs::GB18030);
    }
    NameEncoding::Other(guess)
}

/// Decodes IBM code page 437 (not part of `encoding_rs`, which only covers WHATWG encodings).
pub fn decode_cp437(raw: &[u8]) -> String {
    raw.iter()
        .map(|&b| {
            if b < 0x80 {
                b as char
            } else {
                CP437_HIGH[(b - 0x80) as usize]
            }
        })
        .collect()
}

const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ', 'Æ',
    'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú', 'ñ', 'Ñ',
    'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕',
    '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦',
    '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐',
    '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', '≡', '±',
    '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(e: &'static Encoding, s: &str) -> Vec<u8> {
        e.encode(s).0.into_owned()
    }

    #[test]
    fn utf8_and_ascii() {
        let names = [b"readme.txt".as_slice(), "说明.txt".as_bytes()];
        assert_eq!(detect(names, Some("zh-CN")), NameEncoding::Utf8);
        assert_eq!(detect([b"a/b.txt".as_slice()], None), NameEncoding::Utf8);
    }

    #[test]
    fn detects_gbk_names() {
        let names: Vec<Vec<u8>> = [
            "项目文档/需求说明书.docx",
            "项目文档/会议纪要.txt",
            "图片/截图一.png",
        ]
        .iter()
        .map(|s| enc(encoding_rs::GBK, s))
        .collect();
        let got = detect(names.iter().map(Vec::as_slice), Some("zh-CN"));
        assert_eq!(got.label(), "gb18030");
        assert_eq!(got.decode(&names[0]), "项目文档/需求说明书.docx");
    }

    #[test]
    fn detects_shift_jis_names() {
        let names: Vec<Vec<u8>> = ["資料/会議の議事録.txt", "写真/東京タワー.jpg", "説明書.pdf"]
            .iter()
            .map(|s| enc(encoding_rs::SHIFT_JIS, s))
            .collect();
        let got = detect(names.iter().map(Vec::as_slice), Some("ja-JP"));
        assert_eq!(got, NameEncoding::Other(encoding_rs::SHIFT_JIS));
    }

    #[test]
    fn detects_big5_names() {
        let names: Vec<Vec<u8>> = ["專案資料/會議紀錄.txt", "照片/臺北夜景.jpg", "說明文件.pdf"]
            .iter()
            .map(|s| enc(encoding_rs::BIG5, s))
            .collect();
        let got = detect(names.iter().map(Vec::as_slice), Some("zh-TW"));
        assert_eq!(got, NameEncoding::Other(encoding_rs::BIG5));
    }

    #[test]
    fn cp437_decoding() {
        assert_eq!(decode_cp437(b"caf\x82"), "café");
        assert_eq!(NameEncoding::Cp437.decode(&[0x80, b'a']), "Ça");
    }

    #[test]
    fn labels_round_trip() {
        for l in all_labels() {
            let e = NameEncoding::from_label(l).unwrap_or_else(|| panic!("label {l}"));
            assert_eq!(e.label(), l, "label {l}");
        }
        assert_eq!(NameEncoding::from_label("GBK").unwrap().label(), "gb18030");
        assert_eq!(
            NameEncoding::from_label("sjis").unwrap().label(),
            "shift_jis"
        );
        assert_eq!(NameEncoding::from_label("auto"), None);
    }
}
