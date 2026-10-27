//! Localization for the Rust side of OmniZip (CLI, shell integration, notifications) using
//! Fluent (`docs/06-i18n.md` §6). The GUI uses i18next with the same language list; error keys
//! are shared through [`omnizip_core::ErrorCode::key`].

use std::{borrow::Cow, collections::HashMap, sync::OnceLock};

use fluent_bundle::{FluentArgs, FluentResource, FluentValue, concurrent::FluentBundle};
use omnizip_core::Error;
use unic_langid::LanguageIdentifier;

/// Languages with translations, in menu order. `en-US` is the source language.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("zh-CN", "简体中文"),
    ("zh-TW", "繁體中文"),
    ("en-US", "English"),
    ("ja-JP", "日本語"),
];

/// The source / fallback language.
pub const FALLBACK: &str = "en-US";

macro_rules! res {
    ($lang:literal, $file:literal) => {
        (
            $lang,
            include_str!(concat!("../../../locales/", $lang, "/", $file)),
        )
    };
}

/// Every embedded Fluent resource.
const RESOURCES: &[(&str, &str)] = &[
    res!("en-US", "cli.ftl"),
    res!("en-US", "errors.ftl"),
    res!("en-US", "shell.ftl"),
    res!("zh-CN", "cli.ftl"),
    res!("zh-CN", "errors.ftl"),
    res!("zh-CN", "shell.ftl"),
    res!("zh-TW", "errors.ftl"),
    res!("zh-TW", "shell.ftl"),
    res!("ja-JP", "errors.ftl"),
    res!("ja-JP", "shell.ftl"),
];

/// Parameters every error message may reference; missing ones render as empty text.
const ERROR_PARAMS: &[&str] = &[
    "path",
    "detail",
    "format",
    "method",
    "operation",
    "ratio",
    "size",
    "count",
    "issue",
];

/// A message argument.
#[derive(Debug, Clone)]
pub enum Arg {
    Str(String),
    Num(f64),
}

impl From<&str> for Arg {
    fn from(s: &str) -> Self {
        Arg::Str(s.to_owned())
    }
}
impl From<String> for Arg {
    fn from(s: String) -> Self {
        Arg::Str(s)
    }
}
impl From<&String> for Arg {
    fn from(s: &String) -> Self {
        Arg::Str(s.clone())
    }
}
macro_rules! num_arg {
    ($($t:ty),*) => {$(
        impl From<$t> for Arg {
            fn from(n: $t) -> Self { Arg::Num(n as f64) }
        }
    )*};
}
num_arg!(u8, u16, u32, u64, usize, i32, i64, f64);

type Bundle = FluentBundle<FluentResource>;

fn build_bundle(lang: &str) -> Bundle {
    let id: LanguageIdentifier = lang
        .parse()
        .unwrap_or_else(|_| FALLBACK.parse().expect("valid"));
    let mut b = FluentBundle::new_concurrent(vec![id]);
    // Terminal output: no Unicode bidi isolation marks around placeables.
    b.set_use_isolating(false);
    for (l, src) in RESOURCES {
        if *l == lang {
            let res = FluentResource::try_new((*src).to_owned()).unwrap_or_else(|(r, errs)| {
                debug_assert!(errs.is_empty(), "{lang}: {errs:?}");
                r
            });
            let _ = b.add_resource(res);
        }
    }
    b
}

fn bundles() -> &'static HashMap<&'static str, Bundle> {
    static B: OnceLock<HashMap<&'static str, Bundle>> = OnceLock::new();
    B.get_or_init(|| {
        LANGUAGES
            .iter()
            .map(|(l, _)| (*l, build_bundle(l)))
            .collect()
    })
}

/// Maps any locale string (`zh_CN.UTF-8`, `zh-Hant-HK`, `ja`, `C`) to a supported language.
pub fn normalize(locale: &str) -> Option<&'static str> {
    let l = locale
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .replace('_', "-")
        .to_ascii_lowercase();
    if l.is_empty() || l == "c" || l == "posix" {
        return None;
    }
    let mut parts = l.split('-');
    let lang = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    Some(match lang {
        "zh" if rest
            .iter()
            .any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo")) =>
        {
            "zh-TW"
        }
        "zh" => "zh-CN",
        "ja" => "ja-JP",
        "en" => "en-US",
        _ => return None,
    })
}

