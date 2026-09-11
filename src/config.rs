use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename = "activity-tracker")]
pub struct Config {
    #[serde(rename = "discord-application-id")]
    pub discord_application_id: String,

    #[serde(rename = "poll-seconds", default = "default_poll_seconds")]
    pub poll_seconds: u64,

    #[serde(rename = "activity", default)]
    pub activities: Vec<ActivityDefinition>,
}

#[derive(Debug, Deserialize)]
pub struct ActivityDefinition {
    pub name: String,

    #[serde(rename = "windows-process", default)]
    pub windows_processes: Vec<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let xml = fs::read_to_string(path).context("could not read XML file")?;
        let mut config: Self = quick_xml::de::from_str(&xml).context("invalid XML")?;

        config.discord_application_id = config.discord_application_id.trim().to_owned();
        for activity in &mut config.activities {
            activity.name = activity.name.trim().to_owned();
            trim_all(&mut activity.windows_processes);
        }

        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.discord_application_id.is_empty()
            || !self
                .discord_application_id
                .chars()
                .all(|c| c.is_ascii_digit())
        {
            bail!(
                "discord-application-id must be the numeric ID from the Discord Developer Portal"
            );
        }

        if self.poll_seconds == 0 {
            bail!("poll-seconds must be greater than zero");
        }

        if self.activities.is_empty() {
            bail!("at least one activity is required");
        }

        for (index, activity) in self.activities.iter().enumerate() {
            if activity.name.is_empty() {
                bail!("activity {} has an empty name", index + 1);
            }

            if activity.windows_processes.is_empty() {
                bail!("activity '{}' has no process names", activity.name);
            }
        }

        Ok(())
    }
}

impl ActivityDefinition {
    pub fn process_names(&self) -> &[String] {
        &self.windows_processes
    }
}

fn trim_all(values: &mut Vec<String>) {
    for value in values.iter_mut() {
        *value = value.trim().to_owned();
    }
    values.retain(|value| !value.is_empty());
}

fn default_poll_seconds() -> u64 {
    5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repeated_process_names_in_order() {
        let xml = r#"
            <activity-tracker>
                <discord-application-id>123456789</discord-application-id>
                <poll-seconds>3</poll-seconds>
                <activity>
                    <name>Game</name>
                    <windows-process>Game.exe</windows-process>
                    <windows-process>GameLauncher.exe</windows-process>
                </activity>
            </activity-tracker>
        "#;

        let config: Config = quick_xml::de::from_str(xml).expect("XML should parse");
        assert_eq!(config.poll_seconds, 3);
        assert_eq!(config.activities[0].windows_processes.len(), 2);
        assert_eq!(config.activities[0].windows_processes[0], "Game.exe");
    }
}
