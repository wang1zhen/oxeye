//! Format detection, image encoding and atomic overwrite for the editable formats.
//!
//! Pure logic: no egui/eframe. Encoders are chosen explicitly so JPEG quality can be
//! controlled and so the lossy/lossless behavior of every format is visible at the call site.
//!
//! Known limits (deliberate, see the 0.3.0 plan):
//! - WebP can only be written **lossless** by `image` 0.25.10, so photographic sources may grow.
//! - JPEG has no alpha; `JpegEncoder::encode_image` drops it while encoding.
//! - BMP keeps RGBA8 as a 32-bit BITMAPV4 (BGRA + alpha mask), so alpha survives a round trip.
//! - Metadata (EXIF/ICC/XMP) and animation are never carried over.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use image::codecs::bmp::BmpEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageEncoder, ImageError, RgbaImage};

use crate::model::{MAX_DECODED_BYTES, MAX_SOURCE_SIDE};

/// Default JPEG quality for overwrite/Save As.
pub const JPEG_QUALITY_DEFAULT: u8 = 90;
/// Lowest JPEG quality offered by the UI.
pub const JPEG_QUALITY_MIN: u8 = 40;
/// Highest JPEG quality offered by the UI.
pub const JPEG_QUALITY_MAX: u8 = 100;

/// Writable image formats. GIF/TIFF/ICO stay read-only and map to `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Png,
    Jpeg,
    Webp,
    Bmp,
}

impl ExportFormat {
    /// Canonical lowercase extension used for Save As prefill.
    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
            Self::Bmp => "bmp",
        }
    }
}

/// Everything that can go wrong while exporting.
#[derive(Debug)]
pub enum ExportError {
    /// The target's parent directory does not exist (or is not a directory).
    MissingParent,
    /// The target exists but is a symlink, directory or other non-regular file.
    NotRegularFile,
    /// Zero-sized or over the side/byte budget, so it is not encoded at all.
    InvalidDimensions { width: u32, height: u32 },
    /// The encoder rejected or failed to write the image.
    Encode(ImageError),
    /// Filesystem failure while creating, syncing or replacing the target.
    Io(io::Error),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingParent => write!(f, "target directory does not exist"),
            Self::NotRegularFile => write!(f, "target is not a regular file"),
            Self::InvalidDimensions { width, height } => {
                write!(f, "cannot export a {width}x{height} image")
            }
            Self::Encode(error) => write!(f, "cannot encode image: {error}"),
            Self::Io(error) => write!(f, "cannot write image: {error}"),
        }
    }
}

impl std::error::Error for ExportError {}

impl From<io::Error> for ExportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ImageError> for ExportError {
    fn from(error: ImageError) -> Self {
        Self::Encode(error)
    }
}

/// Extension → format, case-insensitive. Unknown extensions return `None`; the caller decides
/// whether to ask the user for a format instead of guessing.
pub fn format_for_path(path: &Path) -> Option<ExportFormat> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some(ExportFormat::Png),
        "jpg" | "jpeg" => Some(ExportFormat::Jpeg),
        "webp" => Some(ExportFormat::Webp),
        "bmp" => Some(ExportFormat::Bmp),
        _ => None,
    }
}

/// Save As prefill: the original stem plus the canonical extension of its format.
/// Read-only sources (GIF/TIFF/ICO/CBZ/...) have no writable format, so they default to `png`.
pub fn default_file_name(source_name: &str) -> String {
    let path = Path::new(source_name);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("export");
    let extension = format_for_path(path).map_or("png", ExportFormat::extension);
    format!("{stem}.{extension}")
}

/// True when any pixel is not fully opaque, i.e. a JPEG save would visibly drop transparency.
pub fn has_transparency(img: &RgbaImage) -> bool {
    img.pixels().any(|pixel| pixel.0[3] < u8::MAX)
}

fn clamp_quality(quality: u8) -> u8 {
    quality.clamp(JPEG_QUALITY_MIN, JPEG_QUALITY_MAX)
}

/// Rejects images the encoders cannot represent or that would blow the decode budget.
fn validate(img: &RgbaImage) -> Result<(), ExportError> {
    let (width, height) = img.dimensions();
    if width == 0 || height == 0 || width > MAX_SOURCE_SIDE || height > MAX_SOURCE_SIDE {
        return Err(ExportError::InvalidDimensions { width, height });
    }
    let rgba_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4));
    match rgba_bytes {
        Some(bytes) if bytes <= MAX_DECODED_BYTES => Ok(()),
        _ => Err(ExportError::InvalidDimensions { width, height }),
    }
}

