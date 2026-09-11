#[cfg(not(windows))]
compile_error!("discord-activity-mvp currently supports Windows only");

mod config;
mod discord;
mod monitor;
mod resolver;

use std::{
    env,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};

use crate::{
    config::Config,
    discord::DiscordPresence,
    monitor::ProcessMonitor,
    resolver::{ResolvedActivity, resolve_activities},
};

const PRESENCE_REFRESH_SECONDS: u64 = 60;

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

    let running = Arc::new(AtomicBool::new(true));
    let signal = Arc::clone(&running);
    ctrlc::set_handler(move || signal.store(false, Ordering::SeqCst))
        .context("could not install Ctrl+C handler")?;

    let mut monitor = ProcessMonitor::new();
    let mut discord = DiscordPresence::new(config.discord_application_id.clone());
    let mut current: Option<ResolvedActivity> = None;
    let mut session_started_at = unix_timestamp_millis();
    let mut last_successful_send: Option<Instant> = None;

    while running.load(Ordering::SeqCst) {
        let process_names = monitor.running_process_names();
        let detected = resolve_activities(&config.activities, &process_names);
        let changed = detected != current;
        let refresh_due = last_successful_send.is_none_or(|sent| {
            sent.elapsed() >= Duration::from_secs(PRESENCE_REFRESH_SECONDS)
        });

        if changed {
            session_started_at = unix_timestamp_millis();
            print_change(detected.as_ref());
        }

        if changed || refresh_due {
            let result = match detected.as_ref() {
                Some(activity) => discord.set(activity, session_started_at),
                None => discord.clear(),
            };

            match result {
                Ok(()) => {
                    current = detected;
                    last_successful_send = Some(Instant::now());
                }
                Err(error) => {
                    eprintln!("Discord is unavailable; retrying: {error:#}");
                    current = detected;
                    last_successful_send = None;
                }
            }
        }

        sleep_interruptibly(
            Duration::from_secs(config.poll_seconds),
            Arc::clone(&running),
        );
    }

    let _ = discord.clear();
    println!("Stopped.");
    Ok(())
}

fn config_path() -> PathBuf {
    env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("activities.xml"))
}

fn unix_timestamp_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn sleep_interruptibly(duration: Duration, running: Arc<AtomicBool>) {
    let deadline = Instant::now() + duration;
    while running.load(Ordering::SeqCst) && Instant::now() < deadline {
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
