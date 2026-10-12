//! Date/run scopes over measured observations, never inferred process runtime.
use crate::storage::{ActivitySummary, ReminderSummary, Statistics, read_statistics};
use anyhow::{Result, bail};
use rusqlite::{Connection, OpenFlags, params};
use std::{collections::BTreeMap, path::Path, sync::OnceLock, time::Duration};

static CURRENT_RUN: OnceLock<String> = OnceLock::new();
pub fn set_current_run(id: String) {
    let _ = CURRENT_RUN.set(id);
}
pub fn current_run() -> Option<&'static str> {
    CURRENT_RUN.get().map(String::as_str)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StatisticsScope {
    CurrentRun,
    Today,
    ThisWeek,
    #[default]
    Total,
}
impl StatisticsScope {
    pub const ALL: [Self; 4] = [Self::CurrentRun, Self::Today, Self::ThisWeek, Self::Total];
}

/// Apply the host's local timezone before calendar arithmetic, then convert
/// midnight back to UTC. Week starts Monday; never subtract 24h UTC.
fn range_start(connection: &Connection, scope: StatisticsScope, now: i64) -> Result<i64> {
    if matches!(scope, StatisticsScope::CurrentRun | StatisticsScope::Total) {
        return Ok(i64::MIN);
    }
    let days: i64 = if scope == StatisticsScope::ThisWeek {
        connection.query_row(
            "SELECT (CAST(strftime('%w',?1/1000,'unixepoch','localtime') AS INTEGER)+6)%7",
            [now],
            |r| r.get(0),
        )?
    } else {
        0
    };
    Ok(connection.query_row("SELECT CAST(strftime('%s',?1/1000,'unixepoch','localtime','start of day',?2,'utc') AS INTEGER)*1000",
        params![now, format!("-{days} days")], |r| r.get(0))?)
}

fn contribution(value: i64, start: i64, end: i64, from: i64, until: i64) -> u64 {
    let overlap = end.min(until).saturating_sub(start.max(from)).max(0);
    let duration = end.saturating_sub(start);
    if duration <= 0 {
        return 0;
    }
    ((value.max(0) as i128 * overlap as i128) / duration as i128) as u64
}

fn activity<'a>(
    rows: &'a mut BTreeMap<String, ActivitySummary>,
    name: &str,
) -> &'a mut ActivitySummary {
    rows.entry(name.into()).or_insert_with(|| ActivitySummary {
        name: name.into(),
        open_ms: 0,
        active_ms: 0,
        sessions: 0,
        events: 0,
        peak_processes: None,
        peak_window_instances: None,
        last_session: None,
        session_totals_only: false,
    })
}

