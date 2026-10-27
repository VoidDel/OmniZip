//! Theme service (`docs/05-theming.md`): loads built-in and user themes, validates them, derives
//! every token from the required ones, and enforces minimum contrast.

use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Serialize};

const BUILTIN: &[&str] = &[
    include_str!("../../../../themes/builtin/aurora.json"),
    include_str!("../../../../themes/builtin/midnight.json"),
    include_str!("../../../../themes/builtin/graphite.json"),
    include_str!("../../../../themes/builtin/paper.json"),
    include_str!("../../../../themes/builtin/nord.json"),
    include_str!("../../../../themes/builtin/nord-light.json"),
    include_str!("../../../../themes/builtin/catppuccin-mocha.json"),
    include_str!("../../../../themes/builtin/catppuccin-latte.json"),
    include_str!("../../../../themes/builtin/solarized-dark.json"),
    include_str!("../../../../themes/builtin/solarized-light.json"),
    include_str!("../../../../themes/builtin/high-contrast-light.json"),
    include_str!("../../../../themes/builtin/high-contrast-dark.json"),
];

const REQUIRED: &[&str] = &[
    "bg.canvas",
    "bg.surface",
    "bg.elevated",
    "bg.sidebar",
    "fg.primary",
    "fg.secondary",
    "border.default",
    "accent",
    "success",
    "warning",
    "danger",
    "info",
];

/// A theme file (schema version 1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThemeFile {
    #[serde(rename = "$schema", default, skip_serializing)]
    pub schema: Option<String>,
    pub schema_version: u32,
    pub id: String,
    pub name: BTreeMap<String, String>,
    #[serde(default)]
    pub author: Option<String>,
    pub appearance: Appearance,
    #[serde(default)]
    pub high_contrast: bool,
    pub colors: BTreeMap<String, String>,
    #[serde(default)]
    pub components: BTreeMap<String, String>,
    #[serde(default)]
    pub shape: BTreeMap<String, u32>,
    #[serde(default)]
    pub effects: BTreeMap<String, String>,
    #[serde(default)]
    pub icons: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    Dark,
}

/// Summary sent to the UI for the theme picker.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeSummary {
    pub id: String,
    pub name: BTreeMap<String, String>,
    pub appearance: Appearance,
    pub high_contrast: bool,
    pub builtin: bool,
    pub colors: BTreeMap<String, String>,
    /// Contrast of primary text on the canvas (shown on the theme card).
    pub contrast: f64,
}

/// Resolved CSS custom properties (without the leading `--`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedTheme {
    pub id: String,
    pub appearance: Appearance,
    pub high_contrast: bool,
    pub tokens: BTreeMap<String, String>,
    /// Adjustments made by the contrast guard.
    pub adjusted: Vec<String>,
}

fn valid_hex(s: &str) -> bool {
    let h = s.strip_prefix('#').unwrap_or("");
    (h.len() == 6 || h.len() == 8) && h.chars().all(|c| c.is_ascii_hexdigit())
}

/// Parses and validates a theme file.
pub fn parse(json: &str) -> Result<ThemeFile, String> {
    let t: ThemeFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if t.schema_version != 1 {
        return Err(format!("unsupported schemaVersion {}", t.schema_version));
    }
    if t.id.is_empty()
        || t.id.len() > 64
        || !t
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(format!("invalid id {:?}", t.id));
    }
    if !t.name.contains_key("en-US") {
        return Err("name must include en-US".into());
    }
    for k in REQUIRED {
        match t.colors.get(*k) {
            Some(v) if valid_hex(v) => {}
            Some(v) => return Err(format!("{k}: invalid color {v}")),
            None => return Err(format!("missing required color {k}")),
        }
    }
    for (k, v) in t.colors.iter().chain(t.components.iter()) {
        if !valid_hex(v) {
            return Err(format!("{k}: invalid color {v}"));
        }
    }
    Ok(t)
}

/// Built-in themes followed by valid user themes from `dir` (invalid files are skipped).
pub fn load_all(user_dir: Option<&Path>) -> Vec<(ThemeFile, bool)> {
    let mut out: Vec<(ThemeFile, bool)> = BUILTIN
        .iter()
        .filter_map(|j| parse(j).ok())
        .map(|t| (t, true))
        .collect();
    if let Some(dir) = user_dir
        && let Ok(rd) = fs::read_dir(dir)
    {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json")
                && let Ok(text) = fs::read_to_string(&p)
            {
                match parse(&text) {
                    Ok(t) if !out.iter().any(|(o, _)| o.id == t.id) => out.push((t, false)),
                    Ok(_) => tracing::warn!("theme id clash: {}", p.display()),
                    Err(e) => tracing::warn!("invalid theme {}: {e}", p.display()),
                }
            }
        }
    }
    out
}

