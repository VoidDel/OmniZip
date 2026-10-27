use std::{fs, path::Path};

use omnizip_core::{
    CollectOptions, ExtractOptions, Level, Selection, extract, source, test as test_archive,
};

use super::*;

fn ctx() -> JobCtx {
    JobCtx::default()
}

fn tree(root: &Path) -> std::path::PathBuf {
    let p = root.join("linux-mini");
    fs::create_dir_all(p.join("kernel/sched")).unwrap();
    fs::create_dir_all(p.join("very/long/").join("x".repeat(120))).unwrap();
    fs::write(
        p.join("kernel/sched/core.c"),
        "int schedule(void) { return 0; }\n".repeat(300),
    )
    .unwrap();
    fs::write(p.join("README"), "Linux kernel\n").unwrap();
    fs::write(
        p.join("very/long")
            .join("x".repeat(120))
            .join("文件名很长的中文文件.txt"),
        "deep",
    )
    .unwrap();
    p
}

fn build(dir: &Path, format: FormatId, level: Level) -> std::path::PathBuf {
    let src = tree(dir);
    let sources = source::collect(&[src], &CollectOptions::default(), &ctx()).unwrap();
    let dest = dir.join(format!("out{}", format.primary_extension()));
    let opts = WriteOptions {
        format,
        level,
        ..Default::default()
    };
    TarEngine.create(&dest, &sources, &opts, &ctx()).unwrap();
    dest
}

fn open(path: &Path) -> Box<dyn ArchiveReader> {
    let d = omnizip_core::detect::detect_path(path).unwrap();
    let f = TarEngine.refine(path, d.format);
    TarEngine
        .open(path, f, &OpenOptions::default(), &ctx())
        .unwrap()
}

#[test]
fn every_tar_variant_round_trips() {
    for format in [
        FormatId::Tar,
        FormatId::TarGz,
        FormatId::TarBz2,
        FormatId::TarXz,
        FormatId::TarZst,
        FormatId::TarLz4,
        FormatId::TarBr,
    ] {
        let d = tempfile::tempdir().unwrap();
        let a = build(d.path(), format, Level::Normal);
        let mut r = open(&a);
        assert_eq!(r.info().format, format);
        let long = r
            .entries()
            .iter()
            .find(|e| e.path.ends_with("文件名很长的中文文件.txt"))
            .unwrap();
        assert!(long.path.len() > 100, "GNU long names survive");
        let out = d.path().join("x");
        let o = ExtractOptions {
            destination: out.clone(),
            ..Default::default()
        };
        let rep = extract(r.as_mut(), &o, Selection::all(), &ctx()).unwrap();
        assert_eq!(rep.files, 3, "{format}");
        assert_eq!(
            fs::read_to_string(out.join("linux-mini/kernel/sched/core.c")).unwrap(),
            "int schedule(void) { return 0; }\n".repeat(300)
        );
    }
}

#[test]
fn misnamed_files_are_refined() {
    let d = tempfile::tempdir().unwrap();
    let a = build(d.path(), FormatId::TarGz, Level::Fast);
    // A tar.gz renamed to .gz is still a tar.
    let renamed = d.path().join("data.gz");
    fs::rename(&a, &renamed).unwrap();
    let det = omnizip_core::detect::detect_path(&renamed).unwrap();
    assert_eq!(det.format, FormatId::Gz);
    assert_eq!(TarEngine.refine(&renamed, det.format), FormatId::TarGz);
}

#[test]
fn single_file_streams() {
    for format in [
        FormatId::Gz,
        FormatId::Bz2,
        FormatId::Xz,
        FormatId::Zst,
        FormatId::Lz4,
        FormatId::Br,
    ] {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("report.csv");
        let data = "id,value\n".to_owned() + &"1,42\n".repeat(10_000);
        fs::write(&src, &data).unwrap();
        let sources = vec![Source::file(&src, "report.csv").unwrap()];
        let dest = d
            .path()
            .join(format!("report.csv{}", format.primary_extension()));
        let opts = WriteOptions {
            format,
            ..Default::default()
        };
        TarEngine.create(&dest, &sources, &opts, &ctx()).unwrap();

        let mut r = open(&dest);
        assert_eq!(r.info().format, format, "not mistaken for a tar");
        assert_eq!(r.entries().len(), 1);
        assert_eq!(r.entries()[0].path, "report.csv");
        if format == FormatId::Gz {
            assert_eq!(r.entries()[0].size, data.len() as u64, "gzip ISIZE");
        }
        let o = ExtractOptions {
            destination: d.path().join("x"),
            ..Default::default()
        };
        extract(r.as_mut(), &o, Selection::all(), &ctx()).unwrap();
        assert_eq!(
            fs::read_to_string(d.path().join("x/report.csv")).unwrap(),
            data,
            "{format}"
        );
    }
}

#[test]
fn single_file_format_refuses_directories() {
    let d = tempfile::tempdir().unwrap();
    let src = tree(d.path());
    let sources = source::collect(&[src], &CollectOptions::default(), &ctx()).unwrap();
    let opts = WriteOptions {
        format: FormatId::Gz,
        ..Default::default()
    };
    let err = TarEngine
        .create(&d.path().join("x.gz"), &sources, &opts, &ctx())
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::UnsupportedOperation);
}

#[test]
fn corrupt_stream_is_reported() {
    let d = tempfile::tempdir().unwrap();
    let a = build(d.path(), FormatId::TarGz, Level::Normal);
    let mut bytes = fs::read(&a).unwrap();
    let mid = bytes.len() / 2;
    for b in &mut bytes[mid..mid + 64] {
        *b ^= 0xA5;
    }
    let bad = d.path().join("bad.tar.gz");
    fs::write(&bad, bytes).unwrap();
    let err = TarEngine
        .open(&bad, FormatId::TarGz, &OpenOptions::default(), &ctx())
        .err()
        .unwrap();
    assert!(
        matches!(
            err.code(),
            ErrorCode::Corrupt | ErrorCode::Truncated | ErrorCode::CrcMismatch
        ),
        "{err:?}"
    );
}

#[test]
fn test_reports_all_files() {
    let d = tempfile::tempdir().unwrap();
    let a = build(d.path(), FormatId::TarZst, Level::Best);
    let mut r = open(&a);
    let rep = test_archive(r.as_mut(), Selection::all(), &ctx()).unwrap();
    assert_eq!(rep.files, 3);
    assert!(rep.warnings.is_empty());
}
