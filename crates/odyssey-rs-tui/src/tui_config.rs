//! Persistent TUI configuration: theme preference and display settings.
//!
//! The config is stored as JSON at `{config_dir}/odyssey/tui.json`
//! (e.g. `~/.config/odyssey/tui.json` on Linux, or
//! `~/Library/Application Support/LiquidOS/Odyssey/tui.json` on macOS).
//! Missing files are silently treated as the default configuration.

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// TUI-specific user preferences that survive restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuiConfig {
    /// Name of the active theme (matches `Theme::name`).
    #[serde(default = "default_theme")]
    pub theme: String,
}

fn default_theme() -> String {
    "odyssey".to_string()
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
        }
    }
}

impl TuiConfig {
    /// Resolve the platform-appropriate config file path.
    pub fn path() -> Option<PathBuf> {
        ProjectDirs::from("ai", "LiquidOS", "Odyssey")
            .map(|dirs| dirs.config_dir().join("tui.json"))
    }

    fn load_from_path(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    /// Load the config from disk. Returns `Default` if the file does not exist
    /// or cannot be parsed.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        Self::load_from_path(&path)
    }

    fn save_to_path(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Persist the config to disk. Creates parent directories as needed.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path().ok_or_else(|| anyhow::anyhow!("cannot resolve config dir"))?;
        self.save_to_path(&path)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn default_theme_is_odyssey() {
        let cfg = TuiConfig::default();
        assert_eq!(cfg.theme, "odyssey");
    }

    #[test]
    fn round_trip_serialization() {
        let cfg = TuiConfig {
            theme: "dracula".to_string(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: TuiConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.theme, "dracula");
    }

    #[test]
    fn missing_fields_use_defaults() {
        let parsed: TuiConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.theme, "odyssey");
    }

    #[test]
    fn load_from_path_returns_defaults_for_missing_and_invalid_json() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("tui.json");

        assert_eq!(TuiConfig::load_from_path(&path).theme, "odyssey");

        fs::write(&path, "{invalid").expect("write invalid config");
        assert_eq!(TuiConfig::load_from_path(&path).theme, "odyssey");
    }

    #[test]
    fn save_to_path_creates_parent_directories_and_round_trips() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("nested").join("tui.json");
        let config = TuiConfig {
            theme: "dracula".to_string(),
        };

        config.save_to_path(&path).expect("save config");

        let raw = fs::read_to_string(&path).expect("read config");
        assert!(raw.contains("dracula"));
        assert_eq!(TuiConfig::load_from_path(&path).theme, "dracula");
    }
}
