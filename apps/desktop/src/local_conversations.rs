//! Mutable local conversation workspace metadata.
//!
//! Conversation message history remains authoritative in the append-only journal. This store owns
//! user-editable workspace metadata that cannot be derived from message events: empty conversations,
//! explicit titles, archive state, ordering timestamps, and the selected local conversation.

use chatarium_core::LocalConversationId;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-local-conversation-catalog";
const VERSION: u64 = 1;
const MAX_TITLE_BYTES: usize = 512;
const MAX_PROJECT_DIRECTORY_BYTES: usize = 4096;

/// Imported/derived titles are not a user edit, so safely bound them before
/// storing. The on-disk catalog rejects titles exceeding MAX_TITLE_BYTES at
/// reload time. Truncate only at a UTF-8 boundary to keep save/load symmetric.
fn bounded_derived_title(title: Option<String>) -> Option<String> {
    let title = title?;
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    let mut end = title.len().min(MAX_TITLE_BYTES);
    while !title.is_char_boundary(end) {
        end -= 1;
    }
    let title = title[..end].trim_end();
    (!title.is_empty()).then(|| title.to_owned())
}

/// Local chat accents are durable UI metadata, not part of a conversation title.
/// The palette is spread around the hue wheel, with candidate selection maximizing
/// distance from accents already allocated in this catalog.
fn decode_color(hex: &str) -> Option<[u8; 3]> {
    if hex.len() != 7 || !hex.starts_with('#') {
        return None;
    }
    let value = u32::from_str_radix(&hex[1..], 16).ok()?;
    Some([(value >> 16) as u8, (value >> 8) as u8, value as u8])
}

fn hsv_rgb(hue: f32, saturation: f32, value: f32) -> [u8; 3] {
    let sector = hue / 60.0;
    let chroma = value * saturation;
    let second = chroma * (1.0 - ((sector % 2.0) - 1.0).abs());
    let (r, g, b) = match sector as u32 {
        0 => (chroma, second, 0.0),
        1 => (second, chroma, 0.0),
        2 => (0.0, chroma, second),
        3 => (0.0, second, chroma),
        4 => (second, 0.0, chroma),
        _ => (chroma, 0.0, second),
    };
    let minimum = value - chroma;
    [
        ((r + minimum) * 255.0).round() as u8,
        ((g + minimum) * 255.0).round() as u8,
        ((b + minimum) * 255.0).round() as u8,
    ]
}

fn allocate_color(
    id: LocalConversationId,
    entries: &BTreeMap<String, LocalConversationEntry>,
) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in id.to_string().bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    let existing = entries
        .values()
        .filter_map(|entry| decode_color(&entry.color))
        .collect::<Vec<_>>();
    let mut best = None::<([u8; 3], u32)>;
    for attempt in 0..128_u64 {
        let mut seed = hash ^ attempt.wrapping_mul(0x9e3779b97f4a7c15);
        seed ^= seed >> 30;
        seed = seed.wrapping_mul(0xbf58476d1ce4e5b9);
        seed ^= seed >> 27;
        seed = seed.wrapping_mul(0x94d049bb133111eb);
        seed ^= seed >> 31;
        let rgb = hsv_rgb(
            (seed % 360) as f32,
            0.66 + ((seed >> 12) % 22) as f32 / 100.0,
            0.82 + ((seed >> 24) % 16) as f32 / 100.0,
        );
        if existing.contains(&rgb) {
            continue;
        }
        let score = existing
            .iter()
            .map(|other| {
                rgb.iter()
                    .zip(other)
                    .map(|(a, b)| (i32::from(*a) - i32::from(*b)).pow(2) as u32)
                    .sum::<u32>()
            })
            .min()
            .unwrap_or(u32::MAX);
        if best.is_none_or(|(_, previous)| score > previous) {
            best = Some((rgb, score));
        }
    }
    let rgb = best.map(|(rgb, _)| rgb).unwrap_or([240, 172, 108]);
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

