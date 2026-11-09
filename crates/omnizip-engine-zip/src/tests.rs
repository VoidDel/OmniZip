use std::{fs, path::Path, sync::Arc};

use omnizip_core::{
    CollectOptions, ExtractOptions, NullProgress, OverwriteMode, Selection, UpdateOp, extract,
    job::FixedPassword, source,
};

use super::*;

fn ctx() -> JobCtx {
    JobCtx::default()
}

fn pw_ctx(pw: &str) -> JobCtx {
    JobCtx::new(Arc::new(NullProgress), Arc::new(FixedPassword(pw.into())))
}

fn sample_tree(root: &Path) -> std::path::PathBuf {
    let p = root.join("项目");
    fs::create_dir_all(p.join("src/empty")).unwrap();
    fs::write(
        p.join("src/main.rs"),
        "fn main() { println!(\"hi\"); }\n".repeat(50),
    )
    .unwrap();
    fs::write(p.join("说明.txt"), "中文内容".repeat(100)).unwrap();
    p
}

fn build(dir: &Path, opts: &WriteOptions) -> std::path::PathBuf {
    let src = sample_tree(dir);
    let sources = source::collect(&[src], &CollectOptions::default(), &ctx()).unwrap();
    let dest = dir.join("out.zip");
    ZipEngine.create(&dest, &sources, opts, &ctx()).unwrap();
    dest
}

fn open(path: &Path) -> Box<dyn ArchiveReader> {
    ZipEngine
        .open(path, FormatId::Zip, &OpenOptions::default(), &ctx())
        .unwrap()
}

#[test]
fn round_trip_with_unicode_names() {
    let d = tempfile::tempdir().unwrap();
    let zip = build(d.path(), &WriteOptions::default());
    let mut r = open(&zip);
    let names: Vec<_> = r.entries().iter().map(|e| e.path.clone()).collect();
    assert!(names.contains(&"项目/说明.txt".to_owned()), "{names:?}");
    assert!(names.contains(&"项目/src/empty/".to_owned()), "{names:?}");
    assert_eq!(
        r.info().encoding,
        None,
        "UTF-8 flagged names need no detection"
    );
    assert_eq!(r.info().methods, vec!["Deflate".to_owned()]);

    let out = d.path().join("x");
    let opts = ExtractOptions {
        destination: out.clone(),
        ..Default::default()
    };
    let report = extract(r.as_mut(), &opts, Selection::all(), &ctx()).unwrap();
    assert_eq!(report.files, 2);
    assert_eq!(
        fs::read_to_string(out.join("项目/说明.txt")).unwrap(),
        "中文内容".repeat(100),
        "single top-level folder: no extra wrapper folder"
    );
    assert!(out.join("项目/src/empty").is_dir());
}

#[test]
fn every_level_and_method_round_trips() {
    for level in Level::ALL {
        for method in [None, Some("bzip2"), Some("zstd"), Some("xz")] {
            let d = tempfile::tempdir().unwrap();
            let opts = WriteOptions {
                level,
                method: method.map(str::to_owned),
                ..Default::default()
            };
            let zip = build(d.path(), &opts);
            let mut r = open(&zip);
            let rep = omnizip_core::test(r.as_mut(), Selection::all(), &ctx()).unwrap();
            assert!(
                rep.warnings.is_empty(),
                "{level:?} {method:?}: {:?}",
                rep.warnings
            );
            assert_eq!(rep.files, 2);
        }
    }
}

#[test]
fn unsupported_write_method_is_reported() {
    let opts = WriteOptions {
        method: Some("lzma".into()),
        ..Default::default()
    };
    assert_eq!(
        method_and_level(&opts).unwrap_err().code(),
        ErrorCode::UnsupportedMethod
    );
}

