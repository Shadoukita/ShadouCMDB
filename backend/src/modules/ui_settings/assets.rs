//! Logo and favicon: allowed types, size limits and a content check, so the
//! bytes we serve are the image type they claim to be.
//!
//! SVG is allowed because logos are usually vector images, but it is markup:
//! uploads are parsed and must use only allowlisted elements, attributes and
//! references (see [`check_svg`]), and assets are always served with a
//! sandboxing Content-Security-Policy and `nosniff` (see `mod.rs`), so even a
//! file that slipped through could not run script on the application's origin.

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

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// Editor metadata namespaces (Inkscape, Sodipodi, RDF/Dublin Core, Adobe
/// Illustrator). Browsers render nothing from them, so their elements and
/// attributes are allowed; anything inside them is still checked.
const EDITOR_NS: &[&str] = &[
    "http://www.inkscape.org/namespaces/inkscape",
    "http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd",
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
    "http://creativecommons.org/ns#",
    "http://purl.org/dc/elements/1.1/",
    "http://web.resource.org/cc/",
];
const ADOBE_NS_PREFIX: &str = "http://ns.adobe.com/";

/// Static drawing elements. No scripting, animation, links, foreign content,
/// fonts or `feImage` (which loads URLs).
#[rustfmt::skip]
const SVG_ELEMENTS: &[&str] = &[
    "svg", "g", "defs", "symbol", "use", "title", "desc", "metadata", "switch", "style", "path", "rect", "circle",
    "ellipse", "line", "polyline", "polygon", "text", "tspan", "textPath", "image", "linearGradient", "radialGradient",
    "stop", "clipPath", "mask", "pattern", "marker", "filter", "feBlend", "feColorMatrix", "feComponentTransfer",
    "feComposite", "feConvolveMatrix", "feDiffuseLighting", "feDisplacementMap", "feDistantLight", "feDropShadow",
    "feFlood", "feFuncA", "feFuncB", "feFuncG", "feFuncR", "feGaussianBlur", "feMerge", "feMergeNode", "feMorphology",
    "feOffset", "fePointLight", "feSpecularLighting", "feSpotLight", "feTile", "feTurbulence",
];

/// Geometry, presentation and filter attributes. No event handlers.
#[rustfmt::skip]
const SVG_ATTRIBUTES: &[&str] = &[
    // core and structure
    "id", "class", "style", "lang", "role", "focusable", "version", "baseProfile", "viewBox", "preserveAspectRatio",
    "transform", "transform-origin", "requiredFeatures", "requiredExtensions", "systemLanguage", "type", "media",
    // geometry
    "x", "y", "x1", "y1", "x2", "y2", "cx", "cy", "r", "rx", "ry", "fx", "fy", "fr", "width", "height", "d", "points",
    "pathLength", "dx", "dy", "rotate", "textLength", "lengthAdjust", "startOffset", "method", "spacing", "side",
    // paint servers, clipping, masking, markers
    "offset", "gradientUnits", "gradientTransform", "spreadMethod", "patternUnits", "patternContentUnits",
    "patternTransform", "clipPathUnits", "maskUnits", "maskContentUnits", "filterUnits", "primitiveUnits",
    "markerUnits", "markerWidth", "markerHeight", "refX", "refY", "orient",
    // presentation
    "fill", "fill-opacity", "fill-rule", "stroke", "stroke-width", "stroke-linecap", "stroke-linejoin",
    "stroke-miterlimit", "stroke-dasharray", "stroke-dashoffset", "stroke-opacity", "opacity", "color", "display",
    "visibility", "overflow", "clip", "clip-path", "clip-rule", "mask", "filter", "marker", "marker-start",
    "marker-mid", "marker-end", "stop-color", "stop-opacity", "font", "font-family", "font-size", "font-size-adjust",
    "font-stretch", "font-style", "font-variant", "font-weight", "text-anchor", "text-decoration", "text-rendering",
    "letter-spacing", "word-spacing", "writing-mode", "direction", "dominant-baseline", "alignment-baseline",
    "baseline-shift", "unicode-bidi", "white-space", "shape-rendering", "image-rendering", "color-interpolation",
    "color-interpolation-filters", "color-rendering", "flood-color", "flood-opacity", "lighting-color", "paint-order",
    "vector-effect", "mix-blend-mode", "isolation", "enable-background",
    // filter primitives
    "in", "in2", "result", "stdDeviation", "edgeMode", "mode", "operator", "k1", "k2", "k3", "k4", "values",
    "tableValues", "slope", "intercept", "amplitude", "exponent", "order", "kernelMatrix", "divisor", "bias", "targetX",
    "targetY", "preserveAlpha", "surfaceScale", "diffuseConstant", "specularConstant", "specularExponent",
    "kernelUnitLength", "scale", "xChannelSelector", "yChannelSelector", "radius", "baseFrequency", "numOctaves",
    "seed", "stitchTiles", "azimuth", "elevation", "pointsAtX", "pointsAtY", "pointsAtZ", "limitingConeAngle", "z",
];

