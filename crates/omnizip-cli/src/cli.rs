//! Command-line grammar (`docs/08-cli.md` §2–§4). Help texts are replaced at run time with the
//! localized strings from `locales/*/cli.ftl` (ids `cmd-<name>` and `arg-<id>`).

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "omnizip",
    version,
    about = "Create, extract, test and convert archives",
    disable_help_subcommand = true,
    arg_required_else_help = true,
    max_term_width = 110
)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args, Debug, Clone)]
pub struct Global {
    #[arg(short, long, global = true)]
    pub quiet: bool,
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,
    #[arg(long, global = true, conflicts_with = "porcelain")]
    pub json: bool,
    #[arg(long, global = true)]
    pub porcelain: bool,
    #[arg(long, global = true, value_enum, default_value_t = ProgressMode::Auto)]
    pub progress: ProgressMode,
    #[arg(long, global = true, value_enum, default_value_t = ColorMode::Auto)]
    pub color: ColorMode,
    #[arg(long, global = true, env = "OMNIZIP_LANG", value_name = "TAG")]
    pub lang: Option<String>,
    #[arg(long, global = true)]
    pub no_input: bool,
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,
    #[arg(
        long,
        global = true,
        env = "OMNIZIP_THREADS",
        default_value_t = 0,
        value_name = "N"
    )]
    pub threads: u32,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    Auto,
    Bar,
    Plain,
    None,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    #[command(visible_alias = "x")]
    Extract(ExtractArgs),
    #[command(visible_alias = "e")]
    Flat(ExtractArgs),
    #[command(visible_alias = "a")]
    Create(CreateArgs),
    #[command(visible_alias = "l")]
    List(ListArgs),
    #[command(visible_alias = "t")]
    Test(TestArgs),
    #[command(visible_alias = "i")]
    Info(InfoArgs),
    #[command(visible_alias = "d")]
    Delete(DeleteArgs),
    #[command(visible_alias = "rn")]
    Rename(RenameArgs),
    Convert(ConvertArgs),
    Formats,
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    Gui {
        files: Vec<PathBuf>,
    },
    #[command(hide = true)]
    Manpages {
        dir: PathBuf,
    },
    /// The context-menu items for the paths on standard input (one per line), as JSON: what the
    /// file-manager extensions on Linux show (platform/linux/nautilus/omnizip.py).
    #[command(hide = true)]
    ShellMenu,
}

/// Password sources. A bare `-p` prompts; `-p=SECRET` / `--password=SECRET` gives it inline
/// (with a warning, since other processes can see it).
#[derive(Args, Debug, Clone, Default)]
pub struct PasswordArgs {
    #[arg(
        short = 'p',
        long = "password",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "",
        value_name = "SECRET"
    )]
    pub password: Option<String>,
    #[arg(long, value_name = "FILE")]
    pub password_file: Option<PathBuf>,
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathArg {
    #[default]
    Full,
    Flat,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverwriteArg {
    Ask,
    Skip,
    #[value(alias = "overwrite")]
    All,
    Older,
    Rename,
}

#[derive(Args, Debug)]
pub struct ExtractArgs {
    #[arg(required = true, value_name = "ARCHIVE")]
    pub archives: Vec<PathBuf>,
    #[arg(short, long, value_name = "DIR")]
    pub output: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = PathArg::Full)]
    pub path: PathArg,
    #[arg(long)]
    pub each: bool,
    #[arg(long, value_enum)]
    pub overwrite: Option<OverwriteArg>,
    #[command(flatten)]
    pub pw: PasswordArgs,
    #[arg(long, value_name = "GLOB")]
    pub include: Vec<String>,
    #[arg(long, value_name = "GLOB")]
    pub exclude: Vec<String>,
    #[arg(long, value_name = "NAME")]
    pub encoding: Option<String>,
    #[arg(long)]
    pub stdout: bool,
    #[arg(long)]
    pub no_smart: bool,
    #[arg(long)]
    pub keep_broken: bool,
    #[arg(long)]
    pub allow_high_ratio: bool,
    #[arg(long)]
    pub strip_unsafe_paths: bool,
}

#[derive(Args, Debug)]
pub struct CreateArgs {
    pub archive: PathBuf,
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    #[arg(short = 'f', long, value_name = "FORMAT")]
    pub format: Option<String>,
    #[arg(short = 'l', long, value_name = "LEVEL")]
    pub level: Option<String>,
    #[arg(short = 'm', long, value_name = "METHOD")]
    pub method: Option<String>,
    #[arg(long, overrides_with = "no_solid")]
    pub solid: bool,
    #[arg(long)]
    pub no_solid: bool,
    #[arg(long)]
    pub compress_all: bool,
    #[command(flatten)]
    pub pw: PasswordArgs,
    #[arg(long)]
    pub encrypt_headers: bool,
    #[arg(long = "volume", value_name = "SIZE")]
    pub volume: Option<String>,
    #[arg(long, value_name = "GLOB")]
    pub include: Vec<String>,
    #[arg(long, value_name = "GLOB")]
    pub exclude: Vec<String>,
    #[arg(long, value_name = "FILE")]
    pub exclude_from: Option<PathBuf>,
    #[arg(long)]
    pub follow_symlinks: bool,
    #[arg(long)]
    pub skip_hidden: bool,
    #[arg(long, value_name = "TEXT")]
    pub comment: Option<String>,
    #[arg(long)]
    pub delete_source: bool,
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    pub archive: PathBuf,
    #[arg(value_name = "GLOB")]
    pub globs: Vec<String>,
    #[arg(long)]
    pub tree: bool,
    #[command(flatten)]
    pub pw: PasswordArgs,
    #[arg(long, value_name = "NAME")]
    pub encoding: Option<String>,
}

