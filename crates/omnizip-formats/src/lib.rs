//! Every OmniZip engine wired into one [`FormatRegistry`], plus operations that span formats
//! (conversion).

use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use omnizip_core::{
    CollectOptions, ExtractOptions, FormatRegistry, JobCtx, OpenOptions, OverwriteMode, Report,
    Result, Selection, SymlinkPolicy, WriteOptions, extract, source,
};

pub use omnizip_core;

/// A registry with every engine compiled into this build.
pub fn registry() -> FormatRegistry {
    let mut r = FormatRegistry::new();
    r.register(Arc::new(omnizip_engine_tar::TarEngine));
    r.register(Arc::new(omnizip_engine_7z::SevenZEngine));
    r.register(Arc::new(omnizip_engine_zip::ZipEngine));
    #[cfg(feature = "rar")]
    r.register(Arc::new(omnizip_engine_rar::RarEngine));
    r
}

/// A process-wide registry.
pub fn shared() -> &'static FormatRegistry {
    static R: OnceLock<FormatRegistry> = OnceLock::new();
    R.get_or_init(registry)
}

/// Write methods per format, for UIs.
pub fn write_methods(format: omnizip_core::FormatId) -> &'static [&'static str] {
    use omnizip_core::FormatId as F;
    match format {
        F::Zip => omnizip_engine_zip::WRITE_METHODS,
        F::SevenZ => omnizip_engine_7z::WRITE_METHODS,
        _ => &[],
    }
}

/// Converts an archive to another format by unpacking into a private temporary directory and
/// packing again. The temporary directory is removed whatever happens.
pub fn convert(
    registry: &FormatRegistry,
    src: &Path,
    dest: &Path,
    open: &OpenOptions,
    write: &WriteOptions,
    ctx: &JobCtx,
) -> Result<Report> {
    let mut reader = registry.open(src, open, ctx)?;
    let staging_parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staging = tempfile::Builder::new()
        .prefix(".omnizip-convert-")
        .tempdir_in(staging_parent)
        .map_err(|e| omnizip_core::Error::io(e, staging_parent))?;
    let xo = ExtractOptions {
        destination: staging.path().to_owned(),
        smart_folder: false,
        overwrite: OverwriteMode::Overwrite,
        symlinks: SymlinkPolicy::CreateIfInside,
        propagate_origin: false,
        ..Default::default()
    };
    let extracted = extract(reader.as_mut(), &xo, Selection::all(), ctx)?;
    drop(reader);
    let tops: Vec<PathBuf> = std::fs::read_dir(staging.path())
        .map_err(|e| omnizip_core::Error::io(e, staging.path()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    let sources = source::collect(&tops, &CollectOptions::default(), ctx)?;
    let mut report = registry.create(dest, &sources, write, ctx)?;
    report.warnings.extend(extracted.warnings);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use omnizip_core::{Capabilities, ErrorCode, FormatId, Level};

    use super::*;

    fn tree(root: &Path) -> PathBuf {
        let p = root.join("相册");
        fs::create_dir_all(p.join("2026")).unwrap();
        fs::write(p.join("2026/a.txt"), "alpha ".repeat(1000)).unwrap();
        fs::write(p.join("b.txt"), "beta").unwrap();
        p
    }

    #[test]
    fn registry_covers_tier_one() {
        let r = registry();
        for s in r.support() {
            if s.tier == 1 {
                assert!(
                    s.caps.contains(Capabilities::READ),
                    "{} must be readable",
                    s.format
                );
            }
        }
        assert!(r.capabilities(FormatId::Rar).contains(Capabilities::READ));
        assert!(!r.capabilities(FormatId::Rar).contains(Capabilities::WRITE));
        assert!(
            r.engine(FormatId::Iso).is_none(),
            "ISO arrives with libarchive in M2"
        );
    }

    #[test]
    fn every_writable_format_round_trips_through_the_registry() {
        let r = registry();
        let ctx = JobCtx::default();
        for f in [
            FormatId::Zip,
            FormatId::SevenZ,
            FormatId::Tar,
            FormatId::TarGz,
            FormatId::TarXz,
            FormatId::TarZst,
        ] {
            let d = tempfile::tempdir().unwrap();
            let src = tree(d.path());
            let sources = source::collect(&[src], &CollectOptions::default(), &ctx).unwrap();
            let dest = d.path().join(format!("out{}", f.primary_extension()));
            let opts = WriteOptions {
                format: f,
                level: Level::Fast,
                ..Default::default()
            };
            r.create(&dest, &sources, &opts, &ctx).unwrap();
            assert_eq!(r.detect(&dest).unwrap().format, f);
            let mut rd = r.open(&dest, &OpenOptions::default(), &ctx).unwrap();
            let out = d.path().join("x");
            let xo = ExtractOptions {
                destination: out.clone(),
                ..Default::default()
            };
            extract(rd.as_mut(), &xo, Selection::all(), &ctx).unwrap();
            assert_eq!(
                fs::read_to_string(out.join("相册/2026/a.txt")).unwrap(),
                "alpha ".repeat(1000),
                "{f}"
            );
        }
    }

    #[test]
    fn convert_rar_to_7z() {
        let r = registry();
        let ctx = JobCtx::default();
        let d = tempfile::tempdir().unwrap();
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rar/version.rar");
        let dest = d.path().join("version.7z");
        let wo = WriteOptions {
            format: FormatId::SevenZ,
            ..Default::default()
        };
        convert(&r, &src, &dest, &OpenOptions::default(), &wo, &ctx).unwrap();
        let rd = r.open(&dest, &OpenOptions::default(), &ctx).unwrap();
        assert_eq!(rd.entries()[0].path, "VERSION");
        let leftovers = fs::read_dir(d.path()).unwrap().count();
        assert_eq!(leftovers, 1, "staging directory removed");
    }

    #[test]
    fn writing_rar_is_refused() {
        let r = registry();
        let ctx = JobCtx::default();
        let d = tempfile::tempdir().unwrap();
        let src = tree(d.path());
        let sources = source::collect(&[src], &CollectOptions::default(), &ctx).unwrap();
        let wo = WriteOptions {
            format: FormatId::Rar,
            ..Default::default()
        };
        let err = r
            .create(&d.path().join("x.rar"), &sources, &wo, &ctx)
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::UnsupportedOperation);
        let wo = WriteOptions {
            format: FormatId::TarGz,
            password: Some("x".into()),
            ..Default::default()
        };
        let err = r
            .create(&d.path().join("x.tgz"), &sources, &wo, &ctx)
            .unwrap_err();
        assert_eq!(
            err.code(),
            ErrorCode::UnsupportedOperation,
            "tar cannot encrypt"
        );
    }
}