pub fn read_scoped_statistics(
    path: &Path,
    scope: StatisticsScope,
    run: Option<&str>,
    now: i64,
) -> Result<Statistics> {
    if scope == StatisticsScope::Total {
        return read_statistics(path);
    }
    if !path.try_exists()? {
        return Ok(Statistics::default());
    }
    if scope == StatisticsScope::CurrentRun && run.is_none() {
        return Ok(Statistics::default());
    }
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let tx = connection.transaction()?;
    let version: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version != 3 {
        bail!(
            "Scoped statistics require schema 3; found {version}. Start a compatible DAT build first."
        );
    }
    let from = range_start(&tx, scope, now)?;
    // Run membership is independent of wall-clock changes. Every row in this
    // snapshot has already been observed; only date scopes need an upper bound.
    let now = if scope == StatisticsScope::CurrentRun {
        i64::MAX
    } else {
        now
    };
    let run = if scope == StatisticsScope::CurrentRun {
        run
    } else {
        None
    };
    let mut activities = BTreeMap::new();
    {
        let mut stmt = tx.prepare("SELECT activity,start_ms,end_ms,open_ms,active_ms,process_count,window_count
            FROM activity_observations WHERE (?1 IS NULL OR run_id=?1) AND end_ms>?2 AND start_ms<?3")?;
        let mut rows = stmt.query(params![run, from, now])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            let start = row.get(1)?;
            let end = row.get(2)?;
            let summary = activity(&mut activities, &name);
            summary.open_ms += contribution(row.get(3)?, start, end, from, now);
            summary.active_ms += contribution(row.get(4)?, start, end, from, now);
            let processes: i64 = row.get(5)?;
            let windows: i64 = row.get(6)?;
            summary.peak_processes = Some(summary.peak_processes.unwrap_or(0).max(processes));
            summary.peak_window_instances =
                Some(summary.peak_window_instances.unwrap_or(0).max(windows));
        }
    }
    {
        let mut stmt = tx.prepare("SELECT activity,COUNT(*),strftime('%Y-%m-%d %H:%M:%S UTC',MAX(start_ms)/1000,'unixepoch') FROM activity_sessions s
            WHERE (?1 IS NULL OR run_id=?1) AND start_ms<=?3 AND (
                (start_ms>=?2) OR EXISTS (SELECT 1 FROM activity_observations o WHERE o.session_id=s.id AND o.end_ms>?2 AND o.start_ms<?3))
            GROUP BY activity")?;
        let mut rows = stmt.query(params![run, from, now])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            let summary = activity(&mut activities, &name);
            summary.sessions = row.get::<_, i64>(1)?.max(0) as u64;
            summary.last_session = row.get(2)?;
        }
        let mut stmt = tx.prepare("SELECT activity,COUNT(*) FROM activity_events WHERE (?1 IS NULL OR run_id=?1) AND at_ms>=?2 AND at_ms<=?3 GROUP BY activity")?;
        let mut rows = stmt.query(params![run, from, now])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            activity(&mut activities, &name).events = row.get::<_, i64>(1)?.max(0) as u64;
        }
    }
    let reminders = tx.prepare("WITH selected AS (
        SELECT r.*,CASE WHEN status='done' AND done_ms>=?2 AND done_ms<=?3 THEN 1 ELSE 0 END AS completed,
        (SELECT COUNT(*) FROM reminder_events e WHERE e.occurrence_id=r.id AND e.kind='Snoozed' AND e.at_ms>=?2 AND e.at_ms<=?3 AND (?1 IS NULL OR e.run_id=?1)) AS snoozes
        FROM reminder_occurrences r WHERE (?1 IS NULL OR r.run_id=?1) AND (
            first_due_ms BETWEEN ?2 AND ?3 OR done_ms BETWEEN ?2 AND ?3 OR EXISTS (
                SELECT 1 FROM reminder_events e WHERE e.occurrence_id=r.id AND e.at_ms BETWEEN ?2 AND ?3))
        ) SELECT name,SUM(completed),SUM(snoozes),AVG(CASE WHEN completed=1 THEN elapsed_ms END) FROM selected GROUP BY name ORDER BY name COLLATE NOCASE")?
        .query_map(params![run,from,now], |row| Ok(ReminderSummary {
            name: row.get(0)?, completed: row.get::<_,i64>(1)?.max(0) as u64,
            snoozes: row.get::<_,i64>(2)?.max(0) as u64,
            average_response_ms: row.get::<_,Option<f64>>(3)?.map(|value| value.max(0.0).round() as u64),
        }))?.collect::<rusqlite::Result<Vec<_>>>()?;
    tx.commit()?;
    let mut activities: Vec<_> = activities.into_values().collect();
    activities.sort_by_key(|row| row.name.to_lowercase());
    Ok(Statistics {
        activities,
        reminders,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Storage, unique_id};

    #[test]
    fn all_scopes_clip_observations_and_filter_sessions_events_and_reminders() {
        let path = std::env::temp_dir().join(format!("dat-scopes-{}.db", unique_id()));
        let mut store = Storage::new(Some(path.clone()));
        store.flush().unwrap();
        drop(store);
        let c = Connection::open(&path).unwrap();
        let now: i64 = c
            .query_row(
                "SELECT CAST(strftime('%s','2026-10-11 01:30:00','utc') AS INTEGER)*1000",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let day = range_start(&c, StatisticsScope::Today, now).unwrap();
        let week = range_start(&c, StatisticsScope::ThisWeek, now).unwrap();
        for (at, expected) in [(day, "2026-10-11 00:00:00"), (week, "2026-10-05 00:00:00")] {
            assert_eq!(
                c.query_row(
                    "SELECT datetime(?1/1000,'unixepoch','localtime')",
                    [at],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                expected
            );
        }
        c.execute("INSERT INTO dat_runs VALUES ('old',?1,?2,?2,'dat_shutdown'),('current',?3,?4,NULL,NULL)",params![week-1_800_000,week+5_400_000,day-1_800_000,now]).unwrap();
        c.execute(
            "INSERT INTO activity_totals VALUES ('Code',16300000,9100000)",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO activity_sessions VALUES ('old-session','Code',?1,?2,?2,7200000,7200000,2,'closed',1,'old'),('current-session','Code',?3,?4,NULL,9000000,1800000,2,NULL,1,'current')",params![week-1_800_000,week+5_400_000,day-1_800_000,day+7_200_000]).unwrap();
        c.execute("INSERT INTO activity_observations(run_id,activity,session_id,start_ms,end_ms,open_ms,active_ms,process_count,window_count) VALUES ('old','Code','old-session',?1,?2,7200000,7200000,2,1),('current','Code','current-session',?3,?4,1800000,1800000,2,1),('current','Code','current-session',?4,?5,7200000,0,2,1)",params![week-1_800_000,week+5_400_000,day-1_800_000,day,day+7_200_000]).unwrap();
        c.execute("INSERT INTO activity_events(activity,at_ms,kind,pids,foreground,window_count,run_id) VALUES ('Code',?1,'opened','42',1,1,'old'),('Code',?2,'foreground_changed','42',1,1,'old'),('Code',?3,'opened','42',1,1,'current'),('Code',?4,'foreground_changed','42',0,1,'current'),('Code',?5,'windows_changed','42',0,1,'current')",params![week-900_000,week+600_000,day-1_200_000,day+600_000,now+600_000]).unwrap();
        c.execute("INSERT INTO reminder_occurrences VALUES ('water-old','Water',?1,?2,3600000,1,NULL,'done','old'),('water-current','Water',?3,?4,7200000,2,NULL,'done','current')",params![week-7_200_000,week-3_600_000,day-3_600_000,day+3_600_000]).unwrap();
        c.execute("INSERT INTO reminder_events(occurrence_id,at_ms,elapsed_ms,kind,run_id) VALUES ('water-old',?1,1,'Snoozed','old'),('water-current',?2,1,'Snoozed','current'),('water-current',?3,1,'Snoozed','current')",params![week-5_400_000,day-1_800_000,day+1_800_000]).unwrap();
        let before: i64 = c
            .query_row("SELECT total_changes()", [], |r| r.get(0))
            .unwrap();
        for (scope, open, active, sessions, events, completed, snoozes) in [
            (
                StatisticsScope::CurrentRun,
                9_000_000,
                1_800_000,
                1,
                3,
                1,
                2,
            ),
            (StatisticsScope::Today, 5_400_000, 0, 1, 1, 1, 1),
            (StatisticsScope::ThisWeek, 12_600_000, 7_200_000, 2, 3, 1, 2),
            (StatisticsScope::Total, 16_300_000, 9_100_000, 2, 5, 2, 3),
        ] {
            let stats = read_scoped_statistics(&path, scope, Some("current"), now).unwrap();
            let row = &stats.activities[0];
            assert_eq!(
                (row.open_ms, row.active_ms, row.sessions, row.events),
                (open, active, sessions, events),
                "{scope:?}"
            );
            assert_eq!(
                (stats.reminders[0].completed, stats.reminders[0].snoozes),
                (completed, snoozes),
                "{scope:?}"
            );
        }
        assert!(
            read_scoped_statistics(&path, StatisticsScope::CurrentRun, Some("unknown"), now)
                .unwrap()
                .activities
                .is_empty()
        );
        assert_eq!(contribution(1000, 0, 1000, 500, 750), 250);
        assert_eq!(contribution(1000, 0, 1000, 1000, 2000), 0);
        assert_eq!(
            before,
            c.query_row("SELECT total_changes()", [], |r| r.get::<_, i64>(0))
                .unwrap()
        );
        assert_eq!(
            c.query_row(
                "SELECT end_ms FROM activity_sessions WHERE id='current-session'",
                [],
                |r| r.get::<_, Option<i64>>(0)
            )
            .unwrap(),
            None
        );
        drop(c);
        std::fs::remove_file(path).unwrap();
    }
}
