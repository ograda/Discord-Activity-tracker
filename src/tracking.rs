use crate::{
    resolver::ActivityObservation,
    settings::{Settings, TrackingMode},
    storage::{ActivityEvent, Metrics, SessionRecord, Storage, unique_id},
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

struct LiveActivity {
    observation: ActivityObservation,
    metrics: Metrics,
    history: Option<SessionRecord>,
}
pub struct ActivityTracker {
    live: HashMap<String, LiveActivity>,
    previous: Instant,
    max_gap: Duration,
}
impl ActivityTracker {
    pub fn new(now: Instant, poll: Duration) -> Self {
        Self {
            live: HashMap::new(),
            previous: now,
            max_gap: (poll * 3).max(Duration::from_secs(15)),
        }
    }
    pub fn sample(
        &mut self,
        observations: &[ActivityObservation],
        now: Instant,
        wall: i64,
        settings: &Settings,
        store: &mut Storage,
    ) {
        let delta = now.saturating_duration_since(self.previous);
        self.previous = now;
        // Previous observation owns the measured interval. Exclude the whole gap
        // after suspend/stalls; guessing foreground use would fabricate history.
        let ms = if delta <= self.max_gap {
            delta.as_millis() as u64
        } else {
            eprintln!("Excluded observation gap of {} seconds", delta.as_secs());
            0
        };
        for live in self.live.values_mut() {
            let metrics = Metrics {
                open_ms: ms,
                active_ms: if live.observation.foreground { ms } else { 0 },
            };
            live.metrics.add(metrics);
            if settings.persist_activity_time {
                store.add_time(&live.observation.name, metrics);
            }
            if let Some(row) = &mut live.history {
                row.metrics.add(metrics);
                row.last_seen = wall;
            }
        }
        let closed: Vec<_> = self
            .live
            .keys()
            .filter(|name| !observations.iter().any(|o| &o.name == *name))
            .cloned()
            .collect();
        for name in closed {
            let live = self.live.remove(&name).expect("existing");
            if settings.detailed_statistics {
                event(store, &live.observation, wall, "closed");
            }
            close_history(live.history, wall, "closed", store);
        }
        for observation in observations {
            let live = self
                .live
                .entry(observation.name.clone())
                .or_insert_with(|| {
                    if settings.detailed_statistics {
                        event(store, observation, wall, "opened");
                    }
                    LiveActivity {
                        observation: observation.clone(),
                        metrics: Metrics::default(),
                        history: None,
                    }
                });
            if settings.detailed_statistics {
                if live.observation.pids != observation.pids {
                    event(store, observation, wall, "processes_changed");
                }
                if live.observation.window_count != observation.window_count {
                    event(store, observation, wall, "windows_changed");
                }
                if live.observation.foreground != observation.foreground {
                    event(store, observation, wall, "foreground_changed");
                }
            }
            live.observation = observation.clone();
            if settings.session_history || settings.detailed_statistics {
                let row = live.history.get_or_insert_with(|| SessionRecord {
                    id: unique_id(),
                    activity: observation.name.clone(),
                    start: wall,
                    last_seen: wall,
                    end: None,
                    metrics: Metrics::default(),
                    peak_instances: observation.process_count(),
                    peak_window_instances: Some(observation.instance_count()),
                    end_reason: None,
                });
                row.last_seen = wall;
                row.peak_instances = row.peak_instances.max(observation.process_count());
                row.peak_window_instances = Some(
                    row.peak_window_instances
                        .unwrap_or(0)
                        .max(observation.instance_count()),
                );
                store.sessions.insert(row.id.clone(), row.clone());
            } else {
                close_history(live.history.take(), wall, "history_disabled", store);
            }
        }
    }
    pub fn settings_changed(
        &mut self,
        old: &Settings,
        new: &Settings,
        wall: i64,
        store: &mut Storage,
    ) {
        if old.detailed_statistics != new.detailed_statistics {
            for live in self.live.values() {
                event(
                    store,
                    &live.observation,
                    wall,
                    if new.detailed_statistics {
                        "observation_enabled"
                    } else {
                        "observation_disabled"
                    },
                );
            }
        }
        if (old.session_history || old.detailed_statistics)
            && !(new.session_history || new.detailed_statistics)
        {
            for live in self.live.values_mut() {
                close_history(live.history.take(), wall, "history_disabled", store);
            }
        }
    }
    pub fn displayed_ms(&self, name: &str, settings: &Settings, store: &Storage) -> u64 {
        let m = if settings.persist_activity_time {
            store.totals.get(name).copied().unwrap_or_default()
        } else {
            self.live.get(name).map(|l| l.metrics).unwrap_or_default()
        };
        match settings.tracking_mode {
            TrackingMode::Total => m.open_ms,
            TrackingMode::Active => m.active_ms,
        }
    }
    pub fn shutdown(&mut self, now: Instant, wall: i64, settings: &Settings, store: &mut Storage) {
        let observations: Vec<_> = self.live.values().map(|l| l.observation.clone()).collect();
        self.sample(&observations, now, wall, settings, store);
        for (_, live) in self.live.drain() {
            if settings.detailed_statistics {
                event(store, &live.observation, wall, "observation_stopped");
            }
            close_history(live.history, wall, "dat_shutdown", store);
        }
    }
}
fn close_history(row: Option<SessionRecord>, wall: i64, reason: &str, store: &mut Storage) {
    if let Some(mut row) = row {
        row.end = Some(wall);
        row.end_reason = Some(reason.into());
        store.sessions.insert(row.id.clone(), row);
    }
}
fn event(store: &mut Storage, observation: &ActivityObservation, at: i64, kind: &str) {
    store.activity_events.push(ActivityEvent {
        activity: observation.name.clone(),
        at,
        kind: kind.into(),
        pids: observation
            .pids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(","),
        foreground: observation.foreground,
        window_count: observation.window_count,
    });
}
#[cfg(test)]
mod tests {
    #[test]
    fn visibility_changes_do_not_pause_local_time_or_reminders() {
        let start = Instant::now();
        let mut tracker = ActivityTracker::new(start, Duration::from_secs(1));
        let mut store = Storage::new(None);
        let settings = Settings {
            detailed_statistics: true,
            ..Settings::default()
        };
        let activity = [ActivityObservation {
            name: "Game".into(),
            pids: vec![1],
            foreground: true,
            window_count: 1,
        }];
        let definition = crate::config::ReminderDefinition {
            name: "Water".into(),
            interval_minutes: 1,
            snooze_minutes: 1,
            icon: None,
            sound: None,
        };
        let mut reminders = crate::reminders::ReminderManager::new(&[definition], start);
        let mut session = crate::discord::PresenceSession::default();
        for second in 0..=60 {
            let hidden = (10..50).contains(&second);
            let now = start + Duration::from_secs(second);
            assert_eq!(
                session.observe(true, hidden, 1000 + second as i64 * 1000),
                Some(1000)
            );
            tracker.sample(&activity, now, second as i64 * 1000, &settings, &mut store);
            reminders.tick(now);
        }
        assert_eq!(store.totals["Game"].open_ms, 60_000);
        assert_eq!(store.totals["Game"].active_ms, 60_000);
        assert_eq!(store.sessions.len(), 1);
        assert_eq!(reminders.snapshot().pending.len(), 1);
    }

    #[test]
    fn process_and_window_peaks_stay_separate_without_multiplying_time() {
        let now = Instant::now();
        let mut tracker = ActivityTracker::new(now, Duration::from_secs(1));
        let mut store = Storage::new(None);
        let settings = Settings {
            detailed_statistics: true,
            ..Settings::default()
        };
        let mut code = ActivityObservation {
            name: "Code".into(),
            pids: (1..=10).collect(),
            window_count: 1,
            foreground: true,
        };
        tracker.sample(&[code.clone()], now, 0, &settings, &mut store);
        code.window_count = 2;
        tracker.sample(
            &[code.clone()],
            now + Duration::from_secs(1),
            1000,
            &settings,
            &mut store,
        );
        tracker.sample(
            &[],
            now + Duration::from_secs(2),
            2000,
            &settings,
            &mut store,
        );
        assert_eq!(store.totals["Code"].open_ms, 2000);
        assert_eq!(store.totals["Code"].active_ms, 2000);
        let row = store.sessions.values().next().unwrap();
        assert_eq!(row.peak_instances, 10);
        assert_eq!(row.peak_window_instances, Some(2));
        assert_eq!(
            store
                .activity_events
                .iter()
                .filter(|e| e.kind == "windows_changed")
                .count(),
            1
        );
        assert!(
            !store
                .activity_events
                .iter()
                .any(|e| e.kind == "processes_changed")
        );
    }

    use super::*;
    fn obs(name: &str, pids: &[u32], foreground: bool) -> ActivityObservation {
        ActivityObservation {
            name: name.into(),
            pids: pids.into(),
            foreground,
            window_count: pids.len(),
        }
    }
    #[test]
    fn concurrent_total_foreground_instances_reopen_and_shutdown() {
        let now = Instant::now();
        let mut tracker = ActivityTracker::new(now, Duration::from_secs(300));
        let mut store = Storage::new(None);
        let settings = Settings {
            session_history: true,
            ..Settings::default()
        };
        let a = vec![obs("WoW", &[1, 2], true), obs("Code", &[3], false)];
        tracker.sample(&a, now, 0, &settings, &mut store);
        let b = vec![obs("WoW", &[2], false), obs("Code", &[3], true)];
        tracker.sample(
            &b,
            now + Duration::from_secs(300),
            300_000,
            &settings,
            &mut store,
        );
        tracker.sample(
            &[],
            now + Duration::from_secs(600),
            600_000,
            &settings,
            &mut store,
        );
        assert_eq!(
            store.totals["WoW"],
            Metrics {
                open_ms: 600_000,
                active_ms: 300_000
            }
        );
        assert_eq!(store.totals["Code"], store.totals["WoW"]);
        assert_eq!(store.sessions.len(), 2);
        assert!(store.sessions.values().all(|s| s.end.is_some()));
        tracker.sample(
            &a,
            now + Duration::from_secs(601),
            601_000,
            &settings,
            &mut store,
        );
        tracker.shutdown(
            now + Duration::from_secs(901),
            901_000,
            &settings,
            &mut store,
        );
        assert_eq!(store.totals["WoW"].open_ms, 900_000);
        assert!(store.activity_events.is_empty());
    }
    #[test]
    fn pause_gap_and_disabled_persistence() {
        let now = Instant::now();
        let mut tracker = ActivityTracker::new(now, Duration::from_secs(5));
        let mut store = Storage::new(None);
        let settings = Settings {
            persist_activity_time: false,
            ..Settings::default()
        };
        let a = vec![obs("Game", &[1], true)];
        tracker.sample(&a, now, 0, &settings, &mut store);
        tracker.sample(
            &a,
            now + Duration::from_secs(5),
            5000,
            &settings,
            &mut store,
        );
        tracker.sample(
            &a,
            now + Duration::from_secs(3600),
            3_600_000,
            &settings,
            &mut store,
        );
        assert_eq!(tracker.displayed_ms("Game", &settings, &store), 5000);
        assert!(
            store.totals.is_empty()
                && store.sessions.is_empty()
                && store.activity_events.is_empty()
        );
    }

    #[test]
    fn rapid_foreground_switches_and_mode_changes_do_not_reset_totals() {
        let now = Instant::now();
        let mut tracker = ActivityTracker::new(now, Duration::from_secs(1));
        let mut store = Storage::new(None);
        let mut settings = Settings {
            detailed_statistics: true,
            ..Settings::default()
        };
        for second in 0..=10 {
            tracker.sample(
                &[
                    obs("A", &[1, 2], second % 2 == 0),
                    obs("B", &[3], second % 2 != 0),
                ],
                now + Duration::from_secs(second),
                second as i64 * 1000,
                &settings,
                &mut store,
            );
        }
        assert_eq!(tracker.displayed_ms("A", &settings, &store), 10_000);
        settings.tracking_mode = TrackingMode::Active;
        assert_eq!(tracker.displayed_ms("A", &settings, &store), 5_000);
        assert_eq!(tracker.displayed_ms("B", &settings, &store), 5_000);
        assert_eq!(
            store
                .activity_events
                .iter()
                .filter(|e| e.kind == "opened")
                .count(),
            2
        );
        assert_eq!(
            store
                .activity_events
                .iter()
                .filter(|e| e.kind == "foreground_changed")
                .count(),
            20
        );
        let count = store.activity_events.len();
        tracker.sample(
            &[obs("A", &[1, 2], true), obs("B", &[3], false)],
            now + Duration::from_secs(11),
            11_000,
            &settings,
            &mut store,
        );
        assert_eq!(store.activity_events.len(), count); // no row per unchanged sample
    }
    #[test]
    fn runtime_privacy_switches_close_history_and_do_not_backfill() {
        let now = Instant::now();
        let mut tracker = ActivityTracker::new(now, Duration::from_secs(1));
        let mut store = Storage::new(None);
        let enabled = Settings {
            detailed_statistics: true,
            ..Settings::default()
        };
        let disabled = Settings {
            persist_activity_time: false,
            ..Settings::default()
        };
        let a = [obs("Game", &[1], true)];
        tracker.sample(&a, now, 0, &enabled, &mut store);
        tracker.sample(&a, now + Duration::from_secs(5), 5000, &enabled, &mut store);
        tracker.settings_changed(&enabled, &disabled, 5000, &mut store);
        let event_count = store.activity_events.len();
        tracker.sample(
            &[],
            now + Duration::from_secs(10),
            10000,
            &disabled,
            &mut store,
        );
        assert_eq!(store.totals["Game"].open_ms, 5000);
        assert_eq!(store.activity_events.len(), event_count);
        let row = store.sessions.values().next().unwrap();
        assert_eq!(row.end, Some(5000));
        assert_eq!(row.metrics.open_ms, 5000);
        tracker.sample(
            &a,
            now + Duration::from_secs(11),
            11000,
            &disabled,
            &mut store,
        );
        assert_eq!(tracker.displayed_ms("Game", &disabled, &store), 0);
    }
}
