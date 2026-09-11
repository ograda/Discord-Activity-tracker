#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
compile_error!("discord-activity-mvp currently supports Windows only");

mod config;
mod discord;
mod monitor;
mod resolver;
mod tray;

use std::{
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

    let running = Arc::new(AtomicBool::new(true));

    let signal = Arc::clone(&running);
    ctrlc::set_handler(move || {
        signal.store(false, Ordering::SeqCst);
    })
    .context("could not install Ctrl+C handler")?;

    let tracker_running = Arc::clone(&running);

    let tracker_thread = thread::Builder::new()
        .name("activity-tracker".into())
        .spawn(move || run_tracker(config, tracker_running))
        .context("could not start activity-tracker thread")?;

    // The main thread will handle tray events in the next step.
    // For now, it waits until Ctrl+C requests shutdown.
    while running.load(Ordering::SeqCst) {
        match tray_receiver.recv_timeout(
            Duration::from_millis(200),
        ) {
            Ok(TrayCommand::TogglePause) => {
                println!("Pause / Resume selected.");

                // The actual tracker pause state is wired next.
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

fn run_tracker(
    config: Config,
    running: Arc<AtomicBool>,
) -> Result<()> {
    let mut monitor = ProcessMonitor::new();
    let mut discord =
        DiscordPresence::new(config.discord_application_id.clone());

    let mut current: Option<ResolvedActivity> = None;
    let mut session_started_at = unix_timestamp_millis();
    let mut last_successful_send: Option<Instant> = None;

    while running.load(Ordering::SeqCst) {
        let process_names = monitor.running_process_names();

        let detected =
            resolve_activities(&config.activities, &process_names);

        let changed = detected != current;

        let refresh_due = last_successful_send.is_none_or(|sent| {
            sent.elapsed()
                >= Duration::from_secs(PRESENCE_REFRESH_SECONDS)
        });

        if changed {
            session_started_at = unix_timestamp_millis();
            print_change(detected.as_ref());
        }

        if changed || refresh_due {
            let result = match detected.as_ref() {
                Some(activity) => {
                    discord.set(activity, session_started_at)
                }
                None => discord.clear(),
            };

            match result {
                Ok(()) => {
                    current = detected;
                    last_successful_send = Some(Instant::now());
                }
                Err(error) => {
                    eprintln!(
                        "Discord is unavailable; retrying: {error:#}"
                    );

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
    Ok(())
}

fn config_path() -> PathBuf {
    if let Some(path) = env::args_os().nth(1) {
        return PathBuf::from(path);
    }

    env::current_exe()
        .ok()
        .and_then(|executable| {
            executable.parent().map(PathBuf::from)
        })
        .map(|directory| directory.join("activities.xml"))
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
