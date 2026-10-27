use std::{fs, path::Path, sync::Arc};

use omnizip_core::{
    CollectOptions, ExtractOptions, NullProgress, Selection, extract, job::FixedPassword,
    read_entry, source, test as test_archive,
};

use super::*;

fn ctx() -> JobCtx {
    JobCtx::default()
}

fn pw_ctx(pw: &str) -> JobCtx {
    JobCtx::new(Arc::new(NullProgress), Arc::new(FixedPassword(pw.into())))
}

fn content(i: usize) -> String {
    format!("file number {i}\n").repeat(200 + i * 13)
}

fn tree(root: &Path) -> std::path::PathBuf {
    let p = root.join("数据");
    fs::create_dir_all(p.join("sub/empty-dir")).unwrap();
    for i in 0..12 {
        fs::write(p.join(format!("sub/f{i:02}.txt")), content(i)).unwrap();
    }
    fs::write(p.join("empty.bin"), b"").unwrap();
    p
}

fn build(dir: &Path, opts: &WriteOptions) -> std::path::PathBuf {
    let src = tree(dir);
    let sources = source::collect(&[src], &CollectOptions::default(), &ctx()).unwrap();
    let dest = dir.join("out.7z");
    SevenZEngine.create(&dest, &sources, opts, &ctx()).unwrap();
    dest
}

fn open_with(path: &Path, c: &JobCtx) -> Result<Box<dyn ArchiveReader>> {
    SevenZEngine.open(path, FormatId::SevenZ, &OpenOptions::default(), c)
}

fn open(path: &Path) -> Box<dyn ArchiveReader> {
    open_with(path, &ctx()).unwrap()
}

fn opts7z() -> WriteOptions {
    WriteOptions {
        format: FormatId::SevenZ,
        ..Default::default()
    }
}

#[test]
fn solid_round_trip() {
    let d = tempfile::tempdir().unwrap();
    let a = build(d.path(), &opts7z());
    let mut r = open(&a);
    assert!(r.info().solid);
    assert_eq!(r.info().methods, vec!["LZMA2".to_owned()]);
    let e = r
        .entries()
        .iter()
        .find(|e| e.path == "数据/empty.bin")
        .unwrap();
    assert_eq!((e.size, e.kind), (0, EntryKind::File));
    assert!(
        r.entries()
            .iter()
            .any(|e| e.path == "数据/sub/empty-dir/" && e.is_dir())
    );

    let out = d.path().join("x");
    let o = ExtractOptions {
        destination: out.clone(),
        ..Default::default()
    };
    let rep = extract(r.as_mut(), &o, Selection::all(), &ctx()).unwrap();
    assert_eq!(rep.files, 13);
    for i in 0..12 {
        assert_eq!(
            fs::read_to_string(out.join(format!("数据/sub/f{i:02}.txt"))).unwrap(),
            content(i)
        );
    }
    assert_eq!(fs::read(out.join("数据/empty.bin")).unwrap(), b"");
    assert!(out.join("数据/sub/empty-dir").is_dir());
}

#[test]
fn selective_extraction_inside_a_solid_block() {
    let d = tempfile::tempdir().unwrap();
    let a = build(d.path(), &opts7z());
    let mut r = open(&a);
    let id = r
        .entries()
        .iter()
        .find(|e| e.path == "数据/sub/f07.txt")
        .unwrap()
        .id;
    let out = d.path().join("one");
    let o = ExtractOptions {
        destination: out.clone(),
        ..Default::default()
    };
    extract(r.as_mut(), &o, Selection::of([id], "数据/sub"), &ctx()).unwrap();
    assert_eq!(fs::read_to_string(out.join("f07.txt")).unwrap(), content(7));
    assert_eq!(fs::read_dir(&out).unwrap().count(), 1);

    let (data, cut) = read_entry(r.as_mut(), id, 20, &ctx()).unwrap();
    assert_eq!(data, &content(7).as_bytes()[..20]);
    assert!(cut);
}

