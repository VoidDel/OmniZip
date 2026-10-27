//! Subcommand implementations.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use globset::{Glob, GlobSet, GlobSetBuilder};
use omnizip_core::{
    ArchiveReader, Capabilities, CollectOptions, Entry, EntryData, EntryKind, Error, ErrorCode,
    ExtractOptions, ExtractSink, FormatId, JobCtx, Level, OpenOptions, OverwriteMode, PathMode,
    Result, Selection, SortKey, TreeIndex, UpdateOp, WriteOptions, encoding::NameEncoding,
    format::archive_stem, source,
};
use omnizip_formats::shared;
use omnizip_i18n::tr;
use serde_json::json;

use crate::{
    cli::{
        Command, ConvertArgs, CreateArgs, DeleteArgs, ExtractArgs, InfoArgs, ListArgs,
        OverwriteArg, PathArg, RenameArgs, TestArgs, parse_size,
    },
    ui::{Ui, fmt_elapsed, fmt_size, fmt_time, iso_time},
};

/// Runs a command and returns the process exit code.
pub fn dispatch(cmd: Command, ui: &Ui) -> Result<i32> {
    match cmd {
        Command::Extract(a) => extract(a, false, ui),
        Command::Flat(a) => extract(a, true, ui),
        Command::Create(a) => create(a, ui),
        Command::List(a) => list(a, ui),
        Command::Test(a) => test(a, ui),
        Command::Info(a) => info(a, ui),
        Command::Delete(a) => delete(a, ui),
        Command::Rename(a) => rename(a, ui),
        Command::Convert(a) => convert(a, ui),
        Command::Formats => formats(ui),
        Command::Completions { .. } | Command::Manpages { .. } => unreachable!("handled in main"),
        Command::Gui { files } => gui(&files, ui),
    }
}

fn globs(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let p = p.trim_end_matches('/');
        b.add(Glob::new(p).map_err(|e| Error::invalid_argument(format!("{p}: {e}")))?);
        if !p.contains('/') {
            b.add(
                Glob::new(&format!("**/{p}"))
                    .map_err(|e| Error::invalid_argument(e.to_string()))?,
            );
        }
    }
    b.build()
        .map(Some)
        .map_err(|e| Error::invalid_argument(e.to_string()))
}

fn matches(set: &Option<GlobSet>, path: &str) -> bool {
    set.as_ref()
        .is_some_and(|g| g.is_match(path.trim_end_matches('/')))
}

fn select(entries: &[Entry], include: &[String], exclude: &[String]) -> Result<Selection> {
    if include.is_empty() && exclude.is_empty() {
        return Ok(Selection::all());
    }
    let inc = globs(include)?;
    let exc = globs(exclude)?;
    let ids = entries
        .iter()
        .filter(|e| (inc.is_none() || matches(&inc, &e.path)) && !matches(&exc, &e.path))
        .map(|e| e.id);
    Ok(Selection {
        ids: Some(ids.collect()),
        strip_prefix: Vec::new(),
    })
}

fn parse_encoding(e: Option<&str>) -> Result<Option<NameEncoding>> {
    match e {
        None => Ok(None),
        Some(s) if s.eq_ignore_ascii_case("auto") => Ok(None),
        Some(s) => NameEncoding::from_label(s)
            .map(Some)
            .ok_or_else(|| Error::invalid_argument(format!("unknown encoding {s}"))),
    }
}

fn parse_level(l: Option<&str>) -> Result<Level> {
    match l {
        None => Ok(Level::Normal),
        Some(s) => {
            Level::parse(s).ok_or_else(|| Error::invalid_argument(format!("unknown level {s}")))
        }
    }
}

fn parse_format(f: &str) -> Result<FormatId> {
    FormatId::parse(f).ok_or_else(|| Error::invalid_argument(format!("unknown format {f}")))
}

/// An archive path, or standard input copied to a private temporary file.
struct Input {
    path: PathBuf,
    _tmp: Option<tempfile::TempDir>,
}

