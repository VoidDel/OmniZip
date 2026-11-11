//! Fixture tests. The fixtures are written by the real tools (GNU ar, bsdtar, GNU cpio,
//! dpkg-deb), see `fixtures/make-readonly-fixtures.sh`.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use omnizip_core::{
    ArchiveReader, Engine, EntryKind, ExtractOptions, FormatId, JobCtx, OpenOptions, Selection,
    extract, read_entry, test as test_archive,
};

use crate::ReadonlyEngine;

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel)
}

fn open(path: &Path) -> Box<dyn ArchiveReader> {
    let format = omnizip_core::detect::detect_path(path).unwrap().format;
    ReadonlyEngine
        .open(path, format, &OpenOptions::default(), &JobCtx::default())
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn paths(r: &dyn ArchiveReader) -> Vec<&str> {
    r.entries().iter().map(|e| e.path.as_str()).collect()
}

fn content(r: &mut dyn ArchiveReader, path: &str) -> Vec<u8> {
    let id = r
        .entries()
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("no {path}"))
        .id;
    read_entry(r, id, 1 << 20, &JobCtx::default()).unwrap().0
}

/// Extracts everything and checks that `files` came out with the given contents.
fn extract_all(r: &mut dyn ArchiveReader, files: &[(&str, &[u8])]) {
    let d = tempfile::tempdir().unwrap();
    let xo = ExtractOptions {
        destination: d.path().to_owned(),
        smart_folder: false,
        ..Default::default()
    };
    extract(r, &xo, Selection::all(), &JobCtx::default()).unwrap();
    for (p, want) in files {
        assert_eq!(fs::read(d.path().join(p)).unwrap_or_default(), *want, "{p}");
    }
    // Clones read the same; testing decodes everything.
    let mut c = r.try_clone().unwrap();
    test_archive(c.as_mut(), Selection::all(), &JobCtx::default()).unwrap();
}

#[test]
fn ar_gnu_and_bsd() {
    let mut gnu = open(&fixture("ar/gnu.a"));
    assert_eq!(gnu.info().format, FormatId::Ar);
    assert_eq!(
        paths(gnu.as_ref()),
        [
            "hello.txt",
            "a-very-long-file-name-for-ar.txt",
            "中文名.txt"
        ]
    );
    assert_eq!(content(gnu.as_mut(), "中文名.txt"), "中文内容\n".as_bytes());
    // Stored as it is: a mount reads it in place.
    let (off, len) = gnu.stored_range(0).unwrap();
    assert_eq!(len, 6);
    assert_eq!(
        &fs::read(fixture("ar/gnu.a")).unwrap()[off as usize..][..6],
        b"hello\n"
    );
    extract_all(
        gnu.as_mut(),
        &[("a-very-long-file-name-for-ar.txt", b"long\n")],
    );

    let mut bsd = open(&fixture("ar/bsd.a"));
    assert_eq!(
        paths(bsd.as_ref()),
        ["hello.txt", "a-very-long-file-name-for-ar.txt"]
    );
    assert_eq!(
        content(bsd.as_mut(), "a-very-long-file-name-for-ar.txt"),
        b"long\n"
    );
}

#[test]
fn cpio_every_format() {
    for f in ["newc", "crc", "odc", "bin"] {
        let mut r = open(&fixture(&format!("cpio/{f}.cpio")));
        assert_eq!(r.info().format, FormatId::Cpio, "{f}");
        assert_eq!(
            paths(r.as_ref()),
            [
                "a-very-long-file-name-for-ar.txt",
                "hard.txt",
                "hello.txt",
                "link",
                "sub/",
                "sub/数据/",
                "sub/数据/中文名.txt"
            ],
            "{f}"
        );
        let link = r.entries().iter().find(|e| e.path == "link").unwrap();
        assert_eq!(link.kind, EntryKind::Symlink, "{f}");
        assert_eq!(link.link_target.as_deref(), Some("hello.txt"), "{f}");
        assert_eq!(
            content(r.as_mut(), "sub/数据/中文名.txt"),
            "中文内容\n".as_bytes()
        );
        // A hard link's data is kept once (newc: with the last name); either way both names
        // come out with it.
        extract_all(
            r.as_mut(),
            &[
                ("hello.txt", b"hello\n"),
                ("hard.txt", b"hello\n"),
                ("sub/数据/中文名.txt", "中文内容\n".as_bytes()),
            ],
        );
    }
}

