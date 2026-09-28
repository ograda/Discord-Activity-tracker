use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::config::ReminderDefinition;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderToast {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub sound: Option<String>,
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

struct RuntimeReminder {
    id: String,
    name: String,
    icon: Option<String>,
    sound: Option<String>,

    interval: Duration,
    snooze: Duration,

    next_due: Instant,
}

pub struct ReminderManager {
    reminders: Vec<RuntimeReminder>,
    pending: VecDeque<String>,
}

impl ReminderManager {
    pub fn new(definitions: &[ReminderDefinition], now: Instant) -> Self {
        let reminders = definitions
            .iter()
            .map(|definition| {
                let interval = Duration::from_secs(definition.interval_minutes.saturating_mul(60));

                let snooze = Duration::from_secs(definition.snooze_minutes.saturating_mul(60));

                RuntimeReminder {
                    id: definition.name.clone(),
                    name: definition.name.clone(),
                    icon: definition.icon.clone(),
                    sound: definition.sound.clone(),

                    interval,
                    snooze,

                    next_due: now + interval,
                }
            })
            .collect();

        Self {
            reminders,
            pending: VecDeque::new(),
        }
    }

    /// Checks which reminders became due during this tick.
    ///
    /// Newly-due reminders are added to the pending queue and returned so the
    /// caller can react to the event (for example, refresh the HUD and play a
    /// notification sound).
    ///
    /// A reminder that is already pending is never queued a second time.
    pub fn tick(&mut self, now: Instant) -> Vec<ReminderToast> {
        let due_ids: Vec<String> = self
            .reminders
            .iter()
            .filter(|reminder| {
                now >= reminder.next_due
                    && !self
                        .pending
                        .iter()
                        .any(|pending_id| pending_id == &reminder.id)
            })
            .map(|reminder| reminder.id.clone())
            .collect();

        if due_ids.is_empty() {
            return Vec::new();
        }

        for id in &due_ids {
            self.pending.push_back(id.clone());
        }

        due_ids.iter().filter_map(|id| self.toast_for(id)).collect()
    }

    /// Applies an action coming from the reminder HUD.
    ///
    /// Returns true when the visible queue/state changed and the HUD should be
    /// rendered again.
    pub fn apply(&mut self, action: ReminderAction, now: Instant) -> bool {
        match action {
            ReminderAction::Done(id) => self.reschedule(&id, now, false),

            ReminderAction::Snooze(id) => self.reschedule(&id, now, true),

            ReminderAction::Activate(id) => {
                let Some(position) = self.pending.iter().position(|pending_id| pending_id == &id)
                else {
                    return false;
                };

                if position == 0 {
                    return false;
                }

                let Some(id) = self.pending.remove(position) else {
                    return false;
                };

                self.pending.push_front(id);

                true
            }
        }
    }

    /// Returns the current queue in display order.
    ///
    /// pending[0] is the active/full-size toast. The remaining entries are the
    /// compact stacked toasts shown above it.
    pub fn snapshot(&self) -> ReminderSnapshot {
        let pending = self
            .pending
            .iter()
            .filter_map(|id| self.toast_for(id))
            .collect();

        ReminderSnapshot { pending }
    }

    fn toast_for(&self, id: &str) -> Option<ReminderToast> {
        let reminder = self.reminders.iter().find(|reminder| reminder.id == id)?;

        Some(ReminderToast {
            id: reminder.id.clone(),
            name: reminder.name.clone(),
            icon: reminder.icon.clone(),
            sound: reminder.sound.clone(),
        })
    }

    fn reschedule(&mut self, id: &str, now: Instant, snoozed: bool) -> bool {
        let Some(position) = self.pending.iter().position(|pending_id| pending_id == id) else {
            return false;
        };

        self.pending.remove(position);

        let Some(reminder) = self.reminders.iter_mut().find(|reminder| reminder.id == id) else {
            return false;
        };

        reminder.next_due = if snoozed {
            now + reminder.snooze
        } else {
            now + reminder.interval
        };

        true
    }
}
