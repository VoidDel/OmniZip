//! RAR engine (RAR 4 and RAR 5, extraction only).
//!
//! Built on the `unrar` library, whose license allows extraction but forbids using it to create
//! RAR archives; this crate therefore implements no writer and stays isolated from all
//! compression code (`docs/01-overview.md` §8). The C++ library will move into the sandboxed
//! worker process in v2 (`docs/09-security.md` §2).

mod kdf;

use std::path::{Path, PathBuf};

use omnizip_core::{
    ArchiveInfo, ArchiveReader, Capabilities, Engine, Entry, EntryData, EntryKind, Error,
    ErrorCode, ExtractSink, FormatId, JobCtx, OpenOptions, Report, Result, Secret, Selection,
    job::MAX_PASSWORD_ATTEMPTS, timeutil,
};
use unrar::{
    Archive,
    error::{Code, UnrarError},
};

/// The RAR engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct RarEngine;

impl Engine for RarEngine {
    fn name(&self) -> &'static str {
        "unrar"
    }

    fn formats(&self) -> &'static [FormatId] {
        &[FormatId::Rar]
    }

    fn capabilities(&self, _format: FormatId) -> Capabilities {
        Capabilities::READ
            | Capabilities::DECRYPT
            | Capabilities::MULTI_VOLUME
            | Capabilities::SOLID
            | Capabilities::COMMENT
            | Capabilities::MOUNT
    }

    fn open(
        &self,
        path: &Path,
        _format: FormatId,
        opts: &OpenOptions,
        ctx: &JobCtx,
    ) -> Result<Box<dyn ArchiveReader>> {
        Ok(Box::new(RarReader::open(path, opts, ctx)?))
    }
}

fn map_err(e: UnrarError, path: &Path) -> Error {
    let code = match e.code {
        Code::MissingPassword => ErrorCode::PasswordRequired,
        Code::BadPassword => ErrorCode::WrongPassword,
        Code::BadData => ErrorCode::CrcMismatch,
        Code::BadArchive => ErrorCode::Corrupt,
        Code::UnknownFormat => ErrorCode::UnsupportedMethod,
        Code::EOpen => ErrorCode::MissingVolume,
        Code::ECreate | Code::EWrite | Code::EClose => ErrorCode::Io,
        Code::ERead => ErrorCode::Truncated,
        Code::NoMemory | Code::SmallBuf | Code::Unknown | Code::EReference => ErrorCode::Corrupt,
        Code::Success | Code::EndArchive => ErrorCode::Internal,
    };
    Error::new(code)
        .with("path", path.display())
        .detail(format!("{:?} during {:?}", e.code, e.when))
}

/// An opened RAR archive. The native handle is reopened per operation, so the reader itself
/// holds no C++ state and is `Send`.
#[derive(Clone)]
pub struct RarReader {
    path: PathBuf,
    info: ArchiveInfo,
    entries: Vec<Entry>,
    password: Option<Secret>,
    password_verified: bool,
    display_name: String,
}

impl RarReader {
    fn archive<'a>(path: &'a Path, pw: Option<&'a str>) -> Archive<'a> {
        let a = match pw {
            Some(p) => Archive::with_password(path, p),
            None => Archive::new(path),
        };
        a.as_first_part()
    }

