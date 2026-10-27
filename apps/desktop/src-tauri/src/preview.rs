//! Entry previews (`docs/03-formats.md` §9). Images are decoded in Rust and re-encoded as PNG so
//! the WebView never parses untrusted image bytes; text is decoded and sent as plain text.

use std::io::Cursor;

use base64::Engine as _;
use serde::Serialize;

/// Bytes read for a preview.
pub const MAX_PREVIEW_BYTES: usize = 8 << 20;
const MAX_TEXT_CHARS: usize = 2 << 20;
const MAX_IMAGE_SIDE: u32 = 16_384;
const THUMB_SIDE: u32 = 2048;

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Preview {
    Text {
        text: String,
        truncated: bool,
        encoding: String,
        language: Option<String>,
    },
    Image {
        data_url: String,
        width: u32,
        height: u32,
    },
    Binary {
        hex: String,
        truncated: bool,
    },
    Unsupported,
}

const TEXT_EXT: &[&str] = &[
    "txt",
    "md",
    "markdown",
    "rst",
    "log",
    "csv",
    "tsv",
    "ini",
    "cfg",
    "conf",
    "toml",
    "yaml",
    "yml",
    "json",
    "xml",
    "html",
    "htm",
    "css",
    "scss",
    "less",
    "js",
    "mjs",
    "cjs",
    "ts",
    "tsx",
    "jsx",
    "vue",
    "svelte",
    "rs",
    "go",
    "py",
    "rb",
    "java",
    "kt",
    "kts",
    "swift",
    "c",
    "h",
    "cc",
    "cpp",
    "hpp",
    "cs",
    "php",
    "pl",
    "lua",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "bat",
    "cmd",
    "sql",
    "gradle",
    "properties",
    "env",
    "gitignore",
    "gitattributes",
    "editorconfig",
    "dockerfile",
    "makefile",
    "srt",
    "ass",
    "tex",
    "lock",
    "license",
    "readme",
    "ftl",
    "svg",
];
const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"];

fn ext_of(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    match lower.rsplit_once('.') {
        Some((_, e)) => e.to_owned(),
        None => lower, // README, Makefile, LICENSE
    }
}

fn language_of(ext: &str) -> Option<String> {
    let l = match ext {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "json" => "json",
        "md" | "markdown" => "markdown",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "html" | "htm" | "xml" | "svg" | "vue" => "markup",
        "css" | "scss" | "less" => "css",
        "py" => "python",
        "c" | "h" | "cc" | "cpp" | "hpp" => "c",
        "sh" | "bash" | "zsh" => "shell",
        _ => return None,
    };
    Some(l.to_owned())
}

/// Decodes text: BOMs, UTF-8, then a detected legacy encoding.
fn decode_text(bytes: &[u8]) -> (String, String) {
    if let Some((enc, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        let (s, _, _) = enc.decode(&bytes[bom_len..]);
        return (s.into_owned(), enc.name().to_owned());
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_owned(), "UTF-8".to_owned()),
        // A multi-byte character cut by the preview limit is still UTF-8.
        Err(e) if e.error_len().is_none() => (
            String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned(),
            "UTF-8".to_owned(),
        ),
        Err(_) => {
            let mut d = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
            d.feed(bytes, true);
            let enc = d.guess(None, chardetng::Utf8Detection::Allow);
            let (s, _, _) = enc.decode(bytes);
            (s.into_owned(), enc.name().to_owned())
        }
    }
}

fn looks_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(8192)];
    sample.contains(&0) && encoding_rs::Encoding::for_bom(sample).is_none()
}

fn hexdump(bytes: &[u8]) -> String {
    let mut out = String::new();
    for (i, chunk) in bytes.chunks(16).take(256).enumerate() {
        out.push_str(&format!("{:08X}  ", i * 16));
        for j in 0..16 {
            match chunk.get(j) {
                Some(b) => out.push_str(&format!("{b:02X} ")),
                None => out.push_str("   "),
            }
            if j == 7 {
                out.push(' ');
            }
        }
        out.push(' ');
        out.extend(chunk.iter().map(|&b| {
            if (0x20..0x7F).contains(&b) {
                b as char
            } else {
                '.'
            }
        }));
        out.push('\n');
    }
    out
}

fn image_preview(bytes: &[u8]) -> Option<Preview> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(512 << 20);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let (width, height) = (img.width(), img.height());
    let img = if width > THUMB_SIDE || height > THUMB_SIDE {
        img.thumbnail(THUMB_SIDE, THUMB_SIDE)
    } else {
        img
    };
    let mut png = Vec::new();
    img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    let data_url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    );
    Some(Preview::Image {
        data_url,
        width,
        height,
    })
}

/// Builds a preview from the first bytes of an entry.
pub fn build(name: &str, bytes: &[u8], truncated: bool) -> Preview {
    let ext = ext_of(name);
    if IMAGE_EXT.contains(&ext.as_str())
        && !truncated
        && let Some(p) = image_preview(bytes)
    {
        return p;
    }
    let texty = TEXT_EXT.contains(&ext.as_str()) || !looks_binary(bytes);
    if texty && !looks_binary(bytes) {
        let (mut text, encoding) = decode_text(bytes);
        let mut cut = truncated;
        if text.chars().count() > MAX_TEXT_CHARS {
            text = text.chars().take(MAX_TEXT_CHARS).collect();
            cut = true;
        }
        return Preview::Text {
            text,
            truncated: cut,
            encoding,
            language: language_of(&ext),
        };
    }
    if bytes.is_empty() {
        return Preview::Text {
            text: String::new(),
            truncated: false,
            encoding: "UTF-8".into(),
            language: None,
        };
    }
    Preview::Binary {
        hex: hexdump(bytes),
        truncated: truncated || bytes.len() > 4096,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_kinds() {
        match build("a.md", "# 标题\n".as_bytes(), false) {
            Preview::Text {
                text,
                encoding,
                language,
                ..
            } => {
                assert_eq!(text, "# 标题\n");
                assert_eq!(encoding, "UTF-8");
                assert_eq!(language.as_deref(), Some("markdown"));
            }
            other => panic!("{other:?}"),
        }
        let gbk = encoding_rs::GBK
            .encode("这是一段很长的中文说明文字，用来测试编码识别。")
            .0
            .into_owned();
        match build("readme.txt", &gbk, false) {
            Preview::Text { text, .. } => assert!(text.starts_with("这是")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn images_are_reencoded() {
        let img = image::RgbImage::from_pixel(4, 3, image::Rgb([200, 10, 10]));
        let mut png = Vec::new();
        img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        match build("x.png", &png, false) {
            Preview::Image {
                data_url,
                width,
                height,
            } => {
                assert!(data_url.starts_with("data:image/png;base64,"));
                assert_eq!((width, height), (4, 3));
            }
            other => panic!("{other:?}"),
        }
        // A corrupt image falls back to a hex view instead of failing.
        assert!(matches!(
            build("x.png", &[0x89, b'P', b'N', b'G', 0, 1, 2], false),
            Preview::Binary { .. }
        ));
    }

    #[test]
    fn binary_hex() {
        let data: Vec<u8> = (0..64u8).collect();
        match build("a.bin", &data, false) {
            Preview::Binary { hex, .. } => assert!(hex.starts_with("00000000  00 01 02")),
            other => panic!("{other:?}"),
        }
    }
}
