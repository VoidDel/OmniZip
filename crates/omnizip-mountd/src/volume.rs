//! Opening an archive as a volume: what every mount does before a backend serves it, and what
//! the app does to check a password before asking for administrator rights.

use std::{path::PathBuf, sync::Arc};

use omnizip_core::{
    Error, ErrorCode, FixedPassword, JobCtx, NullProgress, OpenOptions, encoding::NameEncoding,
};
use omnizip_vfs::{ArchiveFs, CacheLimits, FsError, FsOptions};

use crate::proto::MountSpec;

/// Opens `spec.archive` as a volume caching under `cache_dir`, with its label, and checks the
/// password: a wrong one is refused before anything is mounted.
pub fn open_volume(
    spec: &MountSpec,
    cache_dir: Option<PathBuf>,
) -> Result<(Arc<ArchiveFs>, String), Error> {
    let password = spec.password.clone();
    let ctx = match &password {
        Some(p) => JobCtx::new(Arc::new(NullProgress), Arc::new(FixedPassword(p.clone()))),
        None => JobCtx::default(),
    };
    let oo = OpenOptions {
        password: password.clone(),
        encoding: spec.encoding.as_deref().and_then(NameEncoding::from_label),
        locale: Some(omnizip_i18n::detect_language(None).to_owned()),
    };
    let reader = omnizip_formats::shared().open(&spec.archive, &oo, &ctx)?;
    // Without a password, encrypted entries could be listed but never read.
    if reader.info().encrypted && password.is_none() {
        return Err(Error::new(ErrorCode::PasswordRequired).with("path", spec.archive.display()));
    }
    let label = spec
        .label
        .clone()
        .filter(|l| !l.trim().is_empty())
        .unwrap_or_else(|| omnizip_vfs::archivefs::volume_label(reader.info()));
    let fs = Arc::new(
        ArchiveFs::new(
            reader,
            FsOptions {
                cache: CacheLimits {
                    disk: spec.cache_bytes.max(1 << 30),
                    ..CacheLimits::default()
                },
                cache_dir: cache_dir.clone(),
                label: Some(label.clone()),
                password,
                ..FsOptions::default()
            },
        )
        .map_err(|e| Error::io(e, cache_dir.unwrap_or_default()))?,
    );
    match fs.check_password() {
        Err(FsError::Access) => {
            return Err(Error::new(ErrorCode::WrongPassword).with("path", spec.archive.display()));
        }
        // A damaged entry does not keep the rest of the archive from being mounted.
        Err(e) => tracing::warn!("mount: checking the password: {e}"),
        Ok(()) => {}
    }
    Ok((fs, label))
}