#[test]
fn aes_encryption() {
    let d = tempfile::tempdir().unwrap();
    let opts = WriteOptions {
        password: Some("s3cret".into()),
        ..Default::default()
    };
    let zip = build(d.path(), &opts);
    let mut r = open(&zip);
    assert!(r.info().encrypted);
    assert_eq!(r.info().encryption.as_deref(), Some("AES-256"));

    // No password available → PasswordRequired.
    let o = ExtractOptions {
        destination: d.path().join("a"),
        ..Default::default()
    };
    let err = extract(r.as_mut(), &o, Selection::all(), &ctx()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::PasswordRequired);

    // Wrong password → WrongPassword after the retry budget.
    let mut r = open(&zip);
    let err = extract(r.as_mut(), &o, Selection::all(), &pw_ctx("nope")).unwrap_err();
    assert_eq!(err.code(), ErrorCode::WrongPassword);

    let mut r = open(&zip);
    let o = ExtractOptions {
        destination: d.path().join("b"),
        ..Default::default()
    };
    extract(r.as_mut(), &o, Selection::all(), &pw_ctx("s3cret")).unwrap();
    assert!(d.path().join("b/项目/src/main.rs").is_file());
}

#[test]
fn update_delete_rename_add() {
    let d = tempfile::tempdir().unwrap();
    let zip = build(d.path(), &WriteOptions::default());
    let before = fs::metadata(&zip).unwrap().len();
    let extra = d.path().join("new.txt");
    fs::write(&extra, "added").unwrap();
    let ops = vec![
        UpdateOp::Delete {
            paths: vec!["项目/src".into()],
        },
        UpdateOp::Rename {
            from: "项目/说明.txt".into(),
            to: "项目/README.txt".into(),
        },
        UpdateOp::Add {
            sources: vec![Source::file(&extra, "项目/docs/new.txt").unwrap()],
        },
    ];
    ZipEngine
        .update(&zip, FormatId::Zip, &ops, &WriteOptions::default(), &ctx())
        .unwrap();
    let r = open(&zip);
    let mut names: Vec<_> = r.entries().iter().map(|e| e.path.clone()).collect();
    names.sort();
    assert_eq!(names, ["项目/", "项目/README.txt", "项目/docs/new.txt"]);
    assert!(fs::metadata(&zip).unwrap().len() < before);
    let leftovers = fs::read_dir(d.path()).unwrap().filter(|e| {
        e.as_ref()
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".omnizip-")
    });
    assert_eq!(leftovers.count(), 0);
}

/// Names stored with `\` (.NET Framework's `ZipFile`) are updated by the `/` names the browser
/// and mounts use.
#[test]
fn updates_find_names_stored_with_backslashes() {
    use std::io::Write;
    let d = tempfile::tempdir().unwrap();
    let zip = d.path().join("dotnet.zip");
    let mut w = zip::ZipWriter::new(fs::File::create(&zip).unwrap());
    for (name, data) in [
        ("Config\\app.json", "v1"),
        ("Config\\other.json", "o"),
        ("Docs\\a.txt", "a"),
        ("top.txt", "t"),
    ] {
        w.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(data.as_bytes()).unwrap();
    }
    w.finish().unwrap();
    let newer = d.path().join("app.json");
    fs::write(&newer, "v2").unwrap();
    let ops = vec![
        UpdateOp::Delete {
            paths: vec!["Docs".into()],
        },
        UpdateOp::Rename {
            from: "Config/other.json".into(),
            to: "Config/renamed.json".into(),
        },
        UpdateOp::Add {
            sources: vec![Source::file(&newer, "Config/app.json").unwrap()],
        },
    ];
    ZipEngine
        .update(&zip, FormatId::Zip, &ops, &WriteOptions::default(), &ctx())
        .unwrap();
    let r = open(&zip);
    let mut names: Vec<_> = r.entries().iter().map(|e| e.path.clone()).collect();
    names.sort();
    assert_eq!(names, ["Config/app.json", "Config/renamed.json", "top.txt"]);
}

