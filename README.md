# Discord Activity Tracker (DAT)

A lightweight Windows Rust application that watches configured processes, rotates
Discord Rich Presence, and displays local reminder notifications. There is no
server, account system, cloud synchronization, telemetry, or external analytics.

## Build and run

Requirements: Windows 10/11, the Rust MSVC toolchain and Visual Studio C++ build
tools. Discord desktop is needed for Rich Presence, but local tracking and
reminders continue when Discord is unavailable. SQLite is bundled into the build.

```powershell
cargo build --release
.\target\release\discord-activity-mvp.exe .\activities.xml
```

Copy `activities.example.xml` to `activities.xml` and configure your Discord
application ID and process names. Without a command-line path, DAT looks for
`activities.xml` beside the executable. Relative reminder sound paths are resolved
against the configuration directory. Existing XML files remain compatible.

The Discord application ID is a public identifier. DAT needs no client secret,
bot token, or authentication credentials. Keep the current application-ID/assets
configuration for both personal use and forks.

Process matching is exact, case-insensitive and trims surrounding whitespace.
Multiple `<windows-process>` elements are aliases for the same activity. Use
Task Manager's Details tab to find executable names. Activity and reminder names
are their persistent identifiers: renaming one starts a separate history.

## Tray and Settings

The tray provides Hide activities / Show activities, Reload activities.xml, Settings...,
Statistics..., and Exit. Reload validates the XML before applying changes; an invalid reload keeps
the working configuration. Pending/snoozed reminders with unchanged names retain
their occurrence and queue position. Removing a reminder removes its pending card
and marks an observed occurrence as `configuration_removed`.

Settings uses native Win32 controls and permits one window at a time. Save
persists and applies changes; Cancel discards them. Open Statistics opens the same
singleton viewer as the tray entry. All mutable data lives in:

```text
%LOCALAPPDATA%\DiscordActivityTracker\
    settings.xml
    dat.db
    dat.lock
    dat-errors.log   (when a database error occurs)
```

`activities.xml` is never rewritten. Settings are replaced atomically. Missing
settings use explicit defaults; malformed settings are logged and use defaults;
unknown tracking/presentation modes fall back to their default. A file lock
prevents simultaneous DAT instances from counting the same observations twice.

| XML setting | Default | Meaning |
| --- | --- | --- |
| `tracking-mode` | `total` | `total` (open) or `active` (foreground) |
| `persist-activity-time` | `true` | Accumulate both metrics across activity and DAT restarts |
| `session-history` | `false` | Store observed activity sessions |
| `multiple-instances` | `true` | Show visible-window instance count in Discord when greater than one |
| `detailed-statistics` | `false` | Store reminder interactions and detailed activity transitions/sessions |
| `display-focused-activity` | `false` | Follow the foreground tracked activity instead of the carousel |
| `presence-display` | `activity-names` | Include other activity count; `selected-only` omits it from details |

Both legacy `activity-names` and `activity-count` settings remain accepted and
now use the requested count-based card format. Settings offers Include other
activity count / Selected activity only. Counts appear in details; dynamic name
continues to identify the selected activity.

Disabling history/statistics stops new collection and retains existing records.
Enabling history midway through an activity starts an observation segment at that
point; it does not backfill previously unrecorded usage. Disabling cumulative
persistence preserves earlier totals, displays current-session time, and stops
adding new time to the saved totals. Re-enabling it resumes from the saved totals
without importing time collected while persistence was off.

## Time tracking and instances

DAT maintains both `total_open_time` and `foreground_active_time` internally.
Total/open time advances once per running activity regardless of focus. Active
time advances only when the foreground window's owning PID belongs to that
activity. For two distinct activities open for ten minutes, both receive about
ten minutes open time; using each for five minutes gives each about five minutes
active time. Changing the tracking mode only selects which metric is displayed.

The process snapshot retains normalized names and PIDs. Full process enumeration
uses `poll-seconds`; the same monitor observes foreground ownership and eligible
top-level application windows once a second against cached matching PIDs. There is one central observation loop, not a thread per
activity. Windows foreground ownership comes from
[GetForegroundWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getforegroundwindow)
and `GetWindowThreadProcessId`.

A session begins at the first observed matching process and ends when the last
matching PID disappears. Multiple windows or matching processes never multiply
activity wall-clock time. `process_count` is the number of matching PIDs (including
renderer/GPU/helpers); `window_count` counts eligible top-level windows owned by
those PIDs. Multiple windows in the same PID count separately. User-facing
`instance_count` uses `window_count`, with a conservative fallback of one when the
activity is running but has no eligible window.

