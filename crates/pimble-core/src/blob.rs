//! Pictures in a node's text: the `src` of an `image` atom
//! (docs/IMAGES_CONTRACT.md "The shape").
//!
//! ```text
//! pimble-blob:<store uuid>/<blob id>
//! ```
//!
//! The bytes live once, beside the store, as an immutable blob; the text only
//! names it. [`BlobUrl`] is the one place the string is built or taken apart,
//! [`BlobId`] the one place an id is minted or checked, and [`ImageMime`] the
//! one list of what a blob may be.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::StoreId;

/// The scheme, with its colon.
pub const BLOB_SCHEME: &str = "pimble-blob:";

/// The most bytes one image may be. A client scales a larger picture down
/// before it uploads it; the store and the server refuse one regardless.
pub const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// What a person is told when a picture is over [`MAX_IMAGE_BYTES`].
pub const IMAGE_TOO_LARGE: &str = "Pictures can be at most 20 MiB.";

/// What a person is told when the bytes are not one of the image types a
/// store keeps, or not the type they were said to be.
pub const NOT_AN_IMAGE: &str = "Only PNG, JPEG, GIF, WebP and AVIF pictures can be added.";

const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// How many characters an id prints as: 128 bits, five to a character.
const ID_CHARS: usize = 26;

/// A blob's name: 128 random bits, printed as 26 characters of lowercase
/// base32 without padding.
///
/// Random, never a hash of the bytes: in an encrypted store the hosted server
/// sees ids, and a hash of the plaintext would let it confirm that a store
/// holds a known picture.
///
/// The printed form is a file name (`<store>/blobs/<blob id>`), so parsing is
/// strict: exactly 26 characters of `a-z2-7`, and only the one spelling of
/// each id (the last character carries two unused bits, which must be zero).
/// Nothing with a path separator, a dot or an uppercase letter is an id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlobId([u8; 16]);

impl BlobId {
    /// A new random id.
    pub fn new() -> Self {
        let mut bytes = [0u8; 16];
        // The operating system's generator (the browser's in wasm). Without
        // one there is no safe id to hand out, and nothing else in Pimble
        // would work either (uuids come from the same place).
        getrandom::getrandom(&mut bytes).expect("the system's random number generator is unavailable");
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// `Some` when `s` is exactly an id as [`BlobId`] prints one.
    pub fn parse(s: &str) -> Option<Self> {
        s.parse().ok()
    }
}

impl Default for BlobId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = [0u8; ID_CHARS];
        let mut acc: u32 = 0;
        let mut bits = 0;
        let mut at = 0;
        for byte in self.0 {
            acc = (acc << 8) | byte as u32;
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                out[at] = BASE32[((acc >> bits) & 31) as usize];
                at += 1;
            }
        }
        // 128 bits leave three over: the last character's two low bits are zero.
        out[at] = BASE32[((acc << (5 - bits)) & 31) as usize];
        f.write_str(std::str::from_utf8(&out).expect("base32 is ASCII"))
    }
}

impl fmt::Debug for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobId({self})")
    }
}

/// Why a string is not a blob id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlobIdError {
    #[error("a blob id is 26 characters long")]
    Length,
    #[error("a blob id is made of the lowercase letters a-z and the digits 2-7")]
    Character,
    #[error("a blob id has one spelling, and this is not it")]
    NotCanonical,
}

impl FromStr for BlobId {
    type Err = BlobIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != ID_CHARS {
            return Err(BlobIdError::Length);
        }
        let mut bytes = [0u8; 16];
        let mut acc: u32 = 0;
        let mut bits = 0;
        let mut at = 0;
        for c in s.bytes() {
            let value = match c {
                b'a'..=b'z' => c - b'a',
                b'2'..=b'7' => c - b'2' + 26,
                _ => return Err(BlobIdError::Character),
            };
            acc = (acc << 5) | value as u32;
            bits += 5;
            if bits >= 8 && at < bytes.len() {
                bits -= 8;
                bytes[at] = (acc >> bits) as u8;
                at += 1;
            }
        }
        // 130 bits were read into 128: the two left over must be zero, or
        // two strings would name one id (and two files one blob).
        if acc & 0b11 != 0 {
            return Err(BlobIdError::NotCanonical);
        }
        Ok(Self(bytes))
    }
}

impl Serialize for BlobId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BlobId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Where a picture's bytes live: a blob in a store. Always the canonical
/// store, as in a `pimble:` link, so an image shown through a mount or copied
/// into another store's text still names where it is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlobUrl {
    pub store: StoreId,
    pub blob: BlobId,
}

