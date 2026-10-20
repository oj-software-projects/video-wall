use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Grid {
    pub rows: u32,
    pub cols: u32,
}

impl Default for Grid {
    fn default() -> Self {
        Grid { rows: 2, cols: 2 }
    }
}

impl Grid {
    pub fn cells(&self) -> usize {
        (self.rows * self.cols) as usize
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub dirs: Vec<String>,
    pub grid: Grid,
    pub shuffle: bool,
    pub loop_: bool,
    pub mute: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            dirs: Vec::new(),
            grid: Grid::default(),
            shuffle: true,
            loop_: false,
            mute: true,
        }
    }
}

impl Config {
    /// Lenient parse: unknown/bad fields fall back to defaults instead of failing.
    pub fn from_json(value: &Value) -> Config {
        let mut cfg = Config::default();
        let Some(obj) = value.as_object() else {
            return cfg;
        };
        if let Some(dirs) = obj.get("dirs").and_then(|v| v.as_array()) {
            cfg.dirs = sanitize_dirs(
                dirs.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string())),
            );
        }
        if let Some(grid) = obj.get("grid").and_then(|v| v.as_object()) {
            cfg.grid.rows = grid_number(grid.get("rows"));
            cfg.grid.cols = grid_number(grid.get("cols"));
        }
        if let Some(v) = obj.get("shuffle").and_then(|v| v.as_bool()) {
            cfg.shuffle = v;
        }
        if let Some(v) = obj.get("loop").and_then(|v| v.as_bool()) {
            cfg.loop_ = v;
        }
        if let Some(v) = obj.get("mute").and_then(|v| v.as_bool()) {
            cfg.mute = v;
        }
        cfg
    }

    pub fn to_json(&self) -> Value {
        json!({
            "dirs": self.dirs,
            "grid": { "rows": self.grid.rows, "cols": self.grid.cols },
            "shuffle": self.shuffle,
            "loop": self.loop_,
            "mute": self.mute,
        })
    }

    /// Corrupt or unreadable config is treated as absent (defaults + warning).
    pub fn load(path: &Path) -> Config {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Config::default(),
            Err(err) => {
                crate::log::warn("config", &format!("cannot read {}: {err}", path.display()));
                return Config::default();
            }
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(value) => Config::from_json(&value),
            Err(err) => {
                crate::log::warn("config", &format!("corrupt config {}: {err}", path.display()));
                Config::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&self.to_json())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        fs::write(path, text + "\n")
    }
}

fn grid_number(value: Option<&Value>) -> u32 {
    let raw = match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    match raw {
        Some(n) if n.is_finite() => (n as i64).clamp(1, 8) as u32,
        _ => 2,
    }
}

pub fn sanitize_dirs<I: IntoIterator<Item = String>>(dirs: I) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for dir in dirs {
        let trimmed = dir.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

pub fn user_data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("video-wall")
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"))
            .join("video-wall")
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".config"))
            .join("video-wall")
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_path() -> PathBuf {
    user_data_dir().join("config.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let cfg = Config::default();
        assert_eq!(cfg.dirs, Vec::<String>::new());
        assert_eq!(cfg.grid, Grid { rows: 2, cols: 2 });
        assert!(cfg.shuffle);
        assert!(!cfg.loop_);
        assert!(cfg.mute);
    }

    #[test]
    fn clamps_and_defaults_grid() {
        let cfg = Config::from_json(&json!({ "grid": { "rows": 0, "cols": 99 } }));
        assert_eq!(cfg.grid, Grid { rows: 1, cols: 8 });
        let cfg = Config::from_json(&json!({ "grid": { "rows": "x", "cols": null } }));
        assert_eq!(cfg.grid, Grid { rows: 2, cols: 2 });
        let cfg = Config::from_json(&json!({ "grid": { "rows": "3", "cols": 4.7 } }));
        assert_eq!(cfg.grid, Grid { rows: 3, cols: 4 });
    }

    #[test]
    fn sanitizes_dirs() {
        let cfg = Config::from_json(&json!({
            "dirs": [" /a ", "", 3, "/a", "/b", "   "]
        }));
        assert_eq!(cfg.dirs, vec!["/a".to_string(), "/b".to_string()]);
    }

    #[test]
    fn ignores_bad_values() {
        let cfg = Config::from_json(&json!({
            "shuffle": "yes", "loop": 1, "mute": null, "dirs": "nope"
        }));
        assert!(cfg.shuffle);
        assert!(!cfg.loop_);
        assert!(cfg.mute);
        assert!(cfg.dirs.is_empty());
    }

    #[test]
    fn corrupt_json_is_default() {
        let dir = std::env::temp_dir().join(format!("tmv-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Config::load(&path), Config::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_roundtrip() {
        let dir = std::env::temp_dir().join(format!("tmv-cfg-save-{}", std::process::id()));
        let path = dir.join("config.json");
        let cfg = Config {
            dirs: vec!["/videos".into()],
            grid: Grid { rows: 4, cols: 2 },
            shuffle: false,
            loop_: true,
            mute: false,
        };
        cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path), cfg);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
