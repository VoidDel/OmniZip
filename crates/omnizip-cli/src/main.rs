//! `omnizip` — the OmniZip command-line tool (`docs/08-cli.md`).

mod cli;
mod commands;
mod ui;

use std::io;

use clap::{CommandFactory, FromArgMatches};
use omnizip_i18n::Localizer;

use crate::cli::{Cli, Command};

/// Replaces clap's English help texts with the localized ones (`cmd-*`, `arg-*`).
fn localize(mut cmd: clap::Command, l: &Localizer) -> clap::Command {
    let ids: Vec<String> = cmd
        .get_arguments()
        .map(|a| a.get_id().to_string())
        .collect();
    for id in ids {
        let key = format!("arg-{}", id.replace('_', "-"));
        if l.has(&key) {
            let text = l.get(&key);
            cmd = cmd.mut_arg(id, |a| a.help(text));
        }
    }
    let subs: Vec<String> = cmd
        .get_subcommands()
        .map(|s| s.get_name().to_owned())
        .collect();
    for name in subs {
        let key = format!("cmd-{name}");
        let about = l.has(&key).then(|| l.get(&key));
        cmd = cmd.mut_subcommand(&name, |sc| {
            let sc = match about {
                Some(text) => sc.about(text),
                None => sc,
            };
            localize(sc, l)
        });
    }
    cmd
}

/// Finds `--lang X` / `--lang=X` before full parsing, so `--help` is already localized.
fn prescan_lang(args: &[std::ffi::OsString]) -> Option<String> {
    let mut it = args.iter().filter_map(|a| a.to_str());
    while let Some(a) = it.next() {
        if a == "--lang" {
            return it.next().map(str::to_owned);
        }
        if let Some(v) = a.strip_prefix("--lang=") {
            return Some(v.to_owned());
        }
    }
    None
}

/// The full command with localized help.
pub fn command(l: &Localizer) -> clap::Command {
    localize(Cli::command(), l).about(l.get("cli-about"))
}

fn run() -> i32 {
    let args = cli::rewrite_compat(std::env::args_os().collect());
    let l = Localizer::detect(prescan_lang(&args).as_deref());
    let matches = match command(&l).try_get_matches_from(&args) {
        Ok(m) => m,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { 2 } else { 0 };
        }
    };
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return 2;
        }
    };
    let l = Localizer::detect(cli.global.lang.as_deref());
    match cli.command {
        Command::Completions { shell } => {
            clap_complete::generate(shell, &mut command(&l), "omnizip", &mut io::stdout());
            return 0;
        }
        Command::Manpages { dir } => {
            return match write_manpages(&dir, &l) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("omnizip: {e}");
                    7
                }
            };
        }
        _ => {}
    }
    let ui = ui::Ui::new(&cli.global, l);
    match commands::dispatch(cli.command, &ui) {
        Ok(code) => code,
        Err(e) => {
            ui.error(&e);
            e.code().exit_code()
        }
    }
}

fn write_manpages(dir: &std::path::Path, l: &Localizer) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let root = command(l);
    let mut f = std::fs::File::create(dir.join("omnizip.1"))?;
    clap_mangen::Man::new(root.clone()).render(&mut f)?;
    for sub in root.get_subcommands().filter(|s| !s.is_hide_set()) {
        let name = format!("omnizip-{}", sub.get_name());
        let mut f = std::fs::File::create(dir.join(format!("{name}.1")))?;
        clap_mangen::Man::new(sub.clone().name(name)).render(&mut f)?;
    }
    Ok(())
}

fn main() {
    std::process::exit(run());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localized_help_is_complete() {
        for lang in ["en-US", "zh-CN"] {
            let l = Localizer::new(lang);
            let cmd = command(&l);
            cmd.clone().debug_assert();
            for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
                assert!(
                    l.has(&format!("cmd-{}", sub.get_name())),
                    "{lang}: cmd-{}",
                    sub.get_name()
                );
                for a in sub.get_arguments() {
                    let id = a.get_id().as_str();
                    if id == "help" || id == "version" {
                        continue;
                    }
                    assert!(
                        l.has(&format!("arg-{}", id.replace('_', "-"))),
                        "{lang}: arg-{id} ({})",
                        sub.get_name()
                    );
                }
            }
        }
    }

    #[test]
    fn lang_prescan() {
        let a: Vec<std::ffi::OsString> = ["omnizip", "--lang", "zh-CN", "x"]
            .iter()
            .map(Into::into)
            .collect();
        assert_eq!(prescan_lang(&a).as_deref(), Some("zh-CN"));
        let a: Vec<std::ffi::OsString> = ["omnizip", "--lang=ja", "x"]
            .iter()
            .map(Into::into)
            .collect();
        assert_eq!(prescan_lang(&a).as_deref(), Some("ja"));
    }
}
