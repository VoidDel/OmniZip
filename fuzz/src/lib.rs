//! Shared drivers for the fuzz targets.
//!
//! Engines open archives by path, so each input is written to a per-process scratch file.
//! Decoding goes through the same `test` path as `omnizip test`: every entry is decoded and
//! checked, nothing is written to disk. Limits are tight so that a fuzzer-made bomb is
//! refused quickly instead of timing out.

use std::{ffi::OsStr, path::PathBuf};

use omnizip_core::{
    FormatId, JobCtx, OpenOptions, SecurityPolicy, Selection, encoding,
    path::{TargetOs, UnsafePathMode, sanitize},
    test,
};
use omnizip_vfs::{ArchiveFs, CacheLimits, FsOptions, ROOT, VirtualFs};

/// Path sanitization never yields `.`, `..`, empty or separator-bearing components, and
/// sanitizing its own output changes nothing.
pub fn sanitize_props(data: &[u8]) {
    let Ok(name) = std::str::from_utf8(data) else {
        return;
    };
    for os in [TargetOs::Windows, TargetOs::Unix] {
        for mode in [UnsafePathMode::Reject, UnsafePathMode::Strip] {
            let Ok(clean) = sanitize(name, os, mode) else {
                continue;
            };
            assert!(!clean.components.is_empty());
            for c in &clean.components {
                assert!(
                    !c.is_empty() && c != "." && c != "..",
                    "{c:?} from {name:?}"
                );
                assert!(!c.contains('/'), "{c:?} from {name:?}");
                if os == TargetOs::Windows {
                    assert!(!c.contains('\\') && !c.contains(':'), "{c:?} from {name:?}");
                }
            }
            let again = sanitize(&clean.to_slash_path(), os, UnsafePathMode::Reject)
                .expect("a sanitized path is accepted");
            assert_eq!(
                again.components, clean.components,
                "idempotent for {name:?}"
            );
        }
    }
}

/// Encoding detection and decoding accept any bytes.
pub fn encoding_props(data: &[u8]) {
    // Split on NUL so detection sees a batch of names, as it does for real archives.
    let names: Vec<&[u8]> = data.split(|b| *b == 0).collect();
    for locale in [None, Some("zh-CN"), Some("ja-JP"), Some("ru-RU")] {
        let enc = encoding::detect(names.iter().copied(), locale);
        for n in &names {
            let _ = enc.decode(n);
        }
    }
    let _ = encoding::decode_cp437(data);
}

fn scratch() -> PathBuf {
    std::env::temp_dir().join(format!("omnizip-fuzz-{}.bin", std::process::id()))
}

fn ctx() -> JobCtx {
    JobCtx::default().with_policy(SecurityPolicy {
        max_ratio: 200,
        ratio_min_bytes: 16 << 20,
        allow_high_ratio: false,
        max_entries: 50_000,
        max_nesting: 2,
    })
}

/// Detects (when `format` is `None`) or forces a format, lists and decodes everything.
pub fn open_and_test(data: &[u8], format: Option<FormatId>) {
    let path = scratch();
    if std::fs::write(&path, data).is_err() {
        return;
    }
    let registry = omnizip_formats::shared();
    let ctx = ctx();
    let opts = OpenOptions {
        password: Some("fuzz".into()),
        ..Default::default()
    };
    let opened = match format {
        Some(f) => registry.open_as(&path, f, &opts, &ctx),
        None => registry.open(&path, &opts, &ctx),
    };
    if let Ok(mut reader) = opened {
        for (i, e) in reader.entries().iter().enumerate() {
            assert_eq!(e.id as usize, i, "entries()[i].id == i");
        }
        let _ = test(reader.as_mut(), Selection::all(), &ctx);
    }
}

/// Mounts the input as a volume and walks it like a file manager would.
pub fn mount_and_walk(data: &[u8]) {
    let path = scratch();
    if std::fs::write(&path, data).is_err() {
        return;
    }
    let Ok(reader) = omnizip_formats::shared().open(&path, &OpenOptions::default(), &ctx()) else {
        return;
    };
    let opts = FsOptions {
        cache: CacheLimits {
            memory: 8 << 20,
            disk: 32 << 20,
            inline_max: 1 << 20,
        },
        ..Default::default()
    };
    let Ok(fs) = ArchiveFs::new(reader, opts) else {
        return;
    };
    let mut stack = vec![ROOT];
    let mut visited = 0;
    while let Some(dir) = stack.pop() {
        let Ok(items) = fs.readdir(dir, 0, 4096) else {
            continue;
        };
        for item in items {
            visited += 1;
            if visited > 10_000 {
                return;
            }
            // Every listed name must resolve back to the same node.
            let found = fs
                .lookup(dir, OsStr::new(&item.name))
                .expect("listed name resolves");
            assert_eq!(found.node, item.attr.node);
            if item.attr.is_dir() {
                stack.push(item.attr.node);
            } else if let Ok(fh) = fs.open(item.attr.node, Default::default()) {
                let _ = fs.read(fh, item.attr.size / 2, 4096);
                fs.release(fh);
            }
        }
    }
}

