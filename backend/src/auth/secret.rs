//! [`Secret`]: a password or client secret that `Debug` prints as `<redacted>`,
//! so a `{:?}` of a request body or of provider settings cannot put it in a log
//! (GitHub #192).
//!
//! It dereferences to `&str` for the code that has to use the value, but has
//! no `Display` and no `Serialize`: it cannot be formatted or echoed by
//! accident.

use std::fmt;
use std::ops::Deref;

use serde::Deserialize;

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// The value, for code that stores or sends it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Secret {
        Secret(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Secret {
        Secret(value.to_owned())
    }
}

impl Deref for Secret {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_value() {
        let s: Secret = serde_json::from_str(r#""hunter2""#).unwrap();
        assert_eq!(s.expose(), "hunter2");
        assert_eq!(format!("{s:?}"), "<redacted>");
        assert_eq!(format!("{:?}", Some(s)), "Some(<redacted>)");
    }
}