#[test]
fn split_volumes_round_trip() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("blob.bin");
    let data: Vec<u8> = (0..400_000u64)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    fs::write(&src, &data).unwrap();
    let sources = vec![Source::file(&src, "blob.bin").unwrap()];
    let opts = WriteOptions {
        level: Level::Store,
        volume_size: Some(100_000),
        ..Default::default()
    };
    let rep = ZipEngine
        .create(&d.path().join("v.zip"), &sources, &opts, &ctx())
        .unwrap();
    assert_eq!(rep.outputs.len(), 5);
    let mut r = open(&d.path().join("v.zip.001"));
    assert_eq!(r.info().volumes, 5);
    let o = ExtractOptions {
        destination: d.path().join("x"),
        ..Default::default()
    };
    extract(r.as_mut(), &o, Selection::all(), &ctx()).unwrap();
    assert_eq!(fs::read(d.path().join("x/blob.bin")).unwrap(), data);
}

// ---------------------------------------------------------------------------------------------
// Hand-made archives: legacy encodings and hostile paths cannot be produced by ZipWriter.

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// A stored ZIP with raw (un-flagged) names.
fn raw_zip(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let off = out.len() as u32;
        let crc = crc32(data);
        let mut h = Vec::new();
        h.extend_from_slice(b"PK\x03\x04");
        h.extend_from_slice(&20u16.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // flags: no UTF-8
        h.extend_from_slice(&0u16.to_le_bytes()); // stored
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(&0x5B21u16.to_le_bytes());
        h.extend_from_slice(&crc.to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(name);
        out.extend_from_slice(&h);
        out.extend_from_slice(data);

        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0x5B21u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 8]);
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&off.to_le_bytes());
        central.extend_from_slice(name);
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn gbk(s: &str) -> Vec<u8> {
    let enc = omnizip_core::encoding::NameEncoding::from_label("gb18030").unwrap();
    enc.encode(s).into_owned()
}

#[test]
fn gbk_names_are_detected_and_overridable() {
    let d = tempfile::tempdir().unwrap();
    let names = [
        gbk("资料/会议纪要.txt"),
        gbk("资料/项目计划书.docx"),
        gbk("图片/截图.png"),
    ];
    let bytes = raw_zip(&[(&names[0], b"a"), (&names[1], b"b"), (&names[2], b"c")]);
    let p = d.path().join("legacy.zip");
    fs::write(&p, bytes).unwrap();

    let opts = OpenOptions {
        locale: Some("zh-CN".into()),
        ..Default::default()
    };
    let r = ZipEngine.open(&p, FormatId::Zip, &opts, &ctx()).unwrap();
    assert_eq!(r.info().encoding.map(|e| e.label()), Some("gb18030"));
    assert_eq!(r.entries()[0].path, "资料/会议纪要.txt");

    // The user can force another encoding from the menu.
    let opts = OpenOptions {
        encoding: NameEncoding::from_label("cp437"),
        ..Default::default()
    };
    let r = ZipEngine.open(&p, FormatId::Zip, &opts, &ctx()).unwrap();
    assert_ne!(r.entries()[0].path, "资料/会议纪要.txt");
    assert_eq!(r.entries()[0].raw_name, names[0]);
}

#[test]
fn overlap_detection() {
    assert!(overlapping(&[(0, 10), (10, 20), (20, 25)]).is_empty());
    assert_eq!(overlapping(&[(20, 25), (0, 10), (5, 30)]), [0, 1, 2]);
    assert_eq!(overlapping(&[(0, 100), (40, 50), (100, 120)]), [0, 1]);
    assert_eq!(
        overlapping(&[(7, 9), (7, 9)]),
        [0, 1],
        "the same data twice"
    );
}