fn input(p: &Path) -> Result<Input> {
    if p.as_os_str() != "-" {
        return Ok(Input {
            path: p.to_owned(),
            _tmp: None,
        });
    }
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("stdin");
    let mut f = fs::File::create(&path).map_err(|e| Error::io(e, &path))?;
    io::copy(&mut io::stdin().lock(), &mut f)?;
    Ok(Input {
        path,
        _tmp: Some(dir),
    })
}

fn display_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn open(
    ui: &Ui,
    path: &Path,
    password: Option<String>,
    encoding: Option<NameEncoding>,
    ctx: &JobCtx,
) -> Result<Box<dyn ArchiveReader>> {
    let opts = OpenOptions {
        password,
        encoding,
        locale: Some(ui.l.lang().to_owned()),
    };
    shared().open(path, &opts, ctx)
}

// ---------------------------------------------------------------------------------------------
// extract / flat

fn extract(a: ExtractArgs, flat: bool, ui: &Ui) -> Result<i32> {
    let pw = ui.password(&a.pw, false)?;
    let enc = parse_encoding(a.encoding.as_deref())?;
    let overwrite = match a.overwrite {
        Some(OverwriteArg::Ask) => OverwriteMode::Ask,
        Some(OverwriteArg::Skip) => OverwriteMode::Skip,
        Some(OverwriteArg::All) => OverwriteMode::Overwrite,
        Some(OverwriteArg::Older) => OverwriteMode::OverwriteOlder,
        Some(OverwriteArg::Rename) => OverwriteMode::Rename,
        None if ui.yes => OverwriteMode::Overwrite,
        None if ui.interactive => OverwriteMode::Ask,
        None => OverwriteMode::Skip,
    };
    let dest_root = match &a.output {
        Some(o) => o.clone(),
        None => std::env::current_dir()?,
    };
    let mut worst = 0;
    for archive in &a.archives {
        match extract_one(
            &a,
            archive,
            flat,
            overwrite,
            &dest_root,
            pw.clone(),
            enc,
            ui,
        ) {
            Ok(code) => worst = worst.max(code),
            Err(e) => {
                ui.error(&e);
                worst = worst.max(e.code().exit_code());
                if e.code() == ErrorCode::Cancelled {
                    break;
                }
            }
        }
    }
    Ok(worst)
}

#[allow(clippy::too_many_arguments)]
fn extract_one(
    a: &ExtractArgs,
    archive: &Path,
    flat: bool,
    overwrite: OverwriteMode,
    dest_root: &Path,
    pw: Option<String>,
    enc: Option<NameEncoding>,
    ui: &Ui,
) -> Result<i32> {
    let input = input(archive)?;
    let name = display_name(archive);
    let (ctx, display) = ui.job(&name, pw.clone(), None);
    let ctx = ctx.with_policy(ui.policy(a.allow_high_ratio || ui.yes));
    let mut reader = open(ui, &input.path, pw, enc, &ctx)?;
    let selection = select(reader.entries(), &a.include, &a.exclude)?;

    if a.stdout {
        let mut sink = StdoutSink {
            selection,
            out: io::stdout().lock(),
        };
        let res = reader.extract_to(&mut sink, &ctx);
        display.finish();
        res?;
        return Ok(0);
    }

    let mut destination = dest_root.to_owned();
    let mut smart = !a.no_smart;
    if a.each {
        let stem = archive_stem(&name);
        destination = destination.join(if stem.is_empty() {
            "archive".to_owned()
        } else {
            stem
        });
        smart = false;
    }
    let opts = ExtractOptions {
        destination,
        path_mode: if flat || a.path == PathArg::Flat {
            PathMode::Flat
        } else {
            PathMode::Full
        },
        smart_folder: smart,
        overwrite,
        keep_broken: a.keep_broken,
        strip_unsafe_paths: a.strip_unsafe_paths,
        ..ExtractOptions::default()
    };
    let started = Instant::now();
    let report = omnizip_core::extract(reader.as_mut(), &opts, selection, &ctx);
    display.finish();
    let report = report?;
    for w in &report.warnings {
        ui.warning(w);
    }
    if ui.json {
        ui.emit(
            &json!({"type": "done", "command": "extract", "archive": archive, "report": report}),
        );
    } else {
        let dest = report
            .outputs
            .first()
            .map_or_else(String::new, |p| p.display().to_string());
        ui.info(tr!(
            ui.l,
            "summary-extracted",
            files = report.files,
            size = fmt_size(report.bytes),
            dest = dest,
            elapsed = fmt_elapsed(started.elapsed())
        ));
        if report.skipped > 0 {
            ui.info(tr!(ui.l, "summary-skipped", count = report.skipped));
        }
    }
    Ok(report.exit_code())
}

