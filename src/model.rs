//! Image loading and manipulation for the viewer.
//!
//! Everything here is read-only with respect to the user's files. Encoded input
//! and decoded images are bounded, and malformed containers are rejected before
//! a decoder can trust their declared sizes. These are per-image bounds, not a
//! whole-process memory guarantee.

use std::io::{Cursor, Read};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use image::codecs::gif::GifDecoder;
use image::metadata::Orientation;
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageReader, Limits, RgbaImage};

/// Extensions the viewer advertises and lists next to the current image.
const SUPPORTED_EXTENSIONS: [&str; 9] = [
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff",
];

/// Largest accepted source image side before CPU budgets apply. GIF logical
/// screens are u16-sized; static images keep the same checked bound.
const MAX_SOURCE_SIDE: u32 = 65_535;
/// Largest accepted decoded buffer in bytes. The RGBA copy is bounded by the
/// same value, so peak use is roughly twice this (plus one rotation copy).
const MAX_DECODED_BYTES: u64 = 256 * 1024 * 1024;
/// Total RGBA bytes retained for one animation. Exceeding it rejects the whole
/// animation instead of silently truncating it.
const MAX_ANIMATION_BYTES: u64 = MAX_DECODED_BYTES;
/// Hard frame-count cap, independent of the byte budget.
const MAX_GIF_FRAMES: usize = 2_048;
/// GIF delays of zero are normalized to this to avoid busy playback loops.
const MIN_FRAME_DELAY: Duration = Duration::from_millis(10);
/// Largest encoded file read into memory. Some decoders copy the whole input
/// before honoring their own limits, so this is enforced before decoding.
const MAX_ENCODED_BYTES: u64 = 256 * 1024 * 1024;

/// How many times an animation repeats after its first play-through.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LoopMode {
    /// Play once, with no additional repetitions.
    Once,
    /// Play `n` additional times after the first play-through.
    Finite(u16),
    /// Repeat indefinitely.
    Infinite,
}

/// What the caller will do with the decoded result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DecodePurpose {
    /// Ordinary viewing: GIFs keep every composited frame.
    #[default]
    Animated,
    /// First-frame use such as thumbnails: GIFs decode only the first frame.
    Still,
}

/// One fully composited logical-screen frame.
#[derive(Debug)]
pub struct MediaFrame {
    pub pixels: Arc<RgbaImage>,
    pub delay: Duration,
}

/// A static image or bounded GIF animation.
#[derive(Debug)]
pub struct LoadedMedia {
    pub path: PathBuf,
    pub frames: Vec<MediaFrame>,
    pub loop_mode: LoopMode,
}

/// A decoded image and the absolute selected path it was opened from.
#[derive(Debug)]
pub struct LoadedImage {
    pub path: PathBuf,
    pub pixels: RgbaImage,
}

/// Decodes `path` into RGBA, applying its EXIF orientation.
///
/// This is a test-only helper retained for static-decode regressions; the UI
/// uses [`load_media`].
#[cfg(test)]
pub fn load_image(path: &Path) -> Result<LoadedImage, String> {
    load_image_bounded(path, MAX_ENCODED_BYTES)
}

#[cfg(test)]
fn load_image_bounded(path: &Path, encoded_limit: u64) -> Result<LoadedImage, String> {
    let absolute = resolve_supported(path)?;
    load_static(&absolute, encoded_limit)
}

#[cfg(test)]
fn load_static(absolute: &Path, encoded_limit: u64) -> Result<LoadedImage, String> {
    let bytes = read_encoded(absolute, encoded_limit)?;
    decode_static_bytes(absolute, bytes)
}

/// Decodes an already-snapshotted encoded buffer as a static image.
fn decode_static_bytes(absolute: &Path, bytes: Vec<u8>) -> Result<LoadedImage, String> {
    validate_webp_chunks(&bytes).map_err(|err| format!("{}: {err}", absolute.display()))?;

    let mut reader = ImageReader::new(Cursor::new(bytes));
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    limits.max_alloc = Some(MAX_DECODED_BYTES);
    reader.limits(limits.clone());
    let reader = reader
        .with_guessed_format()
        .map_err(|err| format!("cannot read {}: {err}", absolute.display()))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|err| format!("cannot decode {}: {err}", absolute.display()))?;
    decoder
        .set_limits(limits)
        .map_err(|err| format!("cannot decode {}: {err}", absolute.display()))?;

    let orientation = decoder
        .orientation()
        .map_err(|err| format!("cannot read metadata of {}: {err}", absolute.display()))?;
    let (width, height) = oriented_dimensions(decoder.dimensions(), orientation);
    validate_dimensions(width, height).map_err(|err| format!("{}: {err}", absolute.display()))?;
    let decoded_bytes = decoder.total_bytes();
    if decoded_bytes > MAX_DECODED_BYTES {
        return Err(format!(
            "{}: decoded data is {decoded_bytes} bytes, limit is {MAX_DECODED_BYTES}",
            absolute.display()
        ));
    }

    let mut image = DynamicImage::from_decoder(decoder)
        .map_err(|err| format!("cannot decode {}: {err}", absolute.display()))?;
    image.apply_orientation(orientation);

    Ok(LoadedImage {
        path: absolute.to_path_buf(),
        pixels: image.into_rgba8(),
    })
}

/// Loads a static image or a fully composited, bounded GIF animation.
///
/// Static images reuse the bounded static decoder and produce one zero-delay
/// frame in [`LoopMode::Once`]. GIF frames are decoded sequentially
/// with the image compositor and checked against the retained-RGBA and
/// frame-count budgets.
pub fn load_media(path: &Path, cancel: &AtomicBool) -> Result<LoadedMedia, String> {
    load_media_with_limits(
        path,
        cancel,
        MAX_ENCODED_BYTES,
        MAX_GIF_FRAMES,
        MAX_ANIMATION_BYTES,
    )
}

/// Loads `path` according to how the caller will display it.
///
/// [`DecodePurpose::Animated`] is identical to [`load_media`]. `Still` always
/// returns one zero-delay [`LoopMode::Once`] frame; animated GIFs decode only
/// their first composited logical screen, reusing the bounded static decoder.
pub fn load_media_for(
    path: &Path,
    cancel: &AtomicBool,
    purpose: DecodePurpose,
) -> Result<LoadedMedia, String> {
    match purpose {
        DecodePurpose::Animated => load_media(path, cancel),
        DecodePurpose::Still => load_still_media(path, cancel),
    }
}

fn load_still_media(path: &Path, cancel: &AtomicBool) -> Result<LoadedMedia, String> {
    let absolute = resolve_supported(path)?;
    if cancelled(cancel) {
        return Err(format!("{}: cancelled", absolute.display()));
    }
    let bytes = read_encoded(&absolute, MAX_ENCODED_BYTES)?;
    let loaded = decode_static_bytes(&absolute, bytes)?;
    if cancelled(cancel) {
        return Err(format!("{}: cancelled", absolute.display()));
    }
    Ok(still_media(loaded))
}

fn still_media(loaded: LoadedImage) -> LoadedMedia {
    LoadedMedia {
        path: loaded.path,
        frames: vec![MediaFrame {
            pixels: Arc::new(loaded.pixels),
            delay: Duration::ZERO,
        }],
        loop_mode: LoopMode::Once,
    }
}

