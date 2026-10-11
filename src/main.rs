#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#[cfg(not(windows))]
compile_error!("discord-activity-mvp currently supports Windows only");

mod config;
mod discord;
mod monitor;
mod reminder_hud;
mod reminder_sound;
mod reminders;
mod resolver;
mod settings;
mod settings_window;
mod statistics_window;
mod storage;
mod tracking;
mod tray;
mod tray_icon;

use crate::{
    config::Config,
    discord::{DiscordPresence, PresenceSession, PresenceText, fit_discord_text},
    monitor::ProcessMonitor,
    reminders::{ReminderAction, ReminderManager},
    resolver::{ActivityObservation, resolve_activities},
    settings::{PresenceDisplay, Settings, SharedSettings},
    storage::{Storage, wall_time},
    tracking::ActivityTracker,
    tray::TrayCommand,
};
use anyhow::{Context, Result, anyhow};
use std::{
    env,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
const PRESENCE_REFRESH_SECONDS: u64 = 60;
const IMAGE_ROTATION_SECONDS: u64 = 30;

fn main() -> Result<()> {
    let config_path = config_path();
    let config_directory = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let config = Config::load(&config_path)
        .with_context(|| format!("could not load {}", config_path.display()))?;
    let directory = settings::data_directory();
    // Exclusive per-user data ownership prevents two DAT executions double-counting
    // the same observations or recovering each other's unfinished sessions.
    let _data_lock = match &directory {
        Ok(directory) => {
            use std::os::windows::fs::OpenOptionsExt;
            let result = std::fs::create_dir_all(directory).and_then(|()| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .share_mode(0)
                    .open(directory.join("dat.lock"))
            });
            match result {
                Ok(lock) => Some(lock),
                Err(error) if error.raw_os_error() == Some(32) => {
                    eprintln!("DAT is already running for this data directory.");
                    return Ok(());
                }
                Err(error) => {
                    eprintln!("Could not acquire local data lock: {error}");
                    None
                }
            }
        }
        Err(_) => None,
    };
    let initial_settings = match &directory {
        Ok(directory) => Settings::load(&directory.join("settings.xml")).unwrap_or_else(|error| {
            eprintln!("Could not load settings; using defaults: {error:#}");
            Settings::default()
        }),
        Err(error) => {
            eprintln!("Local storage unavailable: {error:#}");
            Settings::default()
        }
    };
    let database_path = directory
        .ok()
        .filter(|_| _data_lock.is_some())
        .map(|p| p.join("dat.db"));
    let shared_settings = Arc::new(Mutex::new(initial_settings));
    let (tray_sender, tray_receiver) = mpsc::channel();
    let hidden = Arc::new(AtomicBool::new(false));
    tray::start(tray_sender, shared_settings.clone(), hidden.clone());
    let (reminder_sender, reminder_receiver) = mpsc::channel();
    reminder_hud::start(reminder_sender);
    let running = Arc::new(AtomicBool::new(true));
    let signal = running.clone();
    ctrlc::set_handler(move || signal.store(false, Ordering::SeqCst))
        .context("could not install Ctrl+C handler")?;
    let tracker_hidden = hidden.clone();
    let tracker_running = running.clone();
    let (reload_sender, reload_receiver) = mpsc::channel();
    let tracker_thread = thread::Builder::new()
        .name("activity-tracker".into())
        .spawn(move || {
            run_tracker(
                config,
                config_directory,
                TrackerChannels {
                    running: tracker_running,
                    hidden: tracker_hidden,
                    reminder_actions: reminder_receiver,
                    reloads: reload_receiver,
                    settings: shared_settings,
                },
                database_path,
            )
        })
        .context("could not start activity-tracker thread")?;
    while running.load(Ordering::SeqCst) {
        match tray_receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(TrayCommand::Reload) => match Config::load(&config_path) {
                Ok(config) => {
                    let _ = reload_sender.send(config);
                }
                Err(error) => eprintln!(
                    "Configuration reload failed; keeping current configuration: {error:#}"
                ),
            },
            Ok(TrayCommand::Exit) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                running.store(false, Ordering::SeqCst)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    let result = tracker_thread
        .join()
        .map_err(|_| anyhow!("activity-tracker thread panicked"));
    reminder_hud::shutdown();
    tray::shutdown();
    result?;
    Ok(())
}
struct TrackerChannels {
    running: Arc<AtomicBool>,
    hidden: Arc<AtomicBool>,
    reminder_actions: mpsc::Receiver<ReminderAction>,
    reloads: mpsc::Receiver<Config>,
    settings: SharedSettings,
}
fn run_tracker(
    mut config: Config,
    config_directory: PathBuf,
    channels: TrackerChannels,
    database_path: Option<PathBuf>,
) {
    let mut monitor = ProcessMonitor::new();
    let mut discord = DiscordPresence::new(config.discord_application_id.clone());
    let start = Instant::now();
    let mut settings = channels
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let mut reminders = ReminderManager::new(config.reminders.definitions(), start);
    reminders.set_statistics(settings.detailed_statistics, start);
    let mut store = Storage::new(database_path);
    flush_storage(&mut store);
    let mut tracker = ActivityTracker::new(start, Duration::from_secs(1));
    let mut observations = Vec::<ActivityObservation>::new();
    let mut next_poll = start;
    let mut next_sample = start;
    let mut next_flush = start + Duration::from_secs(30);
    let mut rotation_started = start;
    let mut session = PresenceSession::default();
    let mut last_sent = None;
    let mut last_successful_send: Option<Instant> = None;
    let mut generic_applied = None;
    let mut next_presence_attempt = start;
    let mut previously_hidden = false;
    let mut previous_target = PresentationTarget::Idle;

    while channels.running.load(Ordering::SeqCst) {
        let now = Instant::now();
        let wall = wall_time();
        while let Ok(new_config) = channels.reloads.try_recv() {
            tracker.sample(&observations, now, wall, &settings, &mut store);
            if config.discord_application_id != new_config.discord_application_id {
                if let Err(error) = discord.clear() {
                    eprintln!("Could not clear old Discord application: {error:#}");
                }
                discord = DiscordPresence::new(new_config.discord_application_id.clone());
            }
            reminders.reload(new_config.reminders.definitions(), now);
            reminder_hud::render(reminders.snapshot());
            config = new_config;
            next_poll = now;
            next_presence_attempt = now;
            last_sent = None;
            last_successful_send = None;
            generic_applied = None;
            println!("Reloaded activities.xml");
        }
        let new_settings = channels
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if settings != new_settings {
            tracker.sample(&observations, now, wall, &settings, &mut store);
            tracker.settings_changed(&settings, &new_settings, wall, &mut store);
            reminders.set_statistics(new_settings.detailed_statistics, now);
            if settings.display_focused_activity != new_settings.display_focused_activity {
                rotation_started = now;
            }
            settings = new_settings;
            next_poll = now;
            next_presence_attempt = now;
            last_sent = None;
            last_successful_send = None;
            next_flush = now;
        }
        let mut reminders_changed = false;
        while let Ok(action) = channels.reminder_actions.try_recv() {
            reminders_changed |= reminders.apply(action, now);
        }
        let newly_due = reminders.tick(now);
        if reminders_changed || !newly_due.is_empty() {
            reminder_hud::render(reminders.snapshot());
        }
        if let Some(sound) = newly_due.first().and_then(|r| r.sound.as_deref()) {
            reminder_sound::play(&config_directory.join(sound));
        }
        reminders.checkpoint(&mut store, now);

        if now >= next_sample || now >= next_poll {
            let refresh_processes = now >= next_poll;
            let detected =
                resolve_activities(&config.activities, &monitor.observe(refresh_processes));
            if detected
                .iter()
                .map(|o| &o.name)
                .ne(observations.iter().map(|o| &o.name))
            {
                rotation_started = now;
                println!(
                    "Detected: {}",
                    detected
                        .iter()
                        .map(|o| o.name.as_str())
                        .collect::<Vec<_>>()
                        .join(" + ")
                );
            }
            observations = detected;
            tracker.sample(&observations, now, wall, &settings, &mut store);
            if refresh_processes {
                next_poll = now + Duration::from_secs(config.poll_seconds);
            }
            next_sample = now + Duration::from_secs(1);
        }
        if now >= next_flush {
            flush_storage(&mut store);
            next_flush = now + Duration::from_secs(30);
        }
        let hidden = channels.hidden.load(Ordering::SeqCst);
        if hidden != previously_hidden {
            next_presence_attempt = now;
        }
        if previously_hidden && !hidden {
            rotation_started = now;
        }
        let target = presentation_target(
            &observations,
            settings.display_focused_activity,
            hidden,
            now.saturating_duration_since(rotation_started),
        );
        // A new focus observation must bypass the previous successful send's
        // cooldown, while unchanged targets retain normal retry pacing.
        if settings.display_focused_activity && target != previous_target {
            next_presence_attempt = now;
        }
        let generic = target.is_generic();
        let refresh_due = last_successful_send.is_none_or(|sent| {
            now.saturating_duration_since(sent) >= Duration::from_secs(PRESENCE_REFRESH_SECONDS)
        });
        // Visibility only changes presentation; all local work above continues.
        // Both payloads use the same session timestamp across Hide/Show.
        let session_started = session.observe(!observations.is_empty(), generic, wall);
        if generic {
            if (generic_applied != Some(target) || refresh_due) && now >= next_presence_attempt {
                next_presence_attempt = now + Duration::from_secs(config.poll_seconds);
                let started = session_started.expect("generic presence has a session");
                let result = match target {
                    PresentationTarget::Untracked(count) => discord.set_untracked(started, count),
                    _ => discord.set_hidden(started),
                };
                match result {
                    Ok(()) => {
                        generic_applied = Some(target);
                        last_successful_send = Some(now);
                    }
                    Err(error) => eprintln!("Could not update generic Discord presence: {error:#}"),
                }
            }
            last_sent = None;
        } else {
            if previous_target.is_generic() {
                last_successful_send = None;
                generic_applied = None;
            }
            if target == PresentationTarget::Idle {
                if (last_sent.is_some() || refresh_due || previous_target.is_generic())
                    && now >= next_presence_attempt
                {
                    next_presence_attempt = now + Duration::from_secs(config.poll_seconds);
                    match discord.clear() {
                        Ok(()) => {
                            last_successful_send = Some(now);
                            last_sent = None;
                        }
                        Err(error) => eprintln!("Could not clear Discord: {error:#}"),
                    }
                }
            } else {
                let started = session_started.expect("active presence has a session");
                let PresentationTarget::Activity(index) = target else {
                    unreachable!()
                };
                let selected = &observations[index];
                let definition = config.activities.iter().find(|a| a.name == selected.name);
                let image = definition
                    .and_then(|a| a.image.as_deref())
                    .unwrap_or("tracker");
                let elapsed = format_duration(
                    tracker.displayed_ms(&selected.name, &settings, &store) / 60_000,
                );
                let text = presence_text(&observations, index, &config, &settings, &elapsed);
                let signature = (text.clone(), image.to_owned(), selected.name.clone());
                if (last_sent.as_ref() != Some(&signature) || refresh_due)
                    && now >= next_presence_attempt
                {
                    next_presence_attempt = now + Duration::from_secs(config.poll_seconds);
                    match discord.set(&text, image, definition, started, config.links.as_ref()) {
                        Ok(()) => {
                            last_sent = Some(signature);
                            last_successful_send = Some(now);
                        }
                        Err(error) => eprintln!("Discord is unavailable; retrying: {error:#}"),
                    }
                }
            }
        }
        previously_hidden = hidden;
        previous_target = target;
        // One loop handles all timing and actions. Process refresh remains at the
        // XML poll interval; HUD repaint uses its existing Windows message loop.
        thread::sleep(Duration::from_millis(200));
    }
    let now = Instant::now();
    while let Ok(action) = channels.reminder_actions.try_recv() {
        reminders.apply(action, now);
    }
    tracker.shutdown(now, wall_time(), &settings, &mut store);
    reminders.finish_statistics("dat_shutdown", now);
    reminders.checkpoint(&mut store, now);
    flush_storage(&mut store);
    if let Err(error) = discord.clear() {
        eprintln!("Could not clear Discord on shutdown: {error:#}");
    }
}
fn flush_storage(store: &mut Storage) {
    if let Err(error) = store.flush() {
        store.report_error(&error);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresentationTarget {
    Activity(usize),
    TrackerOnly,
    Untracked(usize),
    Idle,
}
impl PresentationTarget {
    fn is_generic(self) -> bool {
        matches!(self, Self::TrackerOnly | Self::Untracked(_))
    }
}
fn presentation_target(
    observations: &[ActivityObservation],
    focused: bool,
    hidden: bool,
    rotation_elapsed: Duration,
) -> PresentationTarget {
    if hidden {
        return PresentationTarget::TrackerOnly;
    }
    if focused {
        return observations
            .iter()
            .position(|o| o.foreground)
            .map(PresentationTarget::Activity)
            .unwrap_or(PresentationTarget::Untracked(observations.len()));
    }
    if observations.is_empty() {
        return PresentationTarget::Idle;
    }
    PresentationTarget::Activity(
        (rotation_elapsed.as_secs() / IMAGE_ROTATION_SECONDS) as usize % observations.len(),
    )
}

fn presence_text(
    observations: &[ActivityObservation],
    index: usize,
    config: &Config,
    settings: &Settings,
    elapsed: &str,
) -> PresenceText {
    let selected = &observations[index];
    let definition = config.activities.iter().find(|a| a.name == selected.name);
    let action = definition
        .and_then(|a| a.verb.as_deref())
        .unwrap_or("using")
        .trim()
        .to_lowercase();
    // Retain legacy verbs such as "Coding with" without repeating the name.
    let action = action.strip_suffix(" with").unwrap_or(&action);
    let mut characters = action.chars();
    let action = characters
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + characters.as_str())
        .unwrap_or_else(|| "Using".into());
    let instances = selected.displayed_instances(
        definition
            .map(|a| a.instance_count_mode)
            .unwrap_or_default(),
    );
    let suffix = if settings.multiple_instances && instances > 1 {
        format!(" · {instances} instances")
    } else {
        String::new()
    };
    let others = observations.len().saturating_sub(1);
    let mut details = format!("{action} for {elapsed}");
    if others > 0 && settings.presence_display != PresenceDisplay::SelectedOnly {
        details.push_str(&format!(" · also {others} {}", activity_word(others)));
    }
    PresenceText {
        details: fit_discord_text(details),
        state: "Activity Tracker".into(),
        compact: fit_discord_text(format!(
            "{}{suffix}",
            activity_display_name(config, selected)
        )),
        hover: fit_discord_text(format!(
            "{} · {elapsed}",
            activity_display_name(config, selected)
        )),
    }
}

fn activity_display_name<'a>(config: &'a Config, observation: &'a ActivityObservation) -> &'a str {
    config
        .activities
        .iter()
        .find(|a| a.name == observation.name)
        .and_then(|a| a.display_name.as_deref())
        .unwrap_or(&observation.name)
}
fn activity_word(count: usize) -> &'static str {
    if count == 1 { "activity" } else { "activities" }
}
fn format_duration(minutes: u64) -> String {
    let hours = minutes / 60;
    let remainder = minutes % 60;
    if hours == 0 {
        format!("{remainder}m")
    } else {
        format!("{hours}h{remainder:02}m")
    }
}
fn config_path() -> PathBuf {
    if let Some(path) = env::args_os().nth(1) {
        return PathBuf::from(path);
    }
    env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
        .map(|dir| dir.join("activities.xml"))
        .unwrap_or_else(|| PathBuf::from("activities.xml"))
}
#[cfg(test)]
mod tests {
    use crate::settings::TrackingMode;

    #[test]
    fn presentation_selection_focus_carousel_hide_and_tracking_independence() {
        let mut observations = vec![
            observation("WoW", 2, true),
            observation("Chrome", 1, false),
            observation("Code", 1, false),
        ];
        let start = Instant::now();
        let mut tracker = ActivityTracker::new(start, Duration::from_secs(1));
        let mut store = Storage::new(None);
        let settings = Settings {
            display_focused_activity: true,
            ..Settings::default()
        };
        let mut session = PresenceSession::default();
        for second in 0..=90 {
            let duration = Duration::from_secs(second);
            assert_eq!(
                presentation_target(&observations, false, false, duration),
                PresentationTarget::Activity((second / 30) as usize % 3)
            );
            assert_eq!(
                presentation_target(&observations, true, false, duration),
                PresentationTarget::Activity(0)
            );
            tracker.sample(
                &observations,
                start + duration,
                second as i64 * 1000,
                &settings,
                &mut store,
            );
            assert_eq!(session.observe(true, false, second as i64 * 1000), Some(0));
        }
        for observation in &observations {
            assert_eq!(store.totals[&observation.name].open_ms, 90_000);
        }
        assert_eq!(store.totals["WoW"].active_ms, 90_000);
        assert_eq!(store.totals["Chrome"].active_ms, 0);
        observations[0].foreground = false;
        assert_eq!(
            presentation_target(&observations, true, false, Duration::ZERO),
            PresentationTarget::Untracked(3)
        );
        observations[1].foreground = true;
        assert_eq!(
            presentation_target(&observations, true, false, Duration::ZERO),
            PresentationTarget::Activity(1)
        );
        for focus in [0, 1, 2] {
            for (index, activity) in observations.iter_mut().enumerate() {
                activity.foreground = index == focus;
            }
            assert_eq!(
                presentation_target(&observations, true, true, Duration::ZERO),
                PresentationTarget::TrackerOnly
            );
            assert_eq!(session.observe(true, true, 100_000), Some(0));
        }
        observations[2].foreground = false;
        observations[1].foreground = true;
        assert_eq!(
            presentation_target(&observations, true, false, Duration::ZERO),
            PresentationTarget::Activity(1)
        );
        assert_eq!(session.observe(true, false, 110_000), Some(0));
        // Mode changes restart carousel timing; no full interval before selection.
        assert_eq!(
            presentation_target(&observations, false, false, Duration::ZERO),
            PresentationTarget::Activity(0)
        );
        assert_eq!(
            presentation_target(&[], true, false, Duration::ZERO),
            PresentationTarget::Untracked(0)
        );
        assert_eq!(
            presentation_target(&[], false, false, Duration::ZERO),
            PresentationTarget::Idle
        );
    }

    fn presentation_config() -> Config {
        quick_xml::de::from_str(r#"<activity-tracker><discord-application-id>123</discord-application-id>
        <activity><name>Code</name><display-name>Visual Studio Code</display-name><verb>coding</verb><windows-process>code.exe</windows-process></activity>
        <activity><name>WoW</name><display-name>World of Warcraft</display-name><verb>Playing</verb><windows-process>wow.exe</windows-process></activity>
        <activity><name>Chrome</name><verb>Browsing with</verb><windows-process>chrome.exe</windows-process></activity>
        <activity><name>OBS</name><windows-process>obs.exe</windows-process></activity></activity-tracker>"#).unwrap()
    }
    fn observation(name: &str, windows: usize, foreground: bool) -> ActivityObservation {
        ActivityObservation {
            name: name.into(),
            pids: (1..=10).collect(),
            window_count: windows,
            foreground,
        }
    }
    #[test]
    fn detailed_card_format_and_other_counts() {
        let mut config = presentation_config();
        let mut settings = Settings::default();
        let mut observations = vec![observation("WoW", 2, false)];
        let elapsed = format_duration(146);
        let text = presence_text(&observations, 0, &config, &settings, &elapsed);
        assert_eq!(text.compact, "World of Warcraft · 2 instances");
        assert_eq!(text.details, "Playing for 2h26m");
        assert_eq!(text.state, "Activity Tracker");
        for (name, expected) in [("Code", "1 activity"), ("Chrome", "2 activities")] {
            observations.push(observation(name, 1, true));
            let text = presence_text(&observations, 0, &config, &settings, &elapsed);
            assert_eq!(text.details, format!("Playing for 2h26m · also {expected}"));
            assert_eq!(text.compact, "World of Warcraft · 2 instances");
            assert_eq!(text.state, "Activity Tracker");
            assert!(!text.details.contains("Warcraft"));
        }
        settings.presence_display = PresenceDisplay::SelectedOnly;
        assert_eq!(
            presence_text(&observations, 0, &config, &settings, &elapsed).details,
            "Playing for 2h26m"
        );
        config.activities[1].instance_count_mode = config::InstanceCountMode::Processes;
        observations[0].pids = vec![100, 200, 300, 400];
        assert_eq!(
            presence_text(&observations, 0, &config, &settings, &elapsed).compact,
            "World of Warcraft · 4 instances"
        );
        settings.multiple_instances = false;
        assert_eq!(
            presence_text(&observations, 0, &config, &settings, &elapsed).compact,
            "World of Warcraft"
        );
        settings.multiple_instances = true;
        observations[0].pids.truncate(1);
        assert_eq!(
            presence_text(&observations, 0, &config, &settings, &elapsed).compact,
            "World of Warcraft"
        );
        for legacy in [
            PresenceDisplay::ActivityNames,
            PresenceDisplay::ActivityCount,
        ] {
            settings.presence_display = legacy;
            assert!(
                presence_text(&observations, 0, &config, &settings, &elapsed)
                    .details
                    .ends_with("also 2 activities")
            );
        }
        settings.tracking_mode = TrackingMode::Active;
        let text = presence_text(&observations, 1, &config, &settings, "5m");
        assert_eq!(text.compact, "Visual Studio Code");
        assert_eq!(text.details, "Coding for 5m · also 2 activities");
        assert_eq!(text.state, "Activity Tracker");
    }

    use super::*;
    #[test]
    fn discord_text_and_duration_limits() {
        assert_eq!(format_duration(84), "1h24m");
        assert_eq!(format_duration(10), "10m");
        let text = fit_discord_text("遊".repeat(100));
        assert!(text.len() <= 128);
        assert!(text.ends_with("..."));
    }

    // Never send test input to an independently running DAT instance.
    pub(crate) fn find_own_window(class: &str) -> windows_sys::Win32::Foundation::HWND {
        use std::ptr::{null, null_mut};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            FindWindowExW, GetWindowThreadProcessId,
        };
        let class: Vec<u16> = class.encode_utf16().chain(Some(0)).collect();
        let mut window = null_mut();
        loop {
            unsafe {
                window = FindWindowExW(null_mut(), window, class.as_ptr(), null());
                if window.is_null() {
                    return window;
                }
                let mut pid = 0;
                GetWindowThreadProcessId(window, &mut pid);
                if pid == std::process::id() {
                    return window;
                }
            }
        }
    }

    /// Requires an interactive Windows desktop. It does not save user settings
    /// or connect to Discord; it briefly opens Settings and the reminder HUD.
    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn native_windows_smoke() {
        use std::ptr::null_mut;
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        unsafe {
            let previous = GetForegroundWindow();
            let shared = Arc::new(Mutex::new(Settings::default()));
            settings_window::show(shared.clone());
            let settings = find_own_window("DatSettings");
            assert!(!settings.is_null());
            settings_window::show(shared.clone());
            assert_eq!(settings, find_own_window("DatSettings"));
            let tooltip = settings_window::assert_native_help_layout(settings);
            assert_eq!(SendDlgItemMessageW(settings, 101, CB_GETCOUNT, 0, 0), 2);
            assert_eq!(SendDlgItemMessageW(settings, 106, CB_GETCOUNT, 0, 0), 2);
            assert_eq!(SendDlgItemMessageW(settings, 102, BM_GETCHECK, 0, 0), 1);
            assert_eq!(SendDlgItemMessageW(settings, 103, BM_GETCHECK, 0, 0), 0);
            assert_eq!(SendDlgItemMessageW(settings, 105, BM_GETCHECK, 0, 0), 0);
            assert_eq!(SendDlgItemMessageW(settings, 108, BM_GETCHECK, 0, 0), 0);
            SendDlgItemMessageW(settings, 108, BM_SETCHECK, 1, 0);
            SendDlgItemMessageW(settings, 101, CB_SETCURSEL, 1, 0);
            SendMessageW(settings, WM_COMMAND, 2, 0); // Cancel must not apply edits.
            assert_eq!(*shared.lock().unwrap(), Settings::default());
            assert_eq!(IsWindow(settings), 0);
            assert_eq!(IsWindow(tooltip), 0);
            if !previous.is_null() {
                SetForegroundWindow(previous);
            }
            let (sender, receiver) = mpsc::channel();
            reminder_hud::start(sender);
            let pending = (0..4)
                .map(|n| reminders::ReminderToast {
                    id: n.to_string(),
                    name: format!("Smoke reminder {n}"),
                    icon: Some("water".into()),
                    sound: None,
                    first_due: Instant::now() - Duration::from_secs(62),
                })
                .collect();
            reminder_hud::render(reminders::ReminderSnapshot { pending });
            let mut hud = null_mut();
            for _ in 0..100 {
                hud = find_own_window("DiscordActivityTrackerReminderHud");
                if !hud.is_null() && IsWindowVisible(hud) != 0 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            assert!(!hud.is_null());
            assert_ne!(IsWindowVisible(hud), 0);
            let style = GetWindowLongPtrW(hud, GWL_EXSTYLE) as u32;
            assert_ne!(style & WS_EX_NOACTIVATE, 0);
            assert_ne!(style & WS_EX_TOPMOST, 0);
            // A normal application receives focus; demote the HUD deliberately
            // and verify its real timer repairs topmost without reclaiming focus.
            settings_window::show(shared.clone());
            let other = find_own_window("DatSettings");
            SetForegroundWindow(other);
            let foreground = GetForegroundWindow();
            SetWindowPos(
                hud,
                HWND_NOTOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            for _ in 0..100 {
                if GetWindowLongPtrW(hud, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST != 0 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            assert_ne!(
                GetWindowLongPtrW(hud, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST,
                0
            );
            assert_eq!(GetForegroundWindow(), foreground);
            assert!(receiver.try_recv().is_err()); // no Done/Snooze from focus loss
            assert_ne!(IsWindowVisible(hud), 0);
            assert_eq!(hud, find_own_window("DiscordActivityTrackerReminderHud"));
            SendMessageW(hud, WM_TIMER, 1, 0);
            assert_eq!(GetForegroundWindow(), foreground);
            settings_window::close();
            // With four cards the active card starts at y=108; Done is y=166.
            SendMessageW(hud, WM_LBUTTONUP, 0, ((175isize) << 16) | 20);
            assert!(
                matches!(receiver.recv_timeout(Duration::from_secs(1)).unwrap(),ReminderAction::Done(id) if id=="0")
            );
            reminder_hud::shutdown();
            for _ in 0..100 {
                if IsWindow(hud) == 0 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(IsWindow(hud), 0);
        }
    }
}