/// Writes selected file contents to standard output, one after another.
struct StdoutSink<W: Write> {
    selection: Selection,
    out: W,
}

impl<W: Write> ExtractSink for StdoutSink<W> {
    fn wants(&self, entry: &Entry) -> bool {
        entry.kind == EntryKind::File && self.selection.contains(entry.id)
    }

    fn entry(&mut self, _entry: &Entry, data: EntryData<'_>, ctx: &JobCtx) -> Result<()> {
        match data {
            EntryData::Stream(r) => {
                omnizip_core::job::copy_with_progress(r, &mut self.out, ctx, None)?;
            }
            EntryData::WriteTo(w) => {
                let tmp = tempfile::NamedTempFile::new()?.into_temp_path();
                w(&tmp)?;
                io::copy(&mut fs::File::open(&tmp)?, &mut self.out)?;
            }
            EntryData::None => {}
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// create

fn create(a: CreateArgs, ui: &Ui) -> Result<i32> {
    let to_stdout = a.archive.as_os_str() == "-";
    let format = match &a.format {
        Some(f) => parse_format(f)?,
        None if to_stdout => FormatId::TarGz,
        None => FormatId::from_file_name(&display_name(&a.archive)).ok_or_else(|| {
            Error::invalid_argument(tr!(
                ui.l,
                "hint-infer-format",
                name = display_name(&a.archive)
            ))
        })?,
    };
    let level = parse_level(a.level.as_deref())?;
    let volume_size = match &a.volume {
        Some(v) => {
            Some(parse_size(v).ok_or_else(|| Error::invalid_argument(format!("bad size {v}")))?)
        }
        None => None,
    };
    let password = ui.password(&a.pw, true)?;
    let mut exclude = a.exclude.clone();
    if let Some(f) = &a.exclude_from {
        let text = fs::read_to_string(f).map_err(|e| Error::io(e, f))?;
        exclude.extend(
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_owned),
        );
    }
    let staging = if to_stdout {
        Some(tempfile::tempdir()?)
    } else {
        None
    };
    let dest = match &staging {
        Some(d) => d
            .path()
            .join(format!("stdout{}", format.primary_extension())),
        None => a.archive.clone(),
    };
    let name = display_name(&dest);
    let (ctx, display) = ui.job(&name, None, None);
    let started = Instant::now();
    let sources = source::collect(
        &a.paths,
        &CollectOptions {
            include: a.include.clone(),
            exclude,
            follow_symlinks: a.follow_symlinks,
            skip_hidden: a.skip_hidden,
            prefix: String::new(),
            never: vec![dest.clone()],
        },
        &ctx,
    )?;
    let opts = WriteOptions {
        format,
        level,
        method: a.method.clone(),
        threads: ui.threads,
        solid: !a.no_solid,
        password,
        encrypt_names: a.encrypt_headers,
        volume_size,
        comment: a.comment.clone(),
    };
    let reg = shared();
    let exists = dest.exists();
    let updating = exists
        && !a.force
        && volume_size.is_none()
        && reg.capabilities(format).contains(Capabilities::UPDATE)
        && reg.detect(&dest).is_ok_and(|d| d.format == format);
    let result = if updating {
        reg.update(&dest, &[UpdateOp::Add { sources }], &opts, &ctx)
    } else if exists && !a.force && !ui.yes {
        Err(Error::new(ErrorCode::AlreadyExists).with("path", dest.display()))
    } else {
        reg.create(&dest, &sources, &opts, &ctx)
    };
    display.finish();
    let report = result.inspect_err(|e| {
        if e.code() == ErrorCode::UnsupportedOperation
            && e.param("operation") == Some("multiple-files")
        {
            ui.warn_text(&tr!(ui.l, "hint-single-file", format = format.name()));
        }
    })?;
    for w in &report.warnings {
        ui.warning(w);
    }
    if to_stdout {
        let mut f = fs::File::open(&dest).map_err(|e| Error::io(e, &dest))?;
        io::copy(&mut f, &mut io::stdout().lock())?;
        return Ok(report.exit_code());
    }
    let size: u64 = report
        .outputs
        .iter()
        .filter_map(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    if ui.json {
        ui.emit(&json!({"type": "done", "command": if updating { "update" } else { "create" }, "archive": dest, "size": size, "report": report}));
    } else if updating {
        ui.info(tr!(
            ui.l,
            "summary-updated",
            archive = dest.display().to_string()
        ));
    } else {
        ui.info(tr!(
            ui.l,
            "summary-created",
            archive = dest.display().to_string(),
            size = fmt_size(size),
            files = report.files,
            elapsed = fmt_elapsed(started.elapsed())
        ));
        if report.outputs.len() > 1 {
            ui.info(tr!(ui.l, "summary-volumes", count = report.outputs.len()));
        }
    }
    if a.delete_source {
        for p in &a.paths {
            let r = if p.is_dir() {
                fs::remove_dir_all(p)
            } else {
                fs::remove_file(p)
            };
            r.map_err(|e| Error::io(e, p))?;
        }
    }
    Ok(report.exit_code())
}

// ---------------------------------------------------------------------------------------------
// list / info / test

fn list(a: ListArgs, ui: &Ui) -> Result<i32> {
    let pw = ui.password(&a.pw, false)?;
    let input = input(&a.archive)?;
    let (ctx, display) = ui.job(&display_name(&a.archive), pw.clone(), None);
    let reader = open(
        ui,
        &input.path,
        pw,
        parse_encoding(a.encoding.as_deref())?,
        &ctx,
    );
    display.finish();
    let reader = reader?;
    let inc = globs(&a.globs)?;
    let entries: Vec<&Entry> = reader
        .entries()
        .iter()
        .filter(|e| inc.is_none() || matches(&inc, &e.path))
        .collect();

    if ui.json {
        let list: Vec<_> = entries
            .iter()
            .map(|e| {
                json!({
                    "path": e.path, "kind": e.kind, "size": e.size, "sizeKnown": e.size_known,
                    "packedSize": e.packed_size, "mtime": iso_time(e.mtime), "crc32": e.crc32,
                    "encrypted": e.encrypted, "method": e.method, "linkTarget": e.link_target,
                })
            })
            .collect();
        ui.emit(&json!({"archive": a.archive, "info": reader.info(), "entries": list}));
        return Ok(0);
    }
    if ui.porcelain {
        for e in &entries {
            let kind = match e.kind {
                EntryKind::Dir => "d",
                EntryKind::Symlink => "l",
                EntryKind::Hardlink => "h",
                EntryKind::File => "f",
                EntryKind::Other => "o",
            };
            ui.out(format!(
                "{kind}\t{}\t{}\t{}\t{}\t{}",
                e.size,
                e.packed_size.map_or("-".to_owned(), |p| p.to_string()),
                e.mtime
                    .map_or("-".to_owned(), |t| omnizip_core::entry::to_millis(t)
                        .to_string()),
                e.crc32.map_or("-".to_owned(), |c| format!("{c:08X}")),
                e.path
            ));
        }
        return Ok(0);
    }

    let owned: Vec<Entry> = entries.iter().map(|e| (*e).clone()).collect();
    let files = owned.iter().filter(|e| !e.is_dir()).count();
    let dirs = TreeIndex::build(&owned);
    let dir_count = (1..dirs.len() as u32)
        .filter(|&i| dirs.node(i).is_some_and(|n| n.is_dir))
        .count();
    let total: u64 = owned.iter().map(|e| e.size).sum();
    let packed: u64 = owned.iter().filter_map(|e| e.packed_size).sum();
    let packed = if packed == 0 {
        reader.info().archive_size
    } else {
        packed
    };

    if a.tree {
        print_tree(ui, &dirs, TreeIndex::ROOT, "");
    } else {
        let dim = ui.style_dim();
        ui.out(
            dim.apply_to(format!(
                "{:<16}  {:>10}  {:>10}  {}",
                ui.l.get("col-modified"),
                ui.l.get("col-size"),
                ui.l.get("col-packed"),
                ui.l.get("col-name")
            ))
            .to_string(),
        );
        for e in &owned {
            let size = if e.is_dir() {
                String::new()
            } else if e.size_known || e.size > 0 {
                fmt_size(e.size)
            } else {
                "?".into()
            };
            let packed = e.packed_size.map(fmt_size).unwrap_or_default();
            let lock = if e.encrypted { "*" } else { " " };
            let name = if e.is_dir() {
                ui.style_bold().apply_to(&e.path).to_string()
            } else {
                e.path.clone()
            };
            ui.out(format!(
                "{}  {size:>10}  {packed:>10} {lock}{name}",
                fmt_time(e.mtime)
            ));
        }
    }
    ui.out(
        ui.style_dim()
            .apply_to(tr!(
                ui.l,
                "summary-list",
                files = files,
                dirs = dir_count,
                size = fmt_size(total),
                packed = fmt_size(packed)
            ))
            .to_string(),
    );
    Ok(0)
}

fn print_tree(ui: &Ui, t: &TreeIndex, id: u32, prefix: &str) {
    let page = t.list(id, SortKey::Name, false, 0, usize::MAX);
    let n = page.items.len();
    for (i, &c) in page.items.iter().enumerate() {
        let Some(node) = t.node(c) else { continue };
        let last = i + 1 == n;
        let branch = if last { "└── " } else { "├── " };
        let label = if node.is_dir {
            ui.style_bold()
                .apply_to(format!("{}/", node.name))
                .to_string()
        } else {
            format!(
                "{} {}",
                node.name,
                ui.style_dim().apply_to(fmt_size(node.size))
            )
        };
        ui.out(format!("{prefix}{branch}{label}"));
        if node.is_dir {
            let next = format!("{prefix}{}", if last { "    " } else { "│   " });
            print_tree(ui, t, c, &next);
        }
    }
}

fn test(a: TestArgs, ui: &Ui) -> Result<i32> {
    let pw = ui.password(&a.pw, false)?;
    let mut worst = 0;
    for archive in &a.archives {
        let run = || -> Result<i32> {
            let input = input(archive)?;
            let name = display_name(archive);
            let (ctx, display) = ui.job(&name, pw.clone(), None);
            let r = open(ui, &input.path, pw.clone(), None, &ctx)
                .and_then(|mut r| omnizip_core::test(r.as_mut(), Selection::all(), &ctx));
            display.finish();
            let report = r?;
            for w in &report.warnings {
                ui.warning(w);
            }
            let damaged = report
                .warnings
                .iter()
                .filter(|w| {
                    matches!(
                        w.code,
                        ErrorCode::CrcMismatch | ErrorCode::Corrupt | ErrorCode::Truncated
                    )
                })
                .count();
            if ui.json {
                ui.emit(&json!({"type": "done", "command": "test", "archive": archive, "ok": damaged == 0, "report": report}));
            } else if damaged == 0 {
                ui.info(format!(
                    "{} {}",
                    ui.style_ok().apply_to("✓"),
                    tr!(
                        ui.l,
                        "summary-tested",
                        archive = name.as_str(),
                        files = report.files,
                        size = fmt_size(report.bytes)
                    )
                ));
            } else {
                ui.info(tr!(
                    ui.l,
                    "summary-tested-errors",
                    archive = name.as_str(),
                    errors = damaged,
                    files = report.files + damaged as u64
                ));
            }
            Ok(if damaged > 0 {
                ErrorCode::Corrupt.exit_code()
            } else {
                report.exit_code()
            })
        };
        match run() {
            Ok(c) => worst = worst.max(c),
            Err(e) => {
                ui.error(&e);
                worst = worst.max(e.code().exit_code());
                if e.code() == ErrorCode::Cancelled {
                    break;
                }
            }
        }
    }
    Ok(worst)
}

fn info(a: InfoArgs, ui: &Ui) -> Result<i32> {
    let pw = ui.password(&a.pw, false)?;
    let input = input(&a.archive)?;
    let (ctx, display) = ui.job(&display_name(&a.archive), pw.clone(), None);
    let reader = open(
        ui,
        &input.path,
        pw,
        parse_encoding(a.encoding.as_deref())?,
        &ctx,
    );
    display.finish();
    let reader = reader?;
    let i = reader.info();
    let files = reader.entries().iter().filter(|e| !e.is_dir()).count();
    let dirs = reader.entries().iter().filter(|e| e.is_dir()).count();
    let size: u64 = reader.entries().iter().map(|e| e.size).sum();
    if ui.json {
        ui.emit(&json!({"info": i, "files": files, "dirs": dirs, "size": size}));
        return Ok(0);
    }
    let yes_no = |b: bool| ui.l.get(if b { "label-yes" } else { "label-no" });
    let mut rows: Vec<(String, String)> = vec![
        (ui.l.get("label-path"), i.path.display().to_string()),
        (ui.l.get("label-format"), i.format.label()),
        (ui.l.get("label-method"), i.methods.join(", ")),
        (ui.l.get("label-solid"), yes_no(i.solid)),
        (ui.l.get("label-encrypted"), yes_no(i.encrypted)),
    ];
    if let Some(e) = &i.encryption {
        rows.push((ui.l.get("label-encryption"), e.clone()));
    }
    if i.header_encrypted {
        rows.push((ui.l.get("label-header-encrypted"), yes_no(true)));
    }
    if let Some(enc) = i.encoding {
        rows.push((ui.l.get("label-encoding"), enc.label().to_owned()));
    }
    rows.push((ui.l.get("label-volumes"), i.volumes.to_string()));
    rows.push((ui.l.get("label-files"), files.to_string()));
    rows.push((ui.l.get("label-folders"), dirs.to_string()));
    rows.push((ui.l.get("label-size"), fmt_size(size)));
    rows.push((ui.l.get("label-packed"), fmt_size(i.archive_size)));
    if size > 0 {
        rows.push((
            ui.l.get("label-ratio"),
            format!("{:.0}%", i.archive_size as f64 * 100.0 / size as f64),
        ));
    }
    if i.sfx {
        rows.push((ui.l.get("label-sfx"), yes_no(true)));
    }
    if let Some(c) = &i.comment {
        rows.push((ui.l.get("label-comment"), c.clone()));
    }
    let width = rows
        .iter()
        .map(|(k, _)| console::measure_text_width(k))
        .max()
        .unwrap_or(0);
    for (k, v) in rows {
        let pad = width - console::measure_text_width(&k);
        ui.out(format!(
            "{}{}  {v}",
            ui.style_dim().apply_to(&k),
            " ".repeat(pad)
        ));
    }
    Ok(0)
}

// ---------------------------------------------------------------------------------------------
// delete / rename / convert

fn update(ui: &Ui, archive: &Path, ops: &[UpdateOp]) -> Result<omnizip_core::Report> {
    let (ctx, display) = ui.job(&display_name(archive), None, None);
    let r = shared().update(
        archive,
        ops,
        &WriteOptions {
            threads: ui.threads,
            ..Default::default()
        },
        &ctx,
    );
    display.finish();
    r
}

fn delete(a: DeleteArgs, ui: &Ui) -> Result<i32> {
    let report = update(
        ui,
        &a.archive,
        &[UpdateOp::Delete {
            paths: a.entries.clone(),
        }],
    )?;
    if ui.json {
        ui.emit(&json!({"type": "done", "command": "delete", "archive": a.archive, "removed": report.skipped}));
    } else {
        ui.info(tr!(
            ui.l,
            "summary-deleted",
            count = report.skipped,
            archive = a.archive.display().to_string()
        ));
    }
    Ok(0)
}

fn rename(a: RenameArgs, ui: &Ui) -> Result<i32> {
    update(
        ui,
        &a.archive,
        &[UpdateOp::Rename {
            from: a.from.clone(),
            to: a.to.clone(),
        }],
    )?;
    if ui.json {
        ui.emit(&json!({"type": "done", "command": "rename", "archive": a.archive, "from": a.from, "to": a.to}));
    } else {
        ui.info(tr!(
            ui.l,
            "summary-renamed",
            from = a.from.as_str(),
            to = a.to.as_str()
        ));
    }
    Ok(0)
}

fn convert(a: ConvertArgs, ui: &Ui) -> Result<i32> {
    let target = parse_format(&a.to)?;
    let level = parse_level(a.level.as_deref())?;
    let pw = ui.password(&a.pw, false)?;
    let mut worst = 0;
    for src in &a.archives {
        let run = || -> Result<()> {
            let stem = archive_stem(&display_name(src));
            let dir = a
                .out_dir
                .clone()
                .or_else(|| src.parent().map(Path::to_path_buf))
                .unwrap_or_default();
            let dest = dir.join(format!("{stem}{}", target.primary_extension()));
            if dest.exists() && !ui.yes {
                return Err(Error::new(ErrorCode::AlreadyExists).with("path", dest.display()));
            }
            let (ctx, display) = ui.job(&display_name(src), pw.clone(), None);
            let wo = WriteOptions {
                format: target,
                level,
                method: a.method.clone(),
                threads: ui.threads,
                ..Default::default()
            };
            let oo = OpenOptions {
                password: pw.clone(),
                encoding: None,
                locale: Some(ui.l.lang().to_owned()),
            };
            let r = omnizip_formats::convert(shared(), src, &dest, &oo, &wo, &ctx);
            display.finish();
            let report = r?;
            for w in &report.warnings {
                ui.warning(w);
            }
            let size = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            if ui.json {
                ui.emit(&json!({"type": "done", "command": "convert", "source": src, "archive": dest, "size": size}));
            } else {
                ui.info(tr!(
                    ui.l,
                    "summary-converted",
                    src = src.display().to_string(),
                    dest = dest.display().to_string(),
                    size = fmt_size(size)
                ));
            }
            if a.delete_source {
                fs::remove_file(src).map_err(|e| Error::io(e, src))?;
            }
            Ok(())
        };
        if let Err(e) = run() {
            ui.error(&e);
            worst = worst.max(e.code().exit_code());
            if e.code() == ErrorCode::Cancelled {
                break;
            }
        }
    }
    Ok(worst)
}

// ---------------------------------------------------------------------------------------------
// formats / gui

fn formats(ui: &Ui) -> Result<i32> {
    let support = shared().support();
    if ui.json {
        ui.emit(&serde_json::to_value(&support).map_err(Error::internal)?);
        return Ok(0);
    }
    if ui.porcelain {
        for s in &support {
            ui.out(format!(
                "{}\t{}\t{}\t{}",
                s.format,
                s.engine.unwrap_or("-"),
                s.tier,
                s.capabilities.join(",")
            ));
        }
        return Ok(0);
    }
    ui.out(
        ui.style_dim()
            .apply_to(format!(
                "{:<9} {:<13} {:<5} {}",
                ui.l.get("col-format"),
                ui.l.get("col-engine"),
                ui.l.get("col-tier"),
                ui.l.get("col-capabilities")
            ))
            .to_string(),
    );
    for s in &support {
        let caps = if s.engine.is_some() {
            s.capabilities.join(" ").to_lowercase()
        } else {
            ui.l.get("not-available")
        };
        let line = format!(
            "{:<9} {:<13} {:<5} {}",
            s.label,
            s.engine.unwrap_or("—"),
            s.tier,
            caps
        );
        ui.out(if s.engine.is_some() {
            line
        } else {
            ui.style_dim().apply_to(line).to_string()
        });
    }
    Ok(0)
}

fn gui(files: &[PathBuf], ui: &Ui) -> Result<i32> {
    let exe = std::env::current_exe()?;
    let name = if cfg!(windows) {
        "omnizip-desktop.exe"
    } else {
        "omnizip-desktop"
    };
    let candidates = [exe.with_file_name(name), exe.with_file_name("OmniZip.exe")];
    let Some(app) = candidates.iter().find(|p| p.is_file()) else {
        ui.warn_text(&ui.l.get("gui-not-found"));
        return Ok(ErrorCode::MissingDependency.exit_code());
    };
    std::process::Command::new(app)
        .args(files)
        .spawn()
        .map_err(|e| Error::io(e, app))?;
    Ok(0)
}