/// Entries pointing at the same data (an overlap bomb) are marked, and are not extracted
/// together.
#[test]
fn overlapping_entries_are_not_extracted_together() {
    let d = tempfile::tempdir().unwrap();
    let mut bytes = raw_zip(&[
        (b"a.txt", b"same"),
        (b"b.txt", b"same"),
        (b"c.txt", b"else"),
    ]);
    // Point the second central record at the first entry's local header.
    let records: Vec<usize> = bytes
        .windows(4)
        .enumerate()
        .filter(|(_, w)| *w == b"PK\x01\x02")
        .map(|(i, _)| i)
        .collect();
    bytes[records[1] + 42..records[1] + 46].copy_from_slice(&0u32.to_le_bytes());
    let p = d.path().join("overlap.zip");
    fs::write(&p, bytes).unwrap();
    let mut r = open(&p);
    let marked: Vec<bool> = r.entries().iter().map(|e| e.overlaps).collect();
    assert_eq!(marked, [true, true, false]);

    let o = ExtractOptions {
        destination: d.path().join("out"),
        ..Default::default()
    };
    let e = extract(r.as_mut(), &o, Selection::all(), &ctx()).unwrap_err();
    assert_eq!(e.code(), ErrorCode::CompressionBomb);
    assert_eq!(e.param("issue"), Some("overlapping-entries"));
    assert!(!d.path().join("out").exists(), "nothing written");
    // One of them at a time is fine.
    extract(r.as_mut(), &o, Selection::of([0, 2], ""), &ctx()).unwrap();
    let mut names: Vec<String> = walk(&d.path().join("out"))
        .iter()
        .filter(|p| p.is_file())
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    assert_eq!(names, ["a.txt", "c.txt"]);
}

#[test]
fn hostile_paths_never_escape() {
    let d = tempfile::tempdir().unwrap();
    let bytes = raw_zip(&[
        (b"../escape.txt", b"x"),
        (b"..\\escape2.txt", b"x"),
        (b"/abs.txt", b"x"),
        (b"C:/Windows/evil.dll", b"x"),
        (b"ok/fine.txt", b"fine"),
        (b"ok/CON.txt", b"reserved"),
    ]);
    let p = d.path().join("evil.zip");
    fs::write(&p, bytes).unwrap();
    let mut r = open(&p);
    let dest = d.path().join("out");
    let o = ExtractOptions {
        destination: dest.clone(),
        overwrite: OverwriteMode::Overwrite,
        ..Default::default()
    };
    let rep = extract(r.as_mut(), &o, Selection::all(), &ctx()).unwrap();
    assert!(!d.path().join("escape.txt").exists());
    assert!(!d.path().join("escape2.txt").exists());
    assert!(
        dest.join("evil/ok/fine.txt").is_file(),
        "several top-level items → wrapper folder"
    );
    assert_eq!(rep.exit_code(), 8);
    assert!(rep.skipped >= 3, "{rep:?}");
    let walked: Vec<_> = walk(&dest);
    assert!(walked.iter().all(|p| p.starts_with(&dest)));
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        }
        out.push(p);
    }
    out
}

#[test]
fn corrupt_data_is_reported_not_written() {
    let d = tempfile::tempdir().unwrap();
    let mut bytes = raw_zip(&[(b"a.txt", b"hello world")]);
    // Flip a data byte so the CRC no longer matches.
    let pos = bytes.windows(5).position(|w| w == b"hello").unwrap();
    bytes[pos] = b'j';
    let p = d.path().join("bad.zip");
    fs::write(&p, bytes).unwrap();
    let mut r = open(&p);
    let rep = omnizip_core::test(r.as_mut(), Selection::all(), &ctx()).unwrap();
    assert_eq!(rep.warnings.len(), 1);
    assert_eq!(rep.warnings[0].code, ErrorCode::CrcMismatch);
    let mut r = open(&p);
    let o = ExtractOptions {
        destination: d.path().join("out"),
        ..Default::default()
    };
    let rep = extract(r.as_mut(), &o, Selection::all(), &ctx()).unwrap();
    assert_eq!(rep.warnings[0].code, ErrorCode::CrcMismatch);
    assert!(!d.path().join("out/a.txt").exists());
}

