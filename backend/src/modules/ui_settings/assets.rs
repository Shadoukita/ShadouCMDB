//! Logo and favicon: allowed types, size limits and a content check, so the
//! bytes we serve are the image type they claim to be.
//!
//! SVG is allowed because logos are usually vector images, but it is markup:
//! uploads containing scripts, event handlers, `javascript:` URLs or embedded
//! HTML are refused, and assets are always served with a sandboxing
//! Content-Security-Policy and `nosniff` (see `mod.rs`), so even a file that
//! slipped through could not run script on the application's origin.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    /// Header and login page image (PNG, JPEG, WebP or SVG, at most 512 KiB)
    Logo,
    /// Browser tab icon (PNG, ICO or SVG, at most 128 KiB)
    Favicon,
}

impl AssetKind {
    pub const ALL: [AssetKind; 2] = [AssetKind::Logo, AssetKind::Favicon];

    pub fn as_str(self) -> &'static str {
        match self {
            AssetKind::Logo => "logo",
            AssetKind::Favicon => "favicon",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Same limits as the ui_assets_size check constraint.
    pub fn max_bytes(self) -> usize {
        match self {
            AssetKind::Logo => 512 * 1024,
            AssetKind::Favicon => 128 * 1024,
        }
    }

    pub fn allows(self, t: ImageType) -> bool {
        match self {
            AssetKind::Logo => matches!(t, ImageType::Png | ImageType::Jpeg | ImageType::Webp | ImageType::Svg),
            AssetKind::Favicon => matches!(t, ImageType::Png | ImageType::Ico | ImageType::Svg),
        }
    }
}

/// Logo: image/png, image/jpeg, image/webp or image/svg+xml (max 512 KiB). Favicon: image/png, image/x-icon or image/svg+xml (max 128 KiB).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum ImageType {
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/jpeg")]
    Jpeg,
    #[serde(rename = "image/webp")]
    Webp,
    #[serde(rename = "image/svg+xml")]
    Svg,
    #[serde(rename = "image/x-icon")]
    Ico,
}

impl ImageType {
    pub const ALL: [ImageType; 5] = [ImageType::Png, ImageType::Jpeg, ImageType::Webp, ImageType::Svg, ImageType::Ico];

    pub fn as_str(self) -> &'static str {
        match self {
            ImageType::Png => "image/png",
            ImageType::Jpeg => "image/jpeg",
            ImageType::Webp => "image/webp",
            ImageType::Svg => "image/svg+xml",
            ImageType::Ico => "image/x-icon",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }

    /// Whether the bytes look like this type (magic number, or SVG markup).
    fn matches(self, data: &[u8]) -> bool {
        match self {
            ImageType::Png => data.starts_with(b"\x89PNG\r\n\x1a\n"),
            ImageType::Jpeg => data.starts_with(b"\xff\xd8\xff"),
            ImageType::Webp => data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP",
            ImageType::Ico => data.starts_with(b"\x00\x00\x01\x00"),
            ImageType::Svg => std::str::from_utf8(data).is_ok_and(|s| s.to_ascii_lowercase().contains("<svg")),
        }
    }
}

/// Markup that has no place in a logo.
const SVG_FORBIDDEN: &[&str] =
    &["<script", "javascript:", "<foreignobject", "<iframe", "<embed", "<object", "data:text/html"];

fn svg_has_script(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if SVG_FORBIDDEN.iter().any(|m| lower.contains(m)) {
        return true;
    }
    // Event handler attributes: whitespace, "on", letters, optional spaces, "=".
    let bytes = lower.as_bytes();
    bytes.windows(3).enumerate().any(|(i, w)| {
        if !(w[0].is_ascii_whitespace() && w[1] == b'o' && w[2] == b'n') {
            return false;
        }
        let rest = &bytes[i + 3..];
        let letters = rest.iter().take_while(|b| b.is_ascii_alphabetic()).count();
        letters > 0 && rest[letters..].iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'=')
    })
}

/// Checks an upload; the error is (field, code, message).
pub fn check(kind: AssetKind, t: ImageType, data: &[u8]) -> Result<(), (&'static str, &'static str, String)> {
    if !kind.allows(t) {
        let allowed: Vec<&str> = ImageType::ALL.iter().filter(|x| kind.allows(**x)).map(|x| x.as_str()).collect();
        return Err((
            "contentType",
            "invalid_enum_value",
            format!("A {} must be one of: {}", kind.as_str(), allowed.join(", ")),
        ));
    }
    if data.is_empty() {
        return Err(("data", "too_small", "The file is empty".into()));
    }
    if data.len() > kind.max_bytes() {
        return Err((
            "data",
            "too_big",
            format!(
                "A {} can be at most {} KiB; this file is {} KiB",
                kind.as_str(),
                kind.max_bytes() / 1024,
                data.len().div_ceil(1024)
            ),
        ));
    }
    if !t.matches(data) {
        return Err(("data", "invalid_format", format!("The file is not a valid {} image", t.as_str())));
    }
    if t == ImageType::Svg && svg_has_script(std::str::from_utf8(data).unwrap_or_default()) {
        return Err((
            "data",
            "unsafe_content",
            "SVG files with scripts, event handlers or embedded HTML are not allowed".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[test]
    fn types_are_limited_per_kind() {
        assert!(check(AssetKind::Logo, ImageType::Png, PNG).is_ok());
        assert!(check(AssetKind::Favicon, ImageType::Png, PNG).is_ok());
        assert_eq!(check(AssetKind::Favicon, ImageType::Jpeg, b"\xff\xd8\xff\xe0").unwrap_err().0, "contentType");
        assert!(check(AssetKind::Favicon, ImageType::Ico, b"\x00\x00\x01\x00\x01\x00").is_ok());
        assert_eq!(check(AssetKind::Logo, ImageType::Ico, b"\x00\x00\x01\x00").unwrap_err().0, "contentType");
    }

    #[test]
    fn content_must_match_the_declared_type_and_size() {
        assert_eq!(check(AssetKind::Logo, ImageType::Jpeg, PNG).unwrap_err().1, "invalid_format");
        assert_eq!(check(AssetKind::Logo, ImageType::Png, b"").unwrap_err().1, "too_small");
        let mut big = PNG.to_vec();
        big.resize(AssetKind::Favicon.max_bytes() + 1, 0);
        assert_eq!(check(AssetKind::Favicon, ImageType::Png, &big).unwrap_err().1, "too_big");
        assert!(check(AssetKind::Logo, ImageType::Png, &big).is_ok());
        assert!(check(AssetKind::Logo, ImageType::Webp, b"RIFF\x10\0\0\0WEBPVP8 ").is_ok());
    }

    #[test]
    fn svg_without_script_only() {
        let ok = br##"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="#1f6feb"/></svg>"##;
        assert!(check(AssetKind::Logo, ImageType::Svg, ok).is_ok());
        for bad in [
            &br#"<svg><script>alert(1)</script></svg>"#[..],
            br#"<svg><rect onload = "alert(1)"/></svg>"#,
            br#"<svg><a href="JavaScript:alert(1)">x</a></svg>"#,
            br#"<svg><foreignObject><div/></foreignObject></svg>"#,
        ] {
            assert_eq!(check(AssetKind::Logo, ImageType::Svg, bad).unwrap_err().1, "unsafe_content");
        }
        assert_eq!(check(AssetKind::Logo, ImageType::Svg, b"<html></html>").unwrap_err().1, "invalid_format");
        // "on" inside a word or value is fine
        assert!(check(AssetKind::Logo, ImageType::Svg, br#"<svg><text font="icon">online</text></svg>"#).is_ok());
    }
}
