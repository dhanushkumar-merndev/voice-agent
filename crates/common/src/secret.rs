//! A string wrapper that never prints its contents.

use std::fmt;

/// A secret value such as an API key, token or connection string.
///
/// `Debug` prints `Secret(***)` so secrets cannot leak through logs or
/// `{:?}` formatting of the config.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the raw value. Call this only where the secret is handed to
    /// the party that should receive it (an HTTP header, a DB driver).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let s = Secret::new("sk_live_123");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert_eq!(s.expose(), "sk_live_123");
    }
}