pub fn summaries(themes: &[(ThemeFile, bool)]) -> Vec<ThemeSummary> {
    themes
        .iter()
        .map(|(t, builtin)| ThemeSummary {
            id: t.id.clone(),
            name: t.name.clone(),
            appearance: t.appearance,
            high_contrast: t.high_contrast,
            builtin: *builtin,
            colors: t.colors.clone(),
            contrast: (contrast(&t.colors["fg.primary"], &t.colors["bg.canvas"]) * 10.0).round()
                / 10.0,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Color math

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rgb(f64, f64, f64);

fn rgb(hex: &str) -> Rgb {
    let h = hex.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0) as f64;
    Rgb(p(0), p(2), p(4))
}

fn hex(c: Rgb) -> String {
    let f = |v: f64| v.round().clamp(0.0, 255.0) as u8;
    format!("#{:02X}{:02X}{:02X}", f(c.0), f(c.1), f(c.2))
}

fn mix(a: &str, b: &str, t: f64) -> String {
    let (a, b) = (rgb(a), rgb(b));
    hex(Rgb(
        a.0 + (b.0 - a.0) * t,
        a.1 + (b.1 - a.1) * t,
        a.2 + (b.2 - a.2) * t,
    ))
}

fn alpha(c: &str, a: f64) -> String {
    let c = rgb(c);
    format!("rgba({},{},{},{})", c.0 as u8, c.1 as u8, c.2 as u8, a)
}

fn luminance(c: &str) -> f64 {
    let c = rgb(c);
    let lin = |v: f64| {
        let v = v / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.0) + 0.7152 * lin(c.1) + 0.0722 * lin(c.2)
}

/// WCAG contrast ratio.
pub fn contrast(a: &str, b: &str) -> f64 {
    let (l1, l2) = (luminance(a), luminance(b));
    (l1.max(l2) + 0.05) / (l1.min(l2) + 0.05)
}

/// Black or white, whichever reads better on `bg`.
fn on_color(bg: &str) -> String {
    if contrast(bg, "#FFFFFF") >= 3.0 {
        "#FFFFFF".into()
    } else {
        "#111111".into()
    }
}

/// Moves `fg` toward black or white until it reaches `min` contrast against every background.
fn guard(fg: &str, backgrounds: &[&str], min: f64) -> (String, bool) {
    let worst = |c: &str| {
        backgrounds
            .iter()
            .map(|b| contrast(c, b))
            .fold(f64::MAX, f64::min)
    };
    if worst(fg) >= min {
        return (fg.to_owned(), false);
    }
    let dark_bg = luminance(backgrounds[0]) < 0.5;
    let target = if dark_bg { "#FFFFFF" } else { "#000000" };
    for step in 1..=20 {
        let c = mix(fg, target, step as f64 * 0.05);
        if worst(&c) >= min {
            return (c, true);
        }
    }
    (target.to_owned(), true)
}

/// Options that change resolution.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveOptions {
    /// `"theme"`, `"system"` or `#RRGGBB`.
    pub accent: String,
    /// System accent color when known.
    pub system_accent: Option<String>,
    /// Window material active (translucent sidebar).
    pub material: bool,
}

/// Derives every token (`docs/05-theming.md` §2).
pub fn resolve(t: &ThemeFile, o: &ResolveOptions) -> ResolvedTheme {
    let c = |k: &str| t.colors.get(k).cloned().unwrap_or_else(|| "#FF00FF".into());
    let dark = t.appearance == Appearance::Dark;
    let hc = t.high_contrast;
    let (canvas, surface, elevated, sidebar) = (
        c("bg.canvas"),
        c("bg.surface"),
        c("bg.elevated"),
        c("bg.sidebar"),
    );
    let mut adjusted = Vec::new();

    let (fg, a1) = guard(&c("fg.primary"), &[&canvas, &surface, &elevated], 4.5);
    let (fg2, a2) = guard(&c("fg.secondary"), &[&canvas, &surface, &elevated], 3.0);
    if a1 {
        adjusted.push("fg.primary".to_owned());
    }
    if a2 {
        adjusted.push("fg.secondary".to_owned());
    }
    let accent = match o.accent.as_str() {
        "theme" => c("accent"),
        "system" => o
            .system_accent
            .clone()
            .filter(|s| valid_hex(s))
            .unwrap_or_else(|| c("accent")),
        custom if valid_hex(custom) => custom.to_owned(),
        _ => c("accent"),
    };
    let border = c("border.default");
    let material = o.material && !hc;

    let mut tk: BTreeMap<String, String> = BTreeMap::new();
    let mut set = |k: &str, v: String| {
        tk.insert(k.to_owned(), v);
    };
    set("bg-canvas", canvas.clone());
    set("bg-surface", surface.clone());
    set("bg-elevated", elevated.clone());
    set("bg-sidebar", sidebar.clone());
    // Small windows (progress, standalone dialogs) are all material when it is on: a veil of
    // the canvas keeps text readable even over Acrylic's live blur.
    set(
        "canvas-material",
        if material {
            alpha(&canvas, if dark { 0.6 } else { 0.55 })
        } else {
            canvas.clone()
        },
    );
    set(
        "sidebar-material",
        if material {
            // A light veil of the theme color: enough to keep the theme's tint, thin enough for
            // the material (wallpaper-tinted Mica) to read as a material, like native Windows 11 panes.
            alpha(&sidebar, if dark { 0.45 } else { 0.35 })
        } else {
            sidebar.clone()
        },
    );
    set("bg-subtle", mix(&surface, &fg, 0.04));
    set("bg-hover", mix(&surface, &fg, if hc { 0.12 } else { 0.07 }));
    set(
        "bg-pressed",
        mix(&surface, &fg, if hc { 0.2 } else { 0.11 }),
    );
    set("fg-primary", fg.clone());
    set("fg-secondary", fg2);
    set("fg-muted", alpha(&fg, if hc { 0.8 } else { 0.55 }));
    set("fg-disabled", alpha(&fg, if hc { 0.5 } else { 0.35 }));
    set(
        "border-subtle",
        if hc {
            border.clone()
        } else {
            mix(&border, &surface, 0.5)
        },
    );
    set("border-default", border.clone());
    set("border-strong", mix(&border, &fg, 0.25));
    set("border-focus", accent.clone());
    set("accent", accent.clone());
    set("accent-hover", mix(&accent, &fg, 0.12));
    set("accent-pressed", mix(&accent, &fg, 0.22));
    set(
        "accent-subtle",
        alpha(&accent, if hc { 0.28 } else { 0.14 }),
    );
    set("accent-fg", on_color(&accent));
    for s in ["success", "warning", "danger", "info"] {
        set(s, c(s));
        set(
            &format!("{s}-subtle"),
            alpha(&c(s), if s == "warning" { 0.16 } else { 0.14 }),
        );
    }
    set("selection-bg", alpha(&accent, if hc { 0.35 } else { 0.22 }));
    set("selection-fg", fg.clone());
    set(
        "folder-color",
        if dark {
            "#F0C35C".into()
        } else {
            "#E0A82E".into()
        },
    );
    set(
        "shadow-1",
        if dark {
            "0 2px 8px rgba(0,0,0,.45)".into()
        } else {
            "0 2px 8px rgba(20,30,50,.10)".into()
        },
    );
    set(
        "shadow-2",
        if dark {
            "0 16px 48px rgba(0,0,0,.6)".into()
        } else {
            "0 16px 48px rgba(20,30,50,.18)".into()
        },
    );
    set(
        "overlay-scrim",
        if dark {
            "rgba(0,0,0,.55)".into()
        } else {
            "rgba(10,14,22,.4)".into()
        },
    );
    for (k, v) in &t.components {
        tk.insert(k.replace('.', "-"), v.clone());
    }
    for (k, v) in &t.shape {
        tk.insert(k.replace('.', "-"), format!("{v}px"));
    }
    ResolvedTheme {
        id: t.id.clone(),
        appearance: t.appearance,
        high_contrast: hc,
        tokens: tk,
        adjusted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ResolveOptions {
        ResolveOptions {
            accent: "theme".into(),
            system_accent: None,
            material: true,
        }
    }

    #[test]
    fn builtins_parse_and_pass_contrast() {
        let all = load_all(None);
        assert_eq!(all.len(), BUILTIN.len());
        for (t, _) in &all {
            let r = resolve(t, &opts());
            assert!(
                r.adjusted.is_empty(),
                "{} needed adjustment: {:?}",
                t.id,
                r.adjusted
            );
            let fg = &r.tokens["fg-primary"];
            for bg in ["bg-canvas", "bg-surface", "bg-elevated"] {
                assert!(contrast(fg, &r.tokens[bg]) >= 4.5, "{} {bg}", t.id);
            }
            assert!(
                contrast(&r.tokens["accent-fg"], &r.tokens["accent"]) >= 3.0,
                "{}",
                t.id
            );
        }
    }

    #[test]
    fn contrast_guard_repairs_bad_themes() {
        let mut t = parse(BUILTIN[0]).unwrap();
        t.colors.insert("fg.primary".into(), "#C0C0C0".into());
        let r = resolve(&t, &opts());
        assert_eq!(r.adjusted, vec!["fg.primary".to_owned()]);
        assert!(contrast(&r.tokens["fg-primary"], &r.tokens["bg-canvas"]) >= 4.5);
    }

    #[test]
    fn validation() {
        assert!(parse("{}").is_err());
        let mut v: serde_json::Value = serde_json::from_str(BUILTIN[0]).unwrap();
        v["colors"]["accent"] = "blue".into();
        assert!(parse(&v.to_string()).unwrap_err().contains("accent"));
        let mut v: serde_json::Value = serde_json::from_str(BUILTIN[0]).unwrap();
        v["unexpected"] = 1.into();
        assert!(parse(&v.to_string()).is_err());
    }

    #[test]
    fn accent_choices() {
        let t = parse(BUILTIN[0]).unwrap();
        let r = resolve(
            &t,
            &ResolveOptions {
                accent: "#123456".into(),
                ..opts()
            },
        );
        assert_eq!(r.tokens["accent"], "#123456");
        let r = resolve(
            &t,
            &ResolveOptions {
                accent: "system".into(),
                system_accent: Some("#0067C0".into()),
                material: false,
            },
        );
        assert_eq!(r.tokens["accent"], "#0067C0");
        assert_eq!(r.tokens["sidebar-material"], r.tokens["bg-sidebar"]);
    }
}
