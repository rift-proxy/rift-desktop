//! QR codes from images: files and clipboard pictures. The images are drawn in the tests, so
//! the repository holds no binary fixtures (and no real links).
use async_trait::async_trait;
use image::{DynamicImage, ImageFormat, Luma, RgbaImage};
use proptest::prelude::*;
use qrcode::QrCode;
use rift_core::{
    application::Application,
    failure::FailureCode,
    link::{classify, Input, InputPreview},
    model::{Server, Store},
    qr::{read_image, read_rgba, MAX_IMAGE_BYTES, MAX_SIDE},
    runtime::Runtime,
    selection::Probe,
    Result,
};
use std::{io::Cursor, sync::Arc};

/// A QR code of `text`, black on white, `scale` pixels per module, with a quiet border.
fn code(text: &str, scale: u32) -> image::GrayImage {
    QrCode::new(text.as_bytes())
        .unwrap()
        .render::<Luma<u8>>()
        .module_dimensions(scale, scale)
        .quiet_zone(true)
        .build()
}
fn encoded(image: image::GrayImage, format: ImageFormat) -> Vec<u8> {
    let mut bytes = Vec::new();
    DynamicImage::ImageLuma8(image)
        .write_to(&mut Cursor::new(&mut bytes), format)
        .unwrap();
    bytes
}
const SUBSCRIPTION: &str = "https://panel.example/sub?token=abc123";
const SERVER: &str = "trojan://secret@a.example:443#Frankfurt";

#[test]
fn a_png_with_a_subscription_link_is_read() {
    let text = read_image(&encoded(code(SUBSCRIPTION, 6), ImageFormat::Png)).unwrap();
    assert_eq!(text, SUBSCRIPTION);
    assert_eq!(
        classify(&text).unwrap(),
        Input::Subscription(SUBSCRIPTION.into())
    );
}

#[test]
fn a_server_link_and_several_links_are_read() {
    assert_eq!(
        read_image(&encoded(code(SERVER, 6), ImageFormat::Png)).unwrap(),
        SERVER
    );
    let list = "trojan://p@a.example:443#A\ntrojan://p@b.example:443#B\ntrojan://p@c.example:443#C";
    let text = read_image(&encoded(code(list, 5), ImageFormat::Png)).unwrap();
    let Input::Servers(links) = classify(&text).unwrap() else {
        panic!("not servers");
    };
    assert_eq!(links.len(), 3);
}

#[test]
fn a_deep_link_in_a_code_is_read_and_unwrapped_by_the_same_text_path() {
    let wrapped = "rift://add?url=https%3A%2F%2Fpanel.example%2Fsub%3Ftoken%3Dabc123";
    let text = read_image(&encoded(code(wrapped, 6), ImageFormat::Png)).unwrap();
    assert_eq!(
        classify(&text).unwrap(),
        Input::Subscription(SUBSCRIPTION.into())
    );
}

#[test]
fn jpeg_and_bmp_files_are_read_too() {
    for format in [ImageFormat::Jpeg, ImageFormat::Bmp] {
        let bytes = encoded(code(SUBSCRIPTION, 8), format);
        assert_eq!(read_image(&bytes).unwrap(), SUBSCRIPTION, "{format:?}");
    }
}

#[test]
fn a_code_in_a_larger_picture_is_found() {
    // A screenshot: the code is a part of a bigger, otherwise white, image.
    let small = code(SUBSCRIPTION, 4);
    let mut canvas = image::GrayImage::from_pixel(1200, 800, Luma([255]));
    image::imageops::overlay(&mut canvas, &small, 700, 300);
    let bytes = encoded(canvas, ImageFormat::Png);
    assert_eq!(read_image(&bytes).unwrap(), SUBSCRIPTION);
}

#[test]
fn clipboard_pixels_are_read() {
    let gray = code(SERVER, 6);
    let rgba: RgbaImage = DynamicImage::ImageLuma8(gray).to_rgba8();
    assert_eq!(
        read_rgba(rgba.width(), rgba.height(), rgba.as_raw()).unwrap(),
        SERVER
    );
}

#[test]
fn a_picture_without_a_code_is_an_error_not_an_empty_link() {
    let blank = encoded(
        image::GrayImage::from_pixel(300, 300, Luma([255])),
        ImageFormat::Png,
    );
    let error = read_image(&blank).unwrap_err().to_string();
    assert!(error.contains("не найден"), "{error}");
}

#[test]
fn things_that_are_not_readable_images_are_refused() {
    assert!(read_image(b"").is_err());
    assert!(read_image(b"not an image at all").is_err());
    let mut png = encoded(code(SUBSCRIPTION, 6), ImageFormat::Png);
    png.truncate(png.len() / 2);
    assert!(read_image(&png).is_err(), "a truncated file");
    // Pixels that do not fit their stated size.
    assert!(read_rgba(10, 10, &[0; 12]).is_err());
}

#[test]
fn size_limits_apply_before_and_while_decoding() {
    assert!(read_image(&vec![0; MAX_IMAGE_BYTES + 1]).is_err());
    assert!(read_rgba(0, 10, &[]).is_err());
    assert!(read_rgba(MAX_SIDE + 1, 1, &[]).is_err());
    // A small file that claims a huge picture is not decoded.
    let huge = encoded(
        image::GrayImage::from_pixel(MAX_SIDE + 1, 8, Luma([255])),
        ImageFormat::Png,
    );
    assert!(huge.len() < MAX_IMAGE_BYTES);
    assert!(read_image(&huge).is_err());
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = read_image(&bytes);
    }

    #[test]
    fn arbitrary_pixels_never_panic(
        width in 1u32..40,
        height in 1u32..40,
        seed in proptest::collection::vec(any::<u8>(), 4..64),
    ) {
        let pixels: Vec<u8> = seed.iter().copied().cycle().take((width * height * 4) as usize).collect();
        let _ = read_rgba(width, height, &pixels);
    }
}