/// Attributes that hold free text rather than a CSS-like value.
fn is_text_attribute(name: &str) -> bool {
    matches!(name, "id" | "class" | "lang" | "role" | "systemLanguage")
        || name.starts_with("aria-")
        || name.starts_with("data-")
}

fn is_editor_ns(ns: &str) -> bool {
    EDITOR_NS.contains(&ns) || ns.starts_with(ADOBE_NS_PREFIX)
}

const SVG_ALLOWED_SUMMARY: &str = "Logos may contain shapes, text, gradients, patterns, masks, filters, \
     CSS without external references, and embedded PNG, JPEG, GIF or WebP images.";

/// Why an SVG upload was refused.
enum SvgError {
    /// Not well-formed XML, or the root is not `<svg>`.
    Malformed(String),
    /// Well-formed, but uses something outside the allowlist.
    Unsafe(String),
}

/// Allowlist check for SVG uploads. The file is parsed as XML and every
/// element, attribute, namespace, reference and piece of CSS must be on the
/// list; anything else refuses the upload (the file is stored unchanged, never
/// rewritten). DTDs are refused, so no entity can expand into markup, and
/// processing instructions are refused, so no `xml-stylesheet` can load CSS.
fn check_svg(text: &str) -> Result<(), SvgError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if has_internal_dtd_subset(text) {
        return Err(SvgError::Unsafe(
            "SVG files with an internal DTD subset (<!DOCTYPE svg [ … ]>) are not allowed".into(),
        ));
    }
    // A plain DOCTYPE line (older Illustrator exports) is fine: without an
    // internal subset no entity is declared, and external DTDs are never read.
    let opts = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(text, opts)
        .map_err(|e| SvgError::Malformed(format!("The file is not a well-formed SVG image: {e}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "svg" || !matches!(root.tag_name().namespace(), None | Some(SVG_NS)) {
        return Err(SvgError::Malformed("The file is not an SVG image: the root element is not <svg>".into()));
    }
    for node in doc.descendants() {
        if node.is_pi() {
            return Err(SvgError::Unsafe("SVG files with processing instructions (<?…?>) are not allowed".into()));
        }
        if !node.is_element() {
            continue;
        }
        let name = node.tag_name().name();
        let editor = match node.tag_name().namespace() {
            None | Some(SVG_NS) if SVG_ELEMENTS.contains(&name) => false,
            Some(ns) if is_editor_ns(ns) => true,
            _ => {
                return Err(SvgError::Unsafe(format!(
                    "The SVG contains <{name}>, which is not allowed. {SVG_ALLOWED_SUMMARY}"
                )));
            }
        };
        // Every text node: a comment splits `<style>` text into several.
        if name == "style" && !node.children().filter(|c| c.is_text()).filter_map(|c| c.text()).all(css_is_safe) {
            return Err(SvgError::Unsafe(format!(
                "The SVG contains a <style> block with an external reference, an escape or an @-rule \
                 other than @media. {SVG_ALLOWED_SUMMARY}"
            )));
        }
        for attr in node.attributes() {
            let an = attr.name();
            let ok = match attr.namespace() {
                None if an == "href" => href_is_safe(name, attr.value()),
                None if SVG_ATTRIBUTES.contains(&an) || is_text_attribute(an) => {
                    is_text_attribute(an) || css_is_safe(attr.value())
                }
                Some(XLINK_NS) if an == "href" => href_is_safe(name, attr.value()),
                Some(XLINK_NS) => an == "title",
                Some(XML_NS) => matches!(an, "space" | "lang"),
                Some(ns) => is_editor_ns(ns),
                // Editor elements are never rendered, so their own attributes
                // (`pagecolor` on `sodipodi:namedview`) are inert data.
                None => editor && !an.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("on")),
            };
            if !ok {
                return Err(SvgError::Unsafe(format!(
                    "The SVG attribute {an} on <{name}> is not allowed or has an unsafe value. {SVG_ALLOWED_SUMMARY}"
                )));
            }
        }
    }
    Ok(())
}

