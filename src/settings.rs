//! Persisted application settings.
//!
//! The file lives at `dirs::config_dir()/oxeye/settings.json`. Reads are
//! bounded, validated, and never rewrite the file. Saves are serialized by the
//! caller and use a same-directory temporary file plus an atomic persist, so a
//! failed save leaves the previous file untouched.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Upper bound for the settings file. Anything larger is refused unread.
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
/// The on-disk schema version written by this build.
const SETTINGS_VERSION: u32 = 3;
/// Oldest on-disk schema version still understood.
const MIN_SETTINGS_VERSION: u32 = 1;
/// Default preload count in each direction.
const DEFAULT_COUNT: usize = 5;
/// Default decoded-pixel budget in MiB.
const DEFAULT_MEMORY_MIB: u64 = 256;
/// Largest accepted count per direction.
const MAX_COUNT: usize = 50;
/// Largest accepted decoded-pixel budget in MiB.
const MAX_MEMORY_MIB: u64 = 2_048;

/// User-facing prefetch limits. Zero counts or a zero budget disable prefetch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefetchSettings {
    /// How many previous images to keep preloaded.
    pub before: usize,
    /// How many next images to keep preloaded.
    pub after: usize,
    /// Decoded-pixel budget for the preload cache, in MiB.
    pub memory_mib: u64,
}

impl Default for PrefetchSettings {
    fn default() -> Self {
        Self {
            before: DEFAULT_COUNT,
            after: DEFAULT_COUNT,
            memory_mib: DEFAULT_MEMORY_MIB,
        }
    }
}

impl PrefetchSettings {
    /// Rejects values outside the documented UI bounds.
    pub fn validate(&self) -> Result<(), String> {
        if self.before > MAX_COUNT {
            return Err(format!(
                "before must be at most {MAX_COUNT}, got {}",
                self.before
            ));
        }
        if self.after > MAX_COUNT {
            return Err(format!(
                "after must be at most {MAX_COUNT}, got {}",
                self.after
            ));
        }
        if self.memory_mib > MAX_MEMORY_MIB {
            return Err(format!(
                "memory_mib must be at most {MAX_MEMORY_MIB}, got {}",
                self.memory_mib
            ));
        }
        Ok(())
    }
}

/// Page layout mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingMode {
    /// One file per view.
    #[default]
    Single,
    /// Cover alone, then right-to-left spreads of two files.
    Book,
}

/// Which arrow key advances to the next spread in book mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComicArrowDirection {
    /// Right arrow goes to the next spread.
    #[default]
    RightNext,
    /// Left arrow goes to the next spread.
    LeftNext,
}

/// UI language selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    /// Follow the system locale.
    #[default]
    Automatic,
    /// English.
    English,
    /// Simplified Chinese.
    SimplifiedChinese,
    /// Japanese.
    Japanese,
}

/// Everything the Settings UI edits and persists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppSettings {
    /// Cache parameters, handed to the prefetcher unchanged.
    pub prefetch: PrefetchSettings,
    /// Whether the bottom thumbnail strip is shown.
    pub thumbnails_visible: bool,
    /// Single-page or right-to-left book layout.
    pub reading_mode: ReadingMode,
    /// Whether the right or left arrow advances in book mode.
    pub comic_arrow_direction: ComicArrowDirection,
    /// UI language, or Automatic to follow the system locale.
    pub language: Language,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            prefetch: PrefetchSettings::default(),
            thumbnails_visible: true,
            reading_mode: ReadingMode::default(),
            comic_arrow_direction: ComicArrowDirection::default(),
            language: Language::default(),
        }
    }
}

impl AppSettings {
    /// Rejects invalid cache limits; the reading options are always valid.
    pub fn validate(&self) -> Result<(), String> {
        self.prefetch.validate()
    }
}

/// Settings returned by [`load_settings`] plus a non-fatal problem, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedSettings {
    pub settings: AppSettings,
    /// `Some` when the file was unusable and `settings` fell back to defaults.
    pub warning: Option<String>,
}

fn default_thumbnails_visible() -> bool {
    true
}

