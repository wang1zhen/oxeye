//! Read-only virtual images. Archive names are labels, never filesystem paths.

use std::cmp::Ordering;
use std::fmt;
use std::fs::File;
#[cfg(test)]
use std::io::Cursor;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[cfg(test)]
use image::{ImageDecoder, ImageReader, Limits};
use zip::read::{ArchiveOffset, Config, OwnedZipFileEntry, ZipFileEntry};
use zip::unstable::format::{blocks::Zip32CentralDirectoryEnd, magic::Magic};
use zip::unstable::read::ZipIterable;

use crate::model;

const MAX_IMAGES: usize = 20_000;
const MAX_METADATA: u64 = 16 * 1024 * 1024;
// Mirror zip 9.7's ZipIterable::try_new, including the in-memory footer size.
const EOCD_SEARCH_WINDOW: usize =
    65_536 + size_of::<Zip32CentralDirectoryEnd>() + size_of::<Magic>();

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArchiveKind {
    Zip,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SourceRef {
    File(PathBuf),
    Archive {
        archive: PathBuf,
        archive_len: u64,
        archive_mtime_secs: u64,
        archive_mtime_nanos: u32,
        kind: ArchiveKind,
        index: usize,
        name: String,
        size: u64,
        crc: u32,
    },
}

impl SourceRef {
    /// Compatibility constructor; makes paths absolute without resolving links.
    pub fn file(path: &Path) -> Self {
        Self::File(std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::File(path) => path,
            Self::Archive { archive, .. } => archive,
        }
    }

    pub fn display_name(&self) -> String {
        match self {
            Self::File(path) => path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into(),
            Self::Archive { name, .. } => name.rsplit(['/', '\\']).next().unwrap_or(name).into(),
        }
    }

    pub fn archive_root(&self) -> Option<&Path> {
        match self {
            Self::File(_) => None,
            Self::Archive { archive, .. } => Some(archive),
        }
    }

    pub fn is_archive(&self) -> bool {
        matches!(self, Self::Archive { .. })
    }

    pub fn display(&self) -> impl fmt::Display + '_ {
        self
    }
}

