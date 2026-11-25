//! App settings that belong to this computer, not to a song: the audio output and the
//! metronome. Saved as JSON in `$XDG_CONFIG_HOME/ritmo/settings.json` (else `~/.config/…`);
//! a missing or unreadable file gives the defaults.

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Output device ID (cpal's `host:device` form); None plays on the system default.
    pub device: Option<String>,
    /// Frames per audio buffer; None lets the system choose. Smaller is more responsive but
    /// needs a faster computer.
    pub buffer: Option<u32>,
    pub metronome: bool,
}

/// Buffer sizes offered in the settings window.
pub const BUFFERS: [u32; 6] = [64, 128, 256, 512, 1024, 2048];

impl Settings {
    pub fn load() -> Settings {
        settings_path().and_then(|p| Self::load_from(&p)).unwrap_or_default()
    }

    fn load_from(path: &Path) -> Option<Settings> {
        serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = settings_path().ok_or("no home directory")?;
        self.save_to(&path)
    }

    fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, text).map_err(|e| e.to_string())
    }
}

fn settings_path() -> Option<PathBuf> {
    config_dir(std::env::var_os("XDG_CONFIG_HOME"), std::env::var_os("HOME")).map(|d| d.join("ritmo").join("settings.json"))
}

fn config_dir(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    // The XDG spec says to ignore relative paths.
    let xdg = xdg.map(PathBuf::from).filter(|p| p.is_absolute());
    xdg.or_else(|| home.map(|h| PathBuf::from(h).join(".config")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_fall_back() {
        let dir = std::env::temp_dir().join(format!("ritmo-settings-{}", std::process::id()));
        let path = dir.join("ritmo/settings.json");
        let s = Settings { device: Some("alsa:hw:1,0".into()), buffer: Some(256), metronome: true };
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), Some(s));
        // Fields missing from an older file take their defaults; a damaged file is ignored.
        fs::write(&path, r#"{ "metronome": true }"#).unwrap();
        assert_eq!(Settings::load_from(&path), Some(Settings { metronome: true, ..Settings::default() }));
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load_from(&path), None);
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(config_dir(Some("/x/cfg".into()), Some("/home/u".into())), Some(PathBuf::from("/x/cfg")));
        assert_eq!(config_dir(Some("rel".into()), Some("/home/u".into())), Some(PathBuf::from("/home/u/.config")));
    }
}