`EnumWindows` filtering excludes invisible, child, tool, no-activate, cloaked,
untitled, and zero-size windows. Owned dialogs are excluded unless explicitly
marked `WS_EX_APPWINDOW`. Minimized ordinary application windows still count.
Thus ten Code.exe processes with one eligible VS Code window display one instance,
while two eligible windows display two. This remains a window heuristic, not an
account detector: unusual application windows or windows on other virtual desktops
may be undercounted. No window titles are extracted or stored as metadata.
Disabling instance display hides the count without discarding process/history data.

Timing uses measured monotonic `Instant` deltas, assigned to the previous
observation. Foreground switches are approximate to the one-second sampling
cadence; process starts/stops are approximate to `poll-seconds`. Events entirely
between samples may be missed. Gaps over 15 seconds in the normal one-second
observation cadence are excluded completely, conservatively avoiding invented
usage after suspend/stalls. No activity time is inferred while DAT is stopped.
Wall-clock dates are used only for persisted event/session timestamps and the
native Discord session timestamp.

Session time is separate from cumulative totals. With persistence enabled,
10 minutes followed by closing/reopening and another 5 minutes displays 15
minutes. Totals survive DAT/Windows restarts. Exit/Ctrl+C finalizes observation
sessions; Windows session-end messages also request shutdown. A DAT restart while
an application stays open creates a new *observed* session, not fabricated coverage
of the interval DAT was absent.

Settings groups Tracking mode, cumulative persistence and instance display under
TRACKING. Session history, detailed statistics and Open Statistics are under
STATISTICS & HISTORY (local only). Their storage semantics are unchanged.
Eight `?` indicators provide native hover help for these settings and the Discord
count option. One owned tooltip control uses window-lifetime text buffers and
adds no threads or tab stops.

## Discord presentation

The `display-focused-activity` setting defaults to `false`, including in old
settings files. With it off, the selected activity continues to rotate every
30 seconds. With it on, presentation follows the foreground tracked activity
at the normal one-second observation cadence, without waiting for rotation or
the previous successful send's cooldown. An untracked foreground (or no tracked
activity) uses `Activity Tracker` as name with `Powered by DAT · Tracking 1 activity`
or `Powered by DAT · Tracking N activities` in details. The count is distinct
resolved configured activities, never processes/windows/instances. With none running,
details reads `No activity running`. State, assets and buttons are omitted from
this fallback. Its native timestamp continues unchanged when focus returns and
the corresponding activity's entire normal payload is restored. Explicit Hide
remains private: only `Activity Tracker` and its timestamp, with no count or details.

The checkbox and its `?` hover help are under DISCORD PRESENCE. Save applies it
immediately; disabling it restarts the carousel at its first activity. Hide
Activities has priority, and Show resumes the selected mode without resetting
the native timestamp. This option never changes process observation, time
accounting, reminders, statistics, or database writes. All open activities still
accumulate Total/Open time independently of the selected presentation.

 Normal visible fields:

```text
name:    World of Warcraft · 2 instances
details: Playing for 2h26m · also 2 activities
state:   Activity Tracker
```

`name` contains `display-name` (or `name`) and an instance suffix only when global
multiple-instance display is enabled and the selected count is greater than one.
It remains dynamic, with `StatusDisplayType::Name` for compact/member-list status.
The activity name is not repeated in `details`. Details contains the capitalized
configured action (`verb`, default `Using`), `for`, and the selected Total/Open or
Active/Foreground usage time. Legacy `Coding with` becomes `Coding`. Durations use
`32m` or `2h26m`. Tracking mode still selects the cumulative/session metric.

The existing Include other activity count setting adds `also 1 activity` or
`also N activities` to details only when N > 0. Selected activity only suppresses
that suffix. Normal state always reads `Activity Tracker`. Hidden-mode payloads
and their continuous timestamp are unchanged. All text is truncated safely at
Unicode boundaries within Discord's 128-byte field limit. Discord controls field
rendering and button visibility; the configured DAT Application ID is unchanged.

Optional per-activity logical instance selection:

```xml
<instance-count-mode>windows</instance-count-mode>
```

- `windows` (default, including missing tags): eligible visible top-level application
  windows. A running activity with no eligible windows has one logical instance.
  Ten Chrome/Code helper processes with one application window still mean one.
- `processes`: unique matching process PIDs across every `windows-process` alias.
  Duplicate aliases never count a PID twice. This can suit separate WoW clients.

See `activities.example.xml` for Chrome/windows and WoW/processes examples.
This choice affects presentation only: raw process/window counts, foreground
accounting, session boundaries, stored statistics and wall-clock time are unchanged.
No database migration or rewrite of user-authored XML is required.

Optional, manually configured static metadata:

```xml
<tracker-status>Bloodbath progress: 60</tracker-status>
```