/// Picks the UI language: explicit choice, then `OMNIZIP_LANG`, then the system preferences.
pub fn detect_language(explicit: Option<&str>) -> &'static str {
    let candidates = explicit
        .map(str::to_owned)
        .into_iter()
        .chain(std::env::var("OMNIZIP_LANG").ok())
        .chain(if cfg!(unix) {
            ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
                .iter()
                .filter_map(|v| std::env::var(v).ok())
                .flat_map(|v| v.split(':').map(str::to_owned).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        })
        .chain(sys_locale::get_locales());
    for c in candidates {
        if c.eq_ignore_ascii_case("c") || c.eq_ignore_ascii_case("posix") {
            return FALLBACK;
        }
        if let Some(l) = normalize(&c) {
            return l;
        }
    }
    FALLBACK
}

/// Formats messages in one language with fallback to English.
#[derive(Clone, Copy)]
pub struct Localizer {
    lang: &'static str,
}

impl std::fmt::Debug for Localizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Localizer")
            .field("lang", &self.lang)
            .finish()
    }
}

impl Localizer {
    /// A localizer for a supported language (unknown values fall back to English).
    pub fn new(lang: &str) -> Self {
        let lang = normalize(lang).unwrap_or(FALLBACK);
        Self { lang }
    }

    /// A localizer for the detected language.
    pub fn detect(explicit: Option<&str>) -> Self {
        Self {
            lang: detect_language(explicit),
        }
    }

    /// The language in use.
    pub fn lang(&self) -> &'static str {
        self.lang
    }

    fn format(&self, bundle: &Bundle, id: &str, args: Option<&FluentArgs<'_>>) -> Option<String> {
        let msg = bundle.get_message(id)?;
        let pattern = msg.value()?;
        let mut errors = Vec::new();
        let out = bundle.format_pattern(pattern, args, &mut errors);
        Some(out.into_owned())
    }

    /// Whether the message exists in this language or the fallback.
    pub fn has(&self, id: &str) -> bool {
        let b = bundles();
        b[self.lang].has_message(id) || b[FALLBACK].has_message(id)
    }

    /// Formats a message; returns the id itself when it is missing everywhere.
    pub fn fmt(&self, id: &str, args: &[(&str, Arg)]) -> String {
        let mut fa = FluentArgs::new();
        for (k, v) in args {
            match v {
                Arg::Str(s) => fa.set(Cow::Owned((*k).to_owned()), FluentValue::from(s.clone())),
                Arg::Num(n) => fa.set(Cow::Owned((*k).to_owned()), FluentValue::from(*n)),
            }
        }
        let b = bundles();
        let args = (!args.is_empty()).then_some(&fa);
        self.format(&b[self.lang], id, args)
            .or_else(|| self.format(&b[FALLBACK], id, args))
            .unwrap_or_else(|| id.to_owned())
    }

    /// Formats a message without arguments.
    pub fn get(&self, id: &str) -> String {
        self.fmt(id, &[])
    }

    /// Renders an OmniZip error in this language.
    pub fn error(&self, e: &Error) -> String {
        let mut args: Vec<(&str, Arg)> = ERROR_PARAMS
            .iter()
            .map(|k| (*k, Arg::Str(String::new())))
            .collect();
        for (k, v) in e.params() {
            if let Some(slot) = args.iter_mut().find(|(name, _)| name == k) {
                slot.1 = Arg::Str(v.clone());
            }
        }
        let text = self.fmt(&format!("error-{}", e.code().key()), &args);
        // Collapse the gap left by an empty trailing parameter.
        text.trim_end_matches([' ', ':', '：']).to_owned()
    }
}

/// `tr!(l, "id", name = value, …)`
#[macro_export]
macro_rules! tr {
    ($l:expr, $id:expr) => { $l.get($id) };
    ($l:expr, $id:expr, $($k:ident = $v:expr),+ $(,)?) => {
        $l.fmt($id, &[$((stringify!($k), $crate::Arg::from($v))),+])
    };
}