/// Encodes `img` into `w` without touching the filesystem.
///
/// PNG/WebP/BMP receive the RGBA8 buffer unchanged (WebP is lossless-only, BMP stores
/// 32-bit BGRA). JPEG clamps `quality` to `JPEG_QUALITY_MIN..=JPEG_QUALITY_MAX` and streams
/// 8x8 blocks through `JpegEncoder::encode_image`, so it drops alpha without allocating a
/// second full-size buffer.
pub fn encode_to(
    w: &mut impl Write,
    img: &RgbaImage,
    fmt: ExportFormat,
    quality: u8,
) -> Result<(), ExportError> {
    validate(img)?;
    let (width, height) = img.dimensions();
    match fmt {
        ExportFormat::Png => {
            PngEncoder::new(&mut *w).write_image(
                img.as_raw(),
                width,
                height,
                ExtendedColorType::Rgba8,
            )?;
        }
        ExportFormat::Jpeg => {
            JpegEncoder::new_with_quality(&mut *w, clamp_quality(quality)).encode_image(img)?;
        }
        ExportFormat::Webp => {
            WebPEncoder::new_lossless(&mut *w).write_image(
                img.as_raw(),
                width,
                height,
                ExtendedColorType::Rgba8,
            )?;
        }
        ExportFormat::Bmp => {
            let mut encoder = BmpEncoder::new(&mut *w);
            encoder.encode(img.as_raw(), width, height, ExtendedColorType::Rgba8)?;
        }
    }
    w.flush()?;
    Ok(())
}

/// Writes `img` to `path` via a same-directory temp file, then atomically renames it over the
/// target. When the target exists its permission bits are inherited; a new target keeps the
/// temp file's default (0600) permissions.
///
/// Fails without touching the target when it is a symlink or non-regular file, when the parent
/// directory is missing, when the image is over budget, or when encoding/renaming fails. On
/// failure the temp file is *attempted* to be removed (`tempfile`'s drop ignores unlink errors),
/// and under normal filesystem conditions the original bytes and permissions are unchanged.
/// Boundary: the parent directory itself is not fsynced, and another process replacing the
/// parent directory between the checks and the rename is not defended against.
pub fn atomic_write(
    path: &Path,
    img: &RgbaImage,
    fmt: ExportFormat,
    quality: u8,
) -> Result<(), ExportError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(ExportError::MissingParent);
    }
    // Validate before creating the temp file: a bad image leaves no filesystem trace at all.
    validate(img)?;

    // symlink_metadata: a symlink must be rejected even when its target is a regular file.
    let existing = match fs::symlink_metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(metadata) = existing.as_ref()
        && !metadata.file_type().is_file()
    {
        return Err(ExportError::NotRegularFile);
    }

    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    encode_to(&mut temp, img, fmt, quality)?;
    temp.as_file().sync_all()?;
    if let Some(metadata) = existing {
        fs::set_permissions(temp.path(), metadata.permissions())?;
    }
    replace_temp(temp, path)
}