impl LocalConversationEntry {
    #[must_use]
    pub fn accent_rgb(&self) -> [u8; 3] {
        decode_color(&self.color).unwrap_or([240, 172, 108])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalConversationEntry {
    pub id: LocalConversationId,
    pub title: Option<String>,
    pub color: String,
    pub project_directory: Option<String>,
    pub archived: bool,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalConversationCatalog {
    active: Option<LocalConversationId>,
    entries: BTreeMap<String, LocalConversationEntry>,
}

impl LocalConversationCatalog {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path)
            .map_err(|error| format!("failed to read local conversation catalog: {error}"))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid local conversation catalog JSON: {error}"))?;
        if value.get("schema").and_then(Value::as_str) != Some(SCHEMA)
            || value.get("version").and_then(Value::as_u64) != Some(VERSION)
        {
            return Err("unsupported local conversation catalog schema".to_owned());
        }
        let active = value
            .get("active_conversation_id")
            .and_then(Value::as_str)
            .map(LocalConversationId::from_str)
            .transpose()
            .map_err(|error| format!("invalid active local conversation id: {error}"))?;
        let items = value
            .get("conversations")
            .and_then(Value::as_array)
            .ok_or_else(|| "local conversation catalog items missing".to_owned())?;
        let mut entries = BTreeMap::new();
        for (index, raw) in items.iter().enumerate() {
            let object = raw
                .as_object()
                .ok_or_else(|| format!("local conversation item {index} is not an object"))?;
            let id_text = object
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("local conversation item {index} id missing"))?;
            let id = LocalConversationId::from_str(id_text)
                .map_err(|error| format!("invalid local conversation id: {error}"))?;
            let title = object
                .get("title")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            if title
                .as_ref()
                .is_some_and(|title| title.len() > MAX_TITLE_BYTES)
            {
                return Err("local conversation title is too large".to_owned());
            }
            let project_directory = object
                .get("project_directory")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            if let Some(directory) = project_directory.as_deref() {
                if !Path::new(directory).is_absolute()
                    || directory.len() > MAX_PROJECT_DIRECTORY_BYTES
                    || directory.contains('\0')
                {
                    return Err("invalid local conversation project directory".to_owned());
                }
            }
            let archived = object
                .get("archived")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let created_at_unix_ms = object
                .get("created_at_unix_ms")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let updated_at_unix_ms = object
                .get("updated_at_unix_ms")
                .and_then(Value::as_u64)
                .unwrap_or(created_at_unix_ms);
            let color = object
                .get("color")
                .and_then(Value::as_str)
                .filter(|value| decode_color(value).is_some())
                .filter(|value| {
                    !entries.values().any(|entry: &LocalConversationEntry| {
                        entry.color.eq_ignore_ascii_case(value)
                    })
                })
                .map(|value| value.to_ascii_uppercase())
                .unwrap_or_else(|| allocate_color(id, &entries));
            if entries
                .insert(
                    id.to_string(),
                    LocalConversationEntry {
                        id,
                        title,
                        color,
                        project_directory,
                        archived,
                        created_at_unix_ms,
                        updated_at_unix_ms,
                    },
                )
                .is_some()
            {
                return Err("duplicate local conversation id".to_owned());
            }
        }
        if active.is_some_and(|id| !entries.contains_key(&id.to_string())) {
            return Err("active local conversation is absent from catalog".to_owned());
        }
        Ok(Self { active, entries })
    }

