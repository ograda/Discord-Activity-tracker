use anyhow::{Context, Result};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient, activity};

use crate::config::{ActivityDefinition, ButtonConfig, Links, selected_buttons};

pub struct DiscordPresence {
    application_id: String,
    client: Option<DiscordIpcClient>,
}

impl DiscordPresence {
    pub fn new(application_id: String) -> Self {
        Self {
            application_id,
            client: None,
        }
    }

    pub fn set(
        &mut self,
        text: &PresenceText,
        large_image: &str,
        selected: Option<&ActivityDefinition>,
        started_at: i64,
        links: Option<&Links>,
    ) -> Result<()> {
        self.ensure_connected()?;
        let payload = activity_payload(text, large_image, selected, started_at, links);

        if let Some(client) = &mut self.client
            && let Err(error) = client.set_activity(payload)
        {
            self.disconnect();
            return Err(error).context("could not update Discord Rich Presence");
        }

        Ok(())
    }

    pub fn set_hidden(&mut self, started_at: i64) -> Result<()> {
        self.set_generic(hidden_payload(started_at))
    }

    pub fn set_untracked(&mut self, started_at: i64, activity_count: usize) -> Result<()> {
        self.set_generic(untracked_payload(started_at, activity_count))
    }

    fn set_generic(&mut self, payload: activity::Activity<'static>) -> Result<()> {
        self.ensure_connected()?;

        if let Some(client) = &mut self.client
            && let Err(error) = client.set_activity(payload)
        {
            self.disconnect();
            return Err(error).context("could not update Discord Rich Presence");
        }

        Ok(())
    }

    pub fn clear(&mut self) -> Result<()> {
        if self.client.is_none() {
            return Ok(());
        }

        if let Some(client) = &mut self.client
            && let Err(error) = client.clear_activity()
        {
            self.disconnect();
            return Err(error).context("could not clear Discord Rich Presence");
        }

        Ok(())
    }

    fn ensure_connected(&mut self) -> Result<()> {
        if self.client.is_some() {
            return Ok(());
        }

        let mut client = DiscordIpcClient::new(&self.application_id);
        client
            .connect()
            .context("could not connect to the Discord desktop client")?;
        self.client = Some(client);
        Ok(())
    }

    fn disconnect(&mut self) {
        if let Some(mut client) = self.client.take() {
            let _ = client.close();
        }
    }
}

/// The native presence timer belongs to a session, not its visibility mode.
#[derive(Default)]
pub struct PresenceSession {
    started_at: Option<i64>,
}
impl PresenceSession {
    pub fn observe(&mut self, activities_running: bool, hidden: bool, now_ms: i64) -> Option<i64> {
        if activities_running || hidden {
            self.started_at.get_or_insert(now_ms);
        } else {
            self.started_at = None;
        }
        self.started_at
    }
}
fn hidden_payload(started_at: i64) -> activity::Activity<'static> {
    // No activity-specific text, assets, URLs or buttons leak into hidden mode.
    activity::Activity::new()
        .name("Activity Tracker")
        .status_display_type(activity::StatusDisplayType::Name)
        .timestamps(activity::Timestamps::new().start(started_at))
}

fn untracked_payload(started_at: i64, activity_count: usize) -> activity::Activity<'static> {
    let details = match activity_count {
        0 => "No activity running".into(),
        1 => "Powered by DAT \u{00b7} Tracking 1 activity".into(),
        count => format!("Powered by DAT \u{00b7} Tracking {count} activities"),
    };
    // A fresh generic payload cannot retain the previous activity's assets or buttons.
    hidden_payload(started_at).details(details)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceText {
    pub details: String,
    pub state: String,
    pub hover: String,
    pub compact: String,
}

// One normal payload path keeps buttons/assets/URLs independent of text modes.
fn activity_payload(
    text: &PresenceText,
    large_image: &str,
    selected: Option<&ActivityDefinition>,
    started_at: i64,
    links: Option<&Links>,
) -> activity::Activity<'static> {
    let tooltip = selected
        .and_then(|a| a.tracker_status.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| fit_discord_text(format!("[DAT] {s}")))
        .unwrap_or_else(|| "Activity Tracker".into());
    let mut assets = activity::Assets::new()
        .large_image(large_image.to_owned())
        .large_text(text.hover.clone())
        .small_image("tracker")
        .small_text(tooltip);
    if let Some(repository) = links.and_then(Links::repository) {
        assets = assets.small_url(repository.to_owned());
    }
    if let Some(url) = selected.and_then(|a| a.image_url.as_deref()) {
        assets = assets.large_url(url.to_owned());
    }
    activity::Activity::new()
        .name(text.compact.clone())
        .status_display_type(activity::StatusDisplayType::Name)
        .details(text.details.clone())
        .state(text.state.clone())
        .assets(assets)
        .timestamps(activity::Timestamps::new().start(started_at))
        .buttons(buttons_from_links(links, selected))
}

