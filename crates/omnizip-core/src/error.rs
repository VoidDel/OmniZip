//! Error model shared by every OmniZip component.
//!
//! The core never produces user-facing text. Every failure is an [`ErrorCode`] plus named
//! parameters, so the GUI (i18next) and the CLI (Fluent) can render it in the user's language
//! (see `docs/06-i18n.md` §6). `Display` gives a short English fallback for logs.

use std::{collections::BTreeMap, fmt, io};

use serde::{Deserialize, Serialize};

/// Stable error codes. The numeric id (`E1203`) is part of the public contract: it appears in CLI
/// output, JSON output and translation keys, so codes are only ever added, never renumbered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ErrorCode {
    /// Generic I/O failure.
    Io,
    /// A file or archive does not exist.
    NotFound,
    /// The operating system refused access.
    PermissionDenied,
    /// The destination ran out of space.
    DiskFull,
    /// The input is not an archive OmniZip recognizes.
    UnknownFormat,
    /// The format is recognized but this build cannot handle it.
    UnsupportedFormat,
    /// The format cannot perform the requested operation (for example writing RAR).
    UnsupportedOperation,
    /// The archive uses a compression or encryption method this build cannot decode.
    UnsupportedMethod,
    /// The archive structure is damaged.
    Corrupt,
    /// Decompressed data does not match its checksum.
    CrcMismatch,
    /// The supplied password is wrong.
    WrongPassword,
    /// The archive is encrypted and no password was supplied.
    PasswordRequired,
    /// The archive ends before its declared end.
    Truncated,
    /// A volume of a multi-volume archive is missing.
    MissingVolume,
    /// An entry path would escape the destination directory.
    PathEscape,
    /// The archive expands far beyond its size (possible decompression bomb).
    CompressionBomb,
    /// The archive declares more entries than the security policy allows.
    TooManyEntries,
    /// A symbolic or hard link points outside the destination directory.
    UnsafeLink,
    /// The user or the caller cancelled the job.
    Cancelled,
    /// A caller-supplied argument is invalid.
    InvalidArgument,
    /// An optional component needed for this format is not installed.
    MissingDependency,
    /// The destination already exists and the policy is to keep it.
    AlreadyExists,
    /// A bug in OmniZip.
    Internal,
}

