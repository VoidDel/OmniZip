//! Propagation of the "downloaded from the internet" mark (`docs/09-security.md` §6).
//!
//! Windows stores it in the `Zone.Identifier` alternate data stream (Mark-of-the-Web); files
//! extracted from a marked archive get the same mark so SmartScreen and Office Protected View
//! keep working. macOS quarantine propagation lands with the macOS milestone (M2).

use std::path::Path;

/// The origin mark of an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// Raw `Zone.Identifier` content (Windows).
    pub zone_identifier: String,
    pub zone_id: u32,
}

/// Reads the mark of `archive`. Only internet / untrusted zones (ZoneId ≥ 3) are returned.
#[cfg(windows)]
pub fn read(archive: &Path) -> Option<Origin> {
    let mut ads = archive.as_os_str().to_owned();
    ads.push(":Zone.Identifier");
    let content = std::fs::read_to_string(std::path::PathBuf::from(ads)).ok()?;
    let zone_id = content
        .lines()
        .find_map(|l| l.trim().strip_prefix("ZoneId="))
        .and_then(|v| v.trim().parse::<u32>().ok())?;
    (zone_id >= 3).then_some(Origin {
        zone_identifier: content,
        zone_id,
    })
}

/// Writes the mark to an extracted file.
#[cfg(windows)]
pub fn apply(file: &Path, origin: &Origin) -> std::io::Result<()> {
    let mut ads = file.as_os_str().to_owned();
    ads.push(":Zone.Identifier");
    std::fs::write(
        std::path::PathBuf::from(ads),
        origin.zone_identifier.as_bytes(),
    )
}

#[cfg(not(windows))]
pub fn read(_archive: &Path) -> Option<Origin> {
    None
}

#[cfg(not(windows))]
pub fn apply(_file: &Path, _origin: &Origin) -> std::io::Result<()> {
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn zone_identifier_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        std::fs::write(&archive, b"x").unwrap();
        assert_eq!(read(&archive), None);
        let mark = Origin {
            zone_identifier: "[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://example.com/a.zip\r\n"
                .into(),
            zone_id: 3,
        };
        apply(&archive, &mark).unwrap();
        assert_eq!(read(&archive), Some(mark.clone()));
        let out = dir.path().join("out.txt");
        std::fs::write(&out, b"y").unwrap();
        apply(&out, &mark).unwrap();
        assert_eq!(read(&out).unwrap().zone_id, 3);
    }
}
