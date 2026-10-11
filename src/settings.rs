use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrackingMode {
    Active,
    #[default]
    #[serde(other)]
    Total,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PresenceDisplay {
    SelectedOnly,
    ActivityCount,
    #[default]
    #[serde(other)]
    ActivityNames,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename = "dat-settings", rename_all = "kebab-case")]
pub struct Settings {
    pub tracking_mode: TrackingMode,
    pub persist_activity_time: bool,
    pub session_history: bool,
    pub multiple_instances: bool,
    pub detailed_statistics: bool,
    pub presence_display: PresenceDisplay,
    pub display_focused_activity: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            tracking_mode: TrackingMode::Total,
            persist_activity_time: true,
            session_history: false,
            multiple_instances: true,
            detailed_statistics: false,
            presence_display: PresenceDisplay::ActivityNames,
            display_focused_activity: false,
        }
    }
}
pub type SharedSettings = Arc<Mutex<Settings>>;

pub fn data_directory() -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unavailable")?;
    Ok(PathBuf::from(base).join("DiscordActivityTracker"))
}
impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(xml) => quick_xml::de::from_str(&xml).context("invalid settings XML"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).context("reading settings"),
        }
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("xml.tmp");
        let xml = quick_xml::se::to_string(self)?;
        let mut file = fs::File::create(&temporary)?;
        use std::io::Write;
        file.write_all(xml.as_bytes())?;
        file.sync_all()?;
        drop(file);
        // Atomic replacement preserves the last valid settings on write failure.
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error()).context("replacing settings file");
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_round_trip() {
        let defaults: Settings = quick_xml::de::from_str("<dat-settings/>").unwrap();
        assert_eq!(defaults, Settings::default());
        assert!(!defaults.detailed_statistics && !defaults.session_history);
        assert!(!defaults.display_focused_activity);
        let changed = Settings {
            tracking_mode: TrackingMode::Active,
            display_focused_activity: true,
            detailed_statistics: true,
            ..defaults
        };
        assert_eq!(
            quick_xml::de::from_str::<Settings>(&quick_xml::se::to_string(&changed).unwrap())
                .unwrap(),
            changed
        );
    }
    #[test]
    fn unknown_modes_use_defaults() {
        let value: Settings = quick_xml::de::from_str(
            "<dat-settings><tracking-mode>invalid</tracking-mode></dat-settings>",
        )
        .unwrap();
        assert_eq!(value.tracking_mode, TrackingMode::Total);
    }

    #[test]
    fn saving_atomically_replaces_existing_settings() {
        let directory =
            std::env::temp_dir().join(format!("dat-settings-{}", crate::storage::unique_id()));
        let path = directory.join("settings.xml");
        Settings::default().save(&path).unwrap();
        let changed = Settings {
            tracking_mode: TrackingMode::Active,
            display_focused_activity: true,
            persist_activity_time: false,
            ..Settings::default()
        };
        changed.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), changed);
        assert!(!path.with_extension("xml.tmp").exists());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