fn load_media_with_limits(
    path: &Path,
    cancel: &AtomicBool,
    encoded_limit: u64,
    max_frames: usize,
    max_animation_bytes: u64,
) -> Result<LoadedMedia, String> {
    let absolute = resolve_supported(path)?;
    if cancelled(cancel) {
        return Err(format!("{}: cancelled", absolute.display()));
    }
    let bytes = read_encoded(&absolute, encoded_limit)?;

    // Dispatch on content, not extension: a GIF named .png must animate, and a
    // PNG/JPEG named .gif must remain static. The extension filter above still
    // rejects unsupported names.
    let is_gif = matches!(image::guess_format(&bytes), Ok(image::ImageFormat::Gif));
    if !is_gif {
        let loaded = decode_static_bytes(&absolute, bytes)?;
        if cancelled(cancel) {
            return Err(format!("{}: cancelled", absolute.display()));
        }
        return Ok(still_media(loaded));
    }

    let (loop_mode, expected_frames) =
        preflight_gif(&bytes, cancel, max_frames, max_animation_bytes)
            .map_err(|err| format!("{}: {err}", absolute.display()))?;
    let frames = decode_gif_frames(
        bytes,
        cancel,
        &absolute,
        expected_frames,
        max_frames,
        max_animation_bytes,
        |_| {},
    )?;

    if cancelled(cancel) {
        return Err(format!("{}: cancelled", absolute.display()));
    }
    Ok(LoadedMedia {
        path: absolute,
        frames,
        loop_mode,
    })
}

fn resolve_supported(path: &Path) -> Result<PathBuf, String> {
    let absolute = std::path::absolute(path)
        .map_err(|err| format!("cannot open {}: {err}", path.display()))?;
    if !absolute.is_file() {
        return Err(format!("not a file: {}", absolute.display()));
    }
    if !supported_path(&absolute) {
        return Err(format!("unsupported image format: {}", absolute.display()));
    }
    Ok(absolute)
}

#[cfg(test)]
fn is_gif(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"))
}

fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

/// Reads at most `limit` bytes, so a decoder cannot copy an unbounded file.
fn read_encoded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path)
        .map_err(|err| format!("cannot open {}: {err}", path.display()))?;
    let capacity = file
        .metadata()
        .map(|metadata| metadata.len().min(limit.saturating_add(1)))
        .unwrap_or(0);
    let mut bytes = Vec::with_capacity(usize::try_from(capacity).unwrap_or(0));
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "{}: encoded file is larger than {limit} bytes",
            path.display()
        ));
    }
    Ok(bytes)
}

/// Reads the loop extension and counts frames without decoding LZW pixels.
fn preflight_gif(
    bytes: &[u8],
    cancel: &AtomicBool,
    max_frames: usize,
    max_animation_bytes: u64,
) -> Result<(LoopMode, usize), String> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::Indexed);
    options.check_frame_consistency(true);
    options.skip_frame_decoding(true);
    options.set_memory_limit(gif::MemoryLimit::Bytes(
        NonZeroU64::new(MAX_DECODED_BYTES).expect("decoded budget is nonzero"),
    ));
    let mut decoder = options
        .read_info(Cursor::new(bytes))
        .map_err(|err| format!("cannot read GIF metadata: {err}"))?;

    let width = u32::from(decoder.width());
    let height = u32::from(decoder.height());
    validate_dimensions(width, height)?;
    let screen_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| format!("GIF screen is {width}x{height}, byte size overflowed"))?;

    let loop_mode = match decoder.repeat() {
        // `gif` uses Finite(0) as its default when no Netscape extension exists.
        gif::Repeat::Finite(0) => LoopMode::Once,
        gif::Repeat::Finite(repetitions) => LoopMode::Finite(repetitions),
        gif::Repeat::Infinite => LoopMode::Infinite,
    };

    let mut frame_count = 0usize;
    let mut retained_bytes = 0u64;
    loop {
        if cancelled(cancel) {
            return Err("cancelled".to_owned());
        }
        match decoder.read_next_frame() {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(err) => return Err(format!("cannot read GIF frame metadata: {err}")),
        }
        frame_count += 1;
        if frame_count > max_frames {
            return Err(format!("GIF has more than {max_frames} frames"));
        }
        retained_bytes = retained_bytes
            .checked_add(screen_bytes)
            .ok_or_else(|| "GIF retained frame size overflowed".to_owned())?;
        if retained_bytes > max_animation_bytes {
            return Err(format!(
                "GIF frames need more than {max_animation_bytes} retained RGBA bytes"
            ));
        }
    }
    if frame_count == 0 {
        return Err("GIF has no frames".to_owned());
    }
    Ok((loop_mode, frame_count))
}

/// Composites GIF frames with image's existing disposal/transparency logic.
fn decode_gif_frames(
    bytes: Vec<u8>,
    cancel: &AtomicBool,
    path: &Path,
    expected_frames: usize,
    max_frames: usize,
    max_animation_bytes: u64,
    mut after_frame: impl FnMut(usize),
) -> Result<Vec<MediaFrame>, String> {
    let mut decoder = GifDecoder::new(Cursor::new(bytes))
        .map_err(|err| format!("cannot decode {}: {err}", path.display()))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    limits.max_alloc = Some(MAX_DECODED_BYTES);
    decoder
        .set_limits(limits)
        .map_err(|err| format!("cannot decode {}: {err}", path.display()))?;

    let dimensions = decoder.dimensions();
    let screen_bytes = u64::from(dimensions.0)
        .checked_mul(u64::from(dimensions.1))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| format!("{}: GIF frame size overflowed", path.display()))?;
    let mut frames = Vec::with_capacity(expected_frames.min(64));
    let mut retained_bytes = 0u64;

    let mut frames_iter = decoder.into_frames();
    loop {
        if cancelled(cancel) {
            return Err(format!("{}: cancelled", path.display()));
        }
        let Some(result) = frames_iter.next() else {
            break;
        };
        let frame = result.map_err(|err| format!("cannot decode {}: {err}", path.display()))?;
        if frame.buffer().dimensions() != dimensions {
            return Err(format!(
                "{}: GIF frame size differs from its logical screen",
                path.display()
            ));
        }
        let delay = Duration::from(frame.delay());
        let delay = if delay.is_zero() {
            MIN_FRAME_DELAY
        } else {
            delay
        };
        retained_bytes = retained_bytes
            .checked_add(screen_bytes)
            .ok_or_else(|| format!("{}: GIF retained frame size overflowed", path.display()))?;
        if retained_bytes > max_animation_bytes {
            return Err(format!(
                "{}: GIF frames need more than {max_animation_bytes} retained RGBA bytes",
                path.display()
            ));
        }
        if frames.len() >= max_frames {
            return Err(format!(
                "{}: GIF has more than {max_frames} frames",
                path.display()
            ));
        }
        if cancelled(cancel) {
            return Err(format!("{}: cancelled", path.display()));
        }
        frames.push(MediaFrame {
            pixels: Arc::new(frame.into_buffer()),
            delay,
        });
        after_frame(frames.len());
    }

    if frames.is_empty() {
        return Err(format!("{}: GIF has no frames", path.display()));
    }
    if frames.len() != expected_frames {
        return Err(format!(
            "{}: GIF frame count changed while decoding ({} != {expected_frames})",
            path.display(),
            frames.len()
        ));
    }
    Ok(frames)
}