impl BlobUrl {
    pub fn new(store: StoreId, blob: BlobId) -> Self {
        Self { store, blob }
    }

    /// `Some` when `src` is a Pimble blob URL, `None` for anything else (an
    /// `https:` image, a `data:` URL, or a malformed one).
    pub fn parse(src: &str) -> Option<Self> {
        src.parse().ok()
    }
}

impl fmt::Display for BlobUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{BLOB_SCHEME}{}/{}", self.store, self.blob)
    }
}

/// Why a string is not a Pimble blob URL.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlobUrlError {
    #[error("not a pimble-blob: URL")]
    Scheme,
    #[error("a pimble-blob: URL names a store and a blob")]
    Shape,
    #[error("a pimble-blob: URL's store is not an id")]
    Store,
    #[error("a pimble-blob: URL's blob is not an id: {0}")]
    Blob(#[from] BlobIdError),
}

impl FromStr for BlobUrl {
    type Err = BlobUrlError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s
            .get(..BLOB_SCHEME.len())
            .filter(|head| head.eq_ignore_ascii_case(BLOB_SCHEME))
            .map(|_| &s[BLOB_SCHEME.len()..])
            .ok_or(BlobUrlError::Scheme)?;
        let (store, blob) = rest.split_once('/').ok_or(BlobUrlError::Shape)?;
        let store = StoreId::parse(store).map_err(|_| BlobUrlError::Store)?;
        // Anything after the id (another segment, a query, a fragment) fails
        // here: none of those characters is in an id.
        let blob = blob.parse()?;
        Ok(Self { store, blob })
    }
}

impl Serialize for BlobUrl {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BlobUrl {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// The image types a store keeps. Never SVG: it can carry script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageMime {
    Png,
    Jpeg,
    Gif,
    Webp,
    Avif,
}

impl ImageMime {
    /// Every accepted type: the one list.
    pub const ALL: [ImageMime; 5] = [ImageMime::Png, ImageMime::Jpeg, ImageMime::Gif, ImageMime::Webp, ImageMime::Avif];

    pub fn as_str(self) -> &'static str {
        match self {
            ImageMime::Png => "image/png",
            ImageMime::Jpeg => "image/jpeg",
            ImageMime::Gif => "image/gif",
            ImageMime::Webp => "image/webp",
            ImageMime::Avif => "image/avif",
        }
    }

    /// The accepted type `mime` names, exactly as [`ImageMime::as_str`]
    /// spells it apart from letter case; `None` for anything else
    /// (`image/svg+xml`, `image/jpg`, a type with parameters).
    pub fn parse(mime: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|known| known.as_str().eq_ignore_ascii_case(mime))
    }

    /// What `bytes` are, by their first bytes: a cheap check that a file is
    /// the kind of image it is said to be, with no decoding. `None` for
    /// anything that is not one of the accepted types.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some(ImageMime::Png);
        }
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Some(ImageMime::Jpeg);
        }
        if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            return Some(ImageMime::Gif);
        }
        if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            return Some(ImageMime::Webp);
        }
        // ISO BMFF: a `ftyp` box first, whose major brand or one of whose
        // compatible brands says AVIF (`avif` a still, `avis` a sequence).
        if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
            let size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
            let end = size.clamp(12, bytes.len());
            let brands = std::iter::once(&bytes[8..12]).chain(bytes[12..end].chunks_exact(4).skip(1));
            if brands.into_iter().any(|brand| brand == b"avif" || brand == b"avis") {
                return Some(ImageMime::Avif);
            }
        }
        None
    }
}