/// On-disk shape, including the schema version.
///
/// Cache fields stay flat for compatibility. New fields are optional so a
/// version-1 file loads with defaults for them; unknown fields are still
/// rejected by the version-specific validation below.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsFile {
    version: u32,
    before: usize,
    after: usize,
    memory_mib: u64,
    #[serde(default = "default_thumbnails_visible")]
    thumbnails_visible: bool,
    #[serde(default)]
    reading_mode: ReadingMode,
    #[serde(default)]
    comic_arrow_direction: ComicArrowDirection,
    #[serde(default)]
    language: Language,
}

impl SettingsFile {
    fn from_settings(settings: &AppSettings) -> Self {
        Self {
            version: SETTINGS_VERSION,
            before: settings.prefetch.before,
            after: settings.prefetch.after,
            memory_mib: settings.prefetch.memory_mib,
            thumbnails_visible: settings.thumbnails_visible,
            reading_mode: settings.reading_mode,
            comic_arrow_direction: settings.comic_arrow_direction,
            language: settings.language,
        }
    }

    fn into_settings(self) -> AppSettings {
        AppSettings {
            prefetch: PrefetchSettings {
                before: self.before,
                after: self.after,
                memory_mib: self.memory_mib,
            },
            thumbnails_visible: self.thumbnails_visible,
            reading_mode: self.reading_mode,
            comic_arrow_direction: self.comic_arrow_direction,
            language: self.language,
        }
    }
}

/// Resolves an explicit or automatic language choice.
///
/// Explicit selections are returned unchanged. Automatic uses the process
/// locale once; this does not watch for later OS language changes.
pub fn resolve_language(selected: Language) -> Language {
    match selected {
        Language::Automatic => resolve_locale_tag(sys_locale::get_locale().as_deref()),
        explicit => explicit,
    }
}

fn resolve_locale_tag(raw: Option<&str>) -> Language {
    let Some(raw) = raw else {
        return Language::English;
    };
    let tag = raw.trim().to_ascii_lowercase().replace('_', "-");
    if tag == "en" || tag.starts_with("en-") {
        Language::English
    } else if tag == "ja" || tag.starts_with("ja-") {
        Language::Japanese
    } else if tag == "zh" || tag == "zh-cn" || tag == "zh-sg" || tag.starts_with("zh-hans") {
        Language::SimplifiedChinese
    } else {
        Language::English
    }
}

/// Reads persisted settings, falling back to defaults with a warning.
///
/// A missing file is not a warning. Invalid, oversized, unreadable, or
/// unsupported files are never rewritten here.
pub fn load_settings() -> LoadedSettings {
    match settings_path() {
        Some(path) => load_settings_from(&path),
        None => LoadedSettings {
            settings: AppSettings::default(),
            warning: None,
        },
    }
}

/// Validates and atomically persists `settings`, creating the config directory.
pub fn save_settings(settings: &AppSettings) -> Result<(), String> {
    let path = settings_path().ok_or("no user config directory is available")?;
    save_settings_to(&path, settings)
}

fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("oxeye").join("settings.json"))
}

fn load_settings_from(path: &Path) -> LoadedSettings {
    let defaults = || LoadedSettings {
        settings: AppSettings::default(),
        warning: None,
    };
    let warned = |message: String| LoadedSettings {
        settings: AppSettings::default(),
        warning: Some(format!("{}: {message}", path.display())),
    };

    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return defaults(),
        Err(error) => return warned(error.to_string()),
    };
    if file
        .metadata()
        .map(|metadata| metadata.len() > MAX_SETTINGS_BYTES)
        .unwrap_or(true)
    {
        return warned(format!("file is larger than {MAX_SETTINGS_BYTES} bytes"));
    }

    let mut bytes = Vec::new();
    if let Err(error) = file.take(MAX_SETTINGS_BYTES + 1).read_to_end(&mut bytes) {
        return warned(error.to_string());
    }
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return warned(format!("file is larger than {MAX_SETTINGS_BYTES} bytes"));
    }

    let file: SettingsFile = match serde_json::from_slice(&bytes) {
        Ok(file) => file,
        Err(error) => return warned(format!("invalid settings: {error}")),
    };
    if !(MIN_SETTINGS_VERSION..=SETTINGS_VERSION).contains(&file.version) {
        return warned(format!(
            "unsupported settings version {}, expected {MIN_SETTINGS_VERSION} through {SETTINGS_VERSION}",
            file.version
        ));
    }
    let settings = file.into_settings();
    if let Err(error) = settings.validate() {
        return warned(error);
    }
    LoadedSettings {
        settings,
        warning: None,
    }
}