/// Replaces `path` with an already-written temp file on the same filesystem.
fn replace_temp(temp: tempfile::NamedTempFile, path: &Path) -> Result<(), ExportError> {
    // shortcut: 未 fsync 父目录，掉电可能丢失 rename，若要求强持久化再补
    temp.persist(path)
        .map_err(|error| ExportError::Io(error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba};
    use tempfile::TempDir;

    fn sample(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, y| {
            Rgba([
                (x * 7) as u8,
                (y * 5) as u8,
                ((x + y) * 3) as u8,
                200 + (x % 56) as u8,
            ])
        })
    }

    fn detailed(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, y| {
            let noise = (x.wrapping_mul(37) ^ y.wrapping_mul(97)) as u8;
            Rgba([
                (x * 17 + u32::from(noise)) as u8,
                (y * 29 + u32::from(noise) / 2) as u8,
                (x * y * 3 + u32::from(noise) * 5) as u8,
                255,
            ])
        })
    }

    fn le_u16(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
    }

    fn le_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn opaque_gradient(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, y| {
            Rgba([
                (x * 255 / width.max(1)) as u8,
                (y * 255 / height.max(1)) as u8,
                128,
                255,
            ])
        })
    }

    fn encode(img: &RgbaImage, fmt: ExportFormat, quality: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        encode_to(&mut bytes, img, fmt, quality).expect("encode");
        bytes
    }

    fn decode(bytes: &[u8], format: ImageFormat) -> RgbaImage {
        image::load_from_memory_with_format(bytes, format)
            .expect("decode exported image")
            .to_rgba8()
    }

    #[test]
    fn extension_mapping_is_case_insensitive_and_strict() {
        assert_eq!(format_for_path(Path::new("a.png")), Some(ExportFormat::Png));
        assert_eq!(format_for_path(Path::new("a.PNG")), Some(ExportFormat::Png));
        assert_eq!(
            format_for_path(Path::new("a.JPG")),
            Some(ExportFormat::Jpeg)
        );
        assert_eq!(
            format_for_path(Path::new("a.jpeg")),
            Some(ExportFormat::Jpeg)
        );
        assert_eq!(
            format_for_path(Path::new("a.webp")),
            Some(ExportFormat::Webp)
        );
        assert_eq!(format_for_path(Path::new("a.bmp")), Some(ExportFormat::Bmp));
        assert_eq!(format_for_path(Path::new("a.gif")), None);
        assert_eq!(format_for_path(Path::new("a.png.txt")), None);
        assert_eq!(format_for_path(Path::new("no_extension")), None);
    }

    #[test]
    fn default_name_keeps_disk_format_and_falls_back_to_png() {
        assert_eq!(default_file_name("cover.png"), "cover.png");
        assert_eq!(default_file_name("cover.JPG"), "cover.jpg");
        assert_eq!(default_file_name("scan.jpeg"), "scan.jpg");
        assert_eq!(default_file_name("art.webp"), "art.webp");
        assert_eq!(default_file_name("art.bmp"), "art.bmp");
        // Read-only or archive sources default to PNG.
        assert_eq!(default_file_name("book.cbz"), "book.png");
        assert_eq!(default_file_name("anim.gif"), "anim.png");
        assert_eq!(default_file_name("no_extension"), "no_extension.png");
    }

    #[test]
    fn jpeg_quality_is_clamped_to_the_ui_range() {
        assert_eq!(clamp_quality(39), JPEG_QUALITY_MIN);
        assert_eq!(clamp_quality(0), JPEG_QUALITY_MIN);
        assert_eq!(clamp_quality(101), JPEG_QUALITY_MAX);
        assert_eq!(clamp_quality(255), JPEG_QUALITY_MAX);
        assert_eq!(clamp_quality(90), 90);
        assert_eq!(JPEG_QUALITY_DEFAULT, 90);
    }

    #[test]
    fn transparency_is_detected_for_the_jpeg_warning() {
        let mut img = sample(4, 4);
        for pixel in img.pixels_mut() {
            pixel.0[3] = u8::MAX;
        }
        assert!(!has_transparency(&img));
        img.put_pixel(2, 2, Rgba([1, 2, 3, 254]));
        assert!(has_transparency(&img));
    }

    #[test]
    fn png_round_trip_is_pixel_exact() {
        let img = sample(17, 13);
        let decoded = decode(&encode(&img, ExportFormat::Png, 90), ImageFormat::Png);
        assert_eq!(decoded.dimensions(), img.dimensions());
        assert_eq!(decoded, img);
    }

    #[test]
    fn webp_round_trip_is_pixel_exact_because_it_is_lossless() {
        let img = sample(17, 13);
        let decoded = decode(&encode(&img, ExportFormat::Webp, 90), ImageFormat::WebP);
        assert_eq!(decoded.dimensions(), img.dimensions());
        assert_eq!(decoded, img);
    }

    #[test]
    fn bmp_round_trip_is_pixel_exact_including_alpha() {
        let img = sample(17, 13);
        let decoded = decode(&encode(&img, ExportFormat::Bmp, 90), ImageFormat::Bmp);
        assert_eq!(decoded.dimensions(), img.dimensions());
        assert_eq!(decoded, img);
    }

    #[test]
    fn bmp_header_is_a_v4_rgba_bitmap_with_alpha_masks() {
        let bytes = encode(&sample(3, 3), ExportFormat::Bmp, 90);
        assert_eq!(&bytes[0..2], b"BM");
        assert_eq!(le_u32(&bytes, 14), 108, "BITMAPV4HEADER size");
        assert_eq!(le_u16(&bytes, 28), 32, "bits per pixel");
        assert_eq!(le_u32(&bytes, 30), 3, "BI_BITFIELDS compression");
        assert_eq!(le_u32(&bytes, 54), 0x00ff_0000, "red mask");
        assert_eq!(le_u32(&bytes, 58), 0x0000_ff00, "green mask");
        assert_eq!(le_u32(&bytes, 62), 0x0000_00ff, "blue mask");
        assert_eq!(le_u32(&bytes, 66), 0xff00_0000, "alpha mask");
    }

    #[test]
    fn jpeg_round_trip_keeps_dimensions_and_bounds_error() {
        let img = opaque_gradient(32, 24);
        let decoded = decode(&encode(&img, ExportFormat::Jpeg, 90), ImageFormat::Jpeg);
        assert_eq!(decoded.dimensions(), img.dimensions());

        // Each 8x8 block is encoded at full resolution (1x1 sampling factors, 4:4:4), so a
        // smooth gradient only drifts by a few levels per channel; 4/255 mean absolute error
        // leaves headroom for encoder revisions while still catching a wrong color conversion
        // or flipped channel order.
        let mut total = 0u64;
        for (a, b) in img.pixels().zip(decoded.pixels()) {
            for channel in 0..3 {
                total += u64::from(a.0[channel].abs_diff(b.0[channel]));
            }
        }
        let mean = total as f64 / (u64::from(img.width() * img.height()) * 3) as f64;
        assert!(mean <= 4.0, "mean absolute error was {mean}");
        assert_eq!(decoded.pixels().next().unwrap().0[3], 255);
    }

    #[test]
    fn jpeg_quality_reaches_the_encoder_and_extremes_clamp() {
        let img = detailed(16, 16);
        let low = encode(&img, ExportFormat::Jpeg, JPEG_QUALITY_MIN);
        let high = encode(&img, ExportFormat::Jpeg, JPEG_QUALITY_MAX);
        assert_ne!(low, high, "quality must change the encoded bytes");
        assert_eq!(
            decode(&low, ImageFormat::Jpeg).dimensions(),
            img.dimensions()
        );
        assert_eq!(
            decode(&high, ImageFormat::Jpeg).dimensions(),
            img.dimensions()
        );
        // Out-of-range values are clamped before they reach the encoder.
        assert_eq!(encode(&img, ExportFormat::Jpeg, 0), low);
        assert_eq!(encode(&img, ExportFormat::Jpeg, 255), high);
    }

    #[test]
    fn encode_reports_write_failures_instead_of_panicking() {
        struct FailingWriter {
            remaining: usize,
        }

        impl std::io::Write for FailingWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if self.remaining == 0 {
                    return Err(std::io::Error::other("simulated disk full"));
                }
                let written = buf.len().min(self.remaining);
                self.remaining -= written;
                Ok(written)
            }

            fn flush(&mut self) -> std::io::Result<()> {
                if self.remaining == 0 {
                    return Err(std::io::Error::other("simulated disk full"));
                }
                Ok(())
            }
        }

        let img = sample(64, 64);
        for fmt in [
            ExportFormat::Png,
            ExportFormat::Jpeg,
            ExportFormat::Webp,
            ExportFormat::Bmp,
        ] {
            let mut writer = FailingWriter { remaining: 32 };
            let result = encode_to(&mut writer, &img, fmt, 90);
            assert!(
                matches!(result, Err(ExportError::Encode(_) | ExportError::Io(_))),
                "{fmt:?}: {result:?}"
            );
        }
    }

    #[test]
    fn zero_sized_images_leave_the_target_untouched_without_residue() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cover.png");
        fs::write(&path, b"original").unwrap();

        for img in [RgbaImage::new(0, 4), RgbaImage::new(4, 0)] {
            let error = atomic_write(&path, &img, ExportFormat::Png, 90).unwrap_err();
            assert!(matches!(error, ExportError::InvalidDimensions { .. }));
            assert_eq!(fs::read(&path).unwrap(), b"original");
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn encoding_failure_after_temp_creation_leaves_the_target_untouched() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cover.png");
        let original = sample(5, 5);
        fs::write(&path, encode(&original, ExportFormat::Png, 90)).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let before = fs::read(&path).unwrap();

        // 20000x1 passes `validate` (80 kB RGBA, side below 65535), but image-webp rejects any
        // side above 16384 and returns before writing: image-webp-0.2.4/src/encoder.rs:399-401.
        // So the failure happens after atomic_write has created its temp file.
        let wide = RgbaImage::new(20_000, 1);
        let error = atomic_write(&path, &wide, ExportFormat::Webp, 90).unwrap_err();
        assert!(matches!(error, ExportError::Encode(_) | ExportError::Io(_)));

        assert_eq!(fs::read(&path).unwrap(), before);
        let mut entries: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        assert_eq!(entries, vec!["cover.png".to_string()]);

        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn replace_temp_failure_keeps_the_target_and_removes_the_temp_file() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();

        let temp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        let temp_path = temp.path().to_path_buf();
        // Renaming a file over an existing directory fails deterministically on every platform.
        let error = replace_temp(temp, &target).unwrap_err();
        assert!(matches!(error, ExportError::Io(_)));

        assert!(target.is_dir());
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
        // tempfile's drop handles this case by ignoring unlink errors; on a normal filesystem
        // the temp file is gone here, which the assertion below pins down.
        assert!(
            !temp_path.exists(),
            "persist failure must not leak the temp file"
        );
    }

    #[test]
    fn side_limit_is_inclusive_at_65535() {
        assert!(validate(&RgbaImage::new(MAX_SOURCE_SIDE, 1)).is_ok());
        assert!(validate(&RgbaImage::new(1, MAX_SOURCE_SIDE)).is_ok());
        let error = validate(&RgbaImage::new(MAX_SOURCE_SIDE + 1, 1)).unwrap_err();
        assert!(matches!(error, ExportError::InvalidDimensions { .. }));
    }

    #[test]
    fn atomic_write_creates_a_new_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("new.png");
        let img = sample(9, 7);
        atomic_write(&path, &img, ExportFormat::Png, 90).unwrap();
        assert_eq!(decode(&fs::read(&path).unwrap(), ImageFormat::Png), img);
        // No temp file survives a successful persist.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_replaces_content_and_keeps_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cover.png");
        fs::write(&path, b"old bytes").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        let img = sample(9, 7);
        atomic_write(&path, &img, ExportFormat::Png, 90).unwrap();

        assert_eq!(decode(&fs::read(&path).unwrap(), ImageFormat::Png), img);
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn atomic_write_rejects_a_missing_parent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("absent").join("cover.png");
        let error = atomic_write(&path, &sample(4, 4), ExportFormat::Png, 90).unwrap_err();
        assert!(matches!(error, ExportError::MissingParent));
        assert!(!dir.path().join("absent").exists());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_refuses_symlink_and_directory_targets() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real.png");
        fs::write(&real, b"original").unwrap();
        let link = dir.path().join("link.png");
        symlink(&real, &link).unwrap();

        let error = atomic_write(&link, &sample(4, 4), ExportFormat::Png, 90).unwrap_err();
        assert!(matches!(error, ExportError::NotRegularFile));
        assert_eq!(fs::read(&real).unwrap(), b"original");

        let directory_target = dir.path().join("subdir");
        fs::create_dir(&directory_target).unwrap();
        let error =
            atomic_write(&directory_target, &sample(4, 4), ExportFormat::Png, 90).unwrap_err();
        assert!(matches!(error, ExportError::NotRegularFile));
        assert!(directory_target.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn failed_write_leaves_original_bytes_and_no_temp_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cover.png");
        let original = sample(5, 5);
        fs::write(&path, encode(&original, ExportFormat::Png, 90)).unwrap();
        let before = fs::read(&path).unwrap();

        let restore = fs::metadata(dir.path()).unwrap().permissions();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();

        // root ignores the directory bits; skip rather than pretend the failure was tested.
        if tempfile::NamedTempFile::new_in(dir.path()).is_ok() {
            fs::set_permissions(dir.path(), restore).unwrap();
            eprintln!("skipping read-only directory test: directory is still writable");
            return;
        }

        let result = atomic_write(&path, &sample(9, 9), ExportFormat::Png, 90);
        fs::set_permissions(dir.path(), restore).unwrap();

        assert!(
            result.is_err(),
            "write into a read-only directory must fail"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