#[test]
fn deb_every_compressor() {
    for z in ["xz", "gzip", "zstd", "none"] {
        let p = fixture(&format!("deb/test-{z}.deb"));
        let det = omnizip_core::detect::detect_path(&p).unwrap();
        assert_eq!(det.format, FormatId::Deb);
        let mut r = open(&p);
        let names = paths(r.as_ref());
        for want in [
            "DEBIAN/",
            "DEBIAN/control",
            "usr/bin/omnizip-test",
            "usr/share/doc/omnizip-test/README",
            "usr/share/doc/omnizip-test/run",
        ] {
            assert!(names.contains(&want), "{z}: {want} in {names:?}");
        }
        let run = r
            .entries()
            .iter()
            .find(|e| e.path.ends_with("/run"))
            .unwrap();
        assert_eq!(run.link_target.as_deref(), Some("../../bin/omnizip-test"));
        assert_eq!(
            r.entries()
                .iter()
                .find(|e| e.path == "usr/bin/omnizip-test")
                .unwrap()
                .unix_mode,
            Some(0o755)
        );
        let control = String::from_utf8(content(r.as_mut(), "DEBIAN/control")).unwrap();
        assert!(control.starts_with("Package: omnizip-test\n"), "{z}");
        extract_all(
            r.as_mut(),
            &[
                ("usr/share/doc/omnizip-test/README", b"doc\n"),
                ("usr/bin/omnizip-test", b"#!/bin/sh\necho hi\n"),
            ],
        );
    }
}

/// A header structure with string tags, as RPM writes one.
fn rpm_header(tags: &[(u32, &str)]) -> Vec<u8> {
    let mut index = Vec::new();
    let mut store = Vec::new();
    for (tag, value) in tags {
        index.extend_from_slice(&tag.to_be_bytes());
        index.extend_from_slice(&6u32.to_be_bytes());
        index.extend_from_slice(&(store.len() as u32).to_be_bytes());
        index.extend_from_slice(&1u32.to_be_bytes());
        store.extend_from_slice(value.as_bytes());
        store.push(0);
    }
    let mut h = vec![0x8E, 0xAD, 0xE8, 0x01, 0, 0, 0, 0];
    h.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    h.extend_from_slice(&(store.len() as u32).to_be_bytes());
    h.extend_from_slice(&index);
    h.extend_from_slice(&store);
    h
}

/// An RPM package around the `newc` fixture: lead, signature header (padded to 8 bytes), main
/// header, gzip payload.
fn rpm_package(compressor: Option<&str>) -> Vec<u8> {
    let mut lead = vec![0xED, 0xAB, 0xEE, 0xDB, 3, 0];
    lead.resize(96, 0);
    let mut out = lead;
    out.extend_from_slice(&rpm_header(&[(1000, "sig")]));
    while out.len() % 8 != 0 {
        out.push(0);
    }
    let mut tags = vec![
        (1000, "omnizip-test"),
        (1001, "1.0"),
        (1002, "1.fc40"),
        (1124, "cpio"),
    ];
    if let Some(c) = compressor {
        tags.push((1125, c));
    }
    out.extend_from_slice(&rpm_header(&tags));
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&fs::read(fixture("cpio/newc.cpio")).unwrap())
        .unwrap();
    out.extend_from_slice(&gz.finish().unwrap());
    out
}

#[test]
fn rpm_payload() {
    let d = tempfile::tempdir().unwrap();
    for compressor in [Some("gzip"), None] {
        let p = d.path().join("test.rpm");
        fs::write(&p, rpm_package(compressor)).unwrap();
        let mut r = open(&p);
        assert_eq!(r.info().format, FormatId::Rpm);
        assert_eq!(r.info().comment.as_deref(), Some("omnizip-test-1.0-1.fc40"));
        assert_eq!(r.info().methods, ["Deflate"]);
        assert!(paths(r.as_ref()).contains(&"sub/数据/中文名.txt"));
        assert_eq!(
            r.entries()
                .iter()
                .find(|e| e.path == "hard.txt")
                .unwrap()
                .kind,
            EntryKind::Hardlink
        );
        extract_all(
            r.as_mut(),
            &[("hello.txt", b"hello\n"), ("hard.txt", b"hello\n")],
        );
    }
    // A payload cut short is damage, not a short file.
    let mut pkg = rpm_package(Some("gzip"));
    pkg.truncate(pkg.len() - 200);
    let p = d.path().join("cut.rpm");
    fs::write(&p, pkg).unwrap();
    let err = match ReadonlyEngine.open(
        &p,
        FormatId::Rpm,
        &OpenOptions::default(),
        &JobCtx::default(),
    ) {
        Ok(mut r) => test_archive(r.as_mut(), Selection::all(), &JobCtx::default()).unwrap_err(),
        Err(e) => e,
    };
    assert_eq!(err.code(), omnizip_core::ErrorCode::Corrupt, "{err}");
}

