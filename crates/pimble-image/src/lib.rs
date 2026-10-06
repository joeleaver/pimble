//! Fitting a picture under the size limit before it is stored
//! (docs/IMAGES_CONTRACT.md, "Too large").
//!
//! A picture a person pastes, drops or picks is never refused for its size: one over
//! the limit is scaled down in the client that is inserting it, and the person is told
//! in one sentence. This crate is that one function, [`fit`], for the desktop, the
//! browser and the MCP server alike.
//!
//! A picture already under the limit is passed through untouched, byte for byte.
//! One over it is decoded, turned upright by its EXIF orientation (the rest of its
//! metadata is dropped), scaled by `sqrt(limit / size)` and then by 0.9 a step until
//! its encoding fits, and written as PNG when it has transparency, else as JPEG at
//! quality 90. An animated GIF over the limit keeps its first frame as a still.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};

/// The largest picture a store holds, in bytes (decision 2 of the images contract).
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// The quality an oversize picture without transparency is rewritten at.
const JPEG_QUALITY: u8 = 90;

/// What each further step scales by when the first guess does not fit.
const STEP: f64 = 0.9;

/// A picture ready to store.
#[derive(Debug, Clone, PartialEq)]
pub struct Fitted {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    /// The sentence for the person when the picture was changed to fit; `None` when
    /// it went through as it was.
    pub notice: Option<String>,
}

/// Why a picture cannot be stored at all. Each is a sentence for the person.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum FitError {
    #[error("This is not a picture Pimble can hold (PNG, JPEG, GIF, WebP or AVIF).")]
    NotAnImage,
    #[error("This picture is {size}, over Pimble's {limit} limit, and Pimble cannot scale an AVIF picture down; save it as PNG or JPEG and add that.")]
    OversizeAvif { size: String, limit: String },
    #[error("This picture is {size}, over Pimble's {limit} limit, and it could not be read to scale it down: {reason}")]
    Unreadable { size: String, limit: String, reason: String },
}

/// The MIME type of `bytes` by their first bytes: one of the five kinds a store
/// holds, or `None`. Never SVG, whatever a file's name says.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" && matches!(&bytes[8..12], b"avif" | b"avis") {
        Some("image/avif")
    } else {
        None
    }
}

/// `bytes` as a picture of at most [`MAX_IMAGE_BYTES`].
pub fn fit(bytes: Vec<u8>) -> Result<Fitted, FitError> {
    fit_within(bytes, MAX_IMAGE_BYTES)
}

/// [`fit`] with the limit given, so a test need not build a 20 MiB picture.
pub fn fit_within(bytes: Vec<u8>, limit: usize) -> Result<Fitted, FitError> {
    let mime = sniff(&bytes).ok_or(FitError::NotAnImage)?;
    if bytes.len() <= limit {
        return Ok(Fitted { bytes, mime, notice: None });
    }
    let (size, limit_text) = (mebibytes(bytes.len()), mebibytes(limit));
    if mime == "image/avif" {
        // Nothing in the stack reads AVIF without a C decoder.
        return Err(FitError::OversizeAvif { size, limit: limit_text });
    }
    let unreadable = |reason: String| FitError::Unreadable { size: size.clone(), limit: limit_text.clone(), reason };

    let format = ImageFormat::from_mime_type(mime).ok_or(FitError::NotAnImage)?;
    let mut decoder = ImageReader::with_format(Cursor::new(&bytes), format)
        .into_decoder()
        .map_err(|e| unreadable(e.to_string()))?;
    let orientation = decoder.orientation().map_err(|e| unreadable(e.to_string()))?;
    // A GIF decodes to its first frame, which is what an oversize animation keeps.
    let mut picture = DynamicImage::from_decoder(decoder).map_err(|e| unreadable(e.to_string()))?;
    picture.apply_orientation(orientation);

    let transparent = picture.color().has_alpha() && has_transparency(&picture);
    let mut scale = (limit as f64 / bytes.len() as f64).sqrt().min(1.0);
    loop {
        let width = ((picture.width() as f64 * scale).round() as u32).max(1);
        let height = ((picture.height() as f64 * scale).round() as u32).max(1);
        let scaled = if (width, height) == (picture.width(), picture.height()) {
            picture.clone()
        } else {
            picture.resize_exact(width, height, FilterType::Lanczos3)
        };
        let (encoded, out_mime) = encode(&scaled, transparent).map_err(&unreadable)?;
        if encoded.len() <= limit || (width == 1 && height == 1) {
            let still = if mime == "image/gif" { " An animated GIF this large keeps only its first frame." } else { "" };
            let notice = format!(
                "This picture was {size}, so it was scaled to {width} × {height} ({}) to fit Pimble's {limit_text} limit.{still}",
                mebibytes(encoded.len())
            );
            return Ok(Fitted { bytes: encoded, mime: out_mime, notice: Some(notice) });
        }
        scale *= STEP;
    }
}