    fn open(path: &Path, opts: &OpenOptions, ctx: &JobCtx) -> Result<Self> {
        if let Some(n) = kdf::max_lg2(path).filter(|&n| n > kdf::MAX_LG2) {
            return Err(Error::new(ErrorCode::UnsupportedMethod)
                .with("path", path.display())
                .detail(format!(
                    "trying a password takes 2^{n} key derivation rounds (at most 2^{} accepted)",
                    kdf::MAX_LG2
                )));
        }
        let display_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut password = opts.password.clone();
        let mut attempt = 0;
        loop {
            ctx.check()?;
            let listing = Self::archive(path, password.as_deref()).open_for_listing();
            let open = match listing {
                Ok(o) => o,
                Err(e) if matches!(e.code, Code::MissingPassword | Code::BadPassword) => {
                    if attempt >= MAX_PASSWORD_ATTEMPTS {
                        return Err(map_err(e, path));
                    }
                    password = Some(ctx.ask_password(&display_name, None, attempt)?);
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(map_err(e, path)),
            };
            let header_encrypted = open.has_encrypted_headers();
            let solid = open.is_solid();
            let mut entries = Vec::new();
            let mut failure = None;
            for item in open {
                ctx.check()?;
                match item {
                    Ok(h) => {
                        let id = entries.len() as u32;
                        let kind = if h.is_directory() {
                            EntryKind::Dir
                        } else {
                            EntryKind::File
                        };
                        let mut name = h.filename.to_string_lossy().replace('\\', "/");
                        if kind == EntryKind::Dir && !name.ends_with('/') {
                            name.push('/');
                        }
                        let mut e = Entry::new(id, name, kind);
                        e.size = h.unpacked_size;
                        e.crc32 = (kind == EntryKind::File).then_some(h.file_crc);
                        e.mtime = timeutil::dos32_to_system(h.file_time);
                        e.encrypted = h.is_encrypted() || header_encrypted;
                        e.method = (kind == EntryKind::File).then(|| {
                            if h.method == 0x30 {
                                "Store".to_owned()
                            } else {
                                "RAR".to_owned()
                            }
                        });
                        entries.push(e);
                        ctx.policy.check_entries(entries.len() as u64)?;
                    }
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
            }
            match failure {
                // Encrypted headers: the wrong key shows up while reading headers.
                Some(e) if matches!(e.code, Code::MissingPassword | Code::BadPassword) => {
                    if attempt >= MAX_PASSWORD_ATTEMPTS {
                        return Err(map_err(e, path));
                    }
                    password = Some(ctx.ask_password(&display_name, None, attempt)?);
                    attempt += 1;
                    continue;
                }
                Some(e) => return Err(map_err(e, path)),
                None => {}
            }

            let volumes = unrar::Archive::new(path).as_first_part();
            let volume_count = volumes
                .all_parts_option()
                .and_then(|glob| {
                    let pattern = glob.to_string_lossy().into_owned();
                    let dir = Path::new(&pattern).parent()?.to_owned();
                    Some(
                        std::fs::read_dir(dir)
                            .ok()?
                            .filter_map(|e| e.ok())
                            .filter(|e| glob_match(&pattern, &e.path().to_string_lossy()))
                            .count() as u32,
                    )
                })
                .unwrap_or(1)
                .max(1);
            let archive_size = std::fs::metadata(path)
                .map_err(|e| Error::io(e, path))?
                .len();
            let mut info = ArchiveInfo::new(path, FormatId::Rar, archive_size);
            info.solid = solid;
            info.header_encrypted = header_encrypted;
            info.encrypted = entries.iter().any(|e| e.encrypted);
            if info.encrypted {
                info.encryption = Some("AES".to_owned());
            }
            info.volumes = volume_count;
            if entries.iter().any(|e| e.method.as_deref() == Some("RAR")) {
                info.add_method("RAR");
            }
            if entries.iter().any(|e| e.method.as_deref() == Some("Store")) {
                info.add_method("Store");
            }
            return Ok(Self {
                path: path.to_owned(),
                info,
                entries,
                // Headers that decrypted prove the key.
                password_verified: header_encrypted,
                password,
                display_name,
            });
        }
    }

    /// Runs one processing pass. `handle` decides per entry; returning `Ok(false)` stops.
    fn process(
        &self,
        pw: Option<&str>,
        mut handle: impl FnMut(
            &Entry,
            unrar::OpenArchive<unrar::Process, unrar::CursorBeforeFile>,
        ) -> Result<(
            Option<unrar::OpenArchive<unrar::Process, unrar::CursorBeforeHeader>>,
            bool,
        )>,
    ) -> Result<()> {
        let mut open = Self::archive(&self.path, pw)
            .open_for_processing()
            .map_err(|e| map_err(e, &self.path))?;
        let mut idx = 0usize;
        loop {
            let Some(cur) = open.read_header().map_err(|e| map_err(e, &self.path))? else {
                return Ok(());
            };
            let Some(entry) = self.entries.get(idx) else {
                return Err(
                    Error::new(ErrorCode::Corrupt).detail("entry list changed between passes")
                );
            };
            idx += 1;
            let (next, go_on) = handle(entry, cur)?;
            match next {
                Some(n) if go_on => open = n,
                _ => return Ok(()),
            }
        }
    }

    /// Asks for and checks the password before extracting encrypted entries.
    fn ensure_password(&mut self, ctx: &JobCtx) -> Result<()> {
        if !self.info.encrypted || self.password_verified {
            return Ok(());
        }
        let Some(first) = self
            .entries
            .iter()
            .find(|e| e.encrypted && e.kind == EntryKind::File)
            .map(|e| e.id)
        else {
            return Ok(());
        };
        let mut attempt = 0;
        loop {
            if self.password.is_none() {
                self.password = Some(ctx.ask_password(&self.display_name, None, attempt)?);
            }
            let pw = self.password.clone();
            let res = self.process(pw.as_deref(), |e, cur| {
                if e.id == first {
                    cur.test()
                        .map(|_| (None, false))
                        .map_err(|err| map_err(err, Path::new("")))
                } else {
                    cur.skip()
                        .map(|n| (Some(n), true))
                        .map_err(|err| map_err(err, Path::new("")))
                }
            });
            match res {
                Ok(()) => {
                    self.password_verified = true;
                    return Ok(());
                }
                Err(e)
                    if matches!(
                        e.code(),
                        ErrorCode::WrongPassword
                            | ErrorCode::PasswordRequired
                            | ErrorCode::CrcMismatch
                    ) =>
                {
                    self.password = None;
                    attempt += 1;
                    if attempt >= MAX_PASSWORD_ATTEMPTS {
                        return Err(
                            Error::new(ErrorCode::WrongPassword).with("path", &self.display_name)
                        );
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// Matches unrar's volume glob (only `*` and `?` wildcards).
fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b) && go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.replace('\\', "/").chars().collect();
    let t: Vec<char> = text.replace('\\', "/").chars().collect();
    go(&p, &t)
}

impl ArchiveReader for RarReader {
    fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Every operation opens its own native handle, and the library reports errors per handle,
    /// so clones decode side by side.
    fn try_clone(&self) -> Option<Box<dyn ArchiveReader>> {
        Some(Box::new(self.clone()))
    }

    fn extract_to(&mut self, sink: &mut dyn ExtractSink, ctx: &JobCtx) -> Result<()> {
        let mut remaining = self.entries.iter().filter(|e| sink.wants(e)).count();
        if remaining == 0 {
            return Ok(());
        }
        if self.entries.iter().any(|e| sink.wants(e) && e.encrypted) {
            self.ensure_password(ctx)?;
        }
        let pw = self.password.clone();
        let path = self.path.clone();
        self.process(pw.as_deref(), |e, cur| {
            ctx.check()?;
            if !sink.wants(e) {
                return cur
                    .skip()
                    .map(|n| (Some(n), true))
                    .map_err(|err| map_err(err, &path));
            }
            remaining -= 1;
            if e.is_dir() {
                sink.entry(e, EntryData::None, ctx)?;
                let n = cur.skip().map_err(|err| map_err(err, &path))?;
                return Ok((Some(n), remaining > 0 && !sink.done()));
            }
            let mut slot = Some(cur);
            let mut next = None;
            {
                let mut write = |target: &Path| -> Result<()> {
                    let c = slot
                        .take()
                        .ok_or_else(|| Error::internal("entry written twice"))?;
                    let n = c.extract_to(target).map_err(|err| map_err(err, &path))?;
                    next = Some(n);
                    Ok(())
                };
                sink.entry(e, EntryData::WriteTo(&mut write), ctx)?;
            }
            let n = match (next, slot) {
                (Some(n), _) => n,
                // The sink skipped the entry (for example a conflict answered with "skip").
                (None, Some(c)) => c.skip().map_err(|err| map_err(err, &path))?,
                (None, None) => return Err(Error::internal("unrar cursor lost")),
            };
            Ok((Some(n), remaining > 0 && !sink.done()))
        })
    }

    fn test(&mut self, selection: Selection, ctx: &JobCtx) -> Result<Report> {
        if self
            .entries
            .iter()
            .any(|e| selection.contains(e.id) && e.encrypted)
        {
            self.ensure_password(ctx)?;
        }
        let pw = self.password.clone();
        let path = self.path.clone();
        let mut report = Report::default();
        self.process(pw.as_deref(), |e, cur| {
            ctx.check()?;
            if !selection.contains(e.id) || e.is_dir() {
                if e.is_dir() && selection.contains(e.id) {
                    report.dirs += 1;
                }
                return cur
                    .skip()
                    .map(|n| (Some(n), true))
                    .map_err(|err| map_err(err, &path));
            }
            ctx.begin_file(&e.path);
            match cur.test() {
                Ok(n) => {
                    report.files += 1;
                    report.bytes += e.size;
                    ctx.add_bytes(e.size);
                    ctx.file_done();
                    Ok((Some(n), true))
                }
                Err(err) => {
                    let mapped = map_err(err, &path);
                    if matches!(mapped.code(), ErrorCode::CrcMismatch | ErrorCode::Corrupt) {
                        report.warn(
                            mapped.code(),
                            &e.path,
                            mapped.param("detail").map(str::to_owned),
                        );
                        // The handle cannot continue after a failed test.
                        Ok((None, false))
                    } else {
                        Err(mapped)
                    }
                }
            }
        })?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use omnizip_core::{
        ExtractOptions, NullProgress, extract, job::FixedPassword, test as test_archive,
    };

    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/rar")
            .join(name)
    }

    fn ctx() -> JobCtx {
        JobCtx::default()
    }

    fn pw(p: &str) -> JobCtx {
        JobCtx::new(Arc::new(NullProgress), Arc::new(FixedPassword(p.into())))
    }

    #[test]
    fn plain_archive() {
        let mut r = RarEngine
            .open(
                &fixture("version.rar"),
                FormatId::Rar,
                &OpenOptions::default(),
                &ctx(),
            )
            .unwrap();
        assert_eq!(r.entries().len(), 1);
        assert_eq!(r.entries()[0].path, "VERSION");
        let d = tempfile::tempdir().unwrap();
        let o = ExtractOptions {
            destination: d.path().to_owned(),
            ..Default::default()
        };
        let rep = extract(r.as_mut(), &o, Selection::all(), &ctx()).unwrap();
        assert_eq!(rep.files, 1);
        assert_eq!(
            fs::read_to_string(d.path().join("VERSION")).unwrap(),
            "unrar-0.4.0"
        );
        let rep = test_archive(r.as_mut(), Selection::all(), &ctx()).unwrap();
        assert_eq!(rep.files, 1);
    }

    #[test]
    fn self_extracting_rar_opens() {
        // A PE program whose one section ends at 0x400, then the archive, as WinRAR lays out
        // its SFX modules.
        let mut exe = vec![0u8; 0x400];
        exe[..2].copy_from_slice(b"MZ");
        exe[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        exe[0x80..0x84].copy_from_slice(b"PE\0\0");
        exe[0x86..0x88].copy_from_slice(&1u16.to_le_bytes());
        exe[0x94..0x96].copy_from_slice(&0xE0u16.to_le_bytes());
        let sec = 0x80 + 24 + 0xE0;
        exe[sec + 16..sec + 20].copy_from_slice(&0x200u32.to_le_bytes());
        exe[sec + 20..sec + 24].copy_from_slice(&0x200u32.to_le_bytes());
        exe.extend_from_slice(&fs::read(fixture("version.rar")).unwrap());
        let d = tempfile::tempdir().unwrap();
        let sfx = d.path().join("setup.exe");
        fs::write(&sfx, exe).unwrap();
        let det = omnizip_core::detect::detect_path(&sfx).unwrap();
        assert!(det.sfx && det.format == FormatId::Rar);
        let mut r = RarEngine
            .open(&sfx, FormatId::Rar, &OpenOptions::default(), &ctx())
            .unwrap();
        assert_eq!(r.entries()[0].path, "VERSION");
        let o = ExtractOptions {
            destination: d.path().join("x"),
            ..Default::default()
        };
        assert_eq!(
            extract(r.as_mut(), &o, Selection::all(), &ctx())
                .unwrap()
                .files,
            1
        );
    }

    #[test]
    fn unicode_names() {
        let r = RarEngine
            .open(
                &fixture("unicode-entry.rar"),
                FormatId::Rar,
                &OpenOptions::default(),
                &ctx(),
            )
            .unwrap();
        assert_eq!(r.entries()[0].path, "unicodefilename❤️.txt");
    }

    #[test]
    fn solid_flag() {
        let r = RarEngine
            .open(
                &fixture("solid.rar"),
                FormatId::Rar,
                &OpenOptions::default(),
                &ctx(),
            )
            .unwrap();
        assert!(r.info().solid);
    }

    #[test]
    fn encrypted_data() {
        let p = fixture("crypted.rar");
        let mut r = RarEngine
            .open(&p, FormatId::Rar, &OpenOptions::default(), &ctx())
            .unwrap();
        assert!(r.info().encrypted && !r.info().header_encrypted);
        assert_eq!(r.entries()[0].path, ".gitignore");
        let d = tempfile::tempdir().unwrap();
        let o = ExtractOptions {
            destination: d.path().to_owned(),
            ..Default::default()
        };
        assert_eq!(
            extract(r.as_mut(), &o, Selection::all(), &ctx())
                .unwrap_err()
                .code(),
            ErrorCode::PasswordRequired
        );
        assert_eq!(
            extract(r.as_mut(), &o, Selection::all(), &pw("nope"))
                .unwrap_err()
                .code(),
            ErrorCode::WrongPassword
        );
        extract(r.as_mut(), &o, Selection::all(), &pw("unrar")).unwrap();
        assert_eq!(
            fs::read_to_string(d.path().join(".gitignore")).unwrap(),
            "target\nCargo.lock\n"
        );
    }

    #[test]
    fn encrypted_headers() {
        let p = fixture("comment-hpw-password.rar");
        let err = RarEngine
            .open(&p, FormatId::Rar, &OpenOptions::default(), &ctx())
            .err()
            .unwrap();
        assert_eq!(err.code(), ErrorCode::PasswordRequired);
        let mut r = RarEngine
            .open(&p, FormatId::Rar, &OpenOptions::default(), &pw("password"))
            .unwrap();
        assert!(r.info().header_encrypted);
        let d = tempfile::tempdir().unwrap();
        let o = ExtractOptions {
            destination: d.path().to_owned(),
            ..Default::default()
        };
        // The password from opening is reused.
        extract(r.as_mut(), &o, Selection::all(), &ctx()).unwrap();
        assert!(fs::read_dir(d.path()).unwrap().count() >= 1);
    }

    /// Headers encrypted with 2^24 key derivation rounds (the nightly fuzzer's timeouts, #3 #7
    /// #13): refused at once, not derived.
    #[test]
    fn costly_key_derivation_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("kdf.rar");
        let mut bytes = b"Rar!\x1a\x07\x01\x00".to_vec();
        // CRC, size 33, archive encryption header: version 0, check value present, 2^24.
        bytes.extend([0, 0, 0, 0, 33, 4, 0, 0, 1, 24]);
        bytes.extend([0; 16 + 12]);
        fs::write(&p, bytes).unwrap();
        let started = std::time::Instant::now();
        let err = RarEngine
            .open(&p, FormatId::Rar, &OpenOptions::default(), &pw("password"))
            .err()
            .unwrap();
        assert_eq!(err.code(), ErrorCode::UnsupportedMethod);
        assert!(started.elapsed().as_secs() < 1);
    }

    #[test]
    fn globbing() {
        assert!(glob_match("C:/a/x.part*.rar", "C:\\a\\x.part1.rar"));
        assert!(glob_match("x.r??", "x.r00"));
        assert!(!glob_match("x.r??", "x.zip"));
    }
}
