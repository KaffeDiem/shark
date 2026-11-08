//! Agent avatars are private, content-addressed PNGs beneath the application data directory.
use crate::state::{self, AgentIcon};
use image::{
    RgbaImage,
    imageops::{self, FilterType},
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

const MAX_BYTES: usize = 10 * 1024 * 1024;
const MAX_PIXELS: u64 = 40_000_000;
pub const SIZE: u32 = 96;

pub fn decode_file(path: &Path) -> Result<RgbaImage, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    decode(&bytes)
}

pub fn decode(bytes: &[u8]) -> Result<RgbaImage, String> {
    if bytes.len() > MAX_BYTES {
        return Err("Choose an image smaller than 10 MB".into());
    }
    // Read dimensions before AppKit can allocate decompressed pixels.
    let size = imagesize::blob_size(bytes)
        .map_err(|_| "Cannot open this image. Choose PNG, JPEG, WebP, GIF or HEIC")?;
    if size.width == 0
        || size.height == 0
        || (size.width as u64).saturating_mul(size.height as u64) > MAX_PIXELS
    {
        return Err("Choose an image with at most 40 million pixels".into());
    }
    // AppKit also supports HEIC and chooses the first frame of animated images.
    use objc2::AllocAnyThread as _;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};
    use objc2_foundation::{NSData, NSDictionary};
    let rep = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &NSData::with_bytes(bytes))
        .ok_or("Cannot open this image. Choose PNG, JPEG, WebP, GIF or HEIC")?;
    let (w, h) = (rep.pixelsWide(), rep.pixelsHigh());
    if w <= 0 || h <= 0 || (w as u64).saturating_mul(h as u64) > MAX_PIXELS {
        return Err("Choose an image with at most 40 million pixels".into());
    }
    let png = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
    .ok_or("Cannot decode this image")?;
    image::load_from_memory_with_format(&png.to_vec(), image::ImageFormat::Png)
        .map(|image| image.into_rgba8())
        .map_err(|e| e.to_string())
}

/// A square crop: normalized image center, zoom 1 shows the largest square that fits.
pub fn crop_image(image: &RgbaImage, center: [f32; 2], zoom: f32) -> RgbaImage {
    let (w, h) = image.dimensions();
    if w == 0 || h == 0 {
        return RgbaImage::new(SIZE, SIZE);
    }
    let zoom = if zoom.is_finite() {
        zoom.clamp(1., w.min(h) as f32)
    } else {
        1.
    };
    let side = ((w.min(h) as f32 / zoom).round() as u32).clamp(1, w.min(h));
    let offset = |center: f32, length: u32| {
        let center = if center.is_finite() {
            center.clamp(0., 1.)
        } else {
            0.5
        };
        (center * length as f32 - side as f32 / 2.)
            .round()
            .clamp(0., (length - side) as f32) as u32
    };
    imageops::resize(
        &imageops::crop_imm(
            image,
            offset(center[0], w),
            offset(center[1], h),
            side,
            side,
        )
        .to_image(),
        SIZE,
        SIZE,
        FilterType::Lanczos3,
    )
}

/// Resolve only managed PNG names; never follow an avatar or its directory through a symlink.
pub fn path(file: &str) -> Option<PathBuf> {
    path_at(&state::support_dir(), file)
}

fn path_at(root: &Path, file: &str) -> Option<PathBuf> {
    let name = file.strip_prefix("agent-icons/")?;
    if name.is_empty()
        || !name.ends_with(".png")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        || name.contains("..")
    {
        return None;
    }
    let dir = root.join("agent-icons");
    let path = dir.join(name);
    for entry in [&dir, &path] {
        if let Ok(metadata) = fs::symlink_metadata(entry) {
            if metadata.file_type().is_symlink() {
                return None;
            }
        }
    }
    Some(path)
}

pub fn save_image(slug: &str, image: &RgbaImage) -> Result<AgentIcon, String> {
    save_at(&state::support_dir(), slug, image)
}

fn save_at(root: &Path, slug: &str, image: &RgbaImage) -> Result<AgentIcon, String> {
    if slug.is_empty() || !slug.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err("Invalid agent identity".into());
    }
    if image.width() == 0 || image.height() == 0 {
        return Err("Image is empty".into());
    }
    let image = if image.dimensions() == (SIZE, SIZE) {
        image.clone()
    } else {
        crop_image(image, [0.5, 0.5], 1.)
    };
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let bytes = bytes.into_inner();
    use std::fmt::Write as _;
    let mut hash = String::with_capacity(64);
    for byte in Sha256::digest(&bytes) {
        write!(&mut hash, "{byte:02x}").unwrap();
    }
    let file = format!("agent-icons/{slug}-{hash}.png");
    let path = path_at(root, &file).ok_or("Unsafe agent image path")?;
    state::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(AgentIcon::Image { file })
}

