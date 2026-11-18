//! The desktop app's preferences, kept beside the store as `settings.json`.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrbSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl OrbSize {
    pub fn points(self) -> f32 {
        match self {
            OrbSize::Small => 80.0,
            OrbSize::Medium => 112.0,
            OrbSize::Large => 152.0,
        }
    }

    /// How many orbs share a row when the window has room: 8, 6 or 4.
    pub fn columns(self) -> usize {
        match self {
            OrbSize::Small => 8,
            OrbSize::Medium => 6,
            OrbSize::Large => 4,
        }
    }

    /// Each orb's column (orb plus room for its name) and the gap between columns, in points.
    pub fn column(self) -> (f32, f32) {
        match self {
            OrbSize::Small => (108.0, 18.0),
            OrbSize::Medium => (148.0, 28.0),
            OrbSize::Large => (196.0, 44.0),
        }
    }

    /// The row's width at full strength: a wider window centres it, a narrower one wraps it.
    pub fn row_width(self) -> f32 {
        let (w, gap) = self.column();
        let n = self.columns() as f32;
        n * w + (n - 1.0) * gap
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    /// Most recently active first.
    #[default]
    Recent,
    /// A to Z by name.
    Name,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub orb_size: OrbSize,
    pub order: Order,
    /// Under a nickname, show the folder it stands for.
    pub show_folders: bool,
    /// Show a macOS banner when an agent posts a new update.
    pub notify_updates: bool,
    /// Show Feeder in the macOS menu bar while running.
    #[serde(default = "default_true")]
    pub menubar: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: Theme::System,
            orb_size: OrbSize::Medium,
            order: Order::Recent,
            show_folders: true,
            notify_updates: false,
            menubar: true,
        }
    }
}

impl Settings {
    /// The saved settings, or the defaults when there are none (or they don't parse).
    pub fn load(dir: &Path) -> Settings {
        std::fs::read_to_string(dir.join("settings.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// Writes through a temporary file, so a crash never leaves half a file.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join("settings.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).expect("settings serialize"))?;
        std::fs::rename(tmp, dir.join("settings.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_size_fits_its_row_in_the_default_window() {
        // 1180 pt window less 32 pt of padding each side.
        for size in [OrbSize::Small, OrbSize::Medium, OrbSize::Large] {
            assert!(size.row_width() <= 1180.0 - 64.0, "{size:?}: {}", size.row_width());
            assert!(size.column().0 >= size.points(), "{size:?}");
        }
    }

    #[test]
    fn settings_round_trip_and_missing_fields_take_defaults() {
        let dir = std::env::temp_dir().join(format!("feeder-settings-{}", std::process::id()));
        assert_eq!(Settings::load(&dir), Settings::default());
        let s = Settings {
            theme: Theme::Light,
            orb_size: OrbSize::Large,
            order: Order::Name,
            show_folders: false,
            notify_updates: true,
            menubar: false,
        };
        s.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), s);
        std::fs::write(dir.join("settings.json"), r#"{"theme":"dark"}"#).unwrap();
        assert_eq!(Settings::load(&dir), Settings { theme: Theme::Dark, ..Settings::default() });
        std::fs::remove_dir_all(dir).ok();
    }
}