/// What the clipboard holds for a screenshot: a 32-bit bottom-up BMP (`BITMAPINFOHEADER`).
fn clipboard_bitmap(gray: &image::GrayImage) -> Vec<u8> {
    let (width, height) = (gray.width(), gray.height());
    let pixels = width * height * 4;
    let mut bmp = Vec::with_capacity(54 + pixels as usize);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(54 + pixels).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&(width as i32).to_le_bytes());
    bmp.extend_from_slice(&(height as i32).to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&32u16.to_le_bytes());
    bmp.extend_from_slice(&[0; 4]); // BI_RGB
    bmp.extend_from_slice(&pixels.to_le_bytes());
    bmp.extend_from_slice(&[0; 16]);
    for y in (0..height).rev() {
        for x in 0..width {
            let value = gray.get_pixel(x, y).0[0];
            bmp.extend_from_slice(&[value, value, value, 255]);
        }
    }
    bmp
}

#[test]
fn a_screenshot_bitmap_from_the_clipboard_is_read() {
    let bmp = clipboard_bitmap(&code(SUBSCRIPTION, 5));
    assert_eq!(read_image(&bmp).unwrap(), SUBSCRIPTION);
}

// ---- through the application ---------------------------------------------------------------

struct Idle;
#[async_trait]
impl Runtime for Idle {
    async fn validate(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn start(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(false)
    }
}
struct Fine;
#[async_trait]
impl Probe for Fine {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(1)
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        Ok(1)
    }
    async fn select(&self, _: &str) -> Result<()> {
        Ok(())
    }
}
fn app() -> (Arc<Application>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let app = Application::with_probe(
        Store::default(),
        dir.path().into(),
        Box::new(Idle),
        Arc::new(Fine),
        "12345678901234567890123456789012".into(),
    );
    (app, dir)
}

#[tokio::test]
async fn a_picked_image_file_gives_a_preview_and_adds_nothing() {
    let (app, dir) = app();
    let file = dir.path().join("my-private-name.png");
    std::fs::write(&file, encoded(code(SUBSCRIPTION, 6), ImageFormat::Png)).unwrap();
    let preview = app.scan_qr_file(&file).await.unwrap();
    assert!(matches!(preview, InputPreview::Subscription { url } if url == SUBSCRIPTION));
    let snapshot = app.snapshot();
    assert!(snapshot.subscriptions.is_empty() && snapshot.servers.is_empty());
    assert!(snapshot.error.is_none());
}

#[tokio::test]
async fn servers_in_a_code_are_previewed_with_their_names() {
    let (app, _dir) = app();
    let bytes = encoded(code(SERVER, 6), ImageFormat::Png);
    let InputPreview::Servers { items, rejected } = app.scan_qr_image(bytes).await.unwrap() else {
        panic!("not servers");
    };
    assert_eq!((items.len(), rejected), (1, 0));
    assert_eq!(items[0].server.name, "Frankfurt");
}

#[tokio::test]
async fn failures_are_recorded_with_static_texts_and_no_path() {
    let (app, dir) = app();
    let missing = dir.path().join("secret-folder-name").join("missing.png");
    let error = app.scan_qr_file(&missing).await.unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::Persistence));

    let not_image = dir.path().join("notes.png");
    std::fs::write(&not_image, b"just text").unwrap();
    assert!(app.scan_qr_file(&not_image).await.is_err());

    let blank = encoded(
        image::GrayImage::from_pixel(200, 200, Luma([255])),
        ImageFormat::Png,
    );
    assert!(app.scan_qr_image(blank).await.is_err());

    let snapshot = app.snapshot();
    let errors = snapshot
        .journal
        .iter()
        .filter(|entry| entry.level == "error")
        .count();
    assert_eq!(errors, 3);
    for entry in &snapshot.journal {
        for private in [
            "secret-folder-name",
            "missing.png",
            "notes.png",
            "just text",
        ] {
            assert!(!entry.message.contains(private), "{}", entry.message);
        }
    }
    assert!(snapshot.error.is_some(), "the last failure is shown");
}

#[tokio::test]
async fn an_oversized_file_is_refused_after_reading_no_more_than_the_limit() {
    let (app, dir) = app();
    let big = dir.path().join("big.png");
    let file = std::fs::File::create(&big).unwrap();
    file.set_len(MAX_IMAGE_BYTES as u64 + 1).unwrap();
    let error = app.scan_qr_file(&big).await.unwrap_err();
    assert!(error.to_string().contains("10 MiB"), "{error}");
}

#[tokio::test]
async fn a_good_scan_clears_the_failure_of_the_previous_one() {
    let (app, _dir) = app();
    assert!(app.scan_qr_image(b"nothing".to_vec()).await.is_err());
    assert!(app.snapshot().error.is_some());
    app.scan_qr_image(encoded(code(SERVER, 6), ImageFormat::Png))
        .await
        .unwrap();
    assert!(app.snapshot().error.is_none());
}