/// PNG for a picture with transparency, JPEG at quality 90 for one without.
fn encode(picture: &DynamicImage, transparent: bool) -> Result<(Vec<u8>, &'static str), String> {
    let mut out = Vec::new();
    if transparent {
        picture.write_to(&mut Cursor::new(&mut out), ImageFormat::Png).map_err(|e| e.to_string())?;
        Ok((out, "image/png"))
    } else {
        let rgb = picture.to_rgb8();
        JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY).encode_image(&rgb).map_err(|e| e.to_string())?;
        Ok((out, "image/jpeg"))
    }
}

/// Whether any pixel is less than opaque: an alpha channel that says nothing is not
/// a reason to write a PNG.
fn has_transparency(picture: &DynamicImage) -> bool {
    picture.to_rgba8().pixels().any(|pixel| pixel.0[3] != u8::MAX)
}

/// A size as the person reads it: "34.2 MiB", or kibibytes below one.
fn mebibytes(bytes: usize) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    if (bytes as f64) < MIB {
        format!("{:.0} KiB", (bytes as f64 / 1024.0).max(1.0))
    } else {
        format!("{:.1} MiB", bytes as f64 / MIB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    /// A picture that does not compress: every pixel differs from its neighbours.
    fn noise(width: u32, height: u32, alpha: impl Fn(u32, u32) -> u8) -> RgbaImage {
        let mut state = 0x9E37_79B9u32;
        RgbaImage::from_fn(width, height, |x, y| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let [r, g, b, _] = state.to_be_bytes();
            Rgba([r, g, b, alpha(x, y)])
        })
    }

    fn png(picture: &RgbaImage) -> Vec<u8> {
        let mut out = Vec::new();
        picture.write_to(&mut Cursor::new(&mut out), ImageFormat::Png).unwrap();
        out
    }

    fn dimensions(bytes: &[u8]) -> (u32, u32) {
        let picture = image::load_from_memory(bytes).unwrap();
        (picture.width(), picture.height())
    }

    #[test]
    fn a_picture_under_the_limit_goes_through_byte_for_byte() {
        let bytes = png(&noise(40, 30, |_, _| 255));
        let fitted = fit(bytes.clone()).unwrap();
        assert_eq!(fitted, Fitted { bytes, mime: "image/png", notice: None });
    }

    #[test]
    fn an_opaque_picture_over_the_limit_becomes_a_smaller_jpeg_and_says_so() {
        let bytes = png(&noise(400, 300, |_, _| 255));
        let limit = bytes.len() / 4;
        let fitted = fit_within(bytes.clone(), limit).unwrap();
        assert_eq!(fitted.mime, "image/jpeg");
        assert!(fitted.bytes.len() <= limit);
        let (width, height) = dimensions(&fitted.bytes);
        assert!(width < 400 && height < 300, "{width} x {height}");
        // The aspect ratio holds to the pixel rounding.
        assert!((width as f64 / height as f64 - 4.0 / 3.0).abs() < 0.02, "{width} x {height}");
        let notice = fitted.notice.unwrap();
        assert!(notice.starts_with(&format!("This picture was {}, so it was scaled to {width} × {height} (", mebibytes(bytes.len()))), "{notice}");
        assert!(notice.ends_with(&format!("to fit Pimble's {} limit.", mebibytes(limit))), "{notice}");
    }

    #[test]
    fn a_picture_with_transparency_stays_a_png() {
        let bytes = png(&noise(300, 300, |x, _| if x < 150 { 0 } else { 255 }));
        let limit = bytes.len() / 3;
        let fitted = fit_within(bytes, limit).unwrap();
        assert_eq!(fitted.mime, "image/png");
        assert!(fitted.bytes.len() <= limit);
        let picture = image::load_from_memory(&fitted.bytes).unwrap().to_rgba8();
        assert_eq!(picture.get_pixel(0, 0).0[3], 0, "the transparent half is still transparent");
    }

    #[test]
    fn an_oversize_gif_keeps_its_first_frame_and_the_sentence_says_so() {
        use image::codecs::gif::GifEncoder;
        use image::Frame;
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            for seed in 0..3u8 {
                let mut frame = noise(120, 120, |_, _| 255);
                frame.put_pixel(0, 0, Rgba([seed, 0, 0, 255]));
                encoder.encode_frame(Frame::new(frame)).unwrap();
            }
        }
        assert_eq!(sniff(&bytes), Some("image/gif"));
        let fitted = fit_within(bytes.clone(), bytes.len() / 2).unwrap();
        assert_ne!(fitted.mime, "image/gif");
        assert!(fitted.notice.unwrap().ends_with("An animated GIF this large keeps only its first frame."));
    }

    #[test]
    fn exif_orientation_is_applied_before_scaling() {
        // A 600 x 200 JPEG whose EXIF says "rotate 90 degrees clockwise to view".
        let wide = DynamicImage::ImageRgba8(noise(600, 200, |_, _| 255)).to_rgb8();
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 95).encode_image(&wide).unwrap();
        let exif: &[u8] = &[
            0xFF, 0xE1, 0x00, 0x22, b'E', b'x', b'i', b'f', 0, 0, // APP1, length 34, "Exif\0\0"
            b'M', b'M', 0x00, 0x2A, 0x00, 0x00, 0x00, 0x08, // big-endian TIFF, IFD at 8
            0x00, 0x01, // one entry
            0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x06, 0x00, 0x00, // Orientation = 6
            0x00, 0x00, 0x00, 0x00, // no next IFD
        ];
        let mut bytes = jpeg[..2].to_vec();
        bytes.extend_from_slice(exif);
        bytes.extend_from_slice(&jpeg[2..]);
        let fitted = fit_within(bytes.clone(), bytes.len() / 3).unwrap();
        let (width, height) = dimensions(&fitted.bytes);
        assert!(height > width, "upright after the turn: {width} x {height}");
    }

    #[test]
    fn what_is_not_one_of_the_five_kinds_is_refused() {
        assert_eq!(fit(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec()), Err(FitError::NotAnImage));
        assert_eq!(fit(b"plain text".to_vec()), Err(FitError::NotAnImage));
        assert_eq!(sniff(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"\x00\x00\x00\x1cftypavif\x00\x00\x00\x00"), Some("image/avif"));
    }

    #[test]
    fn an_oversize_avif_is_refused_with_what_to_do() {
        let mut bytes = b"\x00\x00\x00\x1cftypavif".to_vec();
        bytes.resize(4096, 0);
        let err = fit_within(bytes, 1024).unwrap_err();
        assert!(err.to_string().contains("cannot scale an AVIF picture down"), "{err}");
    }
}