/// Whether any `<!DOCTYPE` opens an internal subset (`[`) before its closing
/// `>`, skipping quoted public/system literals. Every occurrence is checked, so
/// a decoy DOCTYPE inside a comment cannot hide the real one; a match inside a
/// comment only ever refuses.
fn has_internal_dtd_subset(text: &str) -> bool {
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(pos) = find_ignore_ascii_case(&b[from..], b"<!doctype") {
        let mut i = from + pos + 9;
        while i < b.len() {
            match b[i] {
                b'[' => return true,
                b'>' => break,
                q @ (b'"' | b'\'') => match b[i + 1..].iter().position(|&c| c == q) {
                    Some(len) => i += len + 1,
                    None => return true,
                },
                _ => {}
            }
            i += 1;
        }
        // Unterminated: the XML parser refuses it as malformed. Resuming after
        // the `>` keeps the scan linear (GitHub #228).
        if i >= b.len() {
            return false;
        }
        from = i + 1;
    }
    false
}

fn find_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w.eq_ignore_ascii_case(needle))
}

/// `href`/`xlink:href`: a reference into the same file, or an embedded raster
/// image on `<image>`. Never an external URL, `javascript:` or an embedded SVG.
fn href_is_safe(element: &str, value: &str) -> bool {
    let v = value.trim();
    if let Some(fragment) = v.strip_prefix('#') {
        return is_fragment(fragment);
    }
    if element != "image" {
        return false;
    }
    let lower = v.to_ascii_lowercase();
    let Some((mime, b64)) = lower.strip_prefix("data:image/").and_then(|r| r.split_once(";base64,")) else {
        return false;
    };
    matches!(mime, "png" | "jpeg" | "jpg" | "gif" | "webp")
        && b64.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=') || b.is_ascii_whitespace())
}

fn is_fragment(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':') || b >= 0x80)
}