#[test]
fn only_unix_entries_carry_a_mode() {
    use std::io::Write as _;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("mixed.zip");
    let mut w = ZipWriter::new(fs::File::create(&p).unwrap());
    let opts = SimpleFileOptions::default();
    w.start_file("dos.txt", opts.system(System::Dos)).unwrap();
    w.write_all(b"a").unwrap();
    w.start_file("unix.sh", opts.system(System::Unix).unix_permissions(0o755))
        .unwrap();
    w.write_all(b"b").unwrap();
    w.finish().unwrap();

    // The zip crate makes a mode up for the DOS entry ...
    let mut z = ZipArchive::new(fs::File::open(&p).unwrap()).unwrap();
    assert!(z.by_index(0).unwrap().unix_mode().is_some());
    // ... which the entry does not take: extraction gives it a new file's permissions.
    let r = open(&p);
    let (dos, unix) = (&r.entries()[0], &r.entries()[1]);
    assert_eq!(dos.unix_mode, None);
    assert!(dos.win_attrs.is_some());
    assert_eq!(unix.unix_mode.map(|m| m & 0o777), Some(0o755));
}

fn noisy(len: usize) -> Vec<u8> {
    let mut x = 0x9E37_79B9_u32;
    (0..len)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            if i % 5 == 0 {
                x as u8
            } else {
                b"OmniZip!"[i % 8]
            }
        })
        .collect()
}

#[test]
fn threads_do_not_change_small_entries() {
    // Files compressed on worker threads come out byte for byte as the archive writer writes
    // them on one thread.
    let d = tempfile::tempdir().unwrap();
    let src = sample_tree(d.path());
    for i in 0..40 {
        fs::write(src.join(format!("src/m{i:02}.bin")), noisy(1000 + i * 997)).unwrap();
    }
    let sources = source::collect(&[src], &CollectOptions::default(), &ctx()).unwrap();
    let make = |threads, name: &str| {
        let dest = d.path().join(name);
        let o = WriteOptions {
            format: FormatId::Zip,
            threads,
            ..Default::default()
        };
        ZipEngine.create(&dest, &sources, &o, &ctx()).unwrap();
        fs::read(dest).unwrap()
    };
    assert_eq!(make(1, "one.zip"), make(4, "four.zip"));
}

#[test]
fn large_files_deflated_on_several_threads() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("大文件");
    fs::create_dir_all(&src).unwrap();
    let big = noisy(20 << 20);
    fs::write(src.join("巨大.bin"), &big).unwrap();
    fs::write(src.join("small.txt"), b"small").unwrap();
    let sources = source::collect(&[src], &CollectOptions::default(), &ctx()).unwrap();
    let make = |threads, name: &str| {
        let dest = d.path().join(name);
        let o = WriteOptions {
            format: FormatId::Zip,
            threads,
            ..Default::default()
        };
        ZipEngine.create(&dest, &sources, &o, &ctx()).unwrap();
        dest
    };
    let (one, four) = (make(1, "one.zip"), make(4, "four.zip"));
    // Same entries, same metadata, same content.
    let mut a = ZipArchive::new(File::open(&one).unwrap()).unwrap();
    let mut b = ZipArchive::new(File::open(&four).unwrap()).unwrap();
    assert_eq!(a.len(), b.len());
    for i in 0..a.len() {
        let (x, mut y) = (a.by_index(i).unwrap(), b.by_index(i).unwrap());
        assert_eq!(x.name().unwrap(), y.name().unwrap());
        assert_eq!(x.compression(), y.compression());
        assert_eq!(x.crc32(), y.crc32());
        assert_eq!(x.size(), y.size());
        assert_eq!(x.unix_mode(), y.unix_mode());
        assert_eq!(x.last_modified(), y.last_modified());
        assert_eq!(x.system(), y.system());
        assert_eq!(x.external_attributes(), y.external_attributes());
        let mut back = Vec::new();
        y.read_to_end(&mut back).unwrap();
        if x.name().unwrap().ends_with("巨大.bin") {
            assert_eq!(back, big);
            assert!(y.compressed_size() < big.len() as u64);
        }
    }
    // And extraction through the engine checks every CRC.
    let mut r = open(&four);
    let eo = ExtractOptions {
        destination: d.path().join("x"),
        ..Default::default()
    };
    let rep = extract(r.as_mut(), &eo, Selection::all(), &ctx()).unwrap();
    assert!(rep.warnings.is_empty(), "{:?}", rep.warnings);
    assert_eq!(fs::read(d.path().join("x/大文件/巨大.bin")).unwrap(), big);
}