/// A short, deterministic stand-in for a fuzzing session that runs on stable: real archives
/// with random byte flips, truncations and splices go through every driver. The real
/// campaign runs nightly with cargo-fuzz (.github/workflows/nightly-fuzz.yml).
#[cfg(test)]
mod smoke {
    use std::fs;

    use omnizip_core::{CollectOptions, Level, WriteOptions, source};

    use super::*;

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    fn seeds() -> Vec<Vec<u8>> {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        fs::create_dir_all(src.join("目录/sub")).unwrap();
        fs::write(src.join("目录/a.txt"), "alpha ".repeat(300)).unwrap();
        fs::write(
            src.join("目录/sub/b.bin"),
            (0..5000u32).map(|i| (i * 7) as u8).collect::<Vec<_>>(),
        )
        .unwrap();
        let ctx = JobCtx::default();
        let sources = source::collect(&[src], &CollectOptions::default(), &ctx).unwrap();
        let mut out = Vec::new();
        for f in [
            FormatId::Zip,
            FormatId::SevenZ,
            FormatId::Tar,
            FormatId::TarGz,
            FormatId::TarXz,
            FormatId::TarZst,
        ] {
            let dest = d.path().join(format!("seed{}", f.primary_extension()));
            let wo = WriteOptions {
                format: f,
                level: Level::Fast,
                ..Default::default()
            };
            omnizip_formats::shared()
                .create(&dest, &sources, &wo, &ctx)
                .unwrap();
            out.push(fs::read(&dest).unwrap());
        }
        let rar = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/rar");
        for e in fs::read_dir(rar).unwrap().flatten() {
            if e.path().extension().is_some_and(|x| x == "rar") {
                out.push(fs::read(e.path()).unwrap());
            }
        }
        out
    }

    fn mutate(rng: &mut Rng, seed: &[u8], other: &[u8]) -> Vec<u8> {
        let mut v = seed.to_vec();
        match rng.below(4) {
            0 => {
                for _ in 0..=rng.below(8) {
                    let i = rng.below(v.len());
                    v[i] ^= 1 << rng.below(8);
                }
            }
            1 => v.truncate(rng.below(v.len())),
            2 => {
                let i = rng.below(v.len());
                let j = rng.below(other.len());
                let n = rng.below(64).min(other.len() - j).min(v.len() - i);
                v[i..i + n].copy_from_slice(&other[j..j + n]);
            }
            _ => {
                let i = rng.below(v.len());
                v[i] = [0x00, 0xFF, 0x7F, 0x80][rng.below(4)];
            }
        }
        v
    }

    #[test]
    fn mutated_archives_never_panic() {
        let seeds = seeds();
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for seed in &seeds {
            open_and_test(seed, None);
            mount_and_walk(seed);
        }
        for _ in 0..600 {
            let a = &seeds[rng.below(seeds.len())];
            let b = &seeds[rng.below(seeds.len())];
            let input = mutate(&mut rng, a, b);
            open_and_test(&input, None);
            mount_and_walk(&input);
        }
    }

    #[test]
    fn hostile_names_keep_their_properties() {
        let mut rng = Rng(42);
        let alphabet = [
            "a", "/", "\\", ".", "..", ":", " ", "C:", "\u{202E}", "CON", "名", "\0", "?", "*",
            "\t",
        ];
        for fixed in [
            "../../etc/passwd",
            r"C:\Windows\x",
            "/abs",
            "a/./b",
            "....",
            "CON.txt",
            " trailing. ",
            r"\\server\share\x",
        ] {
            sanitize_props(fixed.as_bytes());
        }
        for _ in 0..5000 {
            let s: String = (0..rng.below(12))
                .map(|_| alphabet[rng.below(alphabet.len())])
                .collect();
            sanitize_props(s.as_bytes());
            encoding_props(s.as_bytes());
        }
        let bytes: Vec<u8> = (0..4096).map(|_| rng.next() as u8).collect();
        encoding_props(&bytes);
    }
}
