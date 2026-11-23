//! Settings persisted at "<appConfigDir>/settings.json" as
//! { "destDir": string | null, "connections": number }. Missing or corrupt
//! files fall back to { null, 8 }; saves are atomic (temp + rename).

use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// CONTRACT default connection count.
pub const DEFAULT_CONNECTIONS: u32 = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub dest_dir: Option<String>,
    #[serde(default = "default_connections")]
    pub connections: u32,
}

fn default_connections() -> u32 {
    DEFAULT_CONNECTIONS
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            dest_dir: None,
            connections: DEFAULT_CONNECTIONS,
        }
    }
}

/// Load settings; a missing or corrupt file yields the defaults.
pub fn load(path: &Path) -> Settings {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Atomic save (temp file in the same directory, then rename); creates the
/// parent directory if needed.
pub fn save(path: &Path, s: &Settings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_string_pretty(s)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let write = || -> std::io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.write_all(b"\n")
    };
    if let Err(e) = write() {
        // Never leave a half-written temp behind on a failed save.
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Partial update for the save_settings command:
/// - dest_dir: None (key absent) = keep; Some(None) (null) = clear;
///   Some(Some(x)) = set.
/// - connections: None = keep; Some(n) = set clamped to 1..=16.
pub fn apply(current: &mut Settings, dest_dir: Option<Option<String>>, connections: Option<u32>) {
    if let Some(d) = dest_dir {
        current.dest_dir = d;
    }
    if let Some(c) = connections {
        current.connections = c.clamp(1, 16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hydra-unit-settings-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn defaults_on_missing_and_corrupt() {
        let dir = temp_dir("missing");
        let p = dir.join("settings.json");
        let s = load(&p);
        assert_eq!(s.dest_dir, None);
        assert_eq!(s.connections, DEFAULT_CONNECTIONS);
        fs::write(&p, "{corrupt").unwrap();
        let s = load(&p);
        assert_eq!(s.dest_dir, None);
        assert_eq!(s.connections, DEFAULT_CONNECTIONS);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trip_and_partial_updates() {
        let dir = temp_dir("roundtrip");
        let p = dir.join("settings.json");
        let mut s = Settings::default();
        apply(&mut s, Some(Some("/tmp/dl".into())), Some(4));
        assert_eq!(s.dest_dir.as_deref(), Some("/tmp/dl"));
        assert_eq!(s.connections, 4);
        save(&p, &s).unwrap();
        let loaded = load(&p);
        assert_eq!(loaded.dest_dir.as_deref(), Some("/tmp/dl"));
        assert_eq!(loaded.connections, 4);
        // null clears the remembered folder; out-of-range clamps.
        apply(&mut s, Some(None), Some(99));
        assert_eq!(s.dest_dir, None);
        assert_eq!(s.connections, 16);
        // absent keys keep current values.
        let mut s2 = loaded.clone();
        apply(&mut s2, None, None);
        assert_eq!(s2.dest_dir.as_deref(), Some("/tmp/dl"));
        assert_eq!(s2.connections, 4);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wire_shape_is_camel_case() {
        let s = Settings {
            dest_dir: Some("/x".into()),
            connections: 3,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "{\"destDir\":\"/x\",\"connections\":3}");
        let null = serde_json::to_string(&Settings::default()).unwrap();
        assert_eq!(null, "{\"destDir\":null,\"connections\":8}");
    }
}