#[test]
fn update_adds_large_files_on_several_threads() {
    let d = tempfile::tempdir().unwrap();
    let a = build(d.path(), &WriteOptions::default());
    let big = d.path().join("add.bin");
    let data = noisy(17 << 20);
    fs::write(&big, &data).unwrap();
    let sources = source::collect(&[big], &CollectOptions::default(), &ctx()).unwrap();
    let o = WriteOptions {
        threads: 4,
        ..Default::default()
    };
    ZipEngine
        .update(&a, FormatId::Zip, &[UpdateOp::Add { sources }], &o, &ctx())
        .unwrap();
    let mut z = ZipArchive::new(File::open(&a).unwrap()).unwrap();
    let mut back = Vec::new();
    z.by_name("add.bin")
        .unwrap()
        .read_to_end(&mut back)
        .unwrap();
    assert_eq!(back, data);
    assert!(z.by_name("项目/说明.txt").is_ok(), "existing entries kept");
}

/// Pseudo-random bytes (xorshift): content that is compressed already.
fn noise(len: usize) -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

#[test]
fn compressed_files_are_stored() {
    // Fast archiving: a photo by its name, a large file with another name by its content; the
    // large one also takes the path of large files on several threads.
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("media");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("photo.jpg"), noise(300_000)).unwrap();
    fs::write(src.join("data.bin"), noise(17 << 20)).unwrap();
    fs::write(src.join("notes.txt"), "text ".repeat(100_000)).unwrap();
    let sources = source::collect(&[src], &CollectOptions::default(), &ctx()).unwrap();
    for (threads, skip) in [(1, true), (4, true), (4, false)] {
        let dest = d.path().join(format!("{threads}-{skip}.zip"));
        let opts = WriteOptions {
            threads,
            skip_compressed: skip,
            ..Default::default()
        };
        ZipEngine.create(&dest, &sources, &opts, &ctx()).unwrap();
        let mut r = open(&dest);
        let method = |r: &dyn ArchiveReader, name: &str| {
            let e = r.entries().iter().find(|e| e.path.ends_with(name));
            e.and_then(|e| e.method.clone()).unwrap_or_default()
        };
        let packed = if skip { "Store" } else { "Deflate" };
        assert_eq!(method(r.as_ref(), "photo.jpg"), packed, "{threads} {skip}");
        assert_eq!(method(r.as_ref(), "data.bin"), packed, "{threads} {skip}");
        assert_eq!(
            method(r.as_ref(), "notes.txt"),
            "Deflate",
            "{threads} {skip}"
        );
        let t = omnizip_core::test(r.as_mut(), Selection::all(), &ctx()).unwrap();
        assert!(t.warnings.is_empty(), "{:?}", t.warnings);
    }
}

/// `n` files in a few folders, of varied sizes, and one file larger than a unit of a parallel
/// extraction.
fn many_files(root: &Path, n: usize) -> std::path::PathBuf {
    let p = root.join("many");
    for i in 0..n {
        let dir = p.join(format!("d{}", i % 7));
        fs::create_dir_all(&dir).unwrap();
        let body = format!("{i}:").repeat(100 + i * 31);
        fs::write(dir.join(format!("f{i:03}.txt")), body).unwrap();
    }
    fs::write(p.join("big.bin"), noisy(9 << 20)).unwrap();
    p
}