impl ErrorCode {
    /// Every code, in id order.
    pub const ALL: &'static [ErrorCode] = &[
        ErrorCode::Io,
        ErrorCode::NotFound,
        ErrorCode::PermissionDenied,
        ErrorCode::DiskFull,
        ErrorCode::AlreadyExists,
        ErrorCode::UnknownFormat,
        ErrorCode::UnsupportedFormat,
        ErrorCode::UnsupportedOperation,
        ErrorCode::UnsupportedMethod,
        ErrorCode::Corrupt,
        ErrorCode::CrcMismatch,
        ErrorCode::WrongPassword,
        ErrorCode::PasswordRequired,
        ErrorCode::Truncated,
        ErrorCode::MissingVolume,
        ErrorCode::PathEscape,
        ErrorCode::CompressionBomb,
        ErrorCode::TooManyEntries,
        ErrorCode::UnsafeLink,
        ErrorCode::Cancelled,
        ErrorCode::InvalidArgument,
        ErrorCode::MissingDependency,
        ErrorCode::Internal,
    ];

    /// Stable public id such as `E1203`.
    pub const fn id(self) -> &'static str {
        match self {
            ErrorCode::Io => "E1001",
            ErrorCode::NotFound => "E1002",
            ErrorCode::PermissionDenied => "E1003",
            ErrorCode::DiskFull => "E1004",
            ErrorCode::AlreadyExists => "E1005",
            ErrorCode::UnknownFormat => "E1101",
            ErrorCode::UnsupportedFormat => "E1102",
            ErrorCode::UnsupportedOperation => "E1103",
            ErrorCode::UnsupportedMethod => "E1104",
            ErrorCode::Corrupt => "E1201",
            ErrorCode::CrcMismatch => "E1202",
            ErrorCode::WrongPassword => "E1203",
            ErrorCode::PasswordRequired => "E1204",
            ErrorCode::Truncated => "E1205",
            ErrorCode::MissingVolume => "E1206",
            ErrorCode::PathEscape => "E1301",
            ErrorCode::CompressionBomb => "E1302",
            ErrorCode::TooManyEntries => "E1303",
            ErrorCode::UnsafeLink => "E1304",
            ErrorCode::Cancelled => "E1401",
            ErrorCode::InvalidArgument => "E1501",
            ErrorCode::MissingDependency => "E1601",
            ErrorCode::Internal => "E1901",
        }
    }

    /// Kebab-case key used by translation files (`errors.wrong-password`).
    pub const fn key(self) -> &'static str {
        match self {
            ErrorCode::Io => "io",
            ErrorCode::NotFound => "not-found",
            ErrorCode::PermissionDenied => "permission-denied",
            ErrorCode::DiskFull => "disk-full",
            ErrorCode::AlreadyExists => "already-exists",
            ErrorCode::UnknownFormat => "unknown-format",
            ErrorCode::UnsupportedFormat => "unsupported-format",
            ErrorCode::UnsupportedOperation => "unsupported-operation",
            ErrorCode::UnsupportedMethod => "unsupported-method",
            ErrorCode::Corrupt => "corrupt",
            ErrorCode::CrcMismatch => "crc-mismatch",
            ErrorCode::WrongPassword => "wrong-password",
            ErrorCode::PasswordRequired => "password-required",
            ErrorCode::Truncated => "truncated",
            ErrorCode::MissingVolume => "missing-volume",
            ErrorCode::PathEscape => "path-escape",
            ErrorCode::CompressionBomb => "compression-bomb",
            ErrorCode::TooManyEntries => "too-many-entries",
            ErrorCode::UnsafeLink => "unsafe-link",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::InvalidArgument => "invalid-argument",
            ErrorCode::MissingDependency => "missing-dependency",
            ErrorCode::Internal => "internal",
        }
    }

    /// CLI exit code for this error (`docs/08-cli.md` §6).
    pub const fn exit_code(self) -> i32 {
        match self {
            ErrorCode::InvalidArgument => 2,
            ErrorCode::NotFound => 3,
            ErrorCode::UnknownFormat | ErrorCode::UnsupportedFormat => 4,
            ErrorCode::UnsupportedOperation | ErrorCode::UnsupportedMethod => 4,
            ErrorCode::Corrupt
            | ErrorCode::CrcMismatch
            | ErrorCode::Truncated
            | ErrorCode::MissingVolume => 5,
            ErrorCode::WrongPassword | ErrorCode::PasswordRequired => 6,
            ErrorCode::PermissionDenied
            | ErrorCode::DiskFull
            | ErrorCode::AlreadyExists
            | ErrorCode::Io => 7,
            ErrorCode::PathEscape
            | ErrorCode::CompressionBomb
            | ErrorCode::TooManyEntries
            | ErrorCode::UnsafeLink => 8,
            ErrorCode::Cancelled => 9,
            ErrorCode::MissingDependency => 10,
            ErrorCode::Internal => 70,
        }
    }

    /// Whether this code comes from the security policy (exit code 8 family).
    pub const fn is_security(self) -> bool {
        self.exit_code() == 8
    }

    /// Short English description used only as a fallback.
    pub const fn fallback_message(self) -> &'static str {
        match self {
            ErrorCode::Io => "input/output error",
            ErrorCode::NotFound => "file not found",
            ErrorCode::PermissionDenied => "permission denied",
            ErrorCode::DiskFull => "not enough disk space",
            ErrorCode::AlreadyExists => "destination already exists",
            ErrorCode::UnknownFormat => "not a recognized archive",
            ErrorCode::UnsupportedFormat => "archive format not supported by this build",
            ErrorCode::UnsupportedOperation => "operation not supported for this format",
            ErrorCode::UnsupportedMethod => "unsupported compression or encryption method",
            ErrorCode::Corrupt => "archive is damaged",
            ErrorCode::CrcMismatch => "checksum mismatch",
            ErrorCode::WrongPassword => "wrong password",
            ErrorCode::PasswordRequired => "password required",
            ErrorCode::Truncated => "archive is truncated",
            ErrorCode::MissingVolume => "a volume of the archive is missing",
            ErrorCode::PathEscape => "entry path escapes the destination",
            ErrorCode::CompressionBomb => {
                "suspicious expansion ratio (possible decompression bomb)"
            }
            ErrorCode::TooManyEntries => "archive has too many entries",
            ErrorCode::UnsafeLink => "link points outside the destination",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::InvalidArgument => "invalid argument",
            ErrorCode::MissingDependency => "an optional component is missing",
            ErrorCode::Internal => "internal error",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// An OmniZip error: a code, named parameters for message templates, and an optional cause.
pub struct Error {
    code: ErrorCode,
    params: BTreeMap<String, String>,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

/// Result alias used throughout OmniZip.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// Creates an error with no parameters.
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            params: BTreeMap::new(),
            source: None,
        }
    }

    /// Adds a named parameter (for example `path`) used by message templates.
    #[must_use]
    pub fn with(mut self, key: &str, value: impl fmt::Display) -> Self {
        self.params.insert(key.to_owned(), value.to_string());
        self
    }

    /// Attaches the underlying cause.
    #[must_use]
    pub fn caused_by(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Attaches a cause given as plain text (for libraries whose errors are not `Send`).
    #[must_use]
    pub fn detail(self, detail: impl fmt::Display) -> Self {
        self.with("detail", detail)
    }

    /// The error code.
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// Named parameters.
    pub fn params(&self) -> &BTreeMap<String, String> {
        &self.params
    }

    /// Looks up one parameter.
    pub fn param(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(String::as_str)
    }

    /// Shorthand for the cancelled error.
    pub fn cancelled() -> Self {
        Self::new(ErrorCode::Cancelled)
    }

    /// Shorthand for an internal error with a detail message.
    pub fn internal(detail: impl fmt::Display) -> Self {
        Self::new(ErrorCode::Internal).detail(detail)
    }

    /// Shorthand for an invalid-argument error with a detail message.
    pub fn invalid_argument(detail: impl fmt::Display) -> Self {
        Self::new(ErrorCode::InvalidArgument).detail(detail)
    }

    /// Maps an I/O error, recording the path involved.
    pub fn io(err: io::Error, path: impl AsRef<std::path::Path>) -> Self {
        Self::from(err).with("path", path.as_ref().display())
    }

    /// A serializable snapshot for IPC and `--json` output.
    pub fn to_report(&self) -> ErrorReport {
        ErrorReport {
            code: self.code.id().to_owned(),
            key: self.code.key().to_owned(),
            params: self.params.clone(),
            message: self.to_string(),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("code", &self.code)
            .field("params", &self.params)
            .field("source", &self.source.as_ref().map(|s| s.to_string()))
            .finish()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code.fallback_message())?;
        if let Some(path) = self.params.get("path") {
            write!(f, ": {path}")?;
        }
        for (k, v) in &self.params {
            if k != "path" {
                write!(f, " ({k}: {v})")?;
            }
        }
        if let Some(src) = &self.source {
            write!(f, " [{src}]")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        // An I/O error that wraps an OmniZip error (for example a cancellation raised inside a
        // `Read` adapter) is unwrapped back to the original code.
        if err.get_ref().is_some_and(|inner| inner.is::<Error>()) {
            let inner = err.into_inner().expect("checked above");
            return *inner.downcast::<Error>().expect("checked above");
        }
        let code = match err.kind() {
            io::ErrorKind::NotFound => ErrorCode::NotFound,
            io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => ErrorCode::DiskFull,
            io::ErrorKind::AlreadyExists => ErrorCode::AlreadyExists,
            io::ErrorKind::UnexpectedEof => ErrorCode::Truncated,
            io::ErrorKind::InvalidData => ErrorCode::Corrupt,
            io::ErrorKind::Interrupted => ErrorCode::Cancelled,
            _ => ErrorCode::Io,
        };
        Error::new(code).caused_by(err)
    }
}

impl From<Error> for io::Error {
    fn from(err: Error) -> Self {
        let kind = match err.code {
            ErrorCode::NotFound => io::ErrorKind::NotFound,
            ErrorCode::PermissionDenied => io::ErrorKind::PermissionDenied,
            ErrorCode::Cancelled => io::ErrorKind::Interrupted,
            _ => io::ErrorKind::Other,
        };
        io::Error::new(kind, err)
    }
}

/// Serializable form of an [`Error`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorReport {
    /// Stable id, for example `E1203`.
    pub code: String,
    /// Translation key, for example `wrong-password`.
    pub key: String,
    /// Template parameters.
    pub params: BTreeMap<String, String>,
    /// English fallback message.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_stable() {
        let mut ids: Vec<_> = ErrorCode::ALL.iter().map(|c| c.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), ErrorCode::ALL.len());
        assert_eq!(ErrorCode::WrongPassword.id(), "E1203");
        assert_eq!(ErrorCode::WrongPassword.exit_code(), 6);
        assert_eq!(ErrorCode::PathEscape.exit_code(), 8);
    }

    #[test]
    fn io_error_round_trip_keeps_code() {
        let original = Error::cancelled();
        let io: io::Error = original.into();
        let back = Error::from(io);
        assert_eq!(back.code(), ErrorCode::Cancelled);
    }

    #[test]
    fn io_kind_mapping() {
        let e = Error::from(io::Error::new(io::ErrorKind::NotFound, "x"));
        assert_eq!(e.code(), ErrorCode::NotFound);
        let e = Error::io(io::Error::new(io::ErrorKind::PermissionDenied, "x"), "/a/b");
        assert_eq!(e.code(), ErrorCode::PermissionDenied);
        assert_eq!(e.param("path"), Some("/a/b"));
    }
}