fn save_settings_to(path: &Path, settings: &AppSettings) -> Result<(), String> {
    settings.validate()?;
    let directory = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;

    let mut file = tempfile::NamedTempFile::new_in(directory)
        .map_err(|error| format!("cannot create a temporary settings file: {error}"))?;
    serde_json::to_writer_pretty(&mut file, &SettingsFile::from_settings(settings))
        .map_err(|error| format!("cannot serialize settings: {error}"))?;
    file.as_file_mut()
        .flush()
        .map_err(|error| format!("cannot flush settings: {error}"))?;
    file.as_file()
        .sync_all()
        .map_err(|error| format!("cannot sync settings: {error}"))?;
    file.persist(path)
        .map_err(|error| format!("cannot replace {}: {}", path.display(), error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("oxeye-settings-{}-{unique}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn app(prefetch: PrefetchSettings) -> AppSettings {
        AppSettings {
            prefetch,
            ..AppSettings::default()
        }
    }

    #[test]
    fn defaults_are_five_five_256_and_valid() {
        let settings = AppSettings::default();
        assert_eq!(settings.prefetch.before, 5);
        assert_eq!(settings.prefetch.after, 5);
        assert_eq!(settings.prefetch.memory_mib, 256);
        assert!(settings.thumbnails_visible);
        assert_eq!(settings.reading_mode, ReadingMode::Single);
        assert_eq!(
            settings.comic_arrow_direction,
            ComicArrowDirection::RightNext
        );
        assert_eq!(settings.language, Language::Automatic);
        assert_eq!(settings.validate(), Ok(()));
    }

    #[test]
    fn validation_accepts_bounds_and_rejects_overflow() {
        for (before, after, memory) in [(0, 0, 0), (50, 50, 2048), (1, 49, 64)] {
            assert_eq!(
                app(PrefetchSettings {
                    before,
                    after,
                    memory_mib: memory
                })
                .validate(),
                Ok(())
            );
        }
        for settings in [
            PrefetchSettings {
                before: 51,
                ..Default::default()
            },
            PrefetchSettings {
                after: 51,
                ..Default::default()
            },
            PrefetchSettings {
                memory_mib: 2049,
                ..Default::default()
            },
            PrefetchSettings {
                memory_mib: u64::MAX,
                ..Default::default()
            },
        ] {
            assert!(app(settings).validate().is_err());
        }
    }

    #[test]
    fn missing_file_uses_defaults_without_warning() {
        let dir = TempDir::new();
        let loaded = load_settings_from(&dir.path().join("settings.json"));
        assert_eq!(loaded.settings, AppSettings::default());
        assert_eq!(loaded.warning, None);
    }

    #[test]
    fn round_trip_preserves_values() {
        let dir = TempDir::new();
        let path = dir.path().join("oxeye/settings.json");
        let settings = AppSettings {
            prefetch: PrefetchSettings {
                before: 3,
                after: 7,
                memory_mib: 128,
            },
            thumbnails_visible: false,
            reading_mode: ReadingMode::Book,
            comic_arrow_direction: ComicArrowDirection::LeftNext,
            language: Language::Japanese,
        };
        save_settings_to(&path, &settings).expect("save settings");
        let loaded = load_settings_from(&path);
        assert_eq!(loaded.settings, settings);
        assert_eq!(loaded.warning, None);

        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["version"], 3);
        assert_eq!(written["before"], 3);
        assert_eq!(written["after"], 7);
        assert_eq!(written["memory_mib"], 128);
        assert_eq!(written["thumbnails_visible"], false);
        assert_eq!(written["reading_mode"], "book");
        assert_eq!(written["comic_arrow_direction"], "left_next");
        assert_eq!(written["language"], "japanese");
    }

    #[test]
    fn version_three_round_trips_each_language() {
        let dir = TempDir::new();
        let path = dir.path().join("settings.json");
        for (language, expected) in [
            (Language::Automatic, "automatic"),
            (Language::English, "english"),
            (Language::SimplifiedChinese, "simplified_chinese"),
            (Language::Japanese, "japanese"),
        ] {
            let settings = AppSettings {
                language,
                ..AppSettings::default()
            };
            save_settings_to(&path, &settings).expect("save settings");
            let loaded = load_settings_from(&path);
            assert_eq!(loaded.settings, settings);
            assert_eq!(loaded.warning, None);
            let written: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(written["version"], 3);
            assert_eq!(written["language"], expected);
        }
    }

    #[test]
    fn version_one_migrates_without_losing_custom_values() {
        let dir = TempDir::new();
        let path = dir.path().join("settings.json");
        let original = br#"{"version":1,"before":2,"after":7,"memory_mib":128}"#;
        std::fs::write(&path, original).unwrap();

        let loaded = load_settings_from(&path);
        assert_eq!(loaded.warning, None);
        assert_eq!(
            loaded.settings,
            AppSettings {
                prefetch: PrefetchSettings {
                    before: 2,
                    after: 7,
                    memory_mib: 128,
                },
                ..AppSettings::default()
            }
        );
        assert_eq!(loaded.settings.language, Language::Automatic);
        // Loading never rewrites the file; migration happens on the next save.
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn version_two_migrates_custom_values_and_defaults_language() {
        let dir = TempDir::new();
        let path = dir.path().join("settings.json");
        let original = br#"{"version":2,"before":4,"after":6,"memory_mib":64,"thumbnails_visible":false,"reading_mode":"book","comic_arrow_direction":"left_next"}"#;
        std::fs::write(&path, original).unwrap();

        let loaded = load_settings_from(&path);
        assert_eq!(loaded.warning, None);
        assert_eq!(
            loaded.settings,
            AppSettings {
                prefetch: PrefetchSettings {
                    before: 4,
                    after: 6,
                    memory_mib: 64,
                },
                thumbnails_visible: false,
                reading_mode: ReadingMode::Book,
                comic_arrow_direction: ComicArrowDirection::LeftNext,
                language: Language::Automatic,
            }
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn corrupt_oversized_unknown_version_and_unknown_field_fall_back() {
        let dir = TempDir::new();
        let path = dir.path().join("settings.json");

        std::fs::write(&path, b"not json").unwrap();
        let loaded = load_settings_from(&path);
        assert_eq!(loaded.settings, AppSettings::default());
        assert!(loaded.warning.is_some());

        std::fs::write(
            &path,
            r#"{"version":1,"before":1,"after":1,"memory_mib":1}"#
                .repeat(MAX_SETTINGS_BYTES as usize / 40 + 2),
        )
        .unwrap();
        let loaded = load_settings_from(&path);
        assert_eq!(loaded.settings, AppSettings::default());
        assert!(loaded.warning.is_some());

        let future = br#"{"version":4,"before":1,"after":1,"memory_mib":1}"#;
        std::fs::write(&path, future).unwrap();
        let loaded = load_settings_from(&path);
        assert_eq!(loaded.settings, AppSettings::default());
        assert!(loaded.warning.as_deref().unwrap_or("").contains("version"));
        assert_eq!(std::fs::read(&path).unwrap(), future);

        std::fs::write(
            &path,
            br#"{"version":1,"before":99,"after":1,"memory_mib":1}"#,
        )
        .unwrap();
        let loaded = load_settings_from(&path);
        assert_eq!(loaded.settings, AppSettings::default());
        assert!(loaded.warning.is_some());

        std::fs::write(
            &path,
            br#"{"version":2,"before":1,"after":1,"memory_mib":1,"bogus":true}"#,
        )
        .unwrap();
        let loaded = load_settings_from(&path);
        assert_eq!(loaded.settings, AppSettings::default());
        assert!(loaded.warning.is_some());

        std::fs::write(
            &path,
            br#"{"version":2,"before":1,"after":1,"reading_mode":"book"}"#,
        )
        .unwrap();
        let loaded = load_settings_from(&path);
        assert_eq!(loaded.settings, AppSettings::default());
        assert!(loaded.warning.is_some());

        let invalid_language =
            br#"{"version":3,"before":1,"after":1,"memory_mib":1,"language":"klingon"}"#;
        std::fs::write(&path, invalid_language).unwrap();
        let loaded = load_settings_from(&path);
        assert_eq!(loaded.settings, AppSettings::default());
        assert!(loaded.warning.is_some());
        assert_eq!(std::fs::read(&path).unwrap(), invalid_language);
    }

    #[test]
    fn future_version_is_rejected_without_rewriting() {
        let dir = TempDir::new();
        let path = dir.path().join("settings.json");
        let original =
            br#"{"version":4,"before":2,"after":3,"memory_mib":128,"language":"japanese"}"#;
        std::fs::write(&path, original).unwrap();

        let loaded = load_settings_from(&path);
        assert_eq!(loaded.settings, AppSettings::default());
        assert!(loaded.warning.as_deref().unwrap_or("").contains("version"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn resolve_locale_aliases_and_fallbacks() {
        for (tag, expected) in [
            (Some("en-US"), Language::English),
            (Some("en_US"), Language::English),
            (Some("ja-JP"), Language::Japanese),
            (Some("zh-Hans"), Language::SimplifiedChinese),
            (Some("zh_Hans"), Language::SimplifiedChinese),
            (Some("zh-CN"), Language::SimplifiedChinese),
            (Some("zh-SG"), Language::SimplifiedChinese),
            (Some("zh"), Language::SimplifiedChinese),
            (Some("zh-Hant"), Language::English),
            (Some("zh-TW"), Language::English),
            (Some("zh-HK"), Language::English),
            (Some("zh-MO"), Language::English),
            (Some("C"), Language::English),
            (Some("POSIX"), Language::English),
            (None, Language::English),
            (Some("fr-FR"), Language::English),
            (Some("unknown"), Language::English),
            (Some(""), Language::English),
        ] {
            assert_eq!(resolve_locale_tag(tag), expected, "locale {tag:?}");
        }
    }

    #[test]
    fn explicit_language_selection_skips_locale_resolution() {
        assert_eq!(resolve_language(Language::English), Language::English);
        assert_eq!(
            resolve_language(Language::SimplifiedChinese),
            Language::SimplifiedChinese
        );
        assert_eq!(resolve_language(Language::Japanese), Language::Japanese);
    }

    #[test]
    fn invalid_save_is_rejected_and_preserves_previous_file() {
        let dir = TempDir::new();
        let path = dir.path().join("settings.json");
        let good = AppSettings {
            prefetch: PrefetchSettings {
                before: 2,
                after: 4,
                memory_mib: 64,
            },
            ..AppSettings::default()
        };
        save_settings_to(&path, &good).expect("initial save");
        let before = std::fs::read(&path).unwrap();

        let invalid = app(PrefetchSettings {
            before: 51,
            ..Default::default()
        });
        assert!(save_settings_to(&path, &invalid).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(load_settings_from(&path).settings, good);
    }

    #[test]
    fn failed_persist_leaves_existing_file_intact() {
        let dir = TempDir::new();
        let path = dir.path().join("settings.json");
        let good = AppSettings {
            prefetch: PrefetchSettings {
                before: 1,
                after: 2,
                memory_mib: 32,
            },
            ..AppSettings::default()
        };
        save_settings_to(&path, &good).expect("initial save");
        let before = std::fs::read(&path).unwrap();

        // A directory at the target makes persist fail without removing it.
        let blocked = dir.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        let result = save_settings_to(&blocked, &AppSettings::default());
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(blocked.is_dir());
    }
}
