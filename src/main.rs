#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
compile_error!("discord-activity-mvp currently supports Windows only");

mod config;
mod discord;
mod monitor;
mod resolver;
mod tray;

use std::{
    collections::HashMap,
    env,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};

use crate::{
    config::Config,
    discord::DiscordPresence,
    monitor::ProcessMonitor,
    resolver::{ResolvedActivity, resolve_activities},
    tray::TrayCommand,
};

const PRESENCE_REFRESH_SECONDS: u64 = 60;
const IMAGE_ROTATION_SECONDS: u64 = 30;

fn main() -> Result<()> {
    let config_path = config_path();
    let config = Config::load(&config_path)
        .with_context(|| format!("could not load {}", config_path.display()))?;

    println!(
        "Loaded {} activities from {}. Polling every {} seconds.",
        config.activities.len(),
        config_path.display(),
        config.poll_seconds
    );

    let (tray_sender, tray_receiver) = mpsc::channel();
    tray::start(tray_sender);

    let hidden = Arc::new(AtomicBool::new(false));
    let running = Arc::new(AtomicBool::new(true));

    let signal = Arc::clone(&running);
    ctrlc::set_handler(move || {
        signal.store(false, Ordering::SeqCst);
    })
    .context("could not install Ctrl+C handler")?;

    let tracker_hidden = Arc::clone(&hidden);
    let tracker_running = Arc::clone(&running);

    let tracker_thread = thread::Builder::new()
        .name("activity-tracker".into())
        .spawn(move || run_tracker(config, tracker_running, tracker_hidden))
        .context("could not start activity-tracker thread")?;

    // The main thread will handle tray events in the next step.
    // For now, it waits until Ctrl+C requests shutdown.
    while running.load(Ordering::SeqCst) {
        match tray_receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(TrayCommand::ToggleHidden) => {
                println!("Hide / Show activities selected.");
                let was_hidden = hidden.fetch_xor(true, Ordering::SeqCst);

                if was_hidden {
                    println!("Activities are now visible.");
                } else {
                    println!("Activities are now hidden.");
                }
            }

            Ok(TrayCommand::Reload) => {
                println!("Reload selected.");

                // XML replacement is wired next.
            }

            Ok(TrayCommand::Exit) => {
                println!("Exit selected.");
                running.store(false, Ordering::SeqCst);
            }

            Err(mpsc::RecvTimeoutError::Timeout) => {}

            Err(mpsc::RecvTimeoutError::Disconnected) => {
                running.store(false, Ordering::SeqCst);
            }
        }
    }

    tracker_thread
        .join()
        .map_err(|_| anyhow!("activity-tracker thread panicked"))??;

    println!("Stopped.");
    Ok(())
}