/// Call only after saving the state that no longer references this image.
pub fn remove(icon: &AgentIcon) -> io::Result<()> {
    if let AgentIcon::Image { file } = icon {
        if let Some(path) = path(file) {
            match fs::remove_file(path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                result => result?,
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_paths_reject_traversal_and_symlinks() {
        let root = std::env::temp_dir().join(format!("shark-icon-paths-{}", std::process::id()));
        fs::create_dir_all(root.join("agent-icons")).unwrap();
        for file in [
            "/tmp/a.png",
            "agent-icons/../a.png",
            "agent-icons/a/b.png",
            "agent-icons/a\\b.png",
            "agent-icons/a.jpg",
        ] {
            assert!(path_at(&root, file).is_none(), "{file}");
        }
        let target = root.join("outside.png");
        fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, root.join("agent-icons/a.png")).unwrap();
        assert!(path_at(&root, "agent-icons/a.png").is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crop_clamps_center_and_zoom_and_preserves_alpha() {
        let mut input = RgbaImage::from_pixel(192, 96, image::Rgba([255, 0, 0, 128]));
        for x in 96..192 {
            for y in 0..96 {
                input.put_pixel(x, y, image::Rgba([0, 0, 255, 255]));
            }
        }
        let left = crop_image(&input, [0., 0.5], 1.);
        assert_eq!(left.dimensions(), (96, 96));
        assert_eq!(left.get_pixel(48, 48).0, [255, 0, 0, 128]);
        let right = crop_image(&input, [5., -5.], 2.);
        assert_eq!(right.get_pixel(48, 48).0, [0, 0, 255, 255]);
        assert_eq!(
            crop_image(&input, [f32::NAN; 2], f32::NAN).dimensions(),
            (96, 96)
        );
    }

    #[test]
    fn saved_images_are_small_pngs_and_content_addressed() {
        let root = std::env::temp_dir().join(format!("shark-icon-save-{}", std::process::id()));
        let input = RgbaImage::from_pixel(200, 100, image::Rgba([20, 30, 40, 255]));
        let icon = save_at(&root, "agent-a", &input).unwrap();
        assert_eq!(save_at(&root, "agent-a", &input).unwrap(), icon);
        let AgentIcon::Image { file } = icon else {
            panic!()
        };
        let image = image::open(path_at(&root, &file).unwrap()).unwrap();
        assert_eq!((image.width(), image.height()), (96, 96));
        assert!(save_at(&root, "../bad", &input).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_decoder_preserves_png_transparency() {
        let input = RgbaImage::from_pixel(3, 2, image::Rgba([20, 30, 40, 128]));
        let mut png = std::io::Cursor::new(Vec::new());
        input.write_to(&mut png, image::ImageFormat::Png).unwrap();
        assert_eq!(
            imagesize::blob_size(png.get_ref()).unwrap(),
            imagesize::ImageSize {
                width: 3,
                height: 2
            }
        );
        let decoded = decode(png.get_ref()).unwrap();
        assert_eq!(decoded.dimensions(), (3, 2));
        assert_eq!(decoded.get_pixel(1, 1).0, [20, 30, 40, 128]);
    }

    #[test]
    fn native_decoder_accepts_portraits_and_first_gif_frame() {
        for bytes in [
            include_bytes!("../tests/fixtures/agent-icons/portrait.jpg").as_slice(),
            include_bytes!("../tests/fixtures/agent-icons/portrait.webp").as_slice(),
            include_bytes!("../tests/fixtures/agent-icons/animated.gif").as_slice(),
            include_bytes!("../tests/fixtures/agent-icons/portrait.heic").as_slice(),
        ] {
            assert_eq!(
                imagesize::blob_size(bytes).unwrap(),
                imagesize::ImageSize {
                    width: 8,
                    height: 6
                }
            );
            let decoded = decode(bytes).unwrap();
            assert_eq!(decoded.dimensions(), (8, 6));
            let pixel = decoded.get_pixel(4, 3).0;
            assert!(
                pixel[0] > 200 && pixel[2] < 60,
                "first frame should be red: {pixel:?}"
            );
        }
    }

    #[test]
    fn rejects_oversized_dimensions_before_decoding_pixels() {
        // A truncated IHDR has dimensions but no compressed pixel data to allocate.
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&6_401u32.to_be_bytes());
        png.extend_from_slice(&6_400u32.to_be_bytes());
        assert_eq!(
            imagesize::blob_size(&png).unwrap(),
            imagesize::ImageSize {
                width: 6_401,
                height: 6_400
            }
        );
        assert_eq!(
            decode(&png).unwrap_err(),
            "Choose an image with at most 40 million pixels"
        );
    }

    #[test]
    fn pixel_limit_accepts_exact_boundary_and_rejects_empty_dimensions() {
        for (width, height, within_limit) in [
            (8_000u32, 5_000u32, true),
            (8_001, 5_000, false),
            (0, 1, false),
            (1, 0, false),
        ] {
            let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
            png.extend_from_slice(&width.to_be_bytes());
            png.extend_from_slice(&height.to_be_bytes());
            let error = decode(&png).unwrap_err();
            if within_limit {
                assert!(error.starts_with("Cannot open this image"), "{error}");
            } else {
                assert!(error.contains("40 million pixels"), "{error}");
            }
        }
    }

    #[test]
    fn oversize_and_invalid_images_are_rejected() {
        assert!(
            decode(&vec![0; MAX_BYTES + 1])
                .unwrap_err()
                .contains("10 MB")
        );
        assert!(decode(b"not an image").is_err());
    }
}
