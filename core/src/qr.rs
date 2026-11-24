//! Reading a QR code from an image: a file the user picked or a picture from the clipboard.
//!
//! The image is untrusted input: its size is capped before decoding, its dimensions are
//! limited while decoding, and what comes out is only text, which then goes through the same
//! checks and the same confirmation as any pasted text (`link`). The path of a picked file never
//! leaves Rust.
use crate::text::t;
use crate::{failure::FailureCode, Error, Result};
use image::{DynamicImage, ImageReader, Limits};
use std::io::Cursor;

/// Largest image file read.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// Longest side of an image that is decoded.
pub const MAX_SIDE: u32 = 8192;
/// Longest text a QR code may carry here: a subscription link or a few server links.
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}

/// The bytes of the image file at `path`, at most [`MAX_IMAGE_BYTES`]: the file is not read
/// beyond that. A file that cannot be read is reported without its path.
pub fn read_file(path: &std::path::Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let failed = || Error::failure(FailureCode::Persistence, t("core.qr.file_read_failed"));
    let file = std::fs::File::open(path).map_err(|_| failed())?;
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failed())?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(invalid(t("core.qr.file_too_large")));
    }
    Ok(bytes)
}

/// The text of the first QR code found in an image file (PNG, JPEG or BMP).
pub fn read_image(bytes: &[u8]) -> Result<String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(invalid(t("core.qr.file_too_large")));
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| invalid(t("core.qr.image_read_failed")))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    // Room for one decoded image of the largest size, and no more.
    limits.max_alloc = Some(u64::from(MAX_SIDE) * u64::from(MAX_SIDE) * 4);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| invalid(t("core.qr.image_format")))?;
    read_dynamic(&image)
}

/// The text of the first QR code in raw RGBA pixels (a picture from the clipboard).
pub fn read_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<String> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(invalid(t("core.qr.image_too_large")));
    }
    let image = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| invalid(t("core.qr.image_read_failed")))?;
    read_dynamic(&DynamicImage::ImageRgba8(image))
}

fn read_dynamic(image: &DynamicImage) -> Result<String> {
    let gray = image.to_luma8();
    let (width, height) = (gray.width() as usize, gray.height() as usize);
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| {
        gray.as_raw()[y * width + x]
    });
    let mut readable = false;
    for grid in prepared.detect_grids() {
        if let Ok((_, content)) = grid.decode() {
            readable = true;
            if content.len() <= MAX_PAYLOAD_BYTES && !content.trim().is_empty() {
                return Ok(content);
            }
        }
    }
    Err(invalid(if readable {
        t("core.qr.qr_empty_or_large")
    } else {
        t("core.qr.qr_not_found")
    }))
}