pub fn fit_discord_text(mut text: String) -> String {
    const MAX_BYTES: usize = 128;
    if text.len() > MAX_BYTES {
        let mut end = MAX_BYTES - 3;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("...");
    }
    text
}

fn buttons_from_links(
    links: Option<&Links>,
    selected: Option<&ActivityDefinition>,
) -> Vec<activity::Button<'static>> {
    let buttons = selected_buttons(selected, links);

    // Individual buttons replace the global buttons entirely.
    if !buttons.is_empty() {
        return buttons.iter().map(to_discord_button).collect();
    }

    // Explicitly disabled: do not use global fallback.
    if selected.is_some_and(|activity| !activity.buttons_enabled) {
        return Vec::new();
    }

    // Backward compatibility with legacy download/source.
    let Some(links) = links else {
        return Vec::new();
    };

    let mut buttons = Vec::with_capacity(2);

    if let Some(download) = links.download() {
        buttons.push(activity::Button::new(
            "Baixar para Windows",
            download.to_owned(),
        ));
    }

    if let Some(source) = links.source() {
        buttons.push(activity::Button::new("Ver código-fonte", source.to_owned()));
    }

    buttons
}

fn to_discord_button(button: &ButtonConfig) -> activity::Button<'static> {
    activity::Button::new(button.label.clone(), button.url.clone())
}

