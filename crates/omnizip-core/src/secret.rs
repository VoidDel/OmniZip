//! Passwords in memory (`docs/09-security.md` §5).

use std::{fmt, ops::Deref};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroizing;

/// A password. Its bytes are overwritten when it is dropped (each clone clears its own copy),
/// and `Debug` never shows it, so it stays out of logs and error reports. Serialized as a plain
/// string for the IPC that carries it.
///
/// Copies made outside OmniZip's own code (an archive library's key schedule, the WebView's
/// strings, an IPC buffer) are not covered.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(Zeroizing::new(s.into()))
    }

    /// The password text.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Deref for Secret {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl From<String> for Secret {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for Secret {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // The decoded string moves into the secret without another copy.
        String::deserialize(d).map(Self::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_from_debug_and_plain_in_json() {
        let s = Secret::from("hunter2");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert_eq!(format!("{:?}", Some(&s)), "Some(Secret(***))");
        assert_eq!(&*s, "hunter2");
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"hunter2\"");
        assert_eq!(serde_json::from_str::<Secret>(&json).unwrap(), s);
    }
}
