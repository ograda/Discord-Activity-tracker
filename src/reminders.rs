use crate::{
    config::ReminderDefinition,
    storage::{ReminderEvent, ReminderRecord, Storage, unique_id, wall_time},
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderToast {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub sound: Option<String>,
    pub first_due: Instant,
    pub interval_minutes: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReminderSnapshot {
    pub pending: Vec<ReminderToast>,
}
#[derive(Debug, Clone)]
pub enum ReminderAction {
    Done(String),
    Snooze(String),
    Activate(String),
}
struct Occurrence {
    started: Instant,
    record: Option<ReminderRecord>,
}
struct RuntimeReminder {
    id: String,
    name: String,
    icon: Option<String>,
    sound: Option<String>,
    interval: Duration,
    snooze: Duration,
    next_due: Instant,
    occurrence: Option<Occurrence>,
    last_done: Option<Instant>,
}
pub struct ReminderManager {
    reminders: Vec<RuntimeReminder>,
    pending: VecDeque<String>,
    detailed: bool,
    records: Vec<ReminderRecord>,
    events: Vec<ReminderEvent>,
}
impl ReminderManager {
    pub fn new(definitions: &[ReminderDefinition], now: Instant) -> Self {
        Self {
            reminders: definitions
                .iter()
                .map(|d| RuntimeReminder {
                    id: d.name.clone(),
                    name: d.name.clone(),
                    icon: d.icon.clone(),
                    sound: d.sound.clone(),
                    interval: Duration::from_secs(d.interval_minutes.saturating_mul(60)),
                    snooze: Duration::from_secs(d.snooze_minutes.saturating_mul(60)),
                    next_due: now + Duration::from_secs(d.interval_minutes.saturating_mul(60)),
                    occurrence: None,
                    last_done: None,
                })
                .collect(),
            pending: VecDeque::new(),
            detailed: false,
            records: Vec::new(),
            events: Vec::new(),
        }
    }
    pub fn reload(&mut self, definitions: &[ReminderDefinition], now: Instant) {
        let mut replacements = Self::new(definitions, now).reminders;
        for replacement in &mut replacements {
            if let Some(index) = self.reminders.iter().position(|r| r.id == replacement.id) {
                let mut old = self.reminders.remove(index);
                old.name = replacement.name.clone();
                old.icon = replacement.icon.clone();
                old.sound = replacement.sound.clone();
                if old.interval != replacement.interval && old.occurrence.is_none() {
                    old.next_due = now + replacement.interval;
                }
                old.interval = replacement.interval;
                old.snooze = replacement.snooze;
                *replacement = old;
            }
        }
        self.finish_statistics("configuration_removed", now);
        self.reminders = replacements;
        self.pending
            .retain(|id| self.reminders.iter().any(|r| &r.id == id));
    }
    pub fn set_statistics(&mut self, enabled: bool, now: Instant) {
        if self.detailed && !enabled {
            self.finish_statistics("statistics_disabled", now);
        }
        self.detailed = enabled;
    }
    pub fn tick(&mut self, now: Instant) -> Vec<ReminderToast> {
        let wall = wall_time();
        let mut due = Vec::new();
        for reminder in &mut self.reminders {
            if now < reminder.next_due || self.pending.contains(&reminder.id) {
                continue;
            }
            let again = reminder.occurrence.is_some();
            let occurrence = reminder.occurrence.get_or_insert_with(|| Occurrence {
                started: now,
                record: self.detailed.then(|| ReminderRecord {
                    id: unique_id(),
                    name: reminder.name.clone(),
                    first_due: wall,
                    done: None,
                    elapsed_ms: 0,
                    snoozes: 0,
                    interval_ms: None,
                    status: "pending".into(),
                }),
            });
            if let Some(record) = &occurrence.record {
                self.events.push(ReminderEvent {
                    occurrence: record.id.clone(),
                    at: wall,
                    elapsed_ms: elapsed_ms(occurrence.started, now),
                    kind: if again { "PresentedAgain" } else { "Triggered" },
                });
            }
            self.pending.push_back(reminder.id.clone());
            due.push(reminder.id.clone());
        }
        due.iter().filter_map(|id| self.toast_for(id)).collect()
    }
    pub fn apply(&mut self, action: ReminderAction, now: Instant) -> bool {
        match action {
            ReminderAction::Done(id) => self.reschedule(&id, now, false),
            ReminderAction::Snooze(id) => self.reschedule(&id, now, true),
            ReminderAction::Activate(id) => {
                let Some(position) = self.pending.iter().position(|p| p == &id) else {
                    return false;
                };
                if position == 0 {
                    return false;
                }
                self.pending.remove(position);
                self.pending.push_front(id);
                true
            }
        }
    }
    pub fn snapshot(&self) -> ReminderSnapshot {
        ReminderSnapshot {
            pending: self
                .pending
                .iter()
                .filter_map(|id| self.toast_for(id))
                .collect(),
        }
    }
    fn toast_for(&self, id: &str) -> Option<ReminderToast> {
        let r = self.reminders.iter().find(|r| r.id == id)?;
        Some(ReminderToast {
            id: r.id.clone(),
            name: r.name.clone(),
            icon: r.icon.clone(),
            sound: r.sound.clone(),
            first_due: r.occurrence.as_ref()?.started,
            interval_minutes: r.interval.as_secs() / 60,
        })
    }
    fn reschedule(&mut self, id: &str, now: Instant, snoozed: bool) -> bool {
        let Some(position) = self.pending.iter().position(|p| p == id) else {
            return false;
        };
        self.pending.remove(position);
        let r = self
            .reminders
            .iter_mut()
            .find(|r| r.id == id)
            .expect("configured pending reminder");
        let wall = wall_time();
        if let Some(occurrence) = &mut r.occurrence
            && let Some(record) = &mut occurrence.record
        {
            record.elapsed_ms = elapsed_ms(occurrence.started, now);
            if snoozed {
                record.snoozes += 1;
            } else {
                record.done = Some(wall);
                record.status = "done".into();
                record.interval_ms = r.last_done.map(|last| elapsed_ms(last, now));
            }
            self.events.push(ReminderEvent {
                occurrence: record.id.clone(),
                at: wall,
                elapsed_ms: record.elapsed_ms,
                kind: if snoozed { "Snoozed" } else { "Done" },
            });
            self.records.push(record.clone());
        }
        if !snoozed {
            r.occurrence = None;
            r.last_done = Some(now);
        }
        r.next_due = now + if snoozed { r.snooze } else { r.interval };
        true
    }
    pub fn checkpoint(&mut self, store: &mut Storage, now: Instant) {
        for record in self.records.drain(..) {
            store.reminders.insert(record.id.clone(), record);
        }
        for r in &self.reminders {
            if let Some(o) = &r.occurrence
                && let Some(record) = &o.record
            {
                let mut record = record.clone();
                record.elapsed_ms = elapsed_ms(o.started, now);
                store.reminders.insert(record.id.clone(), record);
            }
        }
        store.reminder_events.append(&mut self.events);
    }
    pub fn finish_statistics(&mut self, status: &str, now: Instant) {
        for r in &mut self.reminders {
            if let Some(o) = &mut r.occurrence
                && let Some(mut record) = o.record.take()
            {
                record.elapsed_ms = elapsed_ms(o.started, now);
                record.status = status.into();
                self.records.push(record);
            }
        }
    }
}
fn elapsed_ms(start: Instant, now: Instant) -> u64 {
    now.saturating_duration_since(start).as_millis() as u64
}
pub fn format_elapsed(seconds: u64) -> String {
    if seconds < 3600 {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    } else {
        format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn definition(name: &str) -> ReminderDefinition {
        ReminderDefinition {
            name: name.into(),
            interval_minutes: 1,
            snooze_minutes: 1,
            icon: None,
            sound: None,
        }
    }
    #[test]
    fn queue_snooze_identity_and_statistics() {
        let start = Instant::now();
        let mut m = ReminderManager::new(
            &[
                definition("Water"),
                definition("Eyes"),
                definition("Stand"),
                definition("Move"),
            ],
            start,
        );
        m.set_statistics(true, start);
        let due = start + Duration::from_secs(60);
        assert_eq!(m.tick(due).len(), 4);
        assert!(m.tick(due).is_empty());
        assert!(m.apply(ReminderAction::Activate("Eyes".into()), due));
        assert_eq!(m.snapshot().pending[0].id, "Eyes");
        assert!(m.apply(
            ReminderAction::Snooze("Eyes".into()),
            due + Duration::from_secs(10)
        ));
        assert!(!m.apply(ReminderAction::Done("Eyes".into()), due));
        assert_eq!(m.tick(due + Duration::from_secs(70)).len(), 1);
        assert_eq!(m.snapshot().pending.last().unwrap().first_due, due);
        m.apply(
            ReminderAction::Done("Eyes".into()),
            due + Duration::from_secs(80),
        );
        let mut store = Storage::new(None);
        m.checkpoint(&mut store, due + Duration::from_secs(80));
        let r = store.reminders.values().find(|r| r.name == "Eyes").unwrap();
        assert_eq!(r.snoozes, 1);
        assert_eq!(r.elapsed_ms, 80_000);
        assert_eq!(r.status, "done");
        assert_eq!(
            store
                .reminder_events
                .iter()
                .filter(|e| e.occurrence == r.id)
                .map(|e| e.kind)
                .collect::<Vec<_>>(),
            ["Triggered", "Snoozed", "PresentedAgain", "Done"]
        );
    }
    #[test]
    fn disabled_statistics_and_timer_format() {
        let start = Instant::now();
        let mut m = ReminderManager::new(&[definition("Water")], start);
        let now = start + Duration::from_secs(60);
        m.tick(now);
        m.apply(ReminderAction::Done("Water".into()), now);
        let mut store = Storage::new(None);
        m.checkpoint(&mut store, now);
        assert!(store.reminders.is_empty() && store.reminder_events.is_empty());
        assert_eq!(format_elapsed(42), "00:42");
        assert_eq!(format_elapsed(3661), "01:01:01");
    }

    #[test]
    fn reload_preserves_snoozed_occurrence_and_pending_order() {
        let start = Instant::now();
        let mut m = ReminderManager::new(&[definition("Water"), definition("Eyes")], start);
        m.set_statistics(true, start);
        let due = start + Duration::from_secs(60);
        m.tick(due);
        m.apply(ReminderAction::Snooze("Water".into()), due);
        let id = m.reminders[0]
            .occurrence
            .as_ref()
            .unwrap()
            .record
            .as_ref()
            .unwrap()
            .id
            .clone();
        m.reload(
            &[definition("Water"), definition("Eyes"), definition("Move")],
            due + Duration::from_secs(1),
        );
        assert_eq!(m.snapshot().pending[0].id, "Eyes");
        assert_eq!(m.tick(due + Duration::from_secs(60)).len(), 1);
        assert_eq!(
            m.reminders[0]
                .occurrence
                .as_ref()
                .unwrap()
                .record
                .as_ref()
                .unwrap()
                .id,
            id
        );
        m.reload(&[definition("Eyes")], due + Duration::from_secs(61));
        assert_eq!(m.snapshot().pending.len(), 1);
        assert!(
            m.records
                .iter()
                .any(|r| r.id == id && r.status == "configuration_removed")
        );
    }
    #[test]
    fn disabling_statistics_ends_recording_but_preserves_snooze_and_queue() {
        let start = Instant::now();
        let mut m = ReminderManager::new(&[definition("Water")], start);
        m.set_statistics(true, start);
        let due = start + Duration::from_secs(60);
        m.tick(due);
        m.set_statistics(false, due + Duration::from_secs(1));
        m.apply(
            ReminderAction::Snooze("Water".into()),
            due + Duration::from_secs(2),
        );
        m.tick(due + Duration::from_secs(62));
        m.apply(
            ReminderAction::Done("Water".into()),
            due + Duration::from_secs(70),
        );
        let mut store = Storage::new(None);
        m.checkpoint(&mut store, due + Duration::from_secs(70));
        assert_eq!(store.reminder_events.len(), 1);
        let r = store.reminders.values().next().unwrap();
        assert_eq!(r.status, "statistics_disabled");
        assert_eq!(r.elapsed_ms, 1000);
        assert!(r.done.is_none());
        assert!(m.snapshot().pending.is_empty());
    }
}