impl fmt::Display for ImageMime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> StoreId {
        StoreId::parse("6f1c2b1e-8d3a-4f63-9a54-2c0f5a1e7b90").unwrap()
    }

    #[test]
    fn an_id_prints_as_26_lowercase_base32_characters_and_parses_back() {
        for _ in 0..200 {
            let id = BlobId::new();
            let text = id.to_string();
            assert_eq!(text.len(), 26, "{text}");
            assert!(text.bytes().all(|c| c.is_ascii_lowercase() || (b'2'..=b'7').contains(&c)), "{text}");
            assert_eq!(BlobId::parse(&text), Some(id));
        }
        assert_eq!(BlobId::from_bytes([0; 16]).to_string(), "a".repeat(26));
        assert_eq!(BlobId::from_bytes([0xFF; 16]).to_string(), format!("{}4", "7".repeat(25)));
        assert_ne!(BlobId::new(), BlobId::new());
    }

    #[test]
    fn an_id_is_parsed_strictly() {
        let good = BlobId::new().to_string();
        let upper = good.to_uppercase();
        let short = &good[..25];
        let long = format!("{good}a");
        let dotted = format!("..{}", &good[2..]);
        let slashed = format!("{}/{}", &good[..12], &good[13..]);
        let backslashed = format!("{}\\{}", &good[..12], &good[13..]);
        let padded = format!("{}=", &good[..25]);
        // The same 128 bits with the unused low bits set: a second spelling.
        let twin = format!("{}b", "a".repeat(25));
        for bad in [
            "", "..", "/", "../../etc/passwd", upper.as_str(), short, long.as_str(), dotted.as_str(), slashed.as_str(),
            backslashed.as_str(), padded.as_str(), twin.as_str(), "aaaaaaaaaaaaaaaaaaaaaaaaa1", "aaaaaaaaaaaaaaaaaaaaaaaaa\0",
            "ééééééééééééé",
        ] {
            assert_eq!(BlobId::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_blob_url_prints_and_parses() {
        let url = BlobUrl::new(store(), BlobId::new());
        let text = url.to_string();
        assert_eq!(text, format!("pimble-blob:{}/{}", url.store, url.blob));
        assert_eq!(BlobUrl::parse(&text), Some(url));
        assert_eq!(BlobUrl::parse(&text.replace("pimble-blob:", "PIMBLE-BLOB:")), Some(url));
        let json = serde_json::to_string(&url).unwrap();
        assert_eq!(json, format!("\"{text}\""));
        assert_eq!(serde_json::from_str::<BlobUrl>(&json).unwrap(), url);
    }

    #[test]
    fn anything_else_is_not_a_blob_url() {
        let (store, blob) = (store(), BlobId::new());
        for src in [
            "https://example.com/cat.png".to_string(),
            "data:image/png;base64,AAAA".to_string(),
            "pimble-blob:".to_string(),
            format!("pimble-blob:{store}"),
            format!("pimble-blob:{store}/"),
            format!("pimble-blob:{store}/{blob}/extra"),
            format!("pimble-blob:{store}/{blob}?x=1"),
            format!("pimble-blob:{store}/{blob}#frag"),
            format!("pimble-blob:{store}/../{blob}"),
            format!("pimble-blob:{store}/{}", blob.to_string().to_uppercase()),
            format!("pimble-blob:not-a-uuid/{blob}"),
            format!("pimble:{store}/{blob}"),
            format!("pimble-blobs:{store}/{blob}"),
        ] {
            assert_eq!(BlobUrl::parse(&src), None, "{src}");
        }
        // And a blob URL is not a link to a node.
        assert_eq!(crate::PimbleUrl::parse(&BlobUrl::new(store, blob).to_string()), None);
    }

    #[test]
    fn the_accepted_types_are_five_and_svg_is_not_one() {
        for mime in ImageMime::ALL {
            assert_eq!(ImageMime::parse(mime.as_str()), Some(mime));
        }
        assert_eq!(ImageMime::parse("IMAGE/PNG"), Some(ImageMime::Png));
        for bad in ["image/svg+xml", "image/jpg", "image/png; charset=utf-8", "text/html", "", "image/bmp"] {
            assert_eq!(ImageMime::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_type_is_sniffed_from_the_first_bytes() {
        assert_eq!(ImageMime::sniff(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"), Some(ImageMime::Png));
        assert_eq!(ImageMime::sniff(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 16]), Some(ImageMime::Jpeg));
        assert_eq!(ImageMime::sniff(b"GIF89a\x01\0\x01\0"), Some(ImageMime::Gif));
        assert_eq!(ImageMime::sniff(b"RIFF\x24\0\0\0WEBPVP8 "), Some(ImageMime::Webp));
        assert_eq!(ImageMime::sniff(b"\0\0\0\x1cftypavif\0\0\0\0avifmif1miaf"), Some(ImageMime::Avif));
        assert_eq!(ImageMime::sniff(b"\0\0\0\x1cftypmif1\0\0\0\0mif1avifmiaf"), Some(ImageMime::Avif));
        // Another ISO BMFF file (HEIC, MP4) is not AVIF.
        assert_eq!(ImageMime::sniff(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"), None);
        assert_eq!(ImageMime::sniff(b"\0\0\0\x18ftypisom\0\0\0\0isomavc1 and then avif in the data"), None);
        assert_eq!(ImageMime::sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"), None);
        assert_eq!(ImageMime::sniff(b"<?xml version=\"1.0\"?><svg/>"), None);
        assert_eq!(ImageMime::sniff(b"RIFF\x24\0\0\0WAVEfmt "), None);
        assert_eq!(ImageMime::sniff(b""), None);
    }
}
