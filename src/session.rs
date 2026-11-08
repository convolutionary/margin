//! Settings, the open-tab session, and backups of unsaved work, so closing the window never asks
//! and never loses anything.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::editor::EditorSettings;

/// Unsaved documents up to this size are backed up; bigger ones are asked about on close.
pub const BACKUP_MAX: u64 = 64 << 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub editor: EditorSettings,
    /// "system", "dark" or "light"
    pub theme: String,
    pub restore_session: bool,
    pub recent: Vec<PathBuf>,
    pub find_history: Vec<String>,
    pub fif_dir: String,
    pub fif_filter: String,
    pub fif_hidden: bool,
    pub wrap_search: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            editor: EditorSettings::default(),
            theme: "system".into(),
            restore_session: true,
            recent: Vec::new(),
            find_history: Vec::new(),
            fif_dir: String::new(),
            fif_filter: String::new(),
            fif_hidden: false,
            wrap_search: true,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionTab {
    pub path: Option<PathBuf>,
    /// id of the backup holding unsaved text
    pub backup: Option<String>,
    pub anchor: u64,
    pub head: u64,
    pub top: u64,
    pub encoding: Option<String>,
    pub eol: Option<String>,
    pub lang: Option<String>,
    pub follow: bool,
    pub untitled: usize,
    pub bookmarks: Vec<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub tabs: Vec<SessionTab>,
    pub active: usize,
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("margin")
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("margin")
}

pub fn backup_dir() -> PathBuf {
    data_dir().join("backup")
}

fn read<T: for<'de> Deserialize<'de> + Default>(p: &Path) -> T {
    match std::fs::read(p) {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|_| {
            let _ = std::fs::rename(p, p.with_extension("json.broken"));
            T::default()
        }),
        Err(_) => T::default(),
    }
}

fn write<T: Serialize>(p: &Path, v: &T) {
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = p.with_extension("json.tmp");
    if let Ok(b) = serde_json::to_vec_pretty(v)
        && std::fs::write(&tmp, b).is_ok()
    {
        let _ = std::fs::rename(tmp, p);
    }
}

pub fn load_settings() -> Settings {
    read(&config_dir().join("settings.json"))
}

pub fn save_settings(s: &Settings) {
    write(&config_dir().join("settings.json"), s);
}

pub fn load_session() -> Session {
    read(&data_dir().join("session.json"))
}

pub fn save_session(s: &Session) {
    write(&data_dir().join("session.json"), s);
    // backups nobody refers to any more
    let keep: std::collections::HashSet<String> = s.tabs.iter().filter_map(|t| t.backup.clone()).collect();
    if let Ok(rd) = std::fs::read_dir(backup_dir()) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !keep.contains(name.trim_end_matches(".txt")) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

pub fn backup_path(id: &str) -> PathBuf {
    backup_dir().join(format!("{id}.txt"))
}

pub fn new_backup_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:x}-{:x}", t, std::process::id())
}
