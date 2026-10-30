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

#[test]
fn empty_file_before_many_directories() {
    // An empty file early and twenty directories after it: the kEmptyFile bit vector must cover
    // every stream-less entry, or the header cannot be read back (also by 7-Zip).
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("tree");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("0-empty.txt"), b"").unwrap();
    for i in 0..20 {
        fs::create_dir(src.join(format!("d{i:02}"))).unwrap();
        fs::write(src.join(format!("d{i:02}/x.txt")), content(i)).unwrap();
    }
    let sources = source::collect(&[src], &CollectOptions::default(), &ctx()).unwrap();
    for solid in [true, false] {
        let dest = d.path().join(format!("e-{solid}.7z"));
        let o = WriteOptions { solid, ..opts7z() };
        SevenZEngine.create(&dest, &sources, &o, &ctx()).unwrap();
        let mut r = open(&dest);
        assert_eq!(r.entries().len(), sources.len(), "solid={solid}");
        let eo = ExtractOptions {
            destination: d.path().join(format!("x-{solid}")),
            ..Default::default()
        };
        extract(r.as_mut(), &eo, Selection::all(), &ctx()).unwrap();
        let x = d.path().join(format!("x-{solid}/tree"));
        assert_eq!(fs::read(x.join("0-empty.txt")).unwrap(), b"");
        assert_eq!(
            fs::read_to_string(x.join("d19/x.txt")).unwrap(),
            content(19)
        );
    }
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
fn compressed_files_are_stored_in_blocks_of_their_own() {
    // Fast archiving: the photo and the random file go into a Copy block, the text into LZMA2;
    // with a password the Copy block is encrypted too, and so is the header.
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("media");
    fs::create_dir_all(&src).unwrap();
    let photo = noise(300_000);
    fs::write(src.join("photo.jpg"), &photo).unwrap();
    fs::write(src.join("data.bin"), noise(2 << 20)).unwrap();
    fs::write(src.join("notes.txt"), "text ".repeat(100_000)).unwrap();
    let sources = source::collect(&[src], &CollectOptions::default(), &ctx()).unwrap();
    let cases = [
        (true, true, None),
        (false, true, None),
        (true, false, None),
        (true, true, Some("secret")),
    ];
    for (solid, skip, password) in cases {
        let dest = d
            .path()
            .join(format!("{solid}-{skip}-{}.7z", password.is_some()));
        let o = WriteOptions {
            solid,
            skip_compressed: skip,
            password: password.map(str::to_owned),
            encrypt_names: password.is_some(),
            ..opts7z()
        };
        SevenZEngine.create(&dest, &sources, &o, &ctx()).unwrap();
        let c = password.map_or_else(ctx, pw_ctx);
        let mut r = open_with(&dest, &c).unwrap();
        let method = |r: &dyn ArchiveReader, name: &str| {
            let e = r.entries().iter().find(|e| e.path.ends_with(name));
            e.and_then(|e| e.method.clone()).unwrap_or_default()
        };
        let case = format!("solid={solid} skip={skip} password={password:?}");
        // The reader names the methods of a block in capitals: "COPY", "AES256SHA256 COPY".
        let copy = |r: &dyn ArchiveReader, name: &str| {
            method(r, name).to_ascii_uppercase().contains("COPY")
        };
        assert_eq!(copy(r.as_ref(), "photo.jpg"), skip, "{case}");
        assert_eq!(copy(r.as_ref(), "data.bin"), skip, "{case}");
        assert!(
            method(r.as_ref(), "notes.txt")
                .to_ascii_uppercase()
                .contains("LZMA2"),
            "{case}"
        );
        let t = test_archive(r.as_mut(), Selection::all(), &c).unwrap();
        assert!(t.warnings.is_empty(), "{case}: {:?}", t.warnings);
        let id = r
            .entries()
            .iter()
            .position(|e| e.path.ends_with("photo.jpg"))
            .unwrap();
        let (data, cut) = read_entry(r.as_mut(), id as u32, photo.len(), &c).unwrap();
        assert!(!cut && data == photo, "{case}");
    }
}