impl fmt::Display for SourceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => write!(f, "{}", path.display()),
            Self::Archive {
                archive,
                index,
                name,
                ..
            } => {
                write!(f, "{} / {name} [#{index}]", archive.display())
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Catalog {
    pub items: Vec<SourceRef>,
}

pub fn catalog_for(path: &Path) -> Result<Catalog, String> {
    let path = std::path::absolute(path)
        .map_err(|err| format!("cannot open {}: {err}", path.display()))?;
    if !path.is_file() {
        return Err(format!("not a file: {}", path.display()));
    }
    let archive = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip") || ext.eq_ignore_ascii_case("cbz"));
    if !archive {
        if !model::supported_path(&path) {
            return Err(format!("unsupported image format: {}", path.display()));
        }
        return Ok(Catalog {
            items: model::sibling_images(&path)?
                .into_iter()
                .map(SourceRef::File)
                .collect(),
        });
    }
    let file = File::open(&path).map_err(|err| format!("cannot open {}: {err}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|err| format!("cannot read metadata of {}: {err}", path.display()))?;
    let before = (
        metadata.len(),
        metadata
            .modified()
            .map_err(|err| format!("cannot read metadata of {}: {err}", path.display()))?,
    );
    let (archive_len, archive_mtime_secs, archive_mtime_nanos) = archive_fingerprint(before);
    let mut zip = open_zip_reader(file)
        .map_err(|err| format!("cannot read ZIP {}: {err}", path.display()))?;
    let items = scan_zip(&mut zip, &path)?
        .into_iter()
        .map(|(index, entry)| SourceRef::Archive {
            archive: path.clone(),
            archive_len,
            archive_mtime_secs,
            archive_mtime_nanos,
            kind: ArchiveKind::Zip,
            index,
            name: entry_name(&entry),
            size: entry.size(),
            crc: entry.crc32(),
        })
        .collect::<Vec<_>>();
    if file_identity(&path) != Some(before) {
        return Err(format!("{}: archive changed while listing", path.display()));
    }
    let mut items = items;
    items.sort_by(|a, b| {
        let (SourceRef::Archive { name: a, .. }, SourceRef::Archive { name: b, .. }) = (a, b)
        else {
            unreachable!()
        };
        natural_cmp(a, b)
    });
    Ok(Catalog { items })
}

fn open_zip(path: &Path) -> Result<ZipIterable<File>, String> {
    let file = File::open(path).map_err(|err| format!("cannot open {}: {err}", path.display()))?;
    open_zip_reader(file).map_err(|err| format!("cannot read ZIP {}: {err}", path.display()))
}

fn open_zip_reader<R: Read + Seek>(mut reader: R) -> Result<ZipIterable<R>, String> {
    preflight_zip(&mut reader).map_err(|err| err.to_string())?;
    // shortcut: offsets must be absolute; add bounded prefix recovery if
    // self-extracting archives are needed, never search image payloads.
    ZipIterable::try_new(
        reader,
        Config {
            archive_offset: ArchiveOffset::Known(0),
        },
    )
    .map_err(|err| err.to_string())
}

fn read_at<const N: usize>(
    reader: &mut (impl Read + Seek),
    offset: u64,
) -> std::io::Result<[u8; N]> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut bytes = [0; N];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Check every footer candidate: zip may fall back past a later invalid EOCD.
fn preflight_zip(reader: &mut (impl Read + Seek)) -> std::io::Result<()> {
    let invalid = |message| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    let len = reader.seek(SeekFrom::End(0))?;
    let tail_len = len.min(EOCD_SEARCH_WINDOW as u64) as usize;
    let tail_start = len - tail_len as u64;
    let mut tail = vec![0; tail_len];
    reader.seek(SeekFrom::Start(tail_start))?;
    reader.read_exact(&mut tail)?;
    let mut found = false;
    for (index, magic) in tail.windows(4).enumerate().rev() {
        if magic != b"PK\x05\x06" || tail.len() - index < 22 {
            continue;
        }
        let eocd = &tail[index..index + 22];
        let comment_len = u16::from_le_bytes(eocd[20..22].try_into().unwrap()) as usize;
        if comment_len > tail.len() - index - 22 {
            continue;
        }
        found = true;
        let eocd_offset = tail_start + index as u64;
        let may_be_zip64 = u16::from_le_bytes(eocd[10..12].try_into().unwrap()) == u16::MAX
            || u32::from_le_bytes(eocd[12..16].try_into().unwrap()) == u32::MAX
            || u32::from_le_bytes(eocd[16..20].try_into().unwrap()) == u32::MAX;
        let locator = if let Some(offset) = eocd_offset.checked_sub(20) {
            Some((offset, read_at::<20>(reader, offset)?))
        } else {
            None
        };
        let (cd_offset, cd_size, entries) = if let Some((locator_offset, locator)) =
            locator.filter(|(_, bytes)| may_be_zip64 && &bytes[..4] == b"PK\x06\x07")
        {
            let offset = u64::from_le_bytes(locator[8..16].try_into().unwrap());
            let span = locator_offset
                .checked_sub(offset)
                .filter(|span| (56..=MAX_METADATA).contains(span))
                .ok_or_else(|| invalid("ZIP64 footer exceeds metadata limit or file bounds"))?;
            // Bound the full footer before zip allocates its extensible sector.
            let footer = read_at::<56>(reader, offset)?;
            let size = u64::from_le_bytes(footer[4..12].try_into().unwrap());
            if &footer[..4] != b"PK\x06\x06" || size.checked_add(12) != Some(span) {
                return Err(invalid("invalid ZIP64 footer size or offset"));
            }
            let cd_size = u64::from_le_bytes(footer[40..48].try_into().unwrap());
            let cd_offset = u64::from_le_bytes(footer[48..56].try_into().unwrap());
            if span
                .checked_add(cd_size)
                .is_none_or(|size| size > MAX_METADATA)
                || cd_offset
                    .checked_add(cd_size)
                    .is_none_or(|end| end > offset)
            {
                return Err(invalid("ZIP64 metadata exceeds limit or file bounds"));
            }
            (
                cd_offset,
                cd_size,
                u64::from_le_bytes(footer[32..40].try_into().unwrap()),
            )
        } else {
            let size = u64::from(u32::from_le_bytes(eocd[12..16].try_into().unwrap()));
            let offset = u64::from(u32::from_le_bytes(eocd[16..20].try_into().unwrap()));
            if size > MAX_METADATA || offset.checked_add(size).is_none_or(|end| end > eocd_offset) {
                return Err(invalid("ZIP metadata exceeds limit or file bounds"));
            }
            (
                offset,
                size,
                u64::from(u16::from_le_bytes(eocd[10..12].try_into().unwrap())),
            )
        };
        if entries == 0 && cd_size == 0 {
            continue;
        }
        if entries == 0 || cd_size < 46 || read_at::<4>(reader, cd_offset)? != *b"PK\x01\x02" {
            return Err(invalid(
                "invalid ZIP central-directory signature at declared offset",
            ));
        }
    }
    if !found {
        return Err(invalid("ZIP end-of-central-directory footer not found"));
    }
    Ok(())
}

fn is_file_entry(entry: &ZipFileEntry<'_>) -> bool {
    entry.is_file()
        && !entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o040000)
}

fn entry_name(entry: &ZipFileEntry<'_>) -> String {
    String::from_utf8_lossy(entry.name_raw()).into_owned()
}

/// ZipArchive deduplicates raw names, changing by_index identities. ZipIterable
/// preserves original indices and bounds metadata before allocating a catalog.
fn scan_zip(
    zip: &mut ZipIterable<File>,
    path: &Path,
) -> Result<Vec<(usize, ZipFileEntry<'static>)>, String> {
    let files = zip
        .files()
        .map_err(|err| format!("{}: {err}", path.display()))?;
    let mut images = Vec::new();
    let mut metadata = 0u64;
    let mut index = 0;
    while let Some(entry) = files.next() {
        let entry = entry
            .map_err(|err| format!("{}: invalid ZIP entry #{index}: {err}", path.display()))?;
        let end = files
            .reader_mut()
            .stream_position()
            .map_err(|err| format!("{}: {err}", path.display()))?;
        metadata = metadata.saturating_add(end.saturating_sub(entry.central_header_start()));
        if metadata > MAX_METADATA {
            return Err(format!(
                "{}: ZIP metadata exceeds {MAX_METADATA} bytes",
                path.display()
            ));
        }
        if entry.encrypted() {
            return Err(format!(
                "{}: encrypted archives are not supported",
                path.display()
            ));
        }
        if is_file_entry(&entry) && model::supported_path(Path::new(&entry_name(&entry))) {
            if images.len() >= MAX_IMAGES {
                return Err(format!(
                    "{}: ZIP has more than {MAX_IMAGES} image entries",
                    path.display()
                ));
            }
            images.push((index, entry));
        }
        index += 1;
    }
    Ok(images)
}

pub fn read_source_bytes(source: &SourceRef, cap: u64) -> Result<Vec<u8>, String> {
    match source {
        SourceRef::File(path) => {
            let file = File::open(path).map_err(|err| format!("cannot open {source}: {err}"))?;
            read_bounded(file, source, cap)
        }
        SourceRef::Archive {
            archive,
            index,
            size,
            ..
        } => {
            let before = file_identity(archive)
                .filter(|identity| matches_archive_fingerprint(source, *identity))
                .ok_or_else(|| {
                    format!("{source}: archive changed or unreadable; reopen the archive")
                })?;
            let mut zip = open_zip(archive)?;
            let entry = scan_zip(&mut zip, archive)?
                .into_iter()
                .find(|(i, _)| i == index)
                .map(|(_, entry)| entry)
                .ok_or_else(|| format!("{source}: not an image file entry"))?;
            verify_entry(source, &entry)?;
            let files = zip.files().map_err(|err| format!("{source}: {err}"))?;
            let bytes = read_zip_entry(entry, files.reader_mut(), source, cap)?;
            if bytes.len() as u64 != *size {
                return Err(format!(
                    "{source}: ZIP entry size does not match its metadata"
                ));
            }
            if file_identity(archive)
                .filter(|identity| matches_archive_fingerprint(source, *identity))
                != Some(before)
            {
                return Err(format!("{source}: archive changed while reading"));
            }
            Ok(bytes)
        }
    }
}

fn read_zip_entry(
    entry: ZipFileEntry<'_>,
    reader: &mut (impl Read + Seek),
    source: &SourceRef,
    cap: u64,
) -> Result<Vec<u8>, String> {
    let owned = OwnedZipFileEntry::from(entry);
    // Borrowed metadata prevents ZipFile::drop from draining unread input.
    let file = owned
        .as_ref()
        .with_reader(reader, Default::default())
        .map_err(|err| format!("cannot open {source}: {err}"))?;
    read_bounded(file, source, cap)
}

fn verify_entry(source: &SourceRef, entry: &ZipFileEntry<'_>) -> Result<(), String> {
    let SourceRef::Archive {
        name, size, crc, ..
    } = source
    else {
        unreachable!()
    };
    if !is_file_entry(entry) {
        return Err(format!(
            "{source}: directory or symlink entries are not supported"
        ));
    }
    if entry.encrypted() {
        return Err(format!("{source}: encrypted archives are not supported"));
    }
    if entry_name(entry) != *name || entry.size() != *size || entry.crc32() != *crc {
        return Err(format!(
            "{source}: archive entry changed; reopen the archive"
        ));
    }
    Ok(())
}

fn read_bounded(reader: impl Read, source: &SourceRef, cap: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|err| format!("cannot read {source}: {err}"))?;
    if bytes.len() as u64 > cap {
        return Err(format!("{source}: encoded file is larger than {cap} bytes"));
    }
    Ok(bytes)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceIdentity {
    pub len: u64,
    pub modified: SystemTime,
    pub entry: Option<SourceRef>,
}

fn file_identity(path: &Path) -> Option<(u64, SystemTime)> {
    // Stat first to avoid opening special files during background validation.
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let metadata = File::open(path).ok()?.metadata().ok()?;
    metadata
        .is_file()
        .then_some((metadata.len(), metadata.modified().ok()?))
}

fn archive_fingerprint((len, modified): (u64, SystemTime)) -> (u64, u64, u32) {
    let modified = modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    (len, modified.as_secs(), modified.subsec_nanos())
}

fn matches_archive_fingerprint(source: &SourceRef, identity: (u64, SystemTime)) -> bool {
    matches!(source, SourceRef::Archive {
        archive_len, archive_mtime_secs, archive_mtime_nanos, ..
    } if archive_fingerprint(identity) == (*archive_len, *archive_mtime_secs, *archive_mtime_nanos))
}

pub fn source_identity(source: &SourceRef) -> Option<SourceIdentity> {
    // shortcut: replacements preserving length and mtime need content hashing
    // if this ever becomes a real freshness problem.
    let (len, modified) = file_identity(source.path())?;
    if let SourceRef::Archive { archive, index, .. } = source {
        if !matches_archive_fingerprint(source, (len, modified)) {
            return None;
        }
        let mut zip = open_zip(archive).ok()?;
        let entry = scan_zip(&mut zip, archive)
            .ok()?
            .into_iter()
            .find(|(i, _)| i == index)?
            .1;
        verify_entry(source, &entry).ok()?;
        if file_identity(archive)? != (len, modified) {
            return None;
        }
    }
    Some(SourceIdentity {
        len,
        modified,
        entry: source.is_archive().then(|| source.clone()),
    })
}

// Retained for model's archive/EXIF regressions; production probing starts in C3.
#[cfg(test)]
pub fn probe_dimensions(source: &SourceRef, cap: u64) -> Result<(u32, u32), String> {
    let bytes = read_source_bytes(source, cap)?;
    model::validate_webp_chunks(&bytes).map_err(|err| format!("{source}: {err}"))?;
    let mut reader = ImageReader::new(Cursor::new(bytes));
    let mut limits = Limits::default();
    limits.max_image_width = Some(model::MAX_SOURCE_SIDE);
    limits.max_image_height = Some(model::MAX_SOURCE_SIDE);
    limits.max_alloc = Some(model::MAX_DECODED_BYTES);
    reader.limits(limits.clone());
    let reader = reader
        .with_guessed_format()
        .map_err(|err| format!("{source}: {err}"))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|err| format!("{source}: {err}"))?;
    decoder
        .set_limits(limits)
        .map_err(|err| format!("{source}: {err}"))?;
    let orientation = decoder
        .orientation()
        .map_err(|err| format!("{source}: {err}"))?;
    let dimensions = model::oriented_dimensions(decoder.dimensions(), orientation);
    model::validate_dimensions(dimensions.0, dimensions.1)
        .map_err(|err| format!("{source}: {err}"))?;
    Ok(dimensions)
}

/// Numeric runs compare without integer parsing; zero padding breaks equal runs.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut left, mut right) = (a.as_bytes(), b.as_bytes());
    while !left.is_empty() && !right.is_empty() {
        let order;
        if left[0].is_ascii_digit() && right[0].is_ascii_digit() {
            let l = left
                .iter()
                .position(|c| !c.is_ascii_digit())
                .unwrap_or(left.len());
            let r = right
                .iter()
                .position(|c| !c.is_ascii_digit())
                .unwrap_or(right.len());
            let ln = left[..l].iter().position(|c| *c != b'0').unwrap_or(l);
            let rn = right[..r].iter().position(|c| *c != b'0').unwrap_or(r);
            order = (l - ln)
                .cmp(&(r - rn))
                .then_with(|| left[ln..l].cmp(&right[rn..r]))
                .then_with(|| l.cmp(&r));
            left = &left[l..];
            right = &right[r..];
        } else {
            order = left[0]
                .to_ascii_lowercase()
                .cmp(&right[0].to_ascii_lowercase());
            left = &left[1..];
            right = &right[1..];
        }
        if order != Ordering::Equal {
            return order;
        }
    }
    left.len()
        .cmp(&right.len())
        .then_with(|| a.as_bytes().cmp(b.as_bytes()))
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    pub fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut writer = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, data) in entries {
            writer
                .start_file(
                    *name,
                    SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
    }

    /// Stored ZIPs with raw/duplicate names and flags that ZipWriter rejects.
    pub fn stored_zip(entries: &[(&[u8], &[u8], u32, u16)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut central = Vec::new();
        for &(name, data, mode, flags) in entries {
            let offset = bytes.len() as u32;
            let mut crc = u32::MAX;
            for byte in data {
                crc ^= u32::from(*byte);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (0xedb8_8320u32 & 0u32.wrapping_sub(crc & 1));
                }
            }
            let crc = !crc;
            bytes.extend_from_slice(b"PK\x03\x04");
            bytes.extend_from_slice(&20u16.to_le_bytes());
            bytes.extend_from_slice(&flags.to_le_bytes());
            bytes.extend_from_slice(&[0; 6]); // stored, time, date
            bytes.extend_from_slice(&crc.to_le_bytes());
            bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes.extend_from_slice(name);
            bytes.extend_from_slice(data);
            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&[20, 3]); // Unix attributes
            central.extend_from_slice(&bytes[offset as usize + 4..offset as usize + 28]);
            central.extend_from_slice(&[0; 8]); // extra/comment/disk/internal attributes
            central.extend_from_slice(&(mode << 16).to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name);
        }
        let start = bytes.len() as u32;
        bytes.extend_from_slice(&central);
        bytes.extend_from_slice(b"PK\x05\x06");
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&(central.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&start.to_le_bytes());
        bytes.extend_from_slice(&[0; 2]);
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::{stored_zip, write_zip};

    #[derive(Debug)]
    struct CountingReader<R> {
        inner: R,
        read: usize,
        limit: usize,
    }

    impl<R: Read> Read for CountingReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if buf.len() > self.limit.saturating_sub(self.read) {
                return Err(std::io::Error::other("test read budget exceeded"));
            }
            let count = self.inner.read(buf)?;
            self.read += count;
            Ok(count)
        }
    }

    impl<R: Seek> Seek for CountingReader<R> {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    fn zip64_fixture() -> Vec<u8> {
        let mut bytes = stored_zip(&[(b"a.png", b"pixels", 0o100644, 0)]);
        let eocd = bytes.split_off(bytes.len() - 22);
        let offset = bytes.len() as u64;
        bytes.extend_from_slice(b"PK\x06\x06");
        bytes.extend_from_slice(&44u64.to_le_bytes());
        bytes.extend_from_slice(&[45, 3, 45, 0]);
        bytes.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(
            &u64::from(u32::from_le_bytes(eocd[12..16].try_into().unwrap())).to_le_bytes(),
        );
        bytes.extend_from_slice(
            &u64::from(u32::from_le_bytes(eocd[16..20].try_into().unwrap())).to_le_bytes(),
        );
        bytes.extend_from_slice(b"PK\x06\x07");
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&offset.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        let mut eocd = eocd;
        eocd[8..20].fill(0xff);
        bytes.extend_from_slice(&eocd);
        bytes
    }

    #[test]
    fn preflight_accepts_bounded_zip64_and_maximum_zip32_comment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.zip");
        std::fs::write(&path, zip64_fixture()).unwrap();
        let source = catalog_for(&path).unwrap().items.remove(0);
        assert_eq!(read_source_bytes(&source, 6).unwrap(), b"pixels");

        let mut bytes = stored_zip(&[(b"a.png", b"pixels", 0o100644, 0)]);
        let end = bytes.len();
        bytes[end - 2..].copy_from_slice(&u16::MAX.to_le_bytes());
        bytes.resize(end + usize::from(u16::MAX), b'x');
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(catalog_for(&path).unwrap().items.len(), 1);
        assert!(
            open_zip_reader(Cursor::new(stored_zip(&[])))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn preflight_covers_dependency_window_boundary_and_fallback_candidates() {
        #[cfg(target_pointer_width = "64")]
        assert_eq!(EOCD_SEARCH_WINDOW, 65_572);
        let extra = EOCD_SEARCH_WINDOW - (65_536 + 22);
        assert!(extra > 0);
        for inset in [0, extra - 1] {
            let mut bytes = zip64_fixture();
            let footer = bytes.windows(4).position(|w| w == b"PK\x06\x06").unwrap();
            bytes[footer + 4..footer + 12].copy_from_slice(&u64::MAX.to_le_bytes());
            let eocd = bytes.len() - 22;
            bytes.resize(eocd + EOCD_SEARCH_WINDOW - inset, 0);
            let later = bytes.len() - 22;
            bytes[later..later + 4].copy_from_slice(b"PK\x05\x06");
            bytes[later + 4] = 1; // invalid disk: zip falls back to the earlier footer
            assert_eq!(eocd - (bytes.len() - EOCD_SEARCH_WINDOW), inset);
            let error = preflight_zip(&mut Cursor::new(bytes)).unwrap_err();
            assert!(error.to_string().contains("ZIP64"), "{error}");
        }
    }

    #[test]
    fn declared_directory_signatures_disable_zip32_and_zip64_payload_recovery() {
        use std::io::Write;
        const LEN: u64 = 512 * 1024 * 1024;
        for zip64 in [false, true] {
            let mut suffix = if zip64 {
                let mut bytes = zip64_fixture();
                let footer = bytes.windows(4).position(|w| w == b"PK\x06\x06").unwrap();
                bytes.split_off(footer)
            } else {
                let bytes = stored_zip(&[(b"a.png", b"pixels", 0o100644, 0)]);
                bytes[bytes.len() - 22..].to_vec()
            };
            let offset = LEN - suffix.len() as u64;
            if zip64 {
                suffix[48..56].fill(0); // EOCD64 central directory offset
                suffix[64..72].copy_from_slice(&offset.to_le_bytes()); // locator
            } else {
                suffix[16..20].fill(0); // EOCD32 central directory offset
            }
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(b"data").unwrap();
            file.seek(SeekFrom::Start(offset)).unwrap();
            file.write_all(&suffix).unwrap();
            let mut reader = CountingReader {
                inner: file,
                read: 0,
                limit: EOCD_SEARCH_WINDOW * 4,
            };
            let error = open_zip_reader(&mut reader).unwrap_err();
            assert!(error.contains("central-directory signature"), "{error}");
            assert!(reader.read <= EOCD_SEARCH_WINDOW + 128);

            // Even with a valid declared signature, the distant footer must
            // not cause Detect-mode recovery scans through the sparse payload.
            reader.inner.seek(SeekFrom::Start(0)).unwrap();
            reader.inner.write_all(b"PK\x01\x02").unwrap();
            reader.read = 0;
            assert_eq!(open_zip_reader(&mut reader).unwrap().len(), 1);
            assert!(reader.read <= reader.limit);
        }
    }

    #[test]
    fn preflight_rejects_forged_zip64_footer_and_directory_sizes_and_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forged.zip");
        let good = zip64_fixture();
        assert!(good.len() < 256);
        let footer = good.windows(4).position(|w| w == b"PK\x06\x06").unwrap();
        let locator = footer + 56;
        for (field, value) in [
            (footer + 4, MAX_METADATA + 1),  // extensible-data allocation
            (footer + 4, u64::MAX),          // record length overflow
            (footer + 40, MAX_METADATA + 1), // central-directory metadata
            (footer + 48, u64::MAX),         // central-directory offset overflow
            (locator + 8, u64::MAX),         // ZIP64 record outside the file
        ] {
            let mut bytes = good.clone();
            bytes[field..field + 8].copy_from_slice(&value.to_le_bytes());
            std::fs::write(&path, bytes).unwrap();
            let error = catalog_for(&path).unwrap_err();
            assert!(error.contains("ZIP64"), "{error}");
        }
    }

    #[test]
    fn preflight_rejects_oversized_zip64_sector_even_inside_a_sparse_file() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sparse.zip");
        let mut bytes = zip64_fixture();
        let footer = bytes.windows(4).position(|w| w == b"PK\x06\x06").unwrap();
        let locator = footer + 56;
        bytes[footer + 4..footer + 12].copy_from_slice(&(44 + MAX_METADATA).to_le_bytes());
        let mut file = File::create(&path).unwrap();
        file.write_all(&bytes[..locator]).unwrap();
        file.seek(SeekFrom::Start(locator as u64 + MAX_METADATA))
            .unwrap();
        file.write_all(&bytes[locator..]).unwrap();
        drop(file);
        assert!(
            catalog_for(&path)
                .unwrap_err()
                .contains("ZIP64 footer exceeds")
        );
    }

    #[test]
    fn preflight_checks_zip32_bounds_and_earlier_footer_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forged.zip");
        let good = stored_zip(&[(b"a.png", b"pixels", 0o100644, 0)]);
        for field in [good.len() - 10, good.len() - 6] {
            let mut bytes = good.clone();
            bytes[field..field + 4].copy_from_slice(&((MAX_METADATA + 1) as u32).to_le_bytes());
            std::fs::write(&path, bytes).unwrap();
            assert!(catalog_for(&path).unwrap_err().contains("metadata"));
        }
        let mut bytes = zip64_fixture();
        let footer = bytes.windows(4).position(|w| w == b"PK\x06\x06").unwrap();
        bytes[footer + 4..footer + 12].copy_from_slice(&u64::MAX.to_le_bytes());
        // A later EOCD must not conceal an unsafe earlier candidate from preflight.
        bytes.extend_from_slice(b"PK\x05\x06");
        bytes.extend_from_slice(&[0; 18]);
        std::fs::write(&path, bytes).unwrap();
        assert!(catalog_for(&path).unwrap_err().contains("ZIP64"));
    }

    #[test]
    fn capped_entry_read_does_not_drain_remaining_compressed_input() {
        let bytes = stored_zip(&[(b"a.png", &vec![7; 256 * 1024], 0o100644, 0)]);
        let reader = CountingReader {
            inner: Cursor::new(bytes),
            read: 0,
            limit: usize::MAX,
        };
        let mut zip = ZipIterable::try_new(reader, Default::default()).unwrap();
        let files = zip.files().unwrap();
        let entry = files.next().unwrap().unwrap();
        files.reader_mut().read = 0;
        let error = read_zip_entry(
            entry,
            files.reader_mut(),
            &SourceRef::file(Path::new("a.png")),
            1,
        )
        .unwrap_err();
        assert!(error.contains("larger than"));
        assert!(
            files.reader_mut().read < 128,
            "capped read drained the payload"
        );
    }

    #[test]
    fn unix_directory_mode_without_a_slash_is_not_an_image_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.zip");
        std::fs::write(&path, stored_zip(&[(b"a.png", b"pixels", 0o100644, 0)])).unwrap();
        let source = catalog_for(&path).unwrap().items.remove(0);
        std::fs::write(&path, stored_zip(&[(b"a.png", b"pixels", 0o040755, 0)])).unwrap();
        assert!(catalog_for(&path).unwrap().items.is_empty());
        assert!(read_source_bytes(&source, 100).is_err());
        let mut zip = open_zip(&path).unwrap();
        let entry = zip.files().unwrap().next().unwrap().unwrap();
        assert!(
            verify_entry(&source, &entry)
                .unwrap_err()
                .contains("directory")
        );
    }

    #[test]
    fn natural_sort_is_numeric_case_insensitive_and_deterministic() {
        let mut names = vec![
            "page10",
            "page001",
            "page01",
            "page2",
            "page1",
            "Page1",
            "page000",
            "page0",
            "PAGE1",
            "page999999999999999999999999999999",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            [
                "page0",
                "page000",
                "PAGE1",
                "Page1",
                "page1",
                "page01",
                "page001",
                "page2",
                "page10",
                "page999999999999999999999999999999"
            ]
        );
        for a in &names {
            for b in &names {
                assert_eq!(natural_cmp(a, b), natural_cmp(b, a).reverse());
            }
        }
    }

    #[test]
    fn catalog_sorts_images_preserving_raw_indices_duplicates_and_lossy_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.CBZ");
        let entries: Vec<(&[u8], &[u8], u32, u16)> = vec![
            (b"folder/page10.PNG", b"ten", 0o100644, 0),
            (b"notes.txt", b"ignored", 0o100644, 0),
            (b"folder/page2.png", b"two", 0o100644, 0),
            (b"folder/page2.png", b"duplicate", 0o100644, 0),
            (b"\xff.png", b"raw1", 0o100644, 0x800),
            (b"\xfe.png", b"raw2", 0o100644, 0x800),
            (b"directory.png/", b"", 0o040755, 0),
            (b"link.png", b"target.png", 0o120777, 0),
        ];
        std::fs::write(&path, stored_zip(&entries)).unwrap();
        let catalog = catalog_for(&path).unwrap();
        assert_eq!(catalog.items.len(), 5);
        let indices: Vec<_> = catalog
            .items
            .iter()
            .map(|item| {
                let SourceRef::Archive { index, .. } = item else {
                    panic!()
                };
                *index
            })
            .collect();
        assert_eq!(indices, [2, 3, 0, 4, 5]);
        for (source, index) in catalog.items.iter().zip(indices) {
            assert_eq!(read_source_bytes(source, 32).unwrap(), entries[index].1);
            assert_eq!(source.archive_root(), Some(path.as_path()));
        }
        assert_eq!(catalog.items[0].display_name(), "page2.png");
        assert_ne!(catalog.items[3], catalog.items[4]);
    }

    #[test]
    fn deflated_reads_are_bounded_and_metadata_changes_reject_old_references() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.zip");
        write_zip(&path, &[("a.png", b"abcdef")]);
        let source = catalog_for(&path).unwrap().items.remove(0);
        assert!(
            read_source_bytes(&source, 5)
                .unwrap_err()
                .contains("larger than")
        );
        assert_eq!(read_source_bytes(&source, 6).unwrap(), b"abcdef");
        let old = source_identity(&source).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old.modified + std::time::Duration::from_secs(2))
            .unwrap();
        assert_ne!(source_identity(&source), Some(old));
        write_zip(&path, &[("b.png", b"ghijkl")]);
        assert!(
            read_source_bytes(&source, 10)
                .unwrap_err()
                .contains("changed")
        );
        assert!(source_identity(&source).is_none());
    }

    #[test]
    fn catalog_fingerprints_reject_old_entries_after_mtime_or_length_changes() {
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.zip");
        std::fs::write(
            &path,
            stored_zip(&[
                (b"a.png", b"unchanged", 0o100644, 0),
                (b"b.png", b"old", 0o100644, 0),
            ]),
        )
        .unwrap();
        let items = catalog_for(&path).unwrap().items;
        let before = file_identity(&path).unwrap();
        for source in &items {
            let SourceRef::Archive {
                archive_len,
                archive_mtime_secs,
                archive_mtime_nanos,
                ..
            } = source
            else {
                panic!()
            };
            assert_eq!(
                (*archive_len, *archive_mtime_secs, *archive_mtime_nanos),
                archive_fingerprint(before)
            );
            assert!(source_identity(source).is_some());
        }
        assert_eq!(read_source_bytes(&items[0], 16).unwrap(), b"unchanged");
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(before.1 + Duration::from_secs(2))
            .unwrap();
        assert!(source_identity(&items[0]).is_none());
        assert!(
            read_source_bytes(&items[0], 16)
                .unwrap_err()
                .contains("archive changed")
        );
        let fresh = catalog_for(&path).unwrap().items;
        assert_ne!(fresh[0], items[0]);
        assert_eq!(read_source_bytes(&fresh[0], 16).unwrap(), b"unchanged");
        let new_mtime = file_identity(&path).unwrap().1;
        std::fs::write(
            &path,
            stored_zip(&[
                (b"a.png", b"unchanged", 0o100644, 0),
                (b"b.png", b"a longer replacement", 0o100644, 0),
            ]),
        )
        .unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(new_mtime)
            .unwrap();
        assert_ne!(file_identity(&path).unwrap().0, before.0);
        assert!(source_identity(&fresh[0]).is_none());
        assert!(
            read_source_bytes(&fresh[0], 16)
                .unwrap_err()
                .contains("archive changed")
        );
        let replaced = catalog_for(&path).unwrap().items;
        assert_ne!(replaced[0], fresh[0]);
        assert!(source_identity(&replaced[0]).is_some());
        assert_eq!(read_source_bytes(&replaced[0], 16).unwrap(), b"unchanged");
    }

    #[test]
    fn preserved_archive_fingerprint_still_checks_entries_but_cannot_detect_unchanged_members() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.zip");
        std::fs::write(
            &path,
            stored_zip(&[
                (b"a.png", b"aaaa", 0o100644, 0),
                (b"b.png", b"bbbb", 0o100644, 0),
            ]),
        )
        .unwrap();
        let items = catalog_for(&path).unwrap().items;
        let before = file_identity(&path).unwrap();
        let original = source_identity(&items[0]).unwrap();
        std::fs::write(
            &path,
            stored_zip(&[
                (b"a.png", b"aaaa", 0o100644, 0),
                (b"b.png", b"cccc", 0o100644, 0),
            ]),
        )
        .unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(before.1)
            .unwrap();
        assert_eq!(file_identity(&path), Some(before));
        assert!(source_identity(&items[1]).is_none());
        assert!(
            read_source_bytes(&items[1], 4)
                .unwrap_err()
                .contains("archive entry changed")
        );
        // The unchanged member has the same fingerprint and entry metadata.
        assert_eq!(source_identity(&items[0]), Some(original));
        assert_eq!(read_source_bytes(&items[0], 4).unwrap(), b"aaaa");
        let replaced = catalog_for(&path).unwrap().items;
        assert_eq!(items[0], replaced[0]);
        assert_ne!(items[1], replaced[1]);
        assert_eq!(read_source_bytes(&replaced[1], 4).unwrap(), b"cccc");
    }

    #[test]
    fn archive_fingerprint_keeps_nanoseconds_and_defaults_pre_epoch_times() {
        use std::time::Duration;
        let epoch = SystemTime::UNIX_EPOCH;
        assert_eq!(
            archive_fingerprint((12, epoch + Duration::new(34, 56))),
            (12, 34, 56)
        );
        assert_eq!(
            archive_fingerprint((12, epoch - Duration::from_secs(1))),
            (12, 0, 0)
        );
    }

    #[test]
    fn file_identity_and_folder_order_preserve_existing_behavior() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("page10.png");
        let b = dir.path().join("page2.png");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        assert_eq!(
            catalog_for(&a).unwrap().items,
            [SourceRef::file(&a), SourceRef::file(&b)]
        );
        let source = SourceRef::file(&a);
        let old = source_identity(&source).unwrap();
        std::fs::write(&a, b"longer").unwrap();
        assert_ne!(source_identity(&source), Some(old));
        assert_eq!(read_source_bytes(&source, 6).unwrap(), b"longer");
        assert!(read_source_bytes(&source, 5).is_err());
    }

    #[test]
    fn rejects_crc_corruption_truncation_encryption_directories_and_links() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.zip");
        let good = stored_zip(&[(b"a.png", b"pixels", 0o100644, 0)]);
        std::fs::write(&path, &good).unwrap();
        let mut corrupt = good.clone();
        corrupt[35] ^= 1; // first payload byte, not the central CRC
        std::fs::write(&path, corrupt).unwrap();
        let source = catalog_for(&path).unwrap().items.remove(0);
        assert!(
            read_source_bytes(&source, 100)
                .unwrap_err()
                .contains("cannot read")
        );
        std::fs::write(&path, &good[..good.len() - 12]).unwrap();
        assert!(catalog_for(&path).is_err());
        assert!(read_source_bytes(&source, 100).is_err());
        for (name, mode, flags) in [
            (&b"a.png"[..], 0o100644, 1),
            (&b"a.png/"[..], 0o040755, 0),
            (&b"a.png"[..], 0o120777, 0),
        ] {
            std::fs::write(&path, stored_zip(&[(name, b"pixels", mode, flags)])).unwrap();
            assert!(read_source_bytes(&source, 100).is_err());
        }
    }

    #[test]
    fn actual_output_is_bounded_even_when_declared_size_is_forged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forged.zip");
        for declared in [1u32, 1_000_000] {
            let mut bytes = stored_zip(&[(b"a.png", b"actual bytes", 0o100644, 0)]);
            let central = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
            bytes[22..26].copy_from_slice(&declared.to_le_bytes());
            bytes[central + 24..central + 28].copy_from_slice(&declared.to_le_bytes());
            std::fs::write(&path, bytes).unwrap();
            let source = catalog_for(&path).unwrap().items.remove(0);
            assert!(
                read_source_bytes(&source, 3)
                    .unwrap_err()
                    .contains("larger than")
            );
            assert!(
                read_source_bytes(&source, 100)
                    .unwrap_err()
                    .contains("size")
            );
        }
    }

    #[test]
    fn corrupt_deflate_and_encrypted_non_image_entries_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.zip");
        write_zip(&path, &[("a.png", b"pixels")]);
        let mut bytes = std::fs::read(&path).unwrap();
        let name_len = u16::from_le_bytes(bytes[26..28].try_into().unwrap()) as usize;
        let extra_len = u16::from_le_bytes(bytes[28..30].try_into().unwrap()) as usize;
        bytes[30 + name_len + extra_len] = 0x07; // reserved DEFLATE block type
        std::fs::write(&path, bytes).unwrap();
        let source = catalog_for(&path).unwrap().items.remove(0);
        assert!(
            read_source_bytes(&source, 100)
                .unwrap_err()
                .contains("cannot read")
        );
        std::fs::write(&path, stored_zip(&[(b"notes.txt", b"secret", 0o100644, 1)])).unwrap();
        assert!(catalog_for(&path).unwrap_err().contains("encrypted"));
    }

    #[test]
    fn catalog_rejects_image_count_and_metadata_over_limits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many.zip");
        let entries = vec![(&b"a.png"[..], &b""[..], 0o100644, 0); MAX_IMAGES + 1];
        std::fs::write(&path, stored_zip(&entries)).unwrap();
        assert!(catalog_for(&path).unwrap_err().contains("20000"));
        let name = "x".repeat(60_000) + ".txt";
        let entries = vec![(name.as_bytes(), &b""[..], 0o100644, 0); 280];
        std::fs::write(&path, stored_zip(&entries)).unwrap();
        assert!(catalog_for(&path).unwrap_err().contains("metadata"));
    }
}