/// Rejects WebP data outside the declared RIFF extent or with unsafe nested chunks.
///
/// `image-webp` records chunk ranges before reading their payloads and scans
/// slightly past the declared RIFF size; its EXIF accessor later allocates the
/// recorded range. Every chunk, including ANMF frame chunks, must therefore be
/// contained in the declared data before decoder construction.
fn validate_webp_chunks(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return Ok(());
    }

    let declared = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let riff_end = 8usize
        .checked_add(declared)
        .ok_or_else(|| format!("WebP RIFF declares {declared} bytes"))?;
    if riff_end != bytes.len() {
        return Err(if riff_end < bytes.len() {
            format!(
                "WebP has {} trailing bytes after the RIFF data",
                bytes.len() - riff_end
            )
        } else {
            format!(
                "WebP RIFF declares {declared} bytes but the file has {}",
                bytes.len().saturating_sub(8)
            )
        });
    }

    walk_chunks(bytes, 12, riff_end, |fourcc, payload| {
        if &fourcc == b"ANMF" {
            validate_anmf_frame(bytes, payload).map_err(|err| format!("ANMF: {err}"))?;
        }
        Ok(())
    })
}

/// Walks `start..end`, requiring every chunk and padding to fit exactly.
fn walk_chunks(
    bytes: &[u8],
    mut offset: usize,
    end: usize,
    mut visit: impl FnMut([u8; 4], std::ops::Range<usize>) -> Result<(), String>,
) -> Result<(), String> {
    while offset < end {
        let header_end = offset
            .checked_add(8)
            .filter(|header_end| *header_end <= end)
            .ok_or_else(|| format!("chunk header at offset {offset} runs past the chunk data"))?;
        let fourcc: [u8; 4] = bytes[offset..offset + 4].try_into().unwrap();
        let size = u32::from_le_bytes(bytes[offset + 4..header_end].try_into().unwrap()) as usize;
        let payload_end = header_end
            .checked_add(size)
            .filter(|payload_end| *payload_end <= end)
            .ok_or_else(|| {
                format!("chunk at offset {offset} declares {size} bytes beyond the chunk data")
            })?;
        visit(fourcc, header_end..payload_end)?;
        offset = payload_end
            .checked_add(size & 1)
            .filter(|offset| *offset <= end)
            .ok_or_else(|| format!("chunk at offset {offset} is missing its padding byte"))?;
    }
    Ok(())
}

/// ANMF starts with a 16-byte frame header; its payload may contain either one
/// VP8L image chunk, or an optional ALPH chunk followed by one VP8 chunk.
fn validate_anmf_frame(bytes: &[u8], payload: std::ops::Range<usize>) -> Result<(), String> {
    const ANMF_HEADER: usize = 16;
    if payload.len() < ANMF_HEADER + 8 {
        return Err("frame payload is too short".to_owned());
    }

    let mut saw_alpha = false;
    let mut saw_image = false;
    walk_chunks(
        bytes,
        payload.start + ANMF_HEADER,
        payload.end,
        |fourcc, _| {
            match &fourcc {
                b"ALPH" => {
                    if saw_alpha || saw_image {
                        return Err("ALPH chunk is out of order".to_owned());
                    }
                    saw_alpha = true;
                }
                b"VP8 " => {
                    if saw_image {
                        return Err("more than one image chunk".to_owned());
                    }
                    saw_image = true;
                }
                b"VP8L" => {
                    if saw_image {
                        return Err("more than one image chunk".to_owned());
                    }
                    if saw_alpha {
                        return Err("ALPH chunk is only valid with VP8".to_owned());
                    }
                    saw_image = true;
                }
                _ => {
                    return Err(format!(
                        "unsupported frame chunk {}",
                        String::from_utf8_lossy(&fourcc)
                    ));
                }
            }
            Ok(())
        },
    )?;

    if !saw_image {
        return Err("missing VP8/VP8L image chunk".to_owned());
    }
    Ok(())
}

/// Supported files directly beside `path`, including `path` itself.
///
/// Order is deterministic (path order) and names are kept exactly as stored on
/// disk, so non-UTF-8 file names round-trip.
pub fn sibling_images(path: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let entries =
        std::fs::read_dir(dir).map_err(|err| format!("cannot read {}: {err}", dir.display()))?;

    let mut images: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|candidate| candidate.is_file() && supported_path(candidate))
        .collect();
    images.sort();
    Ok(images)
}

/// Rotates RGBA pixels by 90 degrees in the requested direction.
pub fn rotate_image(pixels: &RgbaImage, clockwise: bool) -> RgbaImage {
    if clockwise {
        image::imageops::rotate90(pixels)
    } else {
        image::imageops::rotate270(pixels)
    }
}

/// Reports whether the path has a supported image extension (case-insensitive).
pub fn supported_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// The dimensions after an EXIF transform swaps the axes.
fn oriented_dimensions((width, height): (u32, u32), orientation: Orientation) -> (u32, u32) {
    match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (height, width),
        _ => (width, height),
    }
}