fn zip_of(dir: &Path, src: &Path, opts: &WriteOptions) -> std::path::PathBuf {
    let sources = source::collect(&[src.to_owned()], &CollectOptions::default(), &ctx()).unwrap();
    let dest = dir.join("many.zip");
    ZipEngine.create(&dest, &sources, opts, &ctx()).unwrap();
    dest
}

/// Relative path → content and modification time of every file below `root`.
fn snapshot(
    root: &Path,
) -> std::collections::BTreeMap<std::path::PathBuf, (Vec<u8>, std::time::SystemTime)> {
    walk(root)
        .into_iter()
        .filter(|p| p.is_file())
        .map(|p| {
            let m = fs::metadata(&p).unwrap();
            (
                p.strip_prefix(root).unwrap().to_owned(),
                (fs::read(&p).unwrap(), m.modified().unwrap()),
            )
        })
        .collect()
}

fn extract_with(zip: &Path, dest: &Path, threads: u32, c: &JobCtx) -> omnizip_core::Report {
    let mut r = open(zip);
    let o = ExtractOptions {
        destination: dest.to_owned(),
        threads,
        ..Default::default()
    };
    extract(r.as_mut(), &o, Selection::all(), c).unwrap()
}

/// Extracting on several threads writes the same files, with the same times, and reports the
/// same totals as on one.
#[test]
fn parallel_extraction_writes_what_serial_does() {
    let d = tempfile::tempdir().unwrap();
    let src = many_files(d.path(), 150);
    let zip = zip_of(d.path(), &src, &WriteOptions::default());
    let one = extract_with(&zip, &d.path().join("one"), 1, &ctx());
    let four = extract_with(&zip, &d.path().join("four"), 4, &ctx());
    assert_eq!(
        (one.files, one.dirs, one.bytes, one.skipped),
        (four.files, four.dirs, four.bytes, four.skipped)
    );
    assert_eq!(four.files, 151);
    assert!(four.warnings.is_empty(), "{:?}", four.warnings);
    assert_eq!(four.outputs, vec![d.path().join("four/many")]);
    let a = snapshot(&d.path().join("one"));
    assert_eq!(a.len(), 151);
    assert_eq!(a, snapshot(&d.path().join("four")));
}

/// Counts the questions it is asked: the password, and conflicts (answered "overwrite all").
#[derive(Default)]
struct Counting {
    passwords: std::sync::atomic::AtomicUsize,
    conflicts: std::sync::atomic::AtomicUsize,
}

impl omnizip_core::Interaction for Counting {
    fn password(&self, _req: &omnizip_core::job::PasswordRequest) -> Option<Secret> {
        self.passwords
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some("s3cret".into())
    }

    fn conflict(&self, _c: &omnizip_core::job::Conflict) -> omnizip_core::job::ConflictDecision {
        self.conflicts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        omnizip_core::job::ConflictDecision {
            choice: omnizip_core::job::ConflictChoice::Overwrite,
            apply_to_all: true,
        }
    }
}

