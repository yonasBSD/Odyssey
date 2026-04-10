//! Persistent input history stored as JSON at `~/.odyssey/history.json`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Maximum number of entries kept in the history file.
const MAX_ENTRIES: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct HistoryFile {
    entries: Vec<String>,
}

/// Resolve the history file path: `~/.odyssey/history.json`.
fn history_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".odyssey").join("tui_history.json"))
}

fn load_from_path(path: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str::<HistoryFile>(&raw)
        .map(|file| file.entries)
        .unwrap_or_default()
}

/// Load saved history entries from disk (oldest first).
pub fn load() -> Vec<String> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    load_from_path(&path)
}

fn push_to_path(path: &Path, entry: &str) {
    if entry.trim().is_empty() {
        return;
    }
    let mut entries = load_from_path(path);
    if entries.last().map(String::as_str) == Some(entry) {
        return;
    }
    entries.push(entry.to_string());
    if entries.len() > MAX_ENTRIES {
        entries.drain(..entries.len() - MAX_ENTRIES);
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = HistoryFile { entries };
    let _ = serde_json::to_string_pretty(&file).map(|json| std::fs::write(path, json));
}

/// Append a single entry and persist. Duplicates of the most recent entry are
/// skipped. The file is capped at [`MAX_ENTRIES`].
pub fn push(entry: &str) {
    if entry.trim().is_empty() {
        return;
    }
    let Some(path) = history_path() else {
        return;
    };
    push_to_path(&path, entry);
}

#[cfg(test)]
mod tests {
    use super::{HistoryFile, MAX_ENTRIES, load_from_path, push_to_path};
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn load_from_path_returns_empty_for_missing_and_invalid_files() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("history.json");

        assert_eq!(load_from_path(&path), Vec::<String>::new());

        fs::write(&path, "{not json").expect("write invalid history");
        assert_eq!(load_from_path(&path), Vec::<String>::new());
    }

    #[test]
    fn push_to_path_skips_blank_and_duplicate_latest_entries() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("nested").join("history.json");

        push_to_path(&path, "   ");
        push_to_path(&path, "first");
        push_to_path(&path, "first");

        let raw = fs::read_to_string(&path).expect("read history");
        let file: HistoryFile = serde_json::from_str(&raw).expect("parse history");
        assert_eq!(file.entries, vec!["first".to_string()]);
    }

    #[test]
    fn push_to_path_truncates_to_max_entries() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("history.json");

        for index in 0..=MAX_ENTRIES {
            push_to_path(&path, &format!("entry-{index}"));
        }

        let entries = load_from_path(&path);
        let expected_last = format!("entry-{MAX_ENTRIES}");
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(entries.first().map(String::as_str), Some("entry-1"));
        assert_eq!(
            entries.last().map(String::as_str),
            Some(expected_last.as_str())
        );
    }
}
