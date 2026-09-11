use std::collections::HashSet;

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

pub struct ProcessMonitor {
    system: System,
}

impl ProcessMonitor {
    pub fn new() -> Self {
        Self {
            system: System::new(),
        }
    }

    pub fn running_process_names(&mut self) -> HashSet<String> {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );

        self.system
            .processes()
            .values()
            .map(|process| normalize_process_name(&process.name().to_string_lossy()))
            .collect()
    }
}

pub fn normalize_process_name(name: &str) -> String {
    name.trim().to_lowercase()
}