This sets the DAT small-icon tooltip to `[DAT] Bloodbath progress: 60`. Missing
or blank metadata retains `Activity Tracker`. This field performs no automatic
metadata discovery. Existing small-icon repository links are preserved.

Images, configured `image-url` activity-page links, global buttons, per-activity
replacement buttons, disabled buttons, and the native session timestamp
remain supported. No URL is invented when absent. Every normal presence goes
through one payload builder that includes the appropriate buttons regardless of
tracking/presentation mode. Discord controls which surfaces actually draw buttons:
member lists, popouts, expanded profiles, Recent Activity cards, and the presence
owner's own view may differ. DAT cannot force buttons to appear everywhere;
clickable configured image URLs remain a fallback.

The native Discord timer represents the DAT presence session, not foreground or
cumulative usage; state carries the selected usage value. Hide/Show preserves the
same start timestamp, including repeated toggles. Hidden presence contains only
`Activity Tracker` and the native timestamp, without activity details, assets,
URLs, or buttons. Showing activities restores the normal payload. Ordinary visible
idle (no configured activity running) clears presence and ends the session; hiding
from idle starts a hidden session.

The tray uses the normal embedded DAT helmet while visible, with `Hide activities`
as its menu action. Hidden mode adds a small red slash with a contrasting outline
to the helmet and changes the action to `Show activities`. Both icons are cached
native HICONs: the hidden variant is composited at runtime from the embedded icon,
without an extra image dependency or runtime asset file. Handles are released after
the shell icon is removed. The hidden tooltip explicitly says local tracking
continues.

One shared hidden flag drives the tracker and tray. The tray command updates the
flag, icon (`Shell_NotifyIconW` / `NIM_MODIFY`), and label (`ModifyMenuW`) together;
the worker applies the current flag on its next iteration. Failed shell updates
are logged and retried, and Explorer's `TaskbarCreated` notification restores the
current icon. Discord reconnects/retries without stopping local tracking: its UI
can lag behind a toggle while IPC is unavailable. Hiding never pauses detection,
activity accounting, reminders, statistics, or database writes. Only configured
Rich Presence fields go to Discord; local history and reminder events are never
transmitted.

## Reminders and statistics

The active/full-size reminder displays `MM:SS`, or `HH:MM:SS` after one hour,
since the occurrence first became pending. A Win32 timer invalidates the existing
HUD once a second and reasserts `HWND_TOPMOST` using `SWP_NOACTIVATE`, without
moving/resizing it or stealing focus. Clicking another application does not
dismiss, snooze, or complete a reminder. No notification window is recreated. The timer includes queue
waiting and snooze time. Promoting a compact card preserves its original start.

The queue retains one active card, two compact previews, and a +N overflow badge.
Done, Snooze, compact-card promotion, custom glyphs, sounds, always-on-top and
no-focus-stealing behavior remain available with statistics disabled. A missing
sound is logged and does not prevent the reminder from appearing.

With detailed statistics enabled, one occurrence retains its identity through
snoozes and stores `Triggered`, `Snoozed`, `PresentedAgain`, and `Done` events,
first-due/completion dates, snooze count, monotonic response duration and the
interval between completions of the same reminder. Across DAT restarts,
completion intervals use stored wall-clock dates; clock adjustments can affect
those cross-run intervals. The database preserves incomplete occurrences as
`dat_shutdown`, `interrupted`, `statistics_disabled`, or `configuration_removed`;
they are not falsely reported as completed. Pending/snoozed reminders are not
restored on startup: the configured scheduling interval starts anew.

Enabling statistics during an existing occurrence begins recording with the next
new occurrence, avoiding a partial interaction history. Disabling it immediately
ends recording for the current occurrence without affecting its timer or actions.

## Statistics window

Tray > Statistics... and Settings > Open Statistics open/focus the same plain
native read-only window. Refresh reloads the committed database snapshot, normally
up to 30 seconds behind the live tracker. It shows:

- Activity: saved Total/Open time, Active/Foreground time, recorded session count,
  peak window instances, peak processes, and last observed session start (UTC).
- Reminder: completed occurrence count, snooze count across all recorded
  occurrences, and average response time of completed occurrences only.

If cumulative totals are absent, recorded-session durations are shown and clearly
labeled. Totals and session sums are never added together or multiplied by joins.
Session counts include open observation segments; they reflect enabled history
periods, not inferred lifetime execution counts. Old process-based peaks are
labeled as process counts; historical window counts are unavailable.

Empty/missing data explains which collection settings to enable. Busy, unsupported,
or inaccessible databases show an error with a Refresh suggestion. Reading does
not create a database, migrate it, recover/close live sessions, or enable tracking.
Previously collected data remains visible when collection is disabled. Styling,
charts, exports, and a detailed history browser remain outside this pass.

## Local database

