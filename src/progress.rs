//! Bounded, versioned reading progress. Loading never writes; callers serialize saves.
//! Archive identity comes from the current catalog's SourceRef, without image I/O.

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::model::source::SourceRef;

const MAX_PROGRESS_BYTES: u64 = 1024 * 1024;
const MAX_BOOKS: usize = 200;
const PROGRESS_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageOverride {
    #[default]
    Auto,
    Single,
    Spread,
}

/// Native path units, never a lossy display name. Foreign OS encodings are
/// refused on load rather than silently converting a different filesystem path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredPath {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
    Text(String),
}

impl StoredPath {
    pub fn from_path(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("progress root must be an absolute path".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Ok(Self::Unix(path.as_os_str().as_bytes().to_vec()))
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Ok(Self::Windows(path.as_os_str().encode_wide().collect()))
        }
        #[cfg(not(any(unix, windows)))]
        {
            path.to_str()
                .map(|path| Self::Text(path.to_owned()))
                .ok_or_else(|| "cannot store this OS's non-UTF-8 path losslessly".into())
        }
    }

    pub fn to_path_buf(&self) -> Result<PathBuf, String> {
        let invalid = match self {
            Self::Unix(bytes) => bytes.is_empty() || bytes.contains(&0),
            Self::Windows(units) => units.is_empty() || units.contains(&0),
            Self::Text(text) => text.is_empty() || text.contains('\0'),
        };
        if invalid {
            return Err("progress root is empty or contains NUL".into());
        }
        let path = match self {
            Self::Unix(bytes) => {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStringExt;
                    PathBuf::from(std::ffi::OsString::from_vec(bytes.clone()))
                }
                #[cfg(not(unix))]
                {
                    let _ = bytes;
                    return Err("Unix progress paths are not native to this OS".into());
                }
            }
            Self::Windows(units) => {
                #[cfg(windows)]
                {
                    use std::os::windows::ffi::OsStringExt;
                    PathBuf::from(std::ffi::OsString::from_wide(units))
                }
                #[cfg(not(windows))]
                {
                    let _ = units;
                    return Err("Windows progress paths are not native to this OS".into());
                }
            }
            Self::Text(text) => PathBuf::from(text),
        };
        if !path.is_absolute() {
            return Err("progress root must be an absolute path".into());
        }
        Ok(path)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookRecord {
    pub root: StoredPath,
    pub archive_fingerprint: Option<(u64, u64, u32)>,
    /// Original archive member index; ordinary file records use zero.
    pub entry_index: usize,
    /// Display only: never used to open a file or resolve an archive member.
    pub entry_name: String,
    pub overrides: BTreeMap<usize, PageOverride>,
    pub last_read_secs: u64,
}

impl BookRecord {
    fn from_source(source: &SourceRef) -> Result<Self, String> {
        let root = absolute_root(source)?;
        let (entry_index, entry_name) = match source {
            SourceRef::File(_) => (0, source.display_name()),
            SourceRef::Archive { index, name, .. } => (*index, name.clone()),
        };
        let record = Self {
            root,
            archive_fingerprint: fingerprint(source),
            entry_index,
            entry_name,
            overrides: BTreeMap::new(),
            last_read_secs: 0,
        };
        record.validate()?;
        Ok(record)
    }