impl Drop for DiscordPresence {
    fn drop(&mut self) {
        self.disconnect();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn untracked_summary_counts_privacy_and_timestamp_transitions() {
        use crate::{PresentationTarget, presentation_target, resolver::ActivityObservation};
        use std::time::Duration;
        for (count, expected) in [
            (0, "No activity running"),
            (1, "Powered by DAT \u{00b7} Tracking 1 activity"),
            (3, "Powered by DAT \u{00b7} Tracking 3 activities"),
        ] {
            let json = serde_json::to_value(untracked_payload(123, count)).unwrap();
            assert_eq!(json["name"], "Activity Tracker");
            assert_eq!(json["details"], expected);
            assert_eq!(json["timestamps"]["start"], 123);
            for field in ["state", "assets", "buttons"] {
                assert!(json.get(field).is_none());
            }
        }
        let mut observations: Vec<_> = ["Chrome", "WoW"]
            .into_iter()
            .map(|name| ActivityObservation {
                name: name.into(),
                pids: (1..=87).collect(),
                window_count: 87,
                foreground: false,
            })
            .collect();
        let mut session = PresenceSession::default();
        // Chrome -> Explorer -> WoW -> hidden -> Explorer -> Chrome.
        for (step, (focus, hidden)) in [
            (Some(0), false),
            (None, false),
            (Some(1), false),
            (None, true),
            (None, false),
            (Some(0), false),
        ]
        .into_iter()
        .enumerate()
        {
            for (index, observation) in observations.iter_mut().enumerate() {
                observation.foreground = focus == Some(index);
            }
            let target = presentation_target(&observations, true, hidden, Duration::ZERO);
            let started = session
                .observe(true, target.is_generic(), 1000 + step as i64 * 1000)
                .unwrap();
            let payload = match target {
                PresentationTarget::TrackerOnly => hidden_payload(started),
                PresentationTarget::Untracked(count) => {
                    assert_eq!(count, 2); // Not 174 processes/windows.
                    untracked_payload(started, count)
                }
                PresentationTarget::Activity(index) => {
                    let name = &observations[index].name;
                    activity_payload(
                        &PresenceText {
                            compact: name.clone(),
                            details: "Playing for 1m".into(),
                            state: "Activity Tracker".into(),
                            hover: name.clone(),
                        },
                        "image",
                        None,
                        started,
                        None,
                    )
                }
                PresentationTarget::Idle => panic!("focused mode must retain generic presence"),
            };
            let json = serde_json::to_value(payload).unwrap();
            assert_eq!(json["timestamps"]["start"], 1000);
            if hidden {
                assert_eq!(target, PresentationTarget::TrackerOnly);
                assert_eq!(json["name"], "Activity Tracker");
                assert!(json.get("details").is_none());
            } else if let Some(index) = focus {
                assert_eq!(json["name"], observations[index].name);
                assert_eq!(json["details"], "Playing for 1m");
                assert!(json.get("assets").is_some());
            } else {
                assert_eq!(
                    json["details"],
                    "Powered by DAT \u{00b7} Tracking 2 activities"
                );
                assert!(json.get("assets").is_none());
            }
        }
        observations.iter_mut().for_each(|o| o.foreground = false);
        let two = presentation_target(&observations, true, false, Duration::ZERO);
        observations.pop();
        let one = presentation_target(&observations, true, false, Duration::ZERO);
        assert_ne!(two, one); // Count changes invalidate the generic send cache.
        assert_eq!(one, PresentationTarget::Untracked(1));
        assert_eq!(
            presentation_target(&observations, false, false, Duration::ZERO),
            PresentationTarget::Activity(0)
        );
        observations.clear();
        assert_eq!(
            presentation_target(&observations, true, false, Duration::ZERO),
            PresentationTarget::Untracked(0)
        );
        assert_eq!(session.observe(false, true, 50_000), Some(1000));
    }

    #[test]
    fn focused_payload_fields_change_together() {
        use crate::{
            PresentationTarget, config::Config, presence_text, presentation_target,
            resolver::ActivityObservation, settings::Settings,
        };
        use std::time::Duration;
        let config: Config = quick_xml::de::from_str(r#"<activity-tracker><discord-application-id>123</discord-application-id>
          <activity><name>WoW</name><verb>Playing</verb><image>wow</image><image-url>https://example.com/wow</image-url><tracker-status>Warrior</tracker-status><instance-count-mode>processes</instance-count-mode><windows-process>wow.exe</windows-process><button><label>WoW link</label><url>https://example.com/wow-button</url></button></activity>
          <activity><name>Chrome</name><verb>Browsing</verb><image>chrome</image><image-url>https://example.com/chrome</image-url><tracker-status>Browser</tracker-status><windows-process>chrome.exe</windows-process><button><label>Chrome link</label><url>https://example.com/chrome-button</url></button></activity>
        </activity-tracker>"#).unwrap();
        let settings = Settings {
            display_focused_activity: true,
            ..Settings::default()
        };
        let mut observations = vec![
            ActivityObservation {
                name: "WoW".into(),
                pids: vec![1, 2],
                window_count: 1,
                foreground: true,
            },
            ActivityObservation {
                name: "Chrome".into(),
                pids: vec![3, 4, 5],
                window_count: 1,
                foreground: false,
            },
        ];
        for (focus, elapsed, verb, name, status) in [
            (0, "2h26m", "Playing", "WoW \u{00b7} 2 instances", "Warrior"),
            (1, "5m", "Browsing", "Chrome", "Browser"),
        ] {
            for (index, activity) in observations.iter_mut().enumerate() {
                activity.foreground = index == focus;
            }
            let PresentationTarget::Activity(index) =
                presentation_target(&observations, true, false, Duration::from_secs(75))
            else {
                panic!("missing focused selection")
            };
            let definition = &config.activities[index];
            let text = presence_text(&observations, index, &config, &settings, elapsed);
            let json = serde_json::to_value(activity_payload(
                &text,
                definition.image.as_deref().unwrap(),
                Some(definition),
                123,
                None,
            ))
            .unwrap();
            assert_eq!(json["name"], name);
            assert_eq!(
                json["details"],
                format!("{verb} for {elapsed} \u{00b7} also 1 activity")
            );
            assert_eq!(json["state"], "Activity Tracker");
            assert_eq!(
                json["assets"]["large_image"],
                definition.image.as_deref().unwrap()
            );
            assert_eq!(
                json["assets"]["large_url"],
                definition.image_url.as_deref().unwrap()
            );
            assert_eq!(json["assets"]["small_text"], format!("[DAT] {status}"));
            assert_eq!(
                json["buttons"][0]["label"],
                format!("{} link", definition.name)
            );
            assert_eq!(json["buttons"][0]["url"], definition.buttons[0].url);
            assert_eq!(json["timestamps"]["start"], 123);
        }
    }

    #[test]
    fn hide_show_payloads_keep_one_timestamp_and_hidden_payload_has_no_details() {
        let mut session = PresenceSession::default();
        let text = PresenceText {
            details: "Playing for 1h32m".into(),
            state: "Activity Tracker".into(),
            hover: "WoW".into(),
            compact: "WoW".into(),
        };
        for (index, hidden) in [false, true, false, true, false].into_iter().enumerate() {
            let started = session
                .observe(true, hidden, 1000 + index as i64 * 60_000)
                .unwrap();
            assert_eq!(started, 1000);
            let payload = if hidden {
                hidden_payload(started)
            } else {
                activity_payload(&text, "wow", None, started, None)
            };
            let json = serde_json::to_value(payload).unwrap();
            assert_eq!(json["timestamps"]["start"], 1000);
            if hidden {
                assert_eq!(json["name"], "Activity Tracker");
                for field in ["details", "state", "assets", "buttons"] {
                    assert!(json.get(field).is_none(), "hidden payload contains {field}");
                }
            } else {
                assert_eq!(json["details"], "Playing for 1h32m");
                assert_eq!(json["state"], "Activity Tracker");
            }
        }
        assert_eq!(session.observe(false, true, 400_000), Some(1000));
        assert_eq!(session.observe(true, false, 500_000), Some(1000));
        // Ordinary visible idle still ends the session, independently of toggles.
        assert_eq!(session.observe(false, false, 600_000), None);
        assert_eq!(session.observe(false, true, 700_000), Some(700_000));
    }

    #[test]
    fn serialized_payload_preserves_two_buttons_urls_metadata_and_status() {
        let global:Links=quick_xml::de::from_str(r#"<links><repository>https://example.com/dat</repository><button><label>Global</label><url>https://example.com/global</url></button></links>"#).unwrap();
        let mut selected:ActivityDefinition=quick_xml::de::from_str(r#"<activity><name>Game</name><image-url>https://example.com/game</image-url><tracker-status>Bloodbath progress: 60</tracker-status><windows-process>game.exe</windows-process><button><label>One</label><url>https://example.com/one</url></button><button><label>Two</label><url>https://example.com/two</url></button></activity>"#).unwrap();
        for compact in ["Game · 2 instances", "Game"] {
            let text = PresenceText {
                details: "Playing for 2h26m · also 2 activities".into(),
                state: "Activity Tracker".into(),
                hover: "Game · 1h32".into(),
                compact: compact.into(),
            };
            let json = serde_json::to_value(activity_payload(
                &text,
                "game",
                Some(&selected),
                123456,
                Some(&global),
            ))
            .unwrap();
            assert_eq!(json["name"], compact);
            assert_eq!(json["status_display_type"], 0);
            assert_eq!(json["details"], text.details);
            assert_eq!(json["state"], text.state);
            assert_eq!(json["buttons"].as_array().unwrap().len(), 2);
            assert_eq!(json["buttons"][1]["label"], "Two");
            assert_eq!(json["buttons"][0]["url"], "https://example.com/one");
            assert_eq!(json["assets"]["large_url"], "https://example.com/game");
            assert_eq!(json["assets"]["small_url"], "https://example.com/dat");
            assert_eq!(json["assets"]["small_text"], "[DAT] Bloodbath progress: 60");
            assert_eq!(json["timestamps"]["start"], 123456);
        }
        selected.tracker_status = None;
        selected.image_url = None;
        selected.buttons.clear();
        let text = PresenceText {
            details: "Playing for 1m".into(),
            state: "Activity Tracker".into(),
            hover: "Game".into(),
            compact: "Game".into(),
        };
        let json = serde_json::to_value(activity_payload(
            &text,
            "game",
            Some(&selected),
            1,
            Some(&global),
        ))
        .unwrap();
        assert_eq!(json["assets"]["small_text"], "Activity Tracker");
        assert!(json["assets"].get("large_url").is_none());
        assert_eq!(json["buttons"][0]["label"], "Global");
        selected.buttons_enabled = false;
        selected.tracker_status = Some("遊".repeat(100));
        let json = serde_json::to_value(activity_payload(
            &text,
            "game",
            Some(&selected),
            1,
            Some(&global),
        ))
        .unwrap();
        assert!(json.get("buttons").is_none());
        let tooltip = json["assets"]["small_text"].as_str().unwrap();
        assert!(tooltip.len() <= 128);
        assert!(tooltip.starts_with("[DAT] "));
    }

    use super::*;
    #[test]
    fn buttons_preserve_global_override_disable_and_legacy_behavior() {
        let global: Links = quick_xml::de::from_str(r#"<links><button><label>Global</label><url>https://example.com/global</url></button></links>"#).unwrap();
        let mut activity: ActivityDefinition = quick_xml::de::from_str(r#"<activity><name>Game</name><windows-process>game.exe</windows-process><button><label>Specific</label><url>https://example.com/game</url></button></activity>"#).unwrap();
        assert_eq!(
            selected_buttons(Some(&activity), Some(&global))[0].label,
            "Specific"
        );
        assert_eq!(buttons_from_links(Some(&global), Some(&activity)).len(), 1);
        activity.buttons.clear();
        assert_eq!(
            selected_buttons(Some(&activity), Some(&global))[0].label,
            "Global"
        );
        activity.buttons_enabled = false;
        assert!(buttons_from_links(Some(&global), Some(&activity)).is_empty());
        let legacy:Links=quick_xml::de::from_str(r#"<links><download>https://example.com/download</download><source>https://example.com/source</source></links>"#).unwrap();
        assert_eq!(buttons_from_links(Some(&legacy), None).len(), 2);
        assert!(buttons_from_links(Some(&legacy), Some(&activity)).is_empty());
    }
}