`rusqlite` uses [bundled SQLite](https://github.com/rusqlite/rusqlite#notes-on-building-rusqlite-and-libsqlite3-sys).
Schema version 2 is tracked with `PRAGMA user_version`. Initialization is
transactional, additive, and rejects newer unsupported versions without deleting
history. Times ending in `_ms` are Unix milliseconds for dates, or integer
milliseconds for durations.

| Table | Columns / purpose |
| --- | --- |
| `activity_totals` | `activity` primary key, `open_ms`, `active_ms` |
| `activity_sessions` | `id` primary key, `activity`, `start_ms`, `last_seen_ms`, `end_ms`, `open_ms`, `active_ms`, `peak_instances` (process peak), `end_reason`, nullable `peak_window_instances` |
| `activity_events` | integer `id`, `activity`, `at_ms`, `kind`, comma-separated `pids`, `foreground`, nullable `window_count` |
| `reminder_occurrences` | `id` primary key, `name`, `first_due_ms`, `done_ms`, `elapsed_ms`, `snooze_count`, `completion_interval_ms`, `status` |
| `reminder_events` | integer `id`, `occurrence_id` foreign key, `at_ms`, `elapsed_ms`, `kind` |

The version 1-to-2 migration only adds the two window-count columns. Existing
rows/totals/reminder events are retained; historical window counts stay NULL.
The Statistics reader also supports an unmigrated version 1 database read-only.

Indexes cover activity session starts and reminder due/completion dates. Detailed
activity events record opening, closing, PID-set changes (including replacements
with unchanged counts), window-count changes, foreground changes, and observation enable/disable/stop.
They only include configured activities and matching PIDs, not every process.

Transactions checkpoint about every 30 seconds, on settings changes, and at
shutdown. Sessions update existing rows instead of inserting polling records.
Failed transactions retain pending changes in memory for retry and log errors;
tracking, Discord, and reminders continue. Database errors also go to a local
`dat-errors.log` (reset after approximately 1 MiB), so release builds without a
console retain diagnostic errors. Startup closes interrupted sessions at
their last durable checkpoint. Forced termination can lose the most recent
unflushed interval. A persistent write failure followed by exit loses buffered
updates; errors are reported rather than silently discarded. Missing local data
access leaves tracking usable in memory. The Statistics window reads saved summaries; detailed history/export UI is deferred.

## Validation

```powershell
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
```

Tests cover concurrent/open and alternating foreground time, cumulative reopen,
shutdown/restart, instance counts, final-instance closure, pause gaps, privacy
toggles, settings replacement, reminder queue/snooze identity, reloads, timer
format/layout, Discord presentation, migration guards, transaction rollback/retry,
and durable reminder completion intervals.

Interactive Windows regression tests open/close temporary native windows to check
one/two-window counting (including two in one PID), helper/owned/hidden filtering,
Settings defaults/Cancel, tooltip ownership/text registration and section placement,
tray and Settings Statistics entry points and singleton
reuse, repeated Hide/Show icon and menu synchronization, shell re-registration,
plus actual timer-driven recovery after deliberate HUD topmost demotion
without focus theft or spurious actions. These do not save settings or connect
to Discord; the Statistics entry-point test reads the existing database only.
Window lookups are restricted to the test process, protecting any running DAT UI.

```powershell
cargo test -- --ignored --test-threads=1 --nocapture
```

Serialized-payload tests verify two activity-specific buttons, global fallback,
disabling, dynamic name/details/state ordering, static tooltips, existing image/small-icon
URLs, and continuous session timestamps through repeated Hide/Show. Native icon
tests cover 16/20/24/32/48/64-pixel composition and transparency. A simulated hidden
period verifies that activity accounting and reminders continue. Database tests verify read-only summaries, completed-
only averages, and version-1 migration without losing legacy rows.

Real Discord appearance, sound output, multi-monitor/high-DPI layouts, actual
Windows suspend/logoff and extended live application sessions still need the 1.x
stabilization pass. Sampled tests validate accounting without ten-minute waits.

## Deferred roadmap

1.0 is feature completion; 1.x is review/testing/stabilization; 1.x.x handles
publishing/distribution preparation; 2.0 handles architectural/code-quality
refactoring. This work adds no installer, updater, release infrastructure,
website, community backend, Discord server/bot integration, accounts, telemetry,
remote API, cloud sync, web dashboard, gamification, or additional OS support.
UI styling, Discord product pages/App Directory publishing/verification, and
distribution work remain deferred. Metadata Inspector/probe, window-title/file/
JSON/local-API/plugin providers, and automatic game integrations are explicitly
POST-2.0 / POST-RELEASE. The full architecture refactor remains 2.0.
The public Discord application/assets policy remains undecided.

## License

MIT