#[test]
fn non_solid_and_methods() {
    for (level, method) in [
        (Level::Store, None),
        (Level::Fastest, None),
        (Level::Ultra, None),
        (Level::Normal, Some("lzma")),
        (Level::Normal, Some("bzip2")),
        (Level::Normal, Some("zstd")),
    ] {
        for solid in [false, true] {
            let d = tempfile::tempdir().unwrap();
            let o = WriteOptions {
                level,
                method: method.map(str::to_owned),
                solid,
                ..opts7z()
            };
            let a = build(d.path(), &o);
            let mut r = open(&a);
            let rep = test_archive(r.as_mut(), Selection::all(), &ctx()).unwrap();
            assert!(
                rep.warnings.is_empty(),
                "{level:?} {method:?} solid={solid}: {:?}",
                rep.warnings
            );
            assert_eq!(rep.files, 13, "{level:?} {method:?}");
        }
    }
}

#[test]
fn encrypted_data_and_names() {
    let d = tempfile::tempdir().unwrap();
    let o = WriteOptions {
        password: Some("密码pw".into()),
        encrypt_names: true,
        ..opts7z()
    };
    let a = build(d.path(), &o);

    // Encrypted file list: cannot even list without the password.
    let err = open_with(&a, &ctx()).err().unwrap();
    assert_eq!(err.code(), ErrorCode::PasswordRequired);
    let err = open_with(&a, &pw_ctx("wrong")).err().unwrap();
    assert_eq!(err.code(), ErrorCode::WrongPassword);

    let c = pw_ctx("密码pw");
    let mut r = open_with(&a, &c).unwrap();
    assert!(r.info().header_encrypted && r.info().encrypted);
    let out = d.path().join("x");
    let eo = ExtractOptions {
        destination: out.clone(),
        ..Default::default()
    };
    // The password from the open is reused; the extraction context offers none.
    extract(r.as_mut(), &eo, Selection::all(), &ctx()).unwrap();
    assert_eq!(
        fs::read_to_string(out.join("数据/sub/f03.txt")).unwrap(),
        content(3)
    );
}

#[test]
fn encrypted_data_with_plain_names() {
    let d = tempfile::tempdir().unwrap();
    let o = WriteOptions {
        password: Some("pw".into()),
        encrypt_names: false,
        ..opts7z()
    };
    let a = build(d.path(), &o);
    let mut r = open(&a);
    assert!(!r.info().header_encrypted);
    assert!(r.info().encrypted);
    let eo = ExtractOptions {
        destination: d.path().join("x"),
        ..Default::default()
    };
    let err = extract(r.as_mut(), &eo, Selection::all(), &pw_ctx("bad")).unwrap_err();
    assert_eq!(err.code(), ErrorCode::WrongPassword);
    let mut r = open(&a);
    extract(r.as_mut(), &eo, Selection::all(), &pw_ctx("pw")).unwrap();
    assert_eq!(
        fs::read_to_string(d.path().join("x/数据/sub/f00.txt")).unwrap(),
        content(0)
    );
}

#[test]
fn split_volumes() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("noise.bin");
    let data: Vec<u8> = (0..300_000u64)
        .map(|i| (i.wrapping_mul(6364136223846793005) >> 33) as u8)
        .collect();
    fs::write(&src, &data).unwrap();
    let sources = vec![Source::file(&src, "noise.bin").unwrap()];
    let o = WriteOptions {
        level: Level::Store,
        volume_size: Some(100_000),
        ..opts7z()
    };
    let rep = SevenZEngine
        .create(&d.path().join("v.7z"), &sources, &o, &ctx())
        .unwrap();
    assert!(rep.outputs.len() >= 3);
    let mut r = open(&d.path().join("v.7z.001"));
    let eo = ExtractOptions {
        destination: d.path().join("x"),
        ..Default::default()
    };
    extract(r.as_mut(), &eo, Selection::all(), &ctx()).unwrap();
    assert_eq!(fs::read(d.path().join("x/noise.bin")).unwrap(), data);
}

#[test]
fn cancellation_leaves_no_archive() {
    let d = tempfile::tempdir().unwrap();
    let src = tree(d.path());
    let sources = source::collect(&[src], &CollectOptions::default(), &ctx()).unwrap();
    let c = ctx();
    c.cancel.cancel();
    let err = SevenZEngine
        .create(&d.path().join("c.7z"), &sources, &opts7z(), &c)
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Cancelled);
    assert!(!d.path().join("c.7z").exists());
}