#[cfg(test)]
mod tests {
    use omnizip_core::ErrorCode;

    use super::*;

    #[test]
    fn every_resource_parses() {
        for (lang, src) in RESOURCES {
            if let Err((_, errs)) = FluentResource::try_new((*src).to_owned()) {
                panic!("{lang}: {errs:?}");
            }
        }
    }

    #[test]
    fn every_error_code_has_a_message_in_every_language() {
        for (lang, _) in LANGUAGES {
            let b = &bundles()[lang];
            for code in ErrorCode::ALL {
                let id = format!("error-{}", code.key());
                assert!(b.has_message(&id), "{lang} lacks {id}");
            }
        }
    }

    #[test]
    fn translated_languages_have_the_same_keys_as_english() {
        for domain in ["cli.ftl", "errors.ftl", "shell.ftl"] {
            let ids = |lang: &str| -> Option<Vec<String>> {
                let src = RESOURCES.iter().find(|(l, _)| *l == lang)?;
                let _ = src;
                let text = RESOURCES
                    .iter()
                    .filter(|(l, _)| *l == lang)
                    .map(|(_, s)| *s)
                    .find(|s| match domain {
                        "cli.ftl" => s.contains("cli-about"),
                        "errors.ftl" => s.contains("error-io"),
                        _ => s.contains("menu-root"),
                    })?;
                let mut v: Vec<String> = text
                    .lines()
                    .filter(|l| !l.starts_with([' ', '#', '*', '[']) && l.contains(" ="))
                    .map(|l| l.split(" =").next().unwrap_or("").trim().to_owned())
                    .collect();
                v.sort();
                Some(v)
            };
            let en = ids("en-US").expect("english");
            for (lang, _) in LANGUAGES {
                if let Some(other) = ids(lang) {
                    assert_eq!(other, en, "{lang} {domain} keys differ from en-US");
                }
            }
        }
    }

    #[test]
    fn formatting_and_fallback() {
        let zh = Localizer::new("zh-CN");
        assert_eq!(tr!(zh, "label-yes"), "是");
        assert_eq!(tr!(zh, "summary-skipped", count = 3u32), "跳过了 3 项");
        let en = Localizer::new("en");
        assert_eq!(
            tr!(en, "summary-skipped", count = 1u32),
            "1 item was skipped"
        );
        assert_eq!(
            tr!(en, "summary-skipped", count = 2u32),
            "2 items were skipped"
        );
        // Japanese has no CLI translation yet: English is used.
        let ja = Localizer::new("ja-JP");
        assert_eq!(tr!(ja, "label-yes"), "yes");
        assert_eq!(tr!(ja, "menu-mount"), "マウント");
        assert_eq!(tr!(en, "no-such-message"), "no-such-message");
    }

    #[test]
    fn errors_render_with_parameters() {
        let zh = Localizer::new("zh-CN");
        let e = Error::new(ErrorCode::WrongPassword).with("path", "a.7z");
        assert_eq!(zh.error(&e), "密码错误：a.7z");
        let e = Error::new(ErrorCode::Cancelled);
        assert_eq!(Localizer::new("en-US").error(&e), "Cancelled");
        let e = Error::new(ErrorCode::NotFound);
        assert_eq!(Localizer::new("en-US").error(&e), "File not found");
    }

    #[test]
    fn locale_normalization() {
        assert_eq!(normalize("zh_CN.UTF-8"), Some("zh-CN"));
        assert_eq!(normalize("zh-Hant-HK"), Some("zh-TW"));
        assert_eq!(normalize("zh-HK"), Some("zh-TW"));
        assert_eq!(normalize("zh-Hans"), Some("zh-CN"));
        assert_eq!(normalize("ja"), Some("ja-JP"));
        assert_eq!(normalize("en-GB"), Some("en-US"));
        assert_eq!(normalize("C"), None);
        assert_eq!(normalize("de-DE"), None);
        assert_eq!(detect_language(Some("zh-TW")), "zh-TW");
    }
}