#[test]
fn damaged_headers_are_refused() {
    let d = tempfile::tempdir().unwrap();
    let ctx = JobCtx::default();
    // An ar member running past the end, a cpio name of absurd length.
    let mut a = fs::read(fixture("ar/gnu.a")).unwrap();
    a.truncate(a.len() - 3);
    let p = d.path().join("cut.a");
    fs::write(&p, a).unwrap();
    assert!(
        ReadonlyEngine
            .open(&p, FormatId::Ar, &OpenOptions::default(), &ctx)
            .is_err()
    );
    let mut c = fs::read(fixture("cpio/newc.cpio")).unwrap();
    c[94..102].copy_from_slice(b"FFFFFFFF");
    let p = d.path().join("bad.cpio");
    fs::write(&p, c).unwrap();
    assert!(
        ReadonlyEngine
            .open(&p, FormatId::Cpio, &OpenOptions::default(), &ctx)
            .is_err()
    );
}

const TREE: [&str; 5] = [
    "A Long File Name With Spaces and Mixed Case.txt",
    "hello.txt",
    "sub/",
    "sub/数据/",
    "sub/数据/中文名.txt",
];

#[test]
fn iso_rock_ridge_joliet_and_plain() {
    for f in ["rockridge", "joliet"] {
        let p = fixture(&format!("iso/{f}.iso"));
        let mut r = open(&p);
        assert_eq!(r.info().format, FormatId::Iso);
        let mut names = paths(r.as_ref());
        names.sort_unstable();
        assert_eq!(names, TREE, "{f}");
        if f == "rockridge" {
            assert_eq!(r.info().comment.as_deref(), Some("OMNIZIP"));
            // Rock Ridge carries POSIX modes.
            assert!(r.entries().iter().all(|e| e.unix_mode.is_some()), "{f}");
        }
        assert!(r.entries().iter().all(|e| e.mtime.is_some()), "{f}");
        // Files lie as they are: a mount reads them in place.
        let id = r
            .entries()
            .iter()
            .find(|e| e.path == "hello.txt")
            .unwrap()
            .id;
        let (off, len) = r.stored_range(id).unwrap();
        assert_eq!(
            &fs::read(&p).unwrap()[off as usize..][..len as usize],
            b"hello
"
        );
        extract_all(
            r.as_mut(),
            &[
                (
                    "hello.txt",
                    b"hello
",
                ),
                (
                    "sub/数据/中文名.txt",
                    "中文内容
"
                    .as_bytes(),
                ),
                (
                    "A Long File Name With Spaces and Mixed Case.txt",
                    b"long
",
                ),
            ],
        );
    }
    // Plain ISO 9660: short upper-case names, without the ";1" version.
    let mut r = open(&fixture("iso/plain.iso"));
    let names = paths(r.as_ref());
    assert!(names.contains(&"HELLO.TXT"), "{names:?}");
    assert_eq!(
        r.entries()
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .count(),
        3
    );
    assert_eq!(
        content(r.as_mut(), "HELLO.TXT"),
        b"hello
"
    );
}

#[test]
fn xar_every_compression() {
    for f in ["test", "bzip2", "none"] {
        let mut r = open(&fixture(&format!("xar/{f}.xar")));
        assert_eq!(r.info().format, FormatId::Xar);
        let mut names = paths(r.as_ref());
        names.sort_unstable();
        assert_eq!(names, TREE, "{f}");
        let id = r
            .entries()
            .iter()
            .find(|e| e.path == "hello.txt")
            .unwrap()
            .id;
        // Only stored data is read in place.
        assert_eq!(r.stored_range(id).is_some(), f == "none", "{f}");
        extract_all(
            r.as_mut(),
            &[
                (
                    "hello.txt",
                    b"hello
",
                ),
                (
                    "sub/数据/中文名.txt",
                    "中文内容
"
                    .as_bytes(),
                ),
            ],
        );
    }
}