    fn validate(&self) -> Result<(), String> {
        self.root.to_path_buf()?;
        if self
            .archive_fingerprint
            .is_some_and(|(_, _, nanos)| nanos >= 1_000_000_000)
        {
            return Err("invalid archive mtime nanoseconds".into());
        }
        if self.entry_name.is_empty() {
            return Err("progress entry name is empty".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoredProgress {
    pub entry_index: usize,
    pub entry_name: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressFile {
    version: u32,
    books: Vec<BookRecord>,
}

impl Default for ProgressFile {
    fn default() -> Self {
        Self {
            version: PROGRESS_VERSION,
            books: Vec::new(),
        }
    }
}

pub struct ProgressStore {
    path: Option<PathBuf>,
    file: ProgressFile,
    writable: bool,
    /// A non-fatal load problem. The original file is protected from saves.
    pub warning: Option<String>,
}

impl ProgressStore {
    pub fn load() -> Self {
        match dirs::config_dir() {
            Some(directory) => Self::load_from(&directory.join("oxeye").join("progress.json")),
            None => Self {
                path: None,
                file: ProgressFile::default(),
                writable: false,
                warning: Some("no user config directory is available".into()),
            },
        }
    }

    /// Explicit-path loader for callers that own a separate configuration directory.
    pub fn load_from(path: &Path) -> Self {
        let mut store = Self {
            path: Some(path.to_path_buf()),
            file: ProgressFile::default(),
            writable: true,
            warning: None,
        };
        match read_progress(path) {
            Ok(file) => {
                store.file = file;
                store.prune();
            }
            Err(error) => {
                store.writable = false;
                store.warning = Some(format!("{}: {error}", path.display()));
            }
        }
        store
    }

    pub fn restore(&self, source: &SourceRef) -> Option<RestoredProgress> {
        let record = self.find(source)?;
        Some(RestoredProgress {
            entry_index: record.entry_index,
            entry_name: record.entry_name.clone(),
        })
    }

    /// Record a committed page; retain overrides only for the same archive version.
    pub fn record(&mut self, source: &SourceRef) -> Result<(), String> {
        let record = self.book_mut(source)?;
        (record.entry_index, record.entry_name) = match source {
            SourceRef::File(_) => (0, source.display_name()),
            SourceRef::Archive { index, name, .. } => (*index, name.clone()),
        };
        record.last_read_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.prune();
        Ok(())
    }

    /// Change layout policy without moving an existing bookmark or read timestamp.
    pub fn set_override(
        &mut self,
        source: &SourceRef,
        entry_index: usize,
        mode: PageOverride,
    ) -> Result<(), String> {
        let record = self.book_mut(source)?;
        if mode == PageOverride::Auto {
            record.overrides.remove(&entry_index);
        } else {
            record.overrides.insert(entry_index, mode);
        }
        self.prune();
        Ok(())
    }

    pub fn overrides_for(&self, source: &SourceRef) -> BTreeMap<usize, PageOverride> {
        self.find(source)
            .map(|record| record.overrides.clone())
            .unwrap_or_default()
    }

    /// Atomic replacement. A rejected load stays read-only, even after in-memory edits.
    pub fn save(&self) -> Result<(), String> {
        if !self.writable {
            return Err("refusing to overwrite an unusable progress file".into());
        }
        let path = self
            .path
            .as_ref()
            .ok_or("no user config directory is available")?;
        let bytes = serde_json::to_vec_pretty(&self.file)
            .map_err(|error| format!("cannot serialize progress: {error}"))?;
        if bytes.len() as u64 > MAX_PROGRESS_BYTES {
            return Err(format!("progress exceeds {MAX_PROGRESS_BYTES} bytes"));
        }
        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
        let mut file = tempfile::NamedTempFile::new_in(directory)
            .map_err(|error| format!("cannot create a temporary progress file: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("cannot write progress: {error}"))?;
        file.as_file_mut()
            .flush()
            .map_err(|error| format!("cannot flush progress: {error}"))?;
        file.as_file()
            .sync_all()
            .map_err(|error| format!("cannot sync progress: {error}"))?;
        file.persist(path)
            .map_err(|error| format!("cannot replace {}: {}", path.display(), error.error))?;
        Ok(())
    }

    fn find(&self, source: &SourceRef) -> Option<&BookRecord> {
        let root = absolute_root(source).ok()?;
        let fingerprint = fingerprint(source);
        self.file
            .books
            .iter()
            .find(|record| record.root == root && record.archive_fingerprint == fingerprint)
    }

    fn book_mut(&mut self, source: &SourceRef) -> Result<&mut BookRecord, String> {
        let new = BookRecord::from_source(source)?;
        let index = self
            .file
            .books
            .iter()
            .position(|record| record.root == new.root);
        let index = match index {
            Some(index) => {
                let old = &mut self.file.books[index];
                if old.archive_fingerprint != new.archive_fingerprint {
                    *old = new;
                }
                index
            }
            None => {
                self.file.books.push(new);
                self.file.books.len() - 1
            }
        };
        Ok(&mut self.file.books[index])
    }

    fn prune(&mut self) {
        self.file.books.sort_by(|a, b| {
            b.last_read_secs
                .cmp(&a.last_read_secs)
                .then_with(|| a.root.cmp(&b.root))
        });
        self.file.books.truncate(MAX_BOOKS);
    }
}

fn absolute_root(source: &SourceRef) -> Result<StoredPath, String> {
    let path = std::path::absolute(source.path())
        .map_err(|error| format!("cannot resolve progress root: {error}"))?;
    StoredPath::from_path(&path)
}

fn fingerprint(source: &SourceRef) -> Option<(u64, u64, u32)> {
    match source {
        SourceRef::File(_) => None,
        SourceRef::Archive {
            archive_len,
            archive_mtime_secs,
            archive_mtime_nanos,
            ..
        } => Some((*archive_len, *archive_mtime_secs, *archive_mtime_nanos)),
    }
}

fn read_progress(path: &Path) -> Result<ProgressFile, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProgressFile::default());
        }
        Err(error) => return Err(error.to_string()),
    };
    if file.metadata().map_err(|error| error.to_string())?.len() > MAX_PROGRESS_BYTES {
        return Err(format!("progress exceeds {MAX_PROGRESS_BYTES} bytes"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_PROGRESS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_PROGRESS_BYTES {
        return Err(format!("progress exceeds {MAX_PROGRESS_BYTES} bytes"));
    }
    let mut data: ProgressFile =
        serde_json::from_slice(&bytes).map_err(|error| format!("invalid progress: {error}"))?;
    if data.version != PROGRESS_VERSION {
        return Err(format!("unsupported progress version {}", data.version));
    }
    let mut roots = HashSet::new();
    for record in &mut data.books {
        record.validate()?;
        record.root = StoredPath::from_path(&record.root.to_path_buf()?)?;
        if !roots.insert(record.root.clone()) {
            return Err("duplicate progress root".into());
        }
        record
            .overrides
            .retain(|_, mode| *mode != PageOverride::Auto);
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::source::ArchiveKind;

    fn archive(path: &Path, index: usize) -> SourceRef {
        SourceRef::Archive {
            archive: path.to_path_buf(),
            archive_len: 123,
            archive_mtime_secs: 456,
            archive_mtime_nanos: 789,
            kind: ArchiveKind::Zip,
            index,
            name: format!("folder/page{index}.png"),
            size: 10,
            crc: 0,
        }
    }

    #[test]
    fn missing_store_defaults_without_creating_or_writing_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oxeye").join("progress.json");
        let store = ProgressStore::load_from(&path);
        assert!(store.warning.is_none());
        assert!(store.file.books.is_empty());
        assert_eq!(store.file.version, 1);
        assert_eq!(PageOverride::default(), PageOverride::Auto);
        assert!(!path.parent().unwrap().exists());
        let source = archive(&directory.path().join("book.cbz"), 0);
        assert!(store.restore(&source).is_none());
        assert!(store.overrides_for(&source).is_empty());
    }

    #[test]
    fn utf8_paths_and_page_overrides_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config").join("progress.json");
        let source = archive(&directory.path().join("漫画-é.cbz"), 7);
        let mut store = ProgressStore::load_from(&path);
        store.record(&source).unwrap();
        store
            .set_override(&source, 7, PageOverride::Spread)
            .unwrap();
        store
            .set_override(&source, 8, PageOverride::Single)
            .unwrap();
        store.save().unwrap();
        let loaded = ProgressStore::load_from(&path);
        assert!(loaded.warning.is_none());
        assert_eq!(
            loaded.restore(&source).unwrap(),
            RestoredProgress {
                entry_index: 7,
                entry_name: "folder/page7.png".into(),
            }
        );
        assert_eq!(
            loaded.overrides_for(&source),
            BTreeMap::from([(7, PageOverride::Spread), (8, PageOverride::Single),])
        );
        let json = std::fs::read_to_string(&path).unwrap();
        assert!(json.contains("\"spread\"") && json.contains("\"single\""));
        let mut text_file: ProgressFile = serde_json::from_str(&json).unwrap();
        text_file.books[0].root = StoredPath::Text(source.path().to_str().unwrap().into());
        std::fs::write(&path, serde_json::to_vec(&text_file).unwrap()).unwrap();
        assert_eq!(
            ProgressStore::load_from(&path).restore(&source),
            loaded.restore(&source)
        );
        assert_eq!(
            serde_json::to_string(&PageOverride::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(
            StoredPath::Text(source.path().to_str().unwrap().into())
                .to_path_buf()
                .unwrap(),
            source.path()
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_round_trip_without_canonicalizing_symlinks() {
        use std::os::unix::ffi::OsStringExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("progress.json");
        let root = directory
            .path()
            .join(std::ffi::OsString::from_vec(b"comic-\xff.cbz".to_vec()));
        let source = archive(&root, 3);
        let mut store = ProgressStore::load_from(&path);
        store.record(&source).unwrap();
        store.save().unwrap();
        let loaded = ProgressStore::load_from(&path);
        assert_eq!(loaded.file.books[0].root.to_path_buf().unwrap(), root);
        assert_eq!(loaded.restore(&source).unwrap().entry_index, 3);
        let target = directory.path().join("target.png");
        std::fs::write(&target, b"not decoded").unwrap();
        let link = directory.path().join("selected.png");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        store.record(&SourceRef::file(&link)).unwrap();
        assert!(store.restore(&SourceRef::file(&target)).is_none());
        assert!(store.restore(&SourceRef::file(&link)).is_some());
    }

    #[cfg(windows)]
    #[test]
    fn windows_utf16_path_units_round_trip_without_unicode_loss() {
        use std::os::windows::ffi::OsStringExt;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join(std::ffi::OsString::from_wide(&[
            0xd800,
            b'.' as u16,
            b'p' as u16,
            b'n' as u16,
            b'g' as u16,
        ]));
        let stored = StoredPath::from_path(&root).unwrap();
        assert_eq!(
            serde_json::from_slice::<StoredPath>(&serde_json::to_vec(&stored).unwrap())
                .unwrap()
                .to_path_buf()
                .unwrap(),
            root
        );
    }

    #[test]
    fn archive_fingerprint_changes_ignore_progress_and_clear_old_overrides() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = ProgressStore::load_from(&directory.path().join("progress.json"));
        let source = archive(&directory.path().join("book.cbz"), 7);
        store
            .set_override(&source, 7, PageOverride::Single)
            .unwrap();
        let mut changed = source.clone();
        let SourceRef::Archive { archive_len, .. } = &mut changed else {
            panic!()
        };
        *archive_len += 1;
        assert!(store.restore(&changed).is_none());
        assert!(store.overrides_for(&changed).is_empty());
        let mut changed_time = source.clone();
        let SourceRef::Archive {
            archive_mtime_nanos,
            ..
        } = &mut changed_time
        else {
            panic!()
        };
        *archive_mtime_nanos += 1;
        assert!(store.restore(&changed_time).is_none());
        let mut changed_seconds = source.clone();
        let SourceRef::Archive {
            archive_mtime_secs, ..
        } = &mut changed_seconds
        else {
            panic!()
        };
        *archive_mtime_secs += 1;
        assert!(store.restore(&changed_seconds).is_none());
        assert!(store.restore(&SourceRef::file(source.path())).is_none());
        store.record(&changed).unwrap();
        assert!(store.overrides_for(&changed).is_empty());
        assert!(store.restore(&source).is_none());
    }

    #[test]
    fn recording_new_pages_preserves_overrides_and_auto_removes_them() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = ProgressStore::load_from(&directory.path().join("progress.json"));
        let source = archive(&directory.path().join("book.cbz"), 7);
        store
            .set_override(&source, 7, PageOverride::Single)
            .unwrap();
        let next = archive(source.path(), 8);
        store.record(&next).unwrap();
        assert_eq!(store.restore(&source).unwrap().entry_index, 8);
        assert_eq!(
            store.overrides_for(&next).get(&7),
            Some(&PageOverride::Single)
        );
        store.set_override(&next, 7, PageOverride::Auto).unwrap();
        assert!(store.overrides_for(&source).is_empty());
        store
            .set_override(&source, 42, PageOverride::Spread)
            .unwrap();
        assert_eq!(store.restore(&source).unwrap().entry_index, 8);
        store.set_override(&source, 42, PageOverride::Auto).unwrap();
        store.save().unwrap();
        assert!(
            ProgressStore::load_from(store.path.as_ref().unwrap())
                .overrides_for(&source)
                .is_empty()
        );
        let file = SourceRef::file(&directory.path().join("page.png"));
        store.record(&file).unwrap();
        assert_eq!(store.restore(&file).unwrap().entry_index, 0);
        assert!(
            store
                .restore(&SourceRef::file(&directory.path().join("other.png")))
                .is_none()
        );
    }

    #[test]
    fn override_changes_preserve_saved_timestamp_and_only_record_refreshes_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("progress.json");
        let source = archive(&directory.path().join("book.cbz"), 7);
        let other = archive(&directory.path().join("other.cbz"), 0);
        let mut store = ProgressStore::load_from(&path);
        store.record(&source).unwrap();
        store.file.books[0].last_read_secs = 100;
        store
            .set_override(&source, 5, PageOverride::Single)
            .unwrap();
        store.record(&other).unwrap();
        store.file.books[0].last_read_secs = 200;
        let old = store.find(&source).unwrap().clone();
        store
            .set_override(&archive(source.path(), 2), 9, PageOverride::Spread)
            .unwrap();
        assert_eq!(store.file.books[0].root, absolute_root(&other).unwrap());
        store.save().unwrap();

        let mut loaded = ProgressStore::load_from(&path);
        let record = loaded.find(&source).unwrap();
        assert_eq!(record.last_read_secs, 100);
        assert_eq!(record.entry_index, old.entry_index);
        assert_eq!(record.entry_name, old.entry_name);
        assert_eq!(
            record.overrides,
            BTreeMap::from([(5, PageOverride::Single), (9, PageOverride::Spread),])
        );
        let next = archive(source.path(), 8);
        loaded.record(&next).unwrap();
        loaded.save().unwrap();
        let saved = ProgressStore::load_from(&path);
        let record = saved.find(&source).unwrap();
        assert!(record.last_read_secs > 200);
        assert_eq!(record.entry_index, 8);
        assert_eq!(record.entry_name, "folder/page8.png");
        assert_eq!(record.overrides.get(&9), Some(&PageOverride::Spread));
        assert_eq!(saved.file.books[0].root, absolute_root(&source).unwrap());

        let unread = archive(&directory.path().join("unread.cbz"), 0);
        loaded
            .set_override(&unread, 0, PageOverride::Single)
            .unwrap();
        assert_eq!(loaded.find(&unread).unwrap().last_read_secs, 0);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn valid_foreign_absolute_paths_are_rejected_without_rewriting() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("progress.json");
        let mut record =
            BookRecord::from_source(&archive(&directory.path().join("book.cbz"), 0)).unwrap();
        #[cfg(unix)]
        {
            record.root = StoredPath::Windows(r"C:\books\comic.cbz".encode_utf16().collect());
        }
        #[cfg(windows)]
        {
            record.root = StoredPath::Unix(b"/books/comic.cbz".to_vec());
        }
        let bytes = serde_json::to_vec(&ProgressFile {
            version: 1,
            books: vec![record],
        })
        .unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let store = ProgressStore::load_from(&path);
        assert!(store.warning.as_ref().unwrap().contains("not native"));
        assert!(store.file.books.is_empty());
        assert!(store.save().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn load_and_record_prune_to_200_recent_books_with_deterministic_ties() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("progress.json");
        let mut file = ProgressFile::default();
        for index in 0..205 {
            let mut record = BookRecord::from_source(&archive(
                &directory.path().join(format!("book-{index:03}.cbz")),
                index,
            ))
            .unwrap();
            record.last_read_secs = index as u64;
            file.books.push(record);
        }
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        let mut store = ProgressStore::load_from(&path);
        assert_eq!(store.file.books.len(), MAX_BOOKS);
        assert_eq!(store.file.books[0].last_read_secs, 204);
        assert_eq!(store.file.books[199].last_read_secs, 5);
        let new = archive(&directory.path().join("new.cbz"), 0);
        store.record(&new).unwrap();
        assert_eq!(store.file.books.len(), MAX_BOOKS);
        assert!(store.restore(&new).is_some());
        for record in &mut store.file.books {
            record.last_read_secs = 10;
        }
        let mut expected: Vec<_> = store
            .file
            .books
            .iter()
            .map(|record| record.root.clone())
            .collect();
        expected.sort();
        store.file.books.reverse();
        store.prune();
        assert_eq!(
            store
                .file
                .books
                .iter()
                .map(|record| record.root.clone())
                .collect::<Vec<_>>(),
            expected
        );
        for record in &mut file.books {
            record.last_read_secs = 10;
        }
        file.books.reverse();
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        let tied = ProgressStore::load_from(&path);
        assert_eq!(tied.file.books[0].entry_index, 0);
        assert_eq!(tied.file.books[199].entry_index, 199);
    }

    #[test]
    fn unusable_files_fall_back_without_rewriting_even_after_recording() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("progress.json");
        let source = archive(&directory.path().join("book.cbz"), 0);
        let mut invalid = BookRecord::from_source(&source).unwrap();
        invalid.archive_fingerprint = Some((1, 2, 1_000_000_000));
        let invalid = serde_json::to_vec(&ProgressFile {
            version: 1,
            books: vec![invalid],
        })
        .unwrap();
        for bytes in [
            b"{not JSON}".to_vec(),
            b"{\"version\":2,\"books\":[]}".to_vec(),
            b"{\"version\":0,\"books\":[]}".to_vec(),
            b"{\"version\":1,\"books\":[],\"unknown\":true}".to_vec(),
            vec![b'x'; MAX_PROGRESS_BYTES as usize + 1],
            invalid,
        ] {
            std::fs::write(&path, &bytes).unwrap();
            let mut store = ProgressStore::load_from(&path);
            assert!(store.warning.is_some());
            assert!(store.file.books.is_empty());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            store.record(&source).unwrap();
            assert!(store.save().is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn invalid_paths_and_duplicate_roots_are_rejected_without_lossy_conversion() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("progress.json");
        let mut record =
            BookRecord::from_source(&archive(&directory.path().join("book.cbz"), 0)).unwrap();
        for root in [
            StoredPath::Text("relative.cbz".into()),
            StoredPath::Unix(vec![b'/', 0]),
            StoredPath::Windows(vec![0]),
        ] {
            record.root = root;
            std::fs::write(
                &path,
                serde_json::to_vec(&ProgressFile {
                    version: 1,
                    books: vec![record.clone()],
                })
                .unwrap(),
            )
            .unwrap();
            assert!(ProgressStore::load_from(&path).warning.is_some());
        }
        record.root = StoredPath::from_path(&directory.path().join("book.cbz")).unwrap();
        std::fs::write(
            &path,
            serde_json::to_vec(&ProgressFile {
                version: 1,
                books: vec![record.clone(), record],
            })
            .unwrap(),
        )
        .unwrap();
        assert!(
            ProgressStore::load_from(&path)
                .warning
                .unwrap()
                .contains("duplicate")
        );
    }

    #[test]
    fn failed_save_preserves_previous_bytes_and_cleans_up_temporary_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("progress.json");
        let source = archive(&directory.path().join("book.cbz"), 0);
        let mut store = ProgressStore::load_from(&path);
        store.record(&source).unwrap();
        store.save().unwrap();
        let before = std::fs::read(&path).unwrap();
        store.file.books[0].entry_name = "x".repeat(MAX_PROGRESS_BYTES as usize + 1);
        assert!(store.save().unwrap_err().contains("exceeds"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        store.file.books[0].entry_name = "page.png".into();
        let blocked = directory.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(blocked.join("keep"), b"old data").unwrap();
        store.path = Some(blocked.clone());
        assert!(store.save().unwrap_err().contains("cannot replace"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read(blocked.join("keep")).unwrap(), b"old data");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }
}
