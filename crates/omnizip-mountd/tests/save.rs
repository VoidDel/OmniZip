//! Saving the changes of a writable mount into the archive, for each kind of format: ZIP in
//! place, 7z and tar.gz packed again, RAR refused.

use std::{
    fs,
    path::{Path, PathBuf},
};

use omnizip_core::{
    CollectOptions, ErrorCode, FormatId, JobCtx, Level, OpenOptions, WriteOptions, source,
};
use omnizip_mountd::{SavedLayer, proto::Changes, save};
use omnizip_vfs::{ArchiveFs, FsError, FsOptions, read_all, resolve};

/// `docs/readme.txt`, `docs/notes.txt`, `keep/a.txt`.
fn archive(dir: &Path, format: FormatId, password: Option<&str>) -> PathBuf {
    let root = dir.join("src");
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::create_dir_all(root.join("keep")).unwrap();
    fs::write(root.join("docs/readme.txt"), "read me").unwrap();
    fs::write(root.join("docs/notes.txt"), "notes").unwrap();
    fs::write(root.join("keep/a.txt"), "alpha").unwrap();
    let ctx = JobCtx::default();
    let roots = [root.join("docs"), root.join("keep")];
    let sources = source::collect(&roots, &CollectOptions::default(), &ctx).unwrap();
    let dest = dir.join(format!("a{}", format.primary_extension()));
    let wo = WriteOptions {
        format,
        level: Level::Fast,
        password: password.map(str::to_owned),
        ..Default::default()
    };
    omnizip_formats::shared()
        .create(&dest, &sources, &wo, &ctx)
        .unwrap();
    dest
}

/// A layer that changes `docs/readme.txt`, deletes `docs/notes.txt` and adds `new/n.txt`.
fn layer(dir: &Path, archive: PathBuf, password: Option<&str>) -> SavedLayer {
    let layer = dir.join("layer");
    fs::create_dir_all(layer.join("docs")).unwrap();
    fs::create_dir_all(layer.join("new")).unwrap();
    fs::write(layer.join("docs/readme.txt"), "READ ME").unwrap();
    fs::write(layer.join("new/n.txt"), "n").unwrap();
    SavedLayer {
        archive,
        layer,
        changes: Changes {
            files: vec!["docs/readme.txt".into(), "new/n.txt".into()],
            dirs: vec!["new".into()],
            deleted: vec!["docs/notes.txt".into()],
            entries: vec!["docs/notes.txt".into()],
        },
        password: password.map(str::to_owned),
        encoding: None,
    }
}

fn open(path: &Path, password: Option<&str>) -> ArchiveFs {
    let oo = OpenOptions {
        password: password.map(str::to_owned),
        ..Default::default()
    };
    let reader = omnizip_formats::shared()
        .open(path, &oo, &JobCtx::default())
        .unwrap();
    ArchiveFs::new(
        reader,
        FsOptions {
            password: password.map(str::to_owned),
            ..Default::default()
        },
    )
    .unwrap()
}

fn text(fs: &ArchiveFs, path: &str) -> String {
    String::from_utf8(read_all(fs, resolve(fs, path).unwrap().node, 4096).unwrap()).unwrap()
}

fn check_saved(format: FormatId, password: Option<&str>) {
    let d = tempfile::tempdir().unwrap();
    let a = archive(d.path(), format, password);
    let saved = layer(d.path(), a.clone(), password);
    assert!(save::can_save(open(&a, password).info()));
    save::save(&saved, 0, &JobCtx::default()).unwrap();
    assert!(
        !saved.layer.exists(),
        "{format}: the layer is removed once saved"
    );

    let fs = open(&a, password);
    assert_eq!(fs.info().format, format);
    assert_eq!(text(&fs, "docs/readme.txt"), "READ ME", "{format}");
    assert_eq!(text(&fs, "new/n.txt"), "n", "{format}");
    assert_eq!(text(&fs, "keep/a.txt"), "alpha", "{format}");
    assert_eq!(
        resolve(&fs, "docs/notes.txt"),
        Err(FsError::NotFound),
        "{format}"
    );
    assert!(fs.info().encrypted == password.is_some(), "{format}");
    // Nothing is left beside the archive.
    let mut beside: Vec<String> = fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    beside.sort();
    assert_eq!(beside, [a.file_name().unwrap().to_str().unwrap(), "src"]);
}

#[test]
fn zip_is_updated_in_place() {
    check_saved(FormatId::Zip, None);
    check_saved(FormatId::Zip, Some("pw-2026"));
}

#[test]
fn other_formats_are_packed_again() {
    check_saved(FormatId::SevenZ, None);
    check_saved(FormatId::SevenZ, Some("pw-2026"));
    check_saved(FormatId::TarGz, None);
}

#[test]
fn rar_cannot_be_saved() {
    let rar = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rar/solid.rar");
    let d = tempfile::tempdir().unwrap();
    let copy = d.path().join("solid.rar");
    fs::copy(&rar, &copy).unwrap();
    assert!(!save::can_save(open(&copy, None).info()));
    let saved = layer(d.path(), copy.clone(), None);
    let e = save::save(&saved, 0, &JobCtx::default()).unwrap_err();
    assert_eq!(e.code(), ErrorCode::UnsupportedOperation);
    // The changes stay for another try (or to be copied out).
    assert!(saved.layer.join("new/n.txt").is_file());
    assert_eq!(fs::read(&copy).unwrap(), fs::read(&rar).unwrap());
}