fn run_tracker(config: Config, running: Arc<AtomicBool>, hidden: Arc<AtomicBool>) -> Result<()> {
    let mut monitor = ProcessMonitor::new();
    let mut discord = DiscordPresence::new(config.discord_application_id.clone());

    let mut current: Option<ResolvedActivity> = None;
    let mut activity_starts: HashMap<String, Instant> = HashMap::new();
    // Native Discord timer: one uninterrupted visible tracking session.
    let mut session_started_at: Option<i64> = None;
    let mut rotation_started_at = Instant::now();
    let mut last_sent: Option<(String, String, String, String, String)> = None;
    let mut last_successful_send: Option<Instant> = None;
    let mut hidden_presence_applied = false;

    while running.load(Ordering::SeqCst) {
        if hidden.load(Ordering::SeqCst) {
            if !hidden_presence_applied {
                match discord.set_hidden(config.links.as_ref()) {
                    Ok(()) => hidden_presence_applied = true,
                    Err(error) => eprintln!("Could not hide activities: {error:#}"),
                }
            }

            // Hidden mode intentionally ends the current visible session.
            current = None;
            activity_starts.clear();
            session_started_at = None;
            last_sent = None;
            last_successful_send = None;

            wait_interruptibly(
                Duration::from_secs(config.poll_seconds),
                &running,
                &hidden,
                true,
            );
            continue;
        }

        if hidden_presence_applied {
            hidden_presence_applied = false;
            rotation_started_at = Instant::now();
            last_successful_send = None;
        }

        let process_names = monitor.running_process_names();
        let detected = resolve_activities(&config.activities, &process_names)
            .filter(|resolved| !resolved.names.is_empty());

        let changed = detected != current;
        if changed {
            rotation_started_at = Instant::now();
            print_change(detected.as_ref());
        }

        let refresh_due = last_successful_send
            .is_none_or(|sent| sent.elapsed() >= Duration::from_secs(PRESENCE_REFRESH_SECONDS));

        // No active application: clear the card and reset both time measurements.
        if detected.is_none() {
            activity_starts.clear();
            session_started_at = None;
            last_sent = None;

            if changed || refresh_due {
                match discord.clear() {
                    Ok(()) => last_successful_send = Some(Instant::now()),
                    Err(error) => {
                        eprintln!("Discord is unavailable; retrying: {error:#}");
                        last_successful_send = None;
                    }
                }
            }

            current = None;
            wait_interruptibly(
                Duration::from_secs(config.poll_seconds),
                &running,
                &hidden,
                false,
            );
            continue;
        }

        let resolved = detected.as_ref().expect("checked above");
        session_started_at.get_or_insert_with(unix_timestamp_millis);

        // Each running activity only needs an Instant, with no per-activity thread.
        activity_starts.retain(|name, _| resolved.names.contains(name));
        for name in &resolved.names {
            activity_starts
                .entry(name.clone())
                .or_insert_with(Instant::now);
        }

        // Rotation changes presentation only; it never resets a tracked duration.
        let index = (rotation_started_at.elapsed().as_secs() / IMAGE_ROTATION_SECONDS) as usize
            % resolved.names.len();
        let selected = &resolved.names[index];
        let definition = config.activities.iter().find(|item| item.name == *selected);
        let image = definition
            .and_then(|item| item.image.as_deref())
            .unwrap_or("tracker");
        let verb = definition
            .and_then(|item| item.verb.as_deref())
            .unwrap_or("Using");
        let display_name = definition
            .and_then(|item| item.display_name.as_deref())
            .unwrap_or(selected);
        let minutes = activity_starts
            .get(selected)
            .expect("active activity has a start time")
            .elapsed()
            .as_secs()
            / 60;
        let elapsed = format_duration(minutes);

        let details = fit_discord_text(format!("{verb} {display_name} · {elapsed}"));
        let others: Vec<&str> = resolved
            .names
            .iter()
            .filter(|name| *name != selected)
            .map(|name| {
                config
                    .activities
                    .iter()
                    .find(|item| item.name == *name)
                    .and_then(|item| item.display_name.as_deref())
                    .unwrap_or(name.as_str())
            })
            .collect();
        let state = if others.is_empty() {
            "1 activity active".to_owned()
        } else {
            fit_discord_text(format!("Also {}", others.join(" + ")))
        };
        let hover = fit_discord_text(format!("{selected} · {elapsed}"));

        // Send only when something visible changes, or for an occasional refresh.
        let signature = (
            details.clone(),
            state.clone(),
            image.to_owned(),
            hover.clone(),
            selected.to_string(),
        );
        if last_sent.as_ref() != Some(&signature) || refresh_due {
            match discord.set(
                &details,
                &state,
                image,
                &hover,
                definition,
                session_started_at.expect("active session has a start time"),
                config.links.as_ref(),
            ) {
                Ok(()) => {
                    last_sent = Some(signature);
                    last_successful_send = Some(Instant::now());
                }
                Err(error) => {
                    eprintln!("Discord is unavailable; retrying: {error:#}");
                    last_successful_send = None;
                }
            }
        }

        current = detected;
        wait_interruptibly(
            Duration::from_secs(config.poll_seconds),
            &running,
            &hidden,
            false,
        );
    }

    let _ = discord.clear();
    Ok(())
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

fn fit_discord_text(mut text: String) -> String {
    const MAX_BYTES: usize = 128;
    if text.len() <= MAX_BYTES {
        return text;
    }
    let mut end = MAX_BYTES - 3;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str("...");
    text
}

fn config_path() -> PathBuf {
    if let Some(path) = env::args_os().nth(1) {
        return PathBuf::from(path);
    }

    env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(PathBuf::from))
        .map(|directory| directory.join("activities.xml"))
        .unwrap_or_else(|| PathBuf::from("activities.xml"))
}

fn unix_timestamp_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn wait_interruptibly(
    duration: Duration,
    running: &AtomicBool,
    hidden: &AtomicBool,
    expected_hidden: bool,
) {
    let deadline = Instant::now() + duration;

    while running.load(Ordering::SeqCst)
        && hidden.load(Ordering::SeqCst) == expected_hidden
        && Instant::now() < deadline
    {
        let remaining = deadline.saturating_duration_since(Instant::now());

        thread::sleep(remaining.min(Duration::from_millis(200)));
    }
}

fn print_change(activity: Option<&ResolvedActivity>) {
    match activity {
        Some(activity) => println!("Detected: {}", activity.names.join(" + ")),
        None => println!("No configured activity detected."),
    }
}
