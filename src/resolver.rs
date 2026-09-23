use std::collections::HashSet;

use crate::{config::ActivityDefinition, monitor::normalize_process_name};

const DISCORD_FIELD_LIMIT: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedActivity {
    pub names: Vec<String>,
    pub details: String,
    pub state: String,
}

pub fn resolve_activities(
    definitions: &[ActivityDefinition],
    running_processes: &HashSet<String>,
) -> Option<ResolvedActivity> {
    let names: Vec<String> = definitions
        .iter()
        .filter(|definition| {
            definition
                .process_names()
                .iter()
                .map(|name| normalize_process_name(name))
                .any(|name| running_processes.contains(&name))
        })
        .map(|definition| definition.name.clone())
        .collect();

    if names.is_empty() {
        return None;
    }

    let details = truncate(&format!("Active: {}", names[0]), DISCORD_FIELD_LIMIT);
    let state = if names.len() == 1 {
        "1 selected activity".to_owned()
    } else {
        truncate(
            &format!("Also: {}", names[1..].join(" + ")),
            DISCORD_FIELD_LIMIT,
        )
    };

    Some(ResolvedActivity {
        names,
        details,
        state,
    })
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }

    let mut truncated: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(name: &str, process: &str) -> ActivityDefinition {
        ActivityDefinition {
            name: name.to_owned(),

            display_name: None,
            verb: None,
            image: None,
            image_url: None,

            buttons_enabled: true,
            buttons: Vec::new(),

            windows_processes: vec![process.to_owned()],
        }
    }

    #[test]
    fn preserves_xml_order_and_combines_matches() {
        let definitions = vec![
            definition("World of Warcraft", "wow"),
            definition("Visual Studio Code", "code"),
            definition("OBS Studio", "obs"),
        ];
        let running = HashSet::from(["obs".to_owned(), "wow".to_owned(), "code".to_owned()]);

        let result = resolve_activities(&definitions, &running).expect("should resolve");
        assert_eq!(
            result.names,
            ["World of Warcraft", "Visual Studio Code", "OBS Studio"]
        );
        assert_eq!(result.details, "Active: World of Warcraft");
        assert_eq!(result.state, "Also: Visual Studio Code + OBS Studio");
    }

    #[test]
    fn returns_none_when_nothing_matches() {
        let definitions = vec![definition("Game", "game")];
        assert!(resolve_activities(&definitions, &HashSet::new()).is_none());
    }

    #[test]
    fn truncation_is_unicode_safe() {
        let value = "é".repeat(200);
        let result = truncate(&value, 128);
        assert_eq!(result.chars().count(), 128);
        assert!(result.ends_with('…'));
    }
}
