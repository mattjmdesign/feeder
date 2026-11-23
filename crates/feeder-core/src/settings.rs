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

    pub fn columns(self) -> usize {
        match self {
            OrbSize::Small => 8,
            OrbSize::Medium => 6,
            OrbSize::Large => 4,
        }
    }

    pub fn column(self) -> (f32, f32) {
        match self {
            OrbSize::Small => (108.0, 18.0),
            OrbSize::Medium => (148.0, 28.0),
            OrbSize::Large => (196.0, 44.0),
        }
    }

    pub fn row_width(self) -> f32 {
        let (w, gap) = self.column();
        let n = self.columns() as f32;
        n * w + (n - 1.0) * gap
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    #[default]
    Recent,
    Name,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub orb_size: OrbSize,
    pub order: Order,
    pub show_folders: bool,
    pub notify_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { theme: Theme::System, orb_size: OrbSize::Medium, order: Order::Recent, show_folders: true, notify_updates: false }
    }
}

impl Settings {
    pub fn load(dir: &Path) -> Settings {
        std::fs::read_to_string(dir.join("settings.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("settings.json.tmp-{}-{:08x}", std::process::id(), fastrand::u32(..)));
        let data = serde_json::to_string_pretty(self).expect("settings serialize");
        let written = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            file.write_all(data.as_bytes())?;
            file.sync_all()
        })();
        if let Err(error) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        std::fs::rename(&tmp, dir.join("settings.json")).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_size_fits_its_row_in_the_default_window() {
        for size in [OrbSize::Small, OrbSize::Medium, OrbSize::Large] {
            assert!(size.row_width() <= 1180.0 - 64.0, "{size:?}: {}", size.row_width());
            assert!(size.column().0 >= size.points(), "{size:?}");
        }
    }

    #[test]
    fn settings_round_trip_and_missing_fields_take_defaults() {
        let dir = std::env::temp_dir().join(format!("feeder-settings-{}", std::process::id()));
        assert_eq!(Settings::load(&dir), Settings::default());
        let s = Settings { theme: Theme::Light, orb_size: OrbSize::Large, order: Order::Name, show_folders: false, notify_updates: true };
        s.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), s);
        std::fs::write(dir.join("settings.json"), r#"{"theme":"dark","menubar":true}"#).unwrap();
        assert_eq!(Settings::load(&dir), Settings { theme: Theme::Dark, ..Settings::default() });
        std::fs::remove_dir_all(dir).ok();
    }
}