/// Rejects empty images and images whose RGBA buffer would blow the budget.
fn validate_dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("image has zero width or height".to_owned());
    }
    if width > MAX_SOURCE_SIDE || height > MAX_SOURCE_SIDE {
        return Err(format!(
            "image is {width}x{height}, maximum source side is {MAX_SOURCE_SIDE}"
        ));
    }
    let rgba_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4));
    match rgba_bytes {
        Some(bytes) if bytes <= MAX_DECODED_BYTES => Ok(()),
        _ => Err(format!(
            "{width}x{height} would decode to more than {MAX_DECODED_BYTES} bytes"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba};
    use std::borrow::Cow;
    use std::fs;

    const GIF_PALETTE: [u8; 12] = [
        0, 0, 0, // transparent slot
        255, 0, 0, // red
        0, 0, 255, // blue
        0, 255, 0, // green
    ];

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("oxeye-test-{}-{unique}", std::process::id()));
            fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sample_pixels() -> RgbaImage {
        RgbaImage::from_fn(3, 2, |x, y| Rgba([x as u8 * 10, y as u8 * 10, 0, 255]))
    }

    fn write_png(dir: &Path, name: &str, pixels: &RgbaImage) -> PathBuf {
        let path = dir.join(name);
        pixels.save(&path).expect("write png fixture");
        path
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// A valid PNG header with no pixel data, for checking early bounds.
    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&ihdr);
        let mut ihdr_with_type = b"IHDR".to_vec();
        ihdr_with_type.extend_from_slice(&ihdr);
        png.extend_from_slice(&crc32(&ihdr_with_type).to_be_bytes());
        let idat = [0x78u8, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];
        png.extend_from_slice(&(idat.len() as u32).to_be_bytes());
        png.extend_from_slice(b"IDAT");
        png.extend_from_slice(&idat);
        let mut idat_with_type = b"IDAT".to_vec();
        idat_with_type.extend_from_slice(&idat);
        png.extend_from_slice(&crc32(&idat_with_type).to_be_bytes());
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IEND");
        png.extend_from_slice(&crc32(b"IEND").to_be_bytes());
        png
    }

    /// Adds an EXIF APP1 segment with an orientation tag after the JPEG SOI.
    fn with_exif_orientation(mut jpeg: Vec<u8>, orientation: u16) -> Vec<u8> {
        let mut segment = vec![0xff, 0xe1];
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend_from_slice(&exif_tiff(orientation));
        segment.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        segment.extend_from_slice(&exif);
        jpeg.splice(2..2, segment);
        jpeg
    }

    /// A little-endian TIFF header with a single orientation entry.
    fn exif_tiff(orientation: u16) -> Vec<u8> {
        let mut exif = vec![0x49, 0x49, 0x2a, 0x00, 0x08, 0x00, 0x00, 0x00];
        exif.extend_from_slice(&[0x01, 0x00]);
        exif.extend_from_slice(&[0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00]);
        exif.extend_from_slice(&orientation.to_le_bytes());
        exif.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        exif
    }

    fn riff_chunk(fourcc: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut chunk = Vec::with_capacity(8 + data.len() + (data.len() & 1));
        chunk.extend_from_slice(fourcc);
        chunk.extend_from_slice(&(data.len() as u32).to_le_bytes());
        chunk.extend_from_slice(data);
        if data.len() & 1 != 0 {
            chunk.push(0);
        }
        chunk
    }

    fn riff_webp(chunks: &[Vec<u8>]) -> Vec<u8> {
        let body_len: usize = chunks.iter().map(Vec::len).sum();
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&((4 + body_len) as u32).to_le_bytes());
        webp.extend_from_slice(b"WEBP");
        for chunk in chunks {
            webp.extend_from_slice(chunk);
        }
        webp
    }

    /// Wraps a simple WebP with a VP8X header and an EXIF chunk.
    fn webp_with_exif(simple: &[u8], exif: &[u8], width: u32, height: u32) -> Vec<u8> {
        let mut vp8x = vec![0x08, 0, 0, 0];
        vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        riff_webp(&[
            riff_chunk(b"VP8X", &vp8x),
            riff_chunk(b"EXIF", exif),
            simple[12..].to_vec(),
        ])
    }

    fn anmf_frame_header(width: u32, height: u32) -> Vec<u8> {
        let mut header = vec![0, 0, 0, 0, 0, 0];
        header.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        header.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        header.extend_from_slice(&[0, 0, 0, 2]);
        header
    }

    fn animated_webp(frame_chunks: &[Vec<u8>], width: u32, height: u32) -> Vec<u8> {
        let mut vp8x = vec![0x02, 0, 0, 0];
        vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        let mut frame = anmf_frame_header(width, height);
        for chunk in frame_chunks {
            frame.extend_from_slice(chunk);
        }
        riff_webp(&[
            riff_chunk(b"VP8X", &vp8x),
            riff_chunk(b"ANIM", &[0, 0, 0, 0, 0, 0]),
            riff_chunk(b"ANMF", &frame),
        ])
    }

    fn write_rgb_webp(path: &Path, width: u32, height: u32) -> Vec<u8> {
        let mut pixels = RgbImage::from_pixel(width, height, Rgb([0, 0, 255]));
        for y in 0..height {
            for x in 0..width / 2 {
                pixels.put_pixel(x, y, Rgb([255, 0, 0]));
            }
        }
        DynamicImage::ImageRgb8(pixels)
            .save_with_format(path, image::ImageFormat::WebP)
            .unwrap();
        fs::read(path).unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn gif_frame(
        width: u16,
        height: u16,
        left: u16,
        top: u16,
        dispose: gif::DisposalMethod,
        transparent: Option<u8>,
        indices: Vec<u8>,
        delay: u16,
    ) -> gif::Frame<'static> {
        gif::Frame {
            width,
            height,
            left,
            top,
            dispose,
            transparent,
            delay,
            buffer: Cow::Owned(indices),
            ..Default::default()
        }
    }

    fn encode_gif(
        width: u16,
        height: u16,
        repeat: Option<gif::Repeat>,
        frames: &[gif::Frame<'_>],
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = gif::Encoder::new(&mut bytes, width, height, &GIF_PALETTE).unwrap();
            if let Some(repeat) = repeat {
                encoder.set_repeat(repeat).unwrap();
            }
            for frame in frames {
                encoder.write_frame(frame).unwrap();
            }
        }
        bytes
    }

    fn gif_path(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn loads_valid_png_as_absolute_path() {
        let dir = TempDir::new();
        let pixels = sample_pixels();
        let path = write_png(dir.path(), "valid.png", &pixels);

        let loaded = load_image(&path).expect("load valid png");

        assert!(loaded.path.is_absolute());
        assert_eq!(loaded.path, std::path::absolute(&path).unwrap());
        assert_eq!(loaded.pixels.dimensions(), pixels.dimensions());
        assert_eq!(loaded.pixels, pixels);
    }

    #[test]
    fn rejects_jpeg_over_encoded_limit_before_decoding() {
        let dir = TempDir::new();
        let path = dir.path().join("large.jpg");
        DynamicImage::ImageRgba8(sample_pixels())
            .save_with_format(&path, image::ImageFormat::Jpeg)
            .unwrap();
        let encoded_len = fs::metadata(&path).unwrap().len();

        let error = load_image_bounded(&path, encoded_len - 1)
            .expect_err("encoded limit must reject before decoding");

        assert!(
            error.contains("encoded file is larger than"),
            "unexpected error: {error}"
        );
        assert!(load_image(&path).is_ok(), "normal limit should still load");
    }

    #[test]
    fn rejects_forged_webp_exif_chunk_size() {
        let dir = TempDir::new();
        let mut forged = b"RIFF".to_vec();
        let mut body = b"WEBP".to_vec();
        let vp8x = vec![0x08, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        body.append(&mut riff_chunk(b"VP8X", &vp8x));
        body.extend_from_slice(b"EXIF");
        body.extend_from_slice(&0xffff_fff0u32.to_le_bytes());
        body.extend_from_slice(&[0x49, 0x49, 0x2a, 0x00]);
        forged.extend_from_slice(&(body.len() as u32).to_le_bytes());
        forged.extend_from_slice(&body);
        let path = dir.path().join("forged.webp");
        fs::write(&path, forged).unwrap();

        let error = load_image(&path).expect_err("forged EXIF length must reject");

        assert!(
            error.contains("declares") && error.contains("beyond the chunk data"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_webp_trailing_forged_exif_chunk() {
        let dir = TempDir::new();
        let path = dir.path().join("trailing.webp");
        let mut bytes = write_rgb_webp(&path, 2, 2);
        bytes.extend_from_slice(b"EXIF");
        bytes.extend_from_slice(&0xffff_fff0u32.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        fs::write(&path, bytes).unwrap();

        let error = load_image(&path).expect_err("trailing EXIF must reject");

        assert!(
            error.contains("trailing bytes after the RIFF data"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_webp_nested_anmf_forged_exif_chunk() {
        let dir = TempDir::new();
        let path = dir.path().join("nested.webp");
        let mut forged = b"EXIF".to_vec();
        forged.extend_from_slice(&0xffff_fff0u32.to_le_bytes());
        forged.extend_from_slice(&[0, 0, 0, 0]);
        fs::write(&path, animated_webp(&[forged], 2, 2)).unwrap();

        let error = load_image(&path).expect_err("nested EXIF must reject");

        assert!(
            error.contains("ANMF:") && error.contains("beyond the chunk data"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn loads_plain_webp() {
        let dir = TempDir::new();
        let path = dir.path().join("plain.webp");
        write_rgb_webp(&path, 4, 2);

        let loaded = load_image(&path).expect("load plain webp");

        assert_eq!(loaded.pixels.dimensions(), (4, 2));
        assert_eq!(*loaded.pixels.get_pixel(0, 0), Rgba([255u8, 0, 0, 255]));
        assert_eq!(*loaded.pixels.get_pixel(3, 0), Rgba([0u8, 0, 255, 255]));
    }

    #[test]
    fn loads_animated_webp_first_frame() {
        let dir = TempDir::new();
        let path = dir.path().join("animated.webp");
        let simple = write_rgb_webp(&path, 16, 8);
        assert_eq!(&simple[12..16], b"VP8L");
        fs::write(&path, animated_webp(&[simple[12..].to_vec()], 16, 8)).unwrap();

        let loaded = load_image(&path).expect("load animated webp");

        assert_eq!(loaded.pixels.dimensions(), (16, 8));
        assert_eq!(*loaded.pixels.get_pixel(2, 2), Rgba([255u8, 0, 0, 255]));
        assert_eq!(*loaded.pixels.get_pixel(12, 2), Rgba([0u8, 0, 255, 255]));
    }

    fn full_red_frame() -> gif::Frame<'static> {
        gif_frame(
            4,
            2,
            0,
            0,
            gif::DisposalMethod::Keep,
            None,
            vec![1; 4 * 2],
            10,
        )
    }

    #[test]
    fn static_media_is_one_zero_delay_once_frame() {
        let dir = TempDir::new();
        let pixels = sample_pixels();
        let path = write_png(dir.path(), "static.png", &pixels);

        let media = load_media(&path, &AtomicBool::new(false)).expect("load static media");

        assert_eq!(media.path, std::path::absolute(&path).unwrap());
        assert_eq!(media.loop_mode, LoopMode::Once);
        assert_eq!(media.frames.len(), 1);
        assert_eq!(media.frames[0].delay, Duration::ZERO);
        assert_eq!(media.frames[0].pixels.as_ref(), &pixels);
        assert!(
            load_media(&path, &AtomicBool::new(true))
                .unwrap_err()
                .contains("cancelled")
        );
    }

    #[test]
    fn gif_still_decodes_only_first_frame_while_animated_keeps_all() {
        let dir = TempDir::new();
        let frames: Vec<_> = (0..42)
            .map(|index| {
                gif_frame(
                    4,
                    2,
                    0,
                    0,
                    gif::DisposalMethod::Keep,
                    None,
                    vec![(index % 2) + 1; 4 * 2],
                    10,
                )
            })
            .collect();
        let path = gif_path(
            dir.path(),
            "forty-two.gif",
            &encode_gif(4, 2, None, &frames),
        );
        let cancel = AtomicBool::new(false);

        let still = load_media_for(&path, &cancel, DecodePurpose::Still).unwrap();
        assert_eq!(still.frames.len(), 1);
        assert_eq!(still.frames[0].delay, Duration::ZERO);
        assert_eq!(still.loop_mode, LoopMode::Once);

        let animated = load_media_for(&path, &cancel, DecodePurpose::Animated).unwrap();
        assert_eq!(animated.frames.len(), 42);
        assert_eq!(still.frames[0].pixels, animated.frames[0].pixels);
    }

    #[test]
    fn gif_still_skips_the_animation_frame_preflight() {
        let dir = TempDir::new();
        let frames: Vec<_> = (0..=MAX_GIF_FRAMES).map(|_| full_red_frame()).collect();
        let path = gif_path(
            dir.path(),
            "still-too-many-frames.gif",
            &encode_gif(4, 2, None, &frames),
        );
        let cancel = AtomicBool::new(false);

        let error = load_media_for(&path, &cancel, DecodePurpose::Animated).unwrap_err();
        assert!(error.contains("more than 2048 frames"), "{error}");
        let still = load_media_for(&path, &cancel, DecodePurpose::Still).unwrap();
        assert_eq!(still.frames.len(), 1);
        assert_eq!(still.frames[0].delay, Duration::ZERO);
    }

    #[test]
    fn still_media_is_one_zero_delay_once_frame_and_cancellable() {
        let dir = TempDir::new();
        let pixels = sample_pixels();
        let path = write_png(dir.path(), "still.png", &pixels);

        let still = load_media_for(&path, &AtomicBool::new(false), DecodePurpose::Still).unwrap();
        assert_eq!(still.frames.len(), 1);
        assert_eq!(still.frames[0].delay, Duration::ZERO);
        assert_eq!(still.loop_mode, LoopMode::Once);
        assert_eq!(still.frames[0].pixels.as_ref(), &pixels);
        assert!(
            load_media_for(&path, &AtomicBool::new(true), DecodePurpose::Still)
                .unwrap_err()
                .contains("cancelled")
        );
    }

    #[test]
    fn gif_still_composites_the_first_frame_onto_the_logical_screen() {
        let dir = TempDir::new();
        let partial = gif_frame(1, 1, 1, 0, gif::DisposalMethod::Keep, None, vec![2], 10);
        let path = gif_path(
            dir.path(),
            "partial-first.gif",
            &encode_gif(4, 2, None, &[partial]),
        );

        let still = load_media_for(&path, &AtomicBool::new(false), DecodePurpose::Still).unwrap();
        let pixels = &still.frames[0].pixels;
        assert_eq!(pixels.dimensions(), (4, 2));
        assert_eq!(*pixels.get_pixel(0, 0), Rgba([0, 0, 0, 0]));
        assert_eq!(*pixels.get_pixel(1, 0), Rgba([0, 0, 255, 255]));
    }

    #[test]
    fn still_animated_webp_decodes_first_frame_only() {
        let dir = TempDir::new();
        let path = dir.path().join("animated.webp");
        let simple = write_rgb_webp(&path, 16, 8);
        fs::write(&path, animated_webp(&[simple[12..].to_vec()], 16, 8)).unwrap();

        let still = load_media_for(&path, &AtomicBool::new(false), DecodePurpose::Still).unwrap();
        assert_eq!(still.frames.len(), 1);
        assert_eq!(still.frames[0].delay, Duration::ZERO);
        assert_eq!(still.frames[0].pixels.dimensions(), (16, 8));
        assert_eq!(
            *still.frames[0].pixels.get_pixel(2, 2),
            Rgba([255u8, 0, 0, 255])
        );
    }

    #[test]
    fn media_dispatch_uses_content_not_extension() {
        let dir = TempDir::new();

        let gif_frames = [
            full_red_frame(),
            gif_frame(1, 1, 0, 0, gif::DisposalMethod::Keep, None, vec![2], 10),
        ];
        let gif_bytes = encode_gif(4, 2, None, &gif_frames);
        let gif_named_png = gif_path(dir.path(), "animation.png", &gif_bytes);
        let media = load_media(&gif_named_png, &AtomicBool::new(false)).unwrap();
        assert_eq!(media.frames.len(), 2);
        assert_ne!(
            media.frames[0].pixels.as_ref(),
            media.frames[1].pixels.as_ref()
        );
        let static_first = load_image(&gif_named_png).unwrap();
        assert_eq!(static_first.pixels.dimensions(), (4, 2));

        let png_pixels = sample_pixels();
        let png_source = dir.path().join("source.png");
        png_pixels.save(&png_source).unwrap();
        let png_named_gif = dir.path().join("photo.gif");
        fs::write(&png_named_gif, fs::read(&png_source).unwrap()).unwrap();
        let media = load_media(&png_named_gif, &AtomicBool::new(false)).unwrap();
        assert_eq!(media.frames.len(), 1);
        assert_eq!(media.frames[0].delay, Duration::ZERO);
        assert_eq!(media.loop_mode, LoopMode::Once);
        assert_eq!(media.frames[0].pixels.as_ref(), &png_pixels);
        let loaded = load_image(&png_named_gif).unwrap();
        assert_eq!(loaded.pixels, png_pixels);
    }

    #[test]
    fn gif_cancellation_between_decoded_frames_is_observed() {
        let frames = [
            full_red_frame(),
            gif_frame(1, 1, 0, 0, gif::DisposalMethod::Keep, None, vec![2], 10),
            gif_frame(1, 1, 0, 0, gif::DisposalMethod::Keep, None, vec![3], 10),
        ];
        let bytes = encode_gif(4, 2, None, &frames);
        let cancel = AtomicBool::new(false);

        let error = decode_gif_frames(
            bytes,
            &cancel,
            Path::new("hook.gif"),
            frames.len(),
            MAX_GIF_FRAMES,
            MAX_ANIMATION_BYTES,
            |decoded| {
                if decoded == 1 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        )
        .unwrap_err();

        assert!(error.contains("cancelled"), "{error}");
    }

    #[test]
    fn gif_malformed_loop_metadata_is_rejected() {
        let dir = TempDir::new();
        let mut bytes = encode_gif(4, 2, Some(gif::Repeat::Infinite), &[full_red_frame()]);
        let signature = bytes
            .windows(11)
            .position(|window| window == b"NETSCAPE2.0")
            .expect("encoded loop extension");
        let sub_block_len = signature + 11;
        assert_eq!(bytes[sub_block_len], 3);
        bytes[sub_block_len] = 0xff;
        bytes.truncate(sub_block_len + 1);
        let path = gif_path(dir.path(), "malformed-loop.gif", &bytes);

        let error = load_media(&path, &AtomicBool::new(false)).unwrap_err();

        assert!(error.contains("GIF"), "unexpected error: {error}");
    }

    #[test]
    fn gif_loop_modes_and_zero_delay() {
        let dir = TempDir::new();
        let cancel = AtomicBool::new(false);
        let frame = full_red_frame();

        let once = gif_path(
            dir.path(),
            "once.gif",
            &encode_gif(
                4,
                2,
                None,
                &[{
                    let mut frame = full_red_frame();
                    frame.delay = 0;
                    frame
                }],
            ),
        );
        let media = load_media(&once, &cancel).unwrap();
        assert_eq!(media.loop_mode, LoopMode::Once);
        assert_eq!(media.frames.len(), 1);
        assert_eq!(media.frames[0].delay, MIN_FRAME_DELAY);

        let finite = gif_path(
            dir.path(),
            "finite.gif",
            &encode_gif(
                4,
                2,
                Some(gif::Repeat::Finite(2)),
                std::slice::from_ref(&frame),
            ),
        );
        let media = load_media(&finite, &cancel).unwrap();
        assert_eq!(media.loop_mode, LoopMode::Finite(2));
        assert_eq!(media.frames[0].delay, Duration::from_millis(100));

        let infinite = gif_path(
            dir.path(),
            "infinite.gif",
            &encode_gif(4, 2, Some(gif::Repeat::Infinite), &[frame]),
        );
        let media = load_media(&infinite, &cancel).unwrap();
        assert_eq!(media.loop_mode, LoopMode::Infinite);
    }

    #[test]
    fn gif_frame_cap_matches_production_limit() {
        let dir = TempDir::new();
        let frames: Vec<_> = (0..=MAX_GIF_FRAMES).map(|_| full_red_frame()).collect();
        let path = gif_path(
            dir.path(),
            "too-many-frames.gif",
            &encode_gif(4, 2, None, &frames),
        );

        let error = load_media(&path, &AtomicBool::new(false)).unwrap_err();

        assert!(error.contains("more than 2048 frames"), "{error}");
    }

    #[test]
    fn gif_keeps_transparent_partial_pixels() {
        let dir = TempDir::new();
        let frames = [
            full_red_frame(),
            gif_frame(
                2,
                1,
                0,
                0,
                gif::DisposalMethod::Keep,
                Some(0),
                vec![0, 2],
                10,
            ),
        ];
        let path = gif_path(
            dir.path(),
            "transparent.gif",
            &encode_gif(4, 2, None, &frames),
        );

        let media = load_media(&path, &AtomicBool::new(false)).unwrap();
        let frame = &media.frames[1];

        assert_eq!(frame.pixels.dimensions(), (4, 2));
        assert_eq!(*frame.pixels.get_pixel(0, 0), Rgba([255u8, 0, 0, 255]));
        assert_eq!(*frame.pixels.get_pixel(1, 0), Rgba([0u8, 0, 255, 255]));
        assert_eq!(*frame.pixels.get_pixel(2, 0), Rgba([255u8, 0, 0, 255]));
    }

    #[test]
    fn gif_background_disposal_clears_previous_pixels() {
        let dir = TempDir::new();
        let frames = [
            full_red_frame(),
            gif_frame(
                1,
                1,
                0,
                0,
                gif::DisposalMethod::Background,
                None,
                vec![2],
                10,
            ),
            gif_frame(1, 1, 1, 0, gif::DisposalMethod::Keep, None, vec![3], 10),
        ];
        let path = gif_path(
            dir.path(),
            "background.gif",
            &encode_gif(4, 2, None, &frames),
        );

        let media = load_media(&path, &AtomicBool::new(false)).unwrap();
        let frame = &media.frames[2];

        assert_eq!(*frame.pixels.get_pixel(0, 0), Rgba([0u8, 0, 0, 0]));
        assert_eq!(*frame.pixels.get_pixel(1, 0), Rgba([0u8, 255, 0, 255]));
    }

    #[test]
    fn gif_previous_disposal_restores_earlier_pixels() {
        let dir = TempDir::new();
        let frames = [
            full_red_frame(),
            gif_frame(1, 1, 0, 0, gif::DisposalMethod::Previous, None, vec![2], 10),
            gif_frame(1, 1, 1, 0, gif::DisposalMethod::Keep, None, vec![3], 10),
        ];
        let path = gif_path(dir.path(), "previous.gif", &encode_gif(4, 2, None, &frames));

        let media = load_media(&path, &AtomicBool::new(false)).unwrap();
        let frame = &media.frames[2];

        assert_eq!(*frame.pixels.get_pixel(0, 0), Rgba([255u8, 0, 0, 255]));
        assert_eq!(*frame.pixels.get_pixel(1, 0), Rgba([0u8, 255, 0, 255]));
    }

    #[test]
    fn gif_budgets_cancel_and_truncation_reject_whole_file() {
        let dir = TempDir::new();
        let mut frames = vec![full_red_frame()];
        for _ in 0..3 {
            frames.push(gif_frame(
                4,
                2,
                0,
                0,
                gif::DisposalMethod::Keep,
                None,
                vec![2; 4 * 2],
                5,
            ));
        }
        let bytes = encode_gif(4, 2, None, &frames);
        let path = gif_path(dir.path(), "budget.gif", &bytes);
        let cancel = AtomicBool::new(false);
        let screen_bytes = 4u64 * 2 * 4;

        let error =
            load_media_with_limits(&path, &cancel, MAX_ENCODED_BYTES, 3, MAX_ANIMATION_BYTES)
                .unwrap_err();
        assert!(error.contains("more than 3 frames"), "{error}");

        let error = load_media_with_limits(
            &path,
            &cancel,
            MAX_ENCODED_BYTES,
            MAX_GIF_FRAMES,
            screen_bytes * 3 - 1,
        )
        .unwrap_err();
        assert!(error.contains("retained RGBA"), "{error}");

        let error = load_media_with_limits(
            &path,
            &cancel,
            bytes.len() as u64 - 1,
            MAX_GIF_FRAMES,
            MAX_ANIMATION_BYTES,
        )
        .unwrap_err();
        assert!(error.contains("encoded file is larger than"), "{error}");

        let cancelled = AtomicBool::new(true);
        let error = load_media(&path, &cancelled).unwrap_err();
        assert!(error.contains("cancelled"), "{error}");

        let truncated = gif_path(dir.path(), "truncated.gif", &bytes[..bytes.len() - 12]);
        assert!(load_media(&truncated, &cancel).is_err());
    }

    #[test]
    fn rejects_truncated_webp_riff_size() {
        let dir = TempDir::new();
        let mut forged = b"RIFF".to_vec();
        forged.extend_from_slice(&1024u32.to_le_bytes());
        forged.extend_from_slice(b"WEBP");
        let path = dir.path().join("truncated.webp");
        fs::write(&path, forged).unwrap();

        let error = load_image(&path).expect_err("truncated RIFF must reject");

        assert!(
            error.contains("RIFF declares 1024 bytes"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn applies_webp_exif_orientation() {
        let dir = TempDir::new();
        let mut pixels = RgbaImage::from_pixel(16, 8, Rgba([0, 0, 255, 255]));
        for y in 0..8 {
            for x in 0..8 {
                pixels.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            }
        }
        let path = dir.path().join("oriented.webp");
        DynamicImage::ImageRgba8(pixels)
            .save_with_format(&path, image::ImageFormat::WebP)
            .unwrap();
        let simple = fs::read(&path).unwrap();
        fs::write(&path, webp_with_exif(&simple, &exif_tiff(6), 16, 8)).unwrap();

        let loaded = load_image(&path).expect("load oriented webp");

        assert_eq!(loaded.pixels.dimensions(), (8, 16));
        let top = loaded.pixels.get_pixel(4, 4);
        let bottom = loaded.pixels.get_pixel(4, 12);
        assert_eq!(*top, Rgba([255u8, 0, 0, 255]));
        assert_eq!(*bottom, Rgba([0u8, 0, 255, 255]));
    }

    #[cfg(unix)]
    #[test]
    fn keeps_selected_symlink_path_and_its_folder() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new();
        let target_dir = TempDir::new();
        let target = target_dir.path().join("extensionless-target");
        RgbaImage::new(2, 2)
            .save_with_format(&target, image::ImageFormat::Png)
            .unwrap();
        let link = dir.path().join("photo.png");
        symlink(&target, &link).unwrap();
        fs::write(dir.path().join("other.jpg"), b"not decoded here").unwrap();
        fs::write(dir.path().join("notes.txt"), b"ignored").unwrap();

        let loaded = load_image(&link).expect("load through symlink");

        assert_eq!(loaded.path, std::path::absolute(&link).unwrap());
        assert_ne!(loaded.path, target.canonicalize().unwrap());
        assert_eq!(loaded.pixels.dimensions(), (2, 2));
        let siblings = sibling_images(&loaded.path).expect("list link folder");
        let names: Vec<String> = siblings
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["other.jpg", "photo.png"]);
    }

    #[test]
    fn rejects_corrupt_file_with_context() {
        let dir = TempDir::new();
        let path = dir.path().join("corrupt.png");
        fs::write(&path, b"this is definitely not a png").unwrap();

        let error = load_image(&path).expect_err("corrupt png must fail");

        assert!(error.contains("corrupt.png"), "unexpected error: {error}");
    }

    #[test]
    fn rejects_unsupported_extension() {
        let dir = TempDir::new();
        let path = dir.path().join("notes.txt");
        fs::write(&path, b"hello").unwrap();

        let error = load_image(&path).expect_err("txt must fail");

        assert!(
            error.contains("unsupported image format"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn accepts_image_at_source_side_limit() {
        let dir = TempDir::new();
        let path = dir.path().join("wide.png");
        RgbaImage::new(MAX_SOURCE_SIDE, 1).save(&path).unwrap();

        let loaded = load_image(&path).expect("65535-pixel side should load");

        assert_eq!(loaded.pixels.dimensions(), (MAX_SOURCE_SIDE, 1));
    }

    #[test]
    fn accepts_source_above_old_gpu_cap() {
        let dir = TempDir::new();
        let path = dir.path().join("large.png");
        RgbaImage::new(4_200, 2).save(&path).unwrap();

        let loaded = load_image(&path).expect("source over 4096 should load under budgets");
        assert_eq!(loaded.pixels.dimensions(), (4_200, 2));

        let media = load_media(&path, &AtomicBool::new(false)).expect("media path also accepts it");
        assert_eq!(media.loop_mode, LoopMode::Once);
        assert_eq!(media.frames.len(), 1);
        assert_eq!(media.frames[0].pixels.dimensions(), (4_200, 2));
    }

    #[test]
    fn rejects_oversized_images_before_decoding() {
        let dir = TempDir::new();
        let too_wide = dir.path().join("too-wide.png");
        fs::write(&too_wide, png_header(MAX_SOURCE_SIDE + 1, 1)).unwrap();
        let error = load_image(&too_wide).expect_err("width limit must reject");
        assert!(error.contains("too-wide.png"), "unexpected error: {error}");

        let too_high = dir.path().join("too-high.png");
        fs::write(&too_high, png_header(1, MAX_SOURCE_SIDE + 1)).unwrap();
        let error = load_image(&too_high).expect_err("height limit must reject");
        assert!(error.contains("too-high.png"), "unexpected error: {error}");

        let too_many_bytes = dir.path().join("too-many-bytes.png");
        fs::write(&too_many_bytes, png_header(MAX_SOURCE_SIDE, 1_025)).unwrap();
        let error = load_image(&too_many_bytes).expect_err("byte budget must reject");
        assert!(
            error.contains("would decode to more than"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn applies_exif_orientation() {
        let dir = TempDir::new();
        let mut pixels = RgbaImage::from_pixel(16, 8, Rgba([0, 0, 255, 255]));
        for y in 0..8 {
            for x in 0..8 {
                pixels.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            }
        }
        let path = dir.path().join("oriented.jpg");
        DynamicImage::ImageRgba8(pixels)
            .save_with_format(&path, image::ImageFormat::Jpeg)
            .unwrap();
        let jpeg = fs::read(&path).unwrap();
        fs::write(&path, with_exif_orientation(jpeg, 6)).unwrap();

        let loaded = load_image(&path).expect("load oriented jpeg");

        // Orientation 6 rotates clockwise, so the red left half becomes the top half.
        assert_eq!(loaded.pixels.dimensions(), (8, 16));
        let top = loaded.pixels.get_pixel(4, 4);
        let bottom = loaded.pixels.get_pixel(4, 12);
        assert!(top[0] > 180 && top[2] < 80, "top pixel was {top:?}");
        assert!(
            bottom[2] > 180 && bottom[0] < 80,
            "bottom pixel was {bottom:?}"
        );
    }

    #[test]
    fn enforces_source_dimension_bounds() {
        assert!(validate_dimensions(65_535, 1).is_ok());
        assert!(validate_dimensions(1, 65_535).is_ok());
        assert!(validate_dimensions(65_535, 1_024).is_ok());
        assert!(validate_dimensions(0, 2).is_err());
        assert!(validate_dimensions(2, 0).is_err());
        assert!(validate_dimensions(65_536, 1).is_err());
        assert!(validate_dimensions(1, 65_536).is_err());
        // Same side cap, but the checked RGBA budget is exceeded.
        assert!(validate_dimensions(65_535, 1_025).is_err());
        // u32::MAX squared must not wrap into an accepted value.
        assert!(validate_dimensions(u32::MAX, u32::MAX).is_err());
    }

    #[test]
    fn lists_supported_siblings_in_deterministic_order() {
        let dir = TempDir::new();
        for name in ["b.png", "A.JPG", "c.txt", "d.webp", "e.PnG"] {
            fs::write(dir.path().join(name), b"not decoded here").unwrap();
        }
        fs::create_dir(dir.path().join("nested.png")).unwrap();

        let siblings = sibling_images(&dir.path().join("b.png")).expect("list siblings");
        let names: Vec<String> = siblings
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert_eq!(names, ["A.JPG", "b.png", "d.webp", "e.PnG"]);
    }

    #[cfg(unix)]
    #[test]
    fn handles_non_utf8_sibling_names() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = TempDir::new();
        let name = OsStr::from_bytes(b"weird\xff.png");
        let path = dir.path().join(name);
        RgbaImage::new(1, 1).save(&path).unwrap();

        assert!(supported_path(&path));
        let siblings = sibling_images(&path).expect("list siblings");
        assert_eq!(siblings.len(), 1);
        assert_eq!(siblings[0].file_name().unwrap(), name);
        assert_eq!(load_image(&path).unwrap().pixels.dimensions(), (1, 1));
    }

    #[test]
    fn four_quarter_turns_are_identity() {
        let pixels = sample_pixels();

        let mut clockwise = pixels.clone();
        for _ in 0..4 {
            clockwise = rotate_image(&clockwise, true);
        }
        assert_eq!(clockwise, pixels);

        let mut counter_clockwise = pixels.clone();
        for _ in 0..4 {
            counter_clockwise = rotate_image(&counter_clockwise, false);
        }
        assert_eq!(counter_clockwise, pixels);
    }

    #[test]
    fn clockwise_rotation_moves_bottom_left_to_top_left() {
        let pixels = sample_pixels();
        let rotated = rotate_image(&pixels, true);

        assert_eq!(rotated.dimensions(), (2, 3));
        assert_eq!(rotated.get_pixel(0, 0), pixels.get_pixel(0, 1));
        assert_eq!(
            rotate_image(&rotated, false),
            pixels,
            "counter-clockwise must undo clockwise"
        );
    }

    #[test]
    fn supported_extensions_are_case_insensitive() {
        for name in [
            "a.png", "b.JPG", "c.Jpeg", "d.GIF", "e.WebP", "f.BMP", "g.ico", "h.TIF", "i.tiff",
        ] {
            assert!(
                supported_path(Path::new(name)),
                "{name} should be supported"
            );
        }
        for name in ["a.svg", "b.txt", "c.png.bak", "no-extension"] {
            assert!(
                !supported_path(Path::new(name)),
                "{name} should be unsupported"
            );
        }
    }

    #[test]
    #[ignore = "requires OXEYE_TEST_IMAGE_ROOT pointing at the local image directory"]
    fn real_input_images() {
        let Some(root) = std::env::var_os("OXEYE_TEST_IMAGE_ROOT") else {
            eprintln!("OXEYE_TEST_IMAGE_ROOT is not set; skipping real input test");
            return;
        };
        let root = PathBuf::from(root);
        assert!(root.is_dir(), "test root is not a directory: {root:?}");

        fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(dir).expect("read test directory") {
                let path = entry.expect("read directory entry").path();
                if path.is_dir() {
                    collect(&path, out);
                } else if path.is_file() && supported_path(&path) {
                    out.push(path);
                }
            }
        }

        let mut paths = Vec::new();
        collect(&root, &mut paths);
        paths.sort();
        assert!(!paths.is_empty(), "no supported images under the test root");

        let mut images = Vec::with_capacity(paths.len());
        for path in paths {
            let dimensions = image::image_dimensions(&path)
                .unwrap_or_else(|err| panic!("inspect {}: {err}", path.display()));
            images.push((path, dimensions));
        }

        let area = |(width, height): (u32, u32)| u64::from(width) * u64::from(height);
        let landscape = images
            .iter()
            .filter(|(_, (width, height))| width > height)
            .max_by_key(|(_, dimensions)| area(*dimensions))
            .expect("no landscape image");
        let mut portraits: Vec<_> = images
            .iter()
            .filter(|(_, (width, height))| height > width)
            .collect();
        portraits.sort_by_key(|(_, dimensions)| area(*dimensions));
        portraits.reverse();
        portraits.truncate(2);
        assert_eq!(portraits.len(), 2, "expected two representative portraits");
        assert!(
            landscape.1.0 > 4_096 || landscape.1.1 > 4_096,
            "largest landscape is not above the old GPU cap"
        );

        for (path, dimensions) in std::iter::once(landscape).chain(portraits) {
            let loaded =
                load_image(path).unwrap_or_else(|err| panic!("load {}: {err}", path.display()));
            assert_eq!(
                loaded.pixels.dimensions(),
                *dimensions,
                "{}",
                path.display()
            );
        }

        let gifs: Vec<_> = images.iter().filter(|(path, _)| is_gif(path)).collect();
        assert_eq!(gifs.len(), 2, "expected exactly two GIF fixtures");
        let cancel = AtomicBool::new(false);
        let mut frame_counts = Vec::new();
        let mut durations = Vec::new();
        for (path, dimensions) in gifs {
            let media = load_media(path, &cancel)
                .unwrap_or_else(|err| panic!("load {}: {err}", path.display()));
            assert_eq!(media.loop_mode, LoopMode::Infinite, "{}", path.display());
            assert!(!media.frames.is_empty());
            for frame in &media.frames {
                assert_eq!(frame.pixels.dimensions(), *dimensions, "{}", path.display());
            }
            assert!(
                media
                    .frames
                    .windows(2)
                    .any(|pair| pair[0].pixels.as_ref() != pair[1].pixels.as_ref()),
                "frames do not change for {}",
                path.display()
            );
            frame_counts.push(media.frames.len());
            durations.push(
                media
                    .frames
                    .iter()
                    .map(|frame| frame.delay.as_millis())
                    .sum::<u128>() as u64,
            );
        }
        frame_counts.sort_unstable();
        durations.sort_unstable();
        assert_eq!(frame_counts, [42, 43]);
        assert_eq!(durations, [3_260, 3_330]);
    }
}