    pub fn save_atomic(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create local catalog directory: {error}"))?;
        }
        let conversations = self
            .entries()
            .into_iter()
            .map(|entry| {
                json!({
                    "id": entry.id.to_string(),
                    "title": entry.title,
                    "color": entry.color,
                    "project_directory": entry.project_directory,
                    "archived": entry.archived,
                    "created_at_unix_ms": entry.created_at_unix_ms,
                    "updated_at_unix_ms": entry.updated_at_unix_ms,
                })
            })
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec_pretty(&json!({
            "schema": SCHEMA,
            "version": VERSION,
            "active_conversation_id": self.active.map(|id| id.to_string()),
            "conversations": conversations,
        }))
        .map_err(|error| format!("failed to encode local conversation catalog: {error}"))?;
        // Do not reuse a predictable staging name. A legacy .json.tmp symlink
        // could redirect a write to a different file; create_new prevents that
        // and also avoids clobbering artifacts from interrupted saves.
        static STAGING_ORDINAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let ordinal = STAGING_ORDINAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temporary = path.with_extension(format!(
            "json.tmp.{}.{}.{ordinal}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| format!("local catalog staging timestamp: {error}"))?
                .as_nanos(),
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| format!("failed to stage local conversation catalog: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(error) = file.set_permissions(fs::Permissions::from_mode(0o600)) {
                drop(file);
                let _ = fs::remove_file(&temporary);
                return Err(format!("local conversation catalog permissions: {error}"));
            }
        }
        use std::io::Write;
        if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "failed to persist local conversation catalog: {error}"
            ));
        }
        drop(file);
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "failed to replace local conversation catalog: {error}"
            ));
        }
        // Atomic rename is not crash-durable on Unix until the containing
        // directory's metadata is synced too.
        #[cfg(unix)]
        {
            let parent = path
                .parent()
                .ok_or("local conversation catalog parent missing")?;
            fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("local conversation catalog directory sync: {error}"))?;
        }
        Ok(())
    }

    #[must_use]
    pub fn active(&self) -> Option<LocalConversationId> {
        self.active
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn ensure(
        &mut self,
        id: LocalConversationId,
        derived_title: Option<String>,
        now_ms: u64,
    ) -> bool {
        let key = id.to_string();
        let derived_title = bounded_derived_title(derived_title);
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.title.is_none() && derived_title.is_some() {
                entry.title = derived_title;
                return true;
            }
            return false;
        }
        let color = allocate_color(id, &self.entries);
        self.entries.insert(
            key,
            LocalConversationEntry {
                id,
                color,
                title: derived_title,
                project_directory: None,
                archived: false,
                created_at_unix_ms: now_ms,
                updated_at_unix_ms: now_ms,
            },
        );
        true
    }

    pub fn create(&mut self, id: LocalConversationId, now_ms: u64) {
        // A repeated create request must never destroy a durable conversation's
        // title, project binding, archive flag, color, or timestamps, nor
        // silently change the active conversation.
        if self.entries.contains_key(&id.to_string()) {
            return;
        }
        let color = allocate_color(id, &self.entries);
        self.entries.insert(
            id.to_string(),
            LocalConversationEntry {
                id,
                title: None,
                color,
                project_directory: None,
                archived: false,
                created_at_unix_ms: now_ms,
                updated_at_unix_ms: now_ms,
            },
        );
        self.active = Some(id);
    }

    pub fn set_active(&mut self, id: LocalConversationId, now_ms: u64) -> Result<(), String> {
        let entry = self
            .entries
            .get_mut(&id.to_string())
            .ok_or_else(|| "local conversation is absent from catalog".to_owned())?;
        entry.updated_at_unix_ms = now_ms;
        self.active = Some(id);
        Ok(())
    }

    pub fn rename(
        &mut self,
        id: LocalConversationId,
        title: Option<String>,
        now_ms: u64,
    ) -> Result<(), String> {
        if title
            .as_ref()
            .is_some_and(|title| title.len() > MAX_TITLE_BYTES)
        {
            return Err("local conversation title is too large".to_owned());
        }
        let entry = self
            .entries
            .get_mut(&id.to_string())
            .ok_or_else(|| "local conversation is absent from catalog".to_owned())?;
        entry.title = title
            .map(|title| title.trim().to_owned())
            .filter(|title| !title.is_empty());
        entry.updated_at_unix_ms = now_ms;
        Ok(())
    }

    /// The directory belongs to the selected chat only; explicit shell cwd
    /// arguments may still override it. An empty value clears the binding.
    pub fn set_project_directory(
        &mut self,
        id: LocalConversationId,
        directory: Option<String>,
        now_ms: u64,
    ) -> Result<(), String> {
        let directory = directory
            .map(|directory| directory.trim().to_owned())
            .filter(|directory| !directory.is_empty())
            .map(|directory| {
                if directory.len() > MAX_PROJECT_DIRECTORY_BYTES
                    || directory.contains('\0')
                    || !Path::new(&directory).is_absolute()
                {
                    return Err("project folder must be an absolute local path".to_owned());
                }
                let canonical = fs::canonicalize(&directory)
                    .map_err(|error| format!("project folder not accessible: {error}"))?;
                if !canonical.is_dir() {
                    return Err("project folder must be an existing directory".to_owned());
                }
                canonical
                    .to_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| "project folder path must be UTF-8".to_owned())
            })
            .transpose()?;
        let entry = self
            .entries
            .get_mut(&id.to_string())
            .ok_or_else(|| "local conversation is absent from catalog".to_owned())?;
        entry.project_directory = directory;
        entry.updated_at_unix_ms = now_ms;
        Ok(())
    }

    pub fn set_archived(
        &mut self,
        id: LocalConversationId,
        archived: bool,
        now_ms: u64,
    ) -> Result<(), String> {
        let entry = self
            .entries
            .get_mut(&id.to_string())
            .ok_or_else(|| "local conversation is absent from catalog".to_owned())?;
        entry.archived = archived;
        entry.updated_at_unix_ms = now_ms;
        if archived && self.active == Some(id) {
            self.active = None;
        }
        Ok(())
    }

    #[must_use]
    pub fn entry(&self, id: LocalConversationId) -> Option<&LocalConversationEntry> {
        self.entries.get(&id.to_string())
    }

    #[must_use]
    pub fn entries(&self) -> Vec<LocalConversationEntry> {
        let mut entries = self.entries.values().cloned().collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            right
                .updated_at_unix_ms
                .cmp(&left.updated_at_unix_ms)
                .then_with(|| left.id.to_string().cmp(&right.id.to_string()))
        });
        entries
    }

    #[must_use]
    pub fn first_unarchived(&self) -> Option<LocalConversationId> {
        self.entries()
            .into_iter()
            .find(|entry| !entry.archived)
            .map(|entry| entry.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-local-conversations-{}-{nonce}.json",
            std::process::id()
        ))
    }

    #[cfg(unix)]
    #[test]
    fn save_is_private_and_ignores_legacy_staging_symlink() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = temp_path();
        fs::create_dir_all(&root).unwrap();
        let path = root.join("local-conversations.json");
        let legacy_staging = path.with_extension("json.tmp");
        let victim = root.join("unrelated-user-file");
        fs::write(&victim, "do not overwrite").unwrap();
        symlink(&victim, &legacy_staging).unwrap();

        let id = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(id, 1);
        catalog.save_atomic(&path).unwrap();
        assert_eq!(LocalConversationCatalog::load(&path).unwrap(), catalog);
        assert_eq!(fs::read_link(&legacy_staging).unwrap(), victim);
        assert_eq!(fs::read_to_string(&victim).unwrap(), "do not overwrite");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        catalog.rename(id, Some("Renamed".to_owned()), 2).unwrap();
        catalog.save_atomic(&path).unwrap();
        assert_eq!(LocalConversationCatalog::load(&path).unwrap(), catalog);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_to_string(&victim).unwrap(), "do not overwrite");
        let remaining = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(remaining.len(), 3);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn create_switch_rename_archive_and_restart() {
        let path = temp_path();
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(first, 1);
        catalog.create(second, 2);
        catalog.rename(first, Some("First".to_owned()), 3).unwrap();
        catalog.set_active(first, 4).unwrap();
        catalog.set_archived(second, true, 5).unwrap();
        catalog.save_atomic(&path).unwrap();

        let loaded = LocalConversationCatalog::load(&path).unwrap();
        assert_eq!(loaded.active(), Some(first));
        assert_eq!(
            loaded.entry(first).and_then(|entry| entry.title.as_deref()),
            Some("First")
        );
        assert!(loaded.entry(second).is_some_and(|entry| entry.archived));
        assert_eq!(
            loaded.entry(first).unwrap().color,
            catalog.entry(first).unwrap().color
        );
        assert_ne!(
            loaded.entry(first).unwrap().color,
            loaded.entry(second).unwrap().color
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn project_folder_is_per_chat_persistent_and_clearable() {
        let path = temp_path();
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(first, 1);
        catalog.create(second, 2);
        let directory = std::env::current_dir().unwrap();
        catalog
            .set_project_directory(first, Some(directory.display().to_string()), 3)
            .unwrap();
        assert_eq!(catalog.entry(second).unwrap().project_directory, None);
        assert!(
            catalog
                .set_project_directory(second, Some("relative-path".to_owned()), 4)
                .is_err()
        );
        catalog.save_atomic(&path).unwrap();
        let mut reloaded = LocalConversationCatalog::load(&path).unwrap();
        assert_eq!(
            reloaded.entry(first).unwrap().project_directory.as_deref(),
            directory.canonicalize().unwrap().to_str()
        );
        reloaded.set_project_directory(first, None, 5).unwrap();
        assert_eq!(reloaded.entry(first).unwrap().project_directory, None);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn legacy_catalog_colors_are_backfilled_and_stay_distinct() {
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(first, 1);
        catalog.create(second, 2);
        let path = temp_path();
        catalog.save_atomic(&path).unwrap();
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for entry in value["conversations"].as_array_mut().unwrap() {
            entry.as_object_mut().unwrap().remove("color");
        }
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let migrated = LocalConversationCatalog::load(&path).unwrap();
        assert_ne!(
            migrated.entry(first).unwrap().color,
            migrated.entry(second).unwrap().color
        );
        assert_eq!(migrated.entry(first).unwrap().accent_rgb().len(), 3);
        migrated.save_atomic(&path).unwrap();
        assert_eq!(LocalConversationCatalog::load(&path).unwrap(), migrated);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn oversized_derived_title_is_utf8_safe_and_restartable() {
        let path = temp_path();
        let id = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        let oversized = format!("  {}  ", "é".repeat(MAX_TITLE_BYTES));
        assert!(catalog.ensure(id, Some(oversized.clone()), 1));
        let title = catalog.entry(id).unwrap().title.as_deref().unwrap();
        assert_eq!(title.len(), MAX_TITLE_BYTES);
        assert_eq!(title.chars().count(), MAX_TITLE_BYTES / 2);
        assert!(title.chars().all(|ch| ch == 'é'));

        // The byte ceiling can land inside a multi-byte character; never
        // store an invalid boundary or a replacement character.
        let crab = bounded_derived_title(Some(format!(
            "{}{}",
            "a".repeat(MAX_TITLE_BYTES - 2),
            "🦀".repeat(4)
        )))
        .unwrap();
        assert_eq!(crab.len(), MAX_TITLE_BYTES - 2);
        assert!(crab.chars().all(|ch| ch == 'a'));

        catalog.save_atomic(&path).unwrap();
        let mut loaded = LocalConversationCatalog::load(&path).unwrap();
        assert_eq!(loaded, catalog);
        assert!(!loaded.ensure(id, Some("replacement must not win".to_owned()), 2));
        assert_eq!(loaded.entry(id).unwrap().title.as_deref(), Some(title));

        let empty = LocalConversationId::new();
        assert!(loaded.ensure(empty, Some("  \n  ".to_owned()), 3));
        assert!(loaded.entry(empty).unwrap().title.is_none());
        loaded.save_atomic(&path).unwrap();
        assert_eq!(LocalConversationCatalog::load(&path).unwrap(), loaded);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn repeated_create_preserves_metadata_archive_and_active_chat() {
        let path = temp_path();
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(first, 1);
        catalog.rename(first, Some("Keep title".to_owned()), 2).unwrap();
        catalog
            .set_project_directory(
                first,
                Some(std::env::current_dir().unwrap().display().to_string()),
                3,
            )
            .unwrap();
        catalog.create(second, 4);
        catalog.set_archived(first, true, 5).unwrap();
        let before = catalog.entry(first).unwrap().clone();

        catalog.create(first, 999_999);
        assert_eq!(catalog.active(), Some(second));
        assert_eq!(catalog.entry(first).unwrap(), &before);
        assert_eq!(catalog.entries().len(), 2);
        catalog.save_atomic(&path).unwrap();
        assert_eq!(LocalConversationCatalog::load(&path).unwrap(), catalog);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn conversations_remain_isolated_by_identity() {
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(first, 1);
        catalog.create(second, 2);
        catalog.rename(first, Some("Alpha".to_owned()), 3).unwrap();

        assert_eq!(
            catalog
                .entry(first)
                .and_then(|entry| entry.title.as_deref()),
            Some("Alpha")
        );
        assert_eq!(
            catalog
                .entry(second)
                .and_then(|entry| entry.title.as_deref()),
            None
        );
    }
}