/// An encrypted archive extracted on several threads asks for its password once, and a
/// conflict answered for all is asked once.
#[test]
fn parallel_extraction_asks_each_question_once() {
    let d = tempfile::tempdir().unwrap();
    let src = many_files(d.path(), 120);
    let zip = zip_of(
        d.path(),
        &src,
        &WriteOptions {
            password: Some("s3cret".into()),
            ..Default::default()
        },
    );
    let asked = Arc::new(Counting::default());
    let c = JobCtx::new(Arc::new(NullProgress), asked.clone());
    let dest = d.path().join("out");
    let first = extract_with(&zip, &dest, 4, &c);
    assert_eq!(first.files, 121);
    assert_eq!(asked.passwords.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(snapshot(&dest.join("many")).len(), 121);

    // Again into the same folder: every file is in the way.
    let mut r = open(&zip);
    let o = ExtractOptions {
        destination: dest.clone(),
        overwrite: OverwriteMode::Ask,
        threads: 4,
        ..Default::default()
    };
    let again = extract(r.as_mut(), &o, Selection::all(), &c).unwrap();
    assert_eq!(again.files, 121);
    assert_eq!(asked.conflicts.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// Two entries landing on the same file are written in archive order, the later one last,
/// even when several threads are asked for.
#[test]
fn entries_on_the_same_file_keep_archive_order() {
    let d = tempfile::tempdir().unwrap();
    let names: Vec<String> = (0..80).map(|i| format!("f{i:02}.txt")).collect();
    let mut entries: Vec<(&[u8], &[u8])> = vec![(b"same.txt", b"older")];
    entries.extend(names.iter().map(|n| (n.as_bytes(), n.as_bytes())));
    entries.push((b"same.txt", b"newer"));
    let zip = d.path().join("dup.zip");
    fs::write(&zip, raw_zip(&entries)).unwrap();
    let mut r = open(&zip);
    let o = ExtractOptions {
        destination: d.path().join("out"),
        smart_folder: false,
        overwrite: OverwriteMode::Overwrite,
        threads: 4,
        ..Default::default()
    };
    extract(r.as_mut(), &o, Selection::all(), &ctx()).unwrap();
    assert_eq!(fs::read(d.path().join("out/same.txt")).unwrap(), b"newer");
    assert_eq!(fs::read(d.path().join("out/f42.txt")).unwrap(), b"f42.txt");
}

/// A damaged entry among many is skipped with a warning by whichever thread meets it, and the
/// others are written; a truncated archive stops the extraction with an error.
#[test]
fn parallel_extraction_reports_damage() {
    let d = tempfile::tempdir().unwrap();
    let names: Vec<String> = (0..90).map(|i| format!("f{i:02}.txt")).collect();
    let bodies: Vec<String> = (0..90).map(|i| format!("body of file {i:02} ")).collect();
    let entries: Vec<(&[u8], &[u8])> = names
        .iter()
        .zip(&bodies)
        .map(|(n, b)| (n.as_bytes(), b.as_bytes()))
        .collect();
    let mut bytes = raw_zip(&entries);
    let pos = bytes
        .windows(15)
        .position(|w| w == b"body of file 57")
        .unwrap();
    bytes[pos] = b'B';
    let p = d.path().join("bad.zip");
    fs::write(&p, &bytes).unwrap();
    let mut r = open(&p);
    let o = ExtractOptions {
        destination: d.path().join("out"),
        smart_folder: false,
        threads: 4,
        ..Default::default()
    };
    let rep = extract(r.as_mut(), &o, Selection::all(), &ctx()).unwrap();
    assert_eq!(rep.warnings.len(), 1, "{:?}", rep.warnings);
    assert_eq!(rep.warnings[0].code, ErrorCode::CrcMismatch);
    assert_eq!((rep.files, rep.skipped), (89, 1));
    assert!(!d.path().join("out/f57.txt").exists());
    assert_eq!(
        fs::read(d.path().join("out/f58.txt")).unwrap(),
        b"body of file 58 "
    );

    // The data of the later entries cut off: an error, not a hang.
    let local_end = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    let mut cut = bytes.clone();
    let start = cut
        .windows(15)
        .position(|w| w == b"body of file 70")
        .unwrap();
    cut.splice(
        start..local_end,
        std::iter::repeat_n(0u8, local_end - start),
    );
    let p = d.path().join("cut.zip");
    fs::write(&p, &cut).unwrap();
    let mut r = open(&p);
    let o = ExtractOptions {
        destination: d.path().join("cut"),
        ..o
    };
    let rep = extract(r.as_mut(), &o, Selection::all(), &ctx());
    match rep {
        Err(e) => assert_ne!(e.code(), ErrorCode::Cancelled),
        Ok(rep) => assert!(!rep.warnings.is_empty(), "the damage is reported"),
    }
}
