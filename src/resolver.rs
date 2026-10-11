use crate::{
    config::{ActivityDefinition, InstanceCountMode},
    monitor::{ProcessSnapshot, normalize_process_name},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityObservation {
    pub name: String,
    pub pids: Vec<u32>,
    pub foreground: bool,
    pub window_count: usize,
}
impl ActivityObservation {
    pub fn process_count(&self) -> usize {
        self.pids.len()
    }
    // Presentation choice only: raw counts and session accounting stay unchanged.
    pub fn displayed_instances(&self, mode: InstanceCountMode) -> usize {
        match mode {
            InstanceCountMode::Windows => self.instance_count(),
            InstanceCountMode::Processes => self.process_count(),
        }
    }
    pub fn instance_count(&self) -> usize {
        self.window_count.max(1)
    }
}

pub fn resolve_activities(
    definitions: &[ActivityDefinition],
    snapshot: &ProcessSnapshot,
) -> Vec<ActivityObservation> {
    definitions
        .iter()
        .filter_map(|definition| {
            let names: Vec<_> = definition
                .process_names()
                .iter()
                .map(|n| normalize_process_name(n))
                .collect();
            let mut pids: Vec<_> = snapshot
                .processes
                .iter()
                .filter(|(_, name)| names.contains(name))
                .map(|(pid, _)| *pid)
                .collect();
            pids.sort_unstable();
            pids.dedup();
            if pids.is_empty() {
                return None;
            }
            let foreground = snapshot
                .foreground_pid
                .is_some_and(|pid| pids.contains(&pid));
            let window_count = snapshot
                .application_windows
                .iter()
                .filter(|pid| pids.contains(pid))
                .count();
            Some(ActivityObservation {
                window_count,
                name: definition.name.clone(),
                pids,
                foreground,
            })
        })
        .collect()
}
#[cfg(test)]
mod tests {

    #[test]
    fn electron_helpers_are_processes_not_user_instances() {
        let definitions: Vec<ActivityDefinition> = vec![
            quick_xml::de::from_str(
                "<activity><name>Code</name><instance-count-mode>windows</instance-count-mode><windows-process>Code.exe</windows-process></activity>",
            )
            .unwrap(),
        ];
        let mut snapshot = ProcessSnapshot {
            processes: (1..=10).map(|pid| (pid, "code.exe".into())).collect(),
            application_windows: vec![1],
            foreground_pid: Some(1),
        };
        let result = resolve_activities(&definitions, &snapshot);
        assert_eq!(result[0].process_count(), 10);
        assert_eq!(result[0].window_count, 1);
        assert_eq!(result[0].instance_count(), 1);
        snapshot.application_windows.push(1); // two top-level windows in one PID
        let result = resolve_activities(&definitions, &snapshot);
        assert_eq!(result[0].instance_count(), 2);
        assert_eq!(result[0].process_count(), 10);
        snapshot.application_windows.clear();
        snapshot.foreground_pid = Some(999);
        let result = resolve_activities(&definitions, &snapshot);
        assert_eq!(result[0].window_count, 0);
        assert_eq!(result[0].instance_count(), 1);
        assert!(!result[0].foreground);
    }

    #[test]
    fn xml_instance_modes_keep_raw_counts_and_deduplicate_aliases() {
        for (tag, mode, expected) in [
            ("", InstanceCountMode::Windows, 1),
            (
                "<instance-count-mode>windows</instance-count-mode>",
                InstanceCountMode::Windows,
                1,
            ),
            (
                "<instance-count-mode>processes</instance-count-mode>",
                InstanceCountMode::Processes,
                3,
            ),
        ] {
            let definition: ActivityDefinition = quick_xml::de::from_str(&format!("<activity><name>Foo</name>{tag}<windows-process>Foo.exe</windows-process><windows-process>FooLegacy.exe</windows-process><windows-process>FOO.exe</windows-process></activity>")).unwrap();
            assert_eq!(definition.instance_count_mode, mode);
            let snapshot = ProcessSnapshot {
                processes: [
                    (100, "foo.exe".into()),
                    (200, "foo.exe".into()),
                    (300, "foolegacy.exe".into()),
                ]
                .into(),
                application_windows: vec![100],
                foreground_pid: Some(300),
            };
            let result = resolve_activities(&[definition], &snapshot);
            assert_eq!(result[0].pids, vec![100, 200, 300]);
            assert_eq!(result[0].process_count(), 3);
            assert_eq!(result[0].window_count, 1);
            assert!(result[0].foreground);
            assert_eq!(result[0].displayed_instances(mode), expected);
        }
    }

    use super::*;
    #[test]
    fn aliases_counts_foreground_and_order() {
        let definitions: Vec<ActivityDefinition> = ["<activity><name>Game</name><windows-process> WOW.exe </windows-process><windows-process>launcher.exe</windows-process><windows-process>wow.exe</windows-process></activity>", "<activity><name>Code</name><windows-process>code.exe</windows-process></activity>"].iter().map(|s| quick_xml::de::from_str(s).unwrap()).collect();
        let mut snapshot = ProcessSnapshot {
            processes: [
                (100, "wow.exe".into()),
                (200, "wow.exe".into()),
                (300, "code.exe".into()),
            ]
            .into(),
            foreground_pid: Some(200),
            application_windows: vec![100, 200, 300],
        };
        let resolved = resolve_activities(&definitions, &snapshot);
        assert_eq!(resolved[0].name, "Game");
        assert_eq!(resolved[0].instance_count(), 2);
        assert!(resolved[0].foreground);
        assert!(!resolved[1].foreground);
        snapshot.processes.remove(&100);
        assert_eq!(
            resolve_activities(&definitions, &snapshot)[0].instance_count(),
            1
        );
        snapshot.processes.clear();
        assert!(resolve_activities(&definitions, &snapshot).is_empty());
    }
}