/// CSS in a `<style>` block, a `style` attribute or a presentation attribute.
/// Allowed: `url(#fragment)` only, quoted strings that are plain names (font
/// families), and `@media`. Refused: backslash escapes (they could spell `url`
/// or `@import` in disguise), `<`, any other `url(…)`, and any other @-rule.
/// Strings may not contain `:`, `/` or `.`, so `image-set("…")` and similar
/// cannot name a resource either.
fn css_is_safe(css: &str) -> bool {
    let b = css.as_bytes();
    if b.iter().any(|&c| c == b'\\' || c == b'<') {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        let rest = &b[i..];
        if rest.len() >= 4 && rest[..4].eq_ignore_ascii_case(b"url(") {
            let Some(close) = rest.iter().position(|&c| c == b')') else { return false };
            let arg = css[i + 4..i + close].trim();
            let arg = arg
                .strip_prefix('"')
                .and_then(|a| a.strip_suffix('"'))
                .or_else(|| arg.strip_prefix('\'').and_then(|a| a.strip_suffix('\'')))
                .unwrap_or(arg);
            if !arg.strip_prefix('#').is_some_and(is_fragment) {
                return false;
            }
            i += close + 1;
        } else if b[i] == b'@' {
            if !(rest.len() >= 6 && rest[1..6].eq_ignore_ascii_case(b"media")) {
                return false;
            }
            i += 6;
        } else if b[i] == b'"' || b[i] == b'\'' {
            let Some(len) = rest[1..].iter().position(|&c| c == b[i]) else { return false };
            if !rest[1..1 + len]
                .iter()
                .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b' ' | b'-' | b'_' | b',') || c >= 0x80)
            {
                return false;
            }
            i += len + 2;
        } else {
            i += 1;
        }
    }
    true
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
    if t == ImageType::Svg {
        match check_svg(std::str::from_utf8(data).unwrap_or_default()) {
            Ok(()) => {}
            Err(SvgError::Malformed(m)) => return Err(("data", "invalid_format", m)),
            Err(SvgError::Unsafe(m)) => return Err(("data", "unsafe_content", m)),
        }
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

    fn svg(s: &str) -> Result<(), (&'static str, &'static str, String)> {
        check(AssetKind::Logo, ImageType::Svg, s.as_bytes())
    }

    #[test]
    fn svg_logos_from_common_editors_pass() {
        let plain = r##"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="#1f6feb"/></svg>"##;
        let inkscape = r##"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<!-- Created with Inkscape (http://www.inkscape.org/) -->
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"
     xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape"
     xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd"
     xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:cc="http://creativecommons.org/ns#"
     xmlns:dc="http://purl.org/dc/elements/1.1/" width="120mm" height="30mm" viewBox="0 0 120 30"
     sodipodi:docname="logo.svg" inkscape:version="1.3">
  <sodipodi:namedview id="nv" pagecolor="#ffffff" inkscape:zoom="1.2"/>
  <defs id="defs1">
    <linearGradient id="lg"><stop offset="0" style="stop-color:#1f6feb;stop-opacity:1"/></linearGradient>
    <linearGradient inkscape:collect="always" xlink:href="#lg" id="lg2" gradientUnits="userSpaceOnUse"/>
    <filter id="f"><feGaussianBlur stdDeviation="0.5" in="SourceGraphic"/></filter>
  </defs>
  <metadata><rdf:RDF><cc:Work rdf:about=""><dc:title>ACME</dc:title></cc:Work></rdf:RDF></metadata>
  <g inkscape:label="Layer 1" inkscape:groupmode="layer" id="layer1">
    <path d="M 0,0 H 30 V 30 Z" style="fill:url(#lg2);stroke:none;filter:url(#f)" id="p1"/>
    <text xml:space="preserve" x="35" y="20" style="font-family:'Open Sans', sans-serif;-inkscape-font-specification:'Open Sans Bold'" id="t1"><tspan sodipodi:role="line" id="ts1">online</tspan></text>
    <use href="#p1" x="90"/>
    <image width="8" height="8" xlink:href="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk
      +M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="/>
  </g>
</svg>"##;
        let illustrator = r##"<?xml version="1.0" encoding="utf-8"?>
<!-- Generator: Adobe Illustrator 27.0.0, SVG Export Plug-In . SVG Version: 6.00 Build 0)  -->
<svg version="1.1" id="Layer_1" xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"
     xmlns:i="http://ns.adobe.com/AdobeIllustrator/10.0/" x="0px" y="0px" viewBox="0 0 200 50"
     style="enable-background:new 0 0 200 50;" xml:space="preserve" i:viewOrigin="0 50">
<style type="text/css"><![CDATA[
	.st0{fill:#E30613;}
	.st1{font-family:'Helvetica-Bold';font-size:24px;}
	@media (prefers-color-scheme: dark) { .st0{fill:#FF4D4D;} }
]]></style>
<circle class="st0" cx="25" cy="25" r="20" aria-label="ACME mark" data-name="mark"/>
<text transform="matrix(1 0 0 1 55 33)" class="st0 st1">ACME</text>
</svg>"##;
        // older Illustrator exports: an external DOCTYPE with no internal subset
        let doctype = r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd">
<svg version="1.1" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><polygon points="0,0 10,0 5,10"/></svg>"#;
        for ok in [plain, inkscape, illustrator, doctype] {
            assert_eq!(svg(ok), Ok(()));
        }
        assert!(check(AssetKind::Favicon, ImageType::Svg, plain.as_bytes()).is_ok());
    }

    #[test]
    fn svg_outside_the_allowlist_is_refused() {
        let open = r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">"#;
        for inner in [
            // the old deny-list's cases
            "<script>alert(1)</script>",
            r#"<rect onload="alert(1)"/>"#,
            r#"<rect onLoad="alert(1)"/>"#,
            r#"<a href="javascript:alert(1)"><text>x</text></a>"#,
            "<foreignObject><div/></foreignObject>",
            // no substring to match, but not on the allowlist
            r#"<html:script xmlns:html="http://www.w3.org/1999/xhtml">alert(1)</html:script>"#,
            r#"<animate attributeName="href" to="javascript:alert(1)"/>"#,
            r#"<set attributeName="fill" to="red"/>"#,
            r#"<feImage href="https://tracker.example/p.png"/>"#,
            r#"<x:thing xmlns:x="urn:unknown"/>"#,
            r#"<rect x:attr="1" xmlns:x="urn:unknown"/>"#,
            r#"<g xml:base="https://evil.example/"/>"#,
            // references that leave the file
            r#"<use href="https://evil.example/sprite.svg#a"/>"#,
            r#"<use xlink:href="javascript:alert(1)"/>"#,
            r##"<use href="#"/>"##,
            r#"<image href="https://tracker.example/p.png"/>"#,
            r#"<image href="data:image/svg+xml;base64,PHN2Zz48L3N2Zz4="/>"#,
            r#"<image href="data:text/html,&lt;script&gt;alert(1)&lt;/script&gt;"/>"#,
            r#"<rect href="data:image/png;base64,AAAA"/>"#,
            // CSS
            "<style>@import url(https://evil.example/x.css);</style>",
            "<style>@font-face{font-family:x;src:url(https://evil.example/f.woff)}</style>",
            r#"<style>.a{background:URL( "//evil.example/x.png" )}</style>"#,
            r"<style>.a{fill:u\72l(https://evil.example/x)}</style>",
            r#"<style>.a{background:image-set("https://evil.example/x.png" 1x)}</style>"#,
            "<style>.a{fill:url(#a</style>",
            "<style>.a{}<!-- split -->@import url(https://evil.example/x.css);</style>",
            "<style>.a{}<![CDATA[@import url(https://evil.example/x.css);]]></style>",
            r#"<sodipodi:namedview xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd" onclick="x"/>"#,
            r#"<rect style="fill:url(https://evil.example/x)"/>"#,
            r#"<rect fill="url(//evil.example/x#a)"/>"#,
            r#"<rect style="font-family:'unterminated"/>"#,
        ] {
            let doc = format!("{open}{inner}</svg>");
            let err = svg(&doc).unwrap_err();
            assert_eq!((err.0, err.1), ("data", "unsafe_content"), "{inner}");
        }
        for bad in [
            // an xml-stylesheet can load CSS from anywhere
            r#"<?xml-stylesheet href="https://evil.example/x.css"?><svg xmlns="http://www.w3.org/2000/svg"/>"#,
            // no internal DTD subset, so no entity can expand into markup
            r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY x "&lt;script&gt;alert(1)&lt;/script&gt;">]><svg xmlns="http://www.w3.org/2000/svg">&x;</svg>"#,
            // a '>' inside a quoted literal does not end the DOCTYPE
            r#"<!DOCTYPE svg PUBLIC "a>b" "c" [<!ENTITY x "y">]><svg xmlns="http://www.w3.org/2000/svg">&x;</svg>"#,
            // a decoy DOCTYPE in a comment does not hide the real one
            r#"<!-- <!DOCTYPE svg> --><!DOCTYPE svg [<!ATTLIST rect onload CDATA "alert(1)">]><svg xmlns="http://www.w3.org/2000/svg"><rect/></svg>"#,
        ] {
            assert_eq!(svg(bad).unwrap_err().1, "unsafe_content", "{bad}");
        }
    }

    #[test]
    fn svg_must_be_well_formed_with_an_svg_root() {
        for bad in [
            "<html></html>",
            "<svg><rect></svg>",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><svg xmlns="http://www.w3.org/2000/svg"/></html>"#,
            r#"<svg xmlns="http://www.w3.org/1999/xhtml"/>"#,
        ] {
            assert_eq!(svg(bad).unwrap_err().1, "invalid_format", "{bad}");
        }
        // the error names the problem, not just the type
        assert!(svg("<svg><rect></svg>").unwrap_err().2.contains("well-formed"));
    }

    /// GitHub #228: the DOCTYPE pre-scan is linear. Many unterminated
    /// `<!doctype` used to rescan to the end of the file once each.
    #[test]
    fn doctype_prescan_is_linear() {
        let mut body = "<!doctype".repeat(58_000);
        body.push_str("<svg xmlns=\"http://www.w3.org/2000/svg\"/>");
        assert!(body.len() <= 512 * 1024);
        let start = std::time::Instant::now();
        assert!(!has_internal_dtd_subset(&body));
        assert!(start.elapsed() < std::time::Duration::from_secs(1), "took {:?}", start.elapsed());
        assert_eq!(svg(&body).unwrap_err().1, "invalid_format");
        // Resuming after one DOCTYPE still finds a subset in the next.
        assert!(has_internal_dtd_subset("<!doctype a><!DOCTYPE b [<!ENTITY x \"y\">]>"));
        assert!(has_internal_dtd_subset("<!doctype a \"q>q\"><!doctype b ["));
    }
}
