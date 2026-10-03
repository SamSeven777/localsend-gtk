//! Receive history metadata. Removing entries never removes received files.
//!
//! A failed load must remain an error: callers must not replace unreadable history
//! with an empty writable history, which would overwrite the original on save.
use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
};

const FORMAT_VERSION: u32 = 1;
const MAX_ENTRIES: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryEntry {
    pub id: String,
    /// RFC 3339 UTC for structured entries; old log strings have no reliable instant.
    pub timestamp: Option<String>,
    pub kind: HistoryKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HistoryKind {
    File {
        name: String,
        path: PathBuf,
        size: u64,
        sender: String,
    },
    Message {
        text: String,
        sender: String,
    },
    /// Preserve old logs verbatim; they may include sent transfers or errors.
    /// Their filenames, sender and local time cannot be recovered reliably.
    Legacy {
        text: String,
    },
}

impl HistoryEntry {
    /// Format the recorded instant in the current local time zone.
    pub fn timestamp_display(&self) -> Option<String> {
        let timestamp = DateTime::parse_from_rfc3339(self.timestamp.as_deref()?).ok()?;
        Some(
            timestamp
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct History {
    // Oldest first, preserving the original Vec<String> order.
    entries: Vec<HistoryEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredHistory {
    version: u32,
    entries: Vec<HistoryEntry>,
}

#[derive(Serialize)]
struct StoredHistoryRef<'a> {
    version: u32,
    entries: &'a [HistoryEntry],
}

impl History {
    /// Missing files start empty. Invalid JSON, unsupported versions and other
    /// read failures are returned without touching the original file.
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(invalid_data)?;
        let entries = if value.is_array() {
            let legacy: Vec<String> = serde_json::from_value(value).map_err(invalid_data)?;
            // Do not truncate on migration or infer structure from display text.
            // IDs become durable when this successfully loaded history is saved.
            legacy
                .into_iter()
                .map(|text| HistoryEntry {
                    id: uuid::Uuid::new_v4().to_string(),
                    timestamp: None,
                    kind: HistoryKind::Legacy { text },
                })
                .collect()
        } else {
            let stored: StoredHistory = serde_json::from_value(value).map_err(invalid_data)?;
            if stored.version != FORMAT_VERSION {
                return Err(invalid_data(format!(
                    "Unsupported history version: {}",
                    stored.version
                )));
            }
            stored.entries
        };
        let mut ids = HashSet::with_capacity(entries.len());
        for entry in &entries {
            let id = uuid::Uuid::parse_str(&entry.id).map_err(invalid_data)?;
            if !ids.insert(id) {
                return Err(invalid_data("Duplicate history entry ID"));
            }
            match &entry.timestamp {
                Some(timestamp) => {
                    DateTime::parse_from_rfc3339(timestamp).map_err(invalid_data)?;
                }
                None if matches!(&entry.kind, HistoryKind::Legacy { .. }) => {}
                None => return Err(invalid_data("Missing history entry timestamp")),
            }
        }
        Ok(Self { entries })
    }

    /// Save only after a successful load (or an explicitly chosen fresh history).
    pub fn save(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(&StoredHistoryRef {
            version: FORMAT_VERSION,
            entries: &self.entries,
        })
        .map_err(invalid_data)?;
        crate::settings::atomic_write(path.as_ref(), &bytes)
    }

    /// Entries are oldest first; reverse the iterator for the receive history UI.
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    pub fn add_file(
        &mut self,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        size: u64,
        sender: impl Into<String>,
    ) -> String {
        self.push(HistoryKind::File {
            name: name.into(),
            path: path.into(),
            size,
            sender: sender.into(),
        })
    }

    pub fn add_message(&mut self, text: impl Into<String>, sender: impl Into<String>) -> String {
        self.push(HistoryKind::Message {
            text: text.into(),
            sender: sender.into(),
        })
    }

    fn push(&mut self, kind: HistoryKind) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.entries.push(HistoryEntry {
            id: id.clone(),
            timestamp: Some(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
            kind,
        });
        if self.entries.len() > MAX_ENTRIES {
            self.entries.drain(..self.entries.len() - MAX_ENTRIES);
        }
        id
    }

    /// Forget metadata only; the received file or message content is not acted on.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.id != id);
        self.entries.len() != before
    }

    /// Forget every entry without deleting or modifying any received file.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_history_starts_empty_without_creating_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        assert!(History::load(&path).unwrap().entries().is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn legacy_migration_preserves_every_string_order_and_durable_id() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        let mut original: Vec<_> = (0..100)
            .map(|index| format!("2026-01-01 12:00  ·  Entry {index} 中文"))
            .collect();
        original[1] = original[0].clone();
        original[2] = "A message\nwith a newline and a · separator".into();
        let bytes = serde_json::to_vec(&original).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let history = History::load(&path).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "Loading migration is read-only"
        );
        assert_eq!(history.entries().len(), original.len());
        let ids: HashSet<_> = history.entries().iter().map(|entry| &entry.id).collect();
        assert_eq!(
            ids.len(),
            original.len(),
            "Even identical legacy log lines have distinct IDs"
        );
        for (entry, text) in history.entries().iter().zip(original) {
            assert_eq!(entry.kind, HistoryKind::Legacy { text });
            assert!(entry.timestamp.is_none());
            assert!(entry.timestamp_display().is_none());
        }
        history.save(&path).unwrap();
        assert_eq!(History::load(&path).unwrap(), history);
    }

    #[test]
    fn structured_history_roundtrips_files_messages_and_timestamps() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/history.json");
        let received = directory.path().join("folder/照片.jpg");
        let mut history = History::default();
        let file_id = history.add_file("folder/照片.jpg", &received, 12345, "Kind Apple");
        let message_id = history.add_message("Settings\n<Hello 🌍>", "Send");
        assert_ne!(file_id, message_id);
        assert!(uuid::Uuid::parse_str(&file_id).is_ok());
        assert_eq!(
            history.entries()[0].kind,
            HistoryKind::File {
                name: "folder/照片.jpg".into(),
                path: received,
                size: 12345,
                sender: "Kind Apple".into(),
            }
        );
        assert_eq!(
            history.entries()[1].kind,
            HistoryKind::Message {
                text: "Settings\n<Hello 🌍>".into(),
                sender: "Send".into()
            }
        );
        for entry in history.entries() {
            let recorded =
                DateTime::parse_from_rfc3339(entry.timestamp.as_deref().unwrap()).unwrap();
            assert_eq!(recorded.offset().local_minus_utc(), 0);
            assert_eq!(
                entry.timestamp_display().unwrap(),
                recorded
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            );
        }
        history.save(&path).unwrap();
        assert_eq!(History::load(&path).unwrap(), history);
    }

    #[test]
    fn invalid_history_is_an_error_and_is_never_rewritten_on_load() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        for bytes in [
            b"{".as_slice(),
            br#"["valid old entry", 42]"#,
            br#"{"version":2,"entries":[]}"#,
            br#"{"version":1,"entries":[{"id":"bad","timestamp":null,"kind":{"type":"legacy","text":"keep"}}]}"#,
            br#"{"version":1,"entries":[],"unknown_new_field":"keep"}"#,
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(History::load(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        assert!(
            History::load(directory.path()).is_err(),
            "Other read failures must not become empty history"
        );
    }

    #[test]
    fn duplicate_ids_and_missing_or_invalid_timestamps_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        let mut history = History::default();
        history.add_message("hello", "sender");
        let entry = serde_json::to_value(&history.entries()[0]).unwrap();
        let duplicate = serde_json::json!({"version":1,"entries":[entry.clone(), entry.clone()]});
        std::fs::write(&path, serde_json::to_vec(&duplicate).unwrap()).unwrap();
        assert!(History::load(&path).is_err());
        for timestamp in [serde_json::Value::Null, serde_json::json!("yesterday")] {
            let mut entry = entry.clone();
            entry["timestamp"] = timestamp;
            std::fs::write(
                &path,
                serde_json::to_vec(&serde_json::json!({"version":1,"entries":[entry]})).unwrap(),
            )
            .unwrap();
            assert!(History::load(&path).is_err());
        }
    }

    #[test]
    fn removing_and_clearing_history_never_delete_received_files() {
        let directory = tempfile::tempdir().unwrap();
        let received = directory.path().join("received.txt");
        let saved_history = directory.path().join("history.json");
        std::fs::write(&received, b"keep these bytes").unwrap();
        let mut history = History::default();
        let id = history.add_file("received.txt", &received, 16, "sender");
        history.add_message("keep this message until cleared", "sender");
        assert!(!history.remove("unknown"));
        assert!(history.remove(&id));
        assert!(!history.remove(&id));
        history.save(&saved_history).unwrap();
        assert_eq!(std::fs::read(&received).unwrap(), b"keep these bytes");
        assert_eq!(History::load(&saved_history).unwrap().entries().len(), 1);
        history.add_file("received.txt", &received, 16, "sender");
        history.clear();
        history.save(&saved_history).unwrap();
        assert_eq!(std::fs::read(&received).unwrap(), b"keep these bytes");
        assert!(History::load(&saved_history).unwrap().entries().is_empty());
    }

    #[test]
    fn new_entries_retain_the_latest_hundred_in_original_order() {
        let mut history = History::default();
        for index in 0..105 {
            history.add_message(index.to_string(), "sender");
        }
        assert_eq!(history.entries().len(), MAX_ENTRIES);
        assert_eq!(
            history.entries()[0].kind,
            HistoryKind::Message {
                text: "5".into(),
                sender: "sender".into()
            }
        );
        assert_eq!(
            history.entries()[99].kind,
            HistoryKind::Message {
                text: "104".into(),
                sender: "sender".into()
            }
        );
    }
}
