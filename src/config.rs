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

    pub links: Option<Links>,

    #[serde(rename = "activity", default)]
    pub activities: Vec<ActivityDefinition>,
}

#[derive(Debug, Deserialize)]
pub struct ActivityDefinition {
    pub name: String,

    #[serde(rename = "display-name", default)]
    pub display_name: Option<String>,

    #[serde(default)]
    pub verb: Option<String>,

    #[serde(default)]
    pub image: Option<String>,

    #[serde(rename = "image-url", default)]
    pub image_url: Option<String>,

    #[serde(rename = "buttons-enabled", default = "default_true")]
    pub buttons_enabled: bool,

    #[serde(rename = "button", default)]
    pub buttons: Vec<ButtonConfig>,

    #[serde(rename = "windows-process", default)]
    pub windows_processes: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Links {
    #[serde(default)]
    repository: Option<String>,

    #[serde(rename = "button", default)]
    buttons: Vec<ButtonConfig>,

    // Legacy link fields remain supported if no <button> fields are defined.
    #[serde(default)]
    download: Option<String>,

    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ButtonConfig {
    pub label: String,
    pub url: String,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let xml = fs::read_to_string(path).context("could not read XML file")?;
        let mut config: Self = quick_xml::de::from_str(&xml).context("invalid XML")?;

        config.discord_application_id = config.discord_application_id.trim().to_owned();
        for activity in &mut config.activities {
            activity.name = activity.name.trim().to_owned();
            normalize_optional(&mut activity.display_name);
            normalize_optional(&mut activity.verb);
            normalize_optional(&mut activity.image);
            normalize_optional(&mut activity.image_url);
            for button in &mut activity.buttons {
                button.trim();
            }
            trim_all(&mut activity.windows_processes);
        }

        if let Some(links) = &mut config.links {
            links.trim();
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

        if let Some(links) = &self.links {
            for (name, url) in [
                ("repository", links.repository()),
                ("download", links.download()),
                ("source", links.source()),
            ] {
                if let Some(url) = url {
                    validate_url(name, url)?;
                }
            }

            validate_buttons("global links", &links.buttons)?;
        }

        if self.activities.is_empty() {
            bail!("at least one activity is required");
        }

        let mut seen = std::collections::HashSet::new();
        for (index, activity) in self.activities.iter().enumerate() {
            if !seen.insert(&activity.name) {
                bail!("duplicate activity name: '{}'", activity.name);
            }
            if activity.name.is_empty() {
                bail!("activity {} has an empty name", index + 1);
            }

            if activity.windows_processes.is_empty() {
                bail!("activity '{}' has no process names", activity.name);
            }

            if let Some(url) = activity.image_url.as_deref() {
                validate_url(&format!("image-url for '{}'", activity.name), url)?;
            }

            validate_buttons(&format!("activity '{}'", activity.name), &activity.buttons)?;

            if !activity.buttons_enabled && !activity.buttons.is_empty() {
                bail!(
                    "activity '{}' has <buttons-enabled>false</buttons-enabled> and also defines <button>",
                    activity.name
                );
            }
        }

        Ok(())
    }
}

impl Links {
    pub fn repository(&self) -> Option<&str> {
        self.repository.as_deref().or_else(|| self.source())
    }

    pub fn buttons(&self) -> &[ButtonConfig] {
        &self.buttons
    }

    pub fn download(&self) -> Option<&str> {
        self.download.as_deref()
    }

    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    fn trim(&mut self) {
        normalize_optional(&mut self.repository);
        normalize_optional(&mut self.download);
        normalize_optional(&mut self.source);
        for button in &mut self.buttons {
            button.label = button.label.trim().to_owned();
            button.url = button.url.trim().to_owned();
        }
    }
}

impl ActivityDefinition {
    pub fn process_names(&self) -> &[String] {
        &self.windows_processes
    }
}

impl ButtonConfig {
    fn trim(&mut self) {
        self.label = self.label.trim().to_owned();
        self.url = self.url.trim().to_owned();
    }
}

pub fn selected_buttons<'a>(
    selected: Option<&'a ActivityDefinition>,
    links: Option<&'a Links>,
) -> &'a [ButtonConfig] {
    if let Some(activity) = selected {
        if !activity.buttons_enabled {
            return &[];
        }

        if !activity.buttons.is_empty() {
            return &activity.buttons;
        }
    }

    links.map(Links::buttons).unwrap_or(&[])
}

fn default_true() -> bool {
    true
}

fn validate_buttons(scope: &str, buttons: &[ButtonConfig]) -> Result<()> {
    if buttons.len() > 2 {
        bail!("{scope} supports at most two <button> entries");
    }

    for button in buttons {
        if button.label.is_empty() || button.label.chars().count() > 32 {
            bail!("{scope}: button labels must contain 1 to 32 characters");
        }

        validate_url(&button.label, &button.url)?;
    }

    Ok(())
}

fn trim_all(values: &mut Vec<String>) {
    for value in values.iter_mut() {
        *value = value.trim().to_owned();
    }
    values.retain(|value| !value.is_empty());
}

fn normalize_optional(value: &mut Option<String>) {
    *value = value
        .take()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty());
}

fn validate_url(name: &str, url: &str) -> Result<()> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        bail!("link '{name}' must start with http:// or https://");
    }
    if url.len() > 512 {
        bail!("link '{name}' must not exceed 512 bytes");
    }
    Ok(())
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