#[derive(Args, Debug)]
pub struct TestArgs {
    #[arg(required = true, value_name = "ARCHIVE")]
    pub archives: Vec<PathBuf>,
    #[command(flatten)]
    pub pw: PasswordArgs,
}

#[derive(Args, Debug)]
pub struct InfoArgs {
    pub archive: PathBuf,
    #[command(flatten)]
    pub pw: PasswordArgs,
    #[arg(long, value_name = "NAME")]
    pub encoding: Option<String>,
}

#[derive(Args, Debug)]
pub struct DeleteArgs {
    pub archive: PathBuf,
    #[arg(required = true, value_name = "ENTRY")]
    pub entries: Vec<String>,
}

#[derive(Args, Debug)]
pub struct RenameArgs {
    pub archive: PathBuf,
    pub from: String,
    #[arg(id = "to_name", value_name = "TO")]
    pub to: String,
}

#[derive(Args, Debug)]
pub struct ConvertArgs {
    #[arg(required = true, value_name = "ARCHIVE")]
    pub archives: Vec<PathBuf>,
    #[arg(long, value_name = "FORMAT")]
    pub to: String,
    #[arg(short = 'l', long, value_name = "LEVEL")]
    pub level: Option<String>,
    #[arg(short = 'm', long, value_name = "METHOD")]
    pub method: Option<String>,
    #[arg(long, value_name = "DIR")]
    pub out_dir: Option<PathBuf>,
    #[arg(long)]
    pub delete_source: bool,
    #[command(flatten)]
    pub pw: PasswordArgs,
}

/// 7-Zip style arguments accepted for migration (`docs/08-cli.md` §2): `-mx=9`, `-mx9`,
/// `-pSECRET`, `-oDIR`, `-tzip`.
pub fn rewrite_compat(args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let mut out = Vec::with_capacity(args.len());
    for (i, a) in args.into_iter().enumerate() {
        let Some(s) = a.to_str().map(str::to_owned) else {
            out.push(a);
            continue;
        };
        if i == 0 || !s.starts_with('-') || s.starts_with("--") {
            out.push(a);
            continue;
        }
        if let Some(rest) = s.strip_prefix("-mx") {
            let level = rest.trim_start_matches('=');
            if !level.is_empty() && level.chars().all(|c| c.is_ascii_digit()) {
                out.push("--level".into());
                out.push(level.into());
                continue;
            }
        }
        if let Some(rest) = s.strip_prefix("-p")
            && !rest.is_empty()
            && !rest.starts_with('=')
        {
            out.push(format!("--password={rest}").into());
            continue;
        }
        if let Some(rest) = s.strip_prefix("-o")
            && !rest.is_empty()
            && !rest.starts_with('=')
            && rest != "utput"
        {
            out.push("--output".into());
            out.push(rest.into());
            continue;
        }
        if let Some(rest) = s.strip_prefix("-t")
            && rest.len() >= 2
            && omnizip_core::FormatId::parse(rest).is_some()
        {
            out.push("--format".into());
            out.push(rest.into());
            continue;
        }
        out.push(a);
    }
    out
}

/// Parses `100m`, `4g`, `700k`, `25MB`, `1.5GiB` or plain bytes (binary multiples).
pub fn parse_size(s: &str) -> Option<u64> {
    let t = s.trim().to_ascii_lowercase();
    let t = t.trim_end_matches("ib").trim_end_matches('b');
    let (num, mult) = match t.chars().last()? {
        'k' => (&t[..t.len() - 1], 1u64 << 10),
        'm' => (&t[..t.len() - 1], 1 << 20),
        'g' => (&t[..t.len() - 1], 1 << 30),
        't' => (&t[..t.len() - 1], 1 << 40),
        _ => (t, 1),
    };
    let v: f64 = num.trim().parse().ok()?;
    (v > 0.0).then_some((v * mult as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn os(v: &[&str]) -> Vec<std::ffi::OsString> {
        v.iter().map(Into::into).collect()
    }

    #[test]
    fn grammar_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn seven_zip_compat() {
        let got = rewrite_compat(os(&[
            "omnizip", "a", "-mx=9", "-psecret", "-ttar.gz", "x.7z", "dir",
        ]));
        assert_eq!(
            got,
            os(&[
                "omnizip",
                "a",
                "--level",
                "9",
                "--password=secret",
                "--format",
                "tar.gz",
                "x.7z",
                "dir"
            ])
        );
        let got = rewrite_compat(os(&["omnizip", "x", "-oout", "-p", "a.7z"]));
        assert_eq!(got, os(&["omnizip", "x", "--output", "out", "-p", "a.7z"]));
    }

    #[test]
    fn bare_password_flag_does_not_eat_the_archive() {
        let cli = Cli::try_parse_from(["omnizip", "x", "-p", "secret.7z"]).unwrap();
        let Command::Extract(a) = cli.command else {
            panic!()
        };
        assert_eq!(a.pw.password.as_deref(), Some(""));
        assert_eq!(a.archives, [PathBuf::from("secret.7z")]);
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("100m"), Some(100 << 20));
        assert_eq!(parse_size("4G"), Some(4 << 30));
        assert_eq!(parse_size("25MB"), Some(25 << 20));
        assert_eq!(parse_size("1.5GiB"), Some(3 << 29));
        assert_eq!(parse_size("65536"), Some(65536));
        assert_eq!(parse_size("x"), None);
        assert_eq!(parse_size("0"), None);
    }
}
