use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    pub open_ms: u64,
    pub active_ms: u64,
}
impl Metrics {
    pub fn add(&mut self, other: Self) {
        self.open_ms += other.open_ms;
        self.active_ms += other.active_ms;
    }
}
#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: String,
    pub activity: String,
    pub start: i64,
    pub last_seen: i64,
    pub end: Option<i64>,
    pub metrics: Metrics,
    // Legacy column retains process-count semantics for backward compatibility.
    pub peak_instances: usize,
    pub peak_window_instances: Option<usize>,
    pub end_reason: Option<String>,
}
#[derive(Debug, Clone)]
pub struct ActivityEvent {
    pub activity: String,
    pub at: i64,
    pub kind: String,
    pub pids: String,
    pub foreground: bool,
    pub window_count: usize,
}
#[derive(Debug, Clone)]
pub struct ReminderRecord {
    pub id: String,
    pub name: String,
    pub first_due: i64,
    pub done: Option<i64>,
    pub elapsed_ms: u64,
    pub snoozes: u32,
    pub interval_ms: Option<u64>,
    pub status: String,
}
#[derive(Debug, Clone)]
pub struct ReminderEvent {
    pub occurrence: String,
    pub at: i64,
    pub elapsed_ms: u64,
    pub kind: &'static str,
}

#[derive(Debug, Clone)]
pub struct ObservationInterval {
    pub activity: String,
    pub session_id: Option<String>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub metrics: Metrics,
    pub process_count: usize,
    pub window_count: usize,
}

pub fn wall_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
pub fn unique_id() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

pub struct Storage {
    path: Option<PathBuf>,
    connection: Option<Connection>,
    pub run_id: String,
    run_started: i64,
    run_end: Option<(i64, String)>,
    observations: HashMap<String, Vec<ObservationInterval>>,
    pub totals: HashMap<String, Metrics>,
    increments: HashMap<String, Metrics>,
    pub sessions: HashMap<String, SessionRecord>,
    pub activity_events: Vec<ActivityEvent>,
    pub reminders: HashMap<String, ReminderRecord>,
    pub reminder_events: Vec<ReminderEvent>,
}
impl Storage {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            connection: None,
            run_id: unique_id(),
            run_started: wall_time(),
            run_end: None,
            observations: HashMap::new(),
            totals: HashMap::new(),
            increments: HashMap::new(),
            sessions: HashMap::new(),
            activity_events: Vec::new(),
            reminders: HashMap::new(),
            reminder_events: Vec::new(),
        }
    }
    pub fn report_error(&self, error: &anyhow::Error) {
        let message = format!(
            "Database write failed; data remains buffered for retry. Unwritten data is lost if DAT exits: {error:#}"
        );
        self.log_error(&message);
    }
    pub fn log_error(&self, message: &str) {
        eprintln!("{message}");
        // Release builds have no console. Keep persistence failures discoverable
        // without logging process/reminder payloads or requiring a GUI prompt.
        if let Some(path) = &self.path {
            use std::io::Write;
            let log = path.with_file_name("dat-errors.log");
            let truncate = log.metadata().is_ok_and(|m| m.len() > 1_048_576);
            match std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(!truncate)
                .truncate(truncate)
                .open(log)
            {
                Ok(mut file) => {
                    if let Err(error) = writeln!(file, "{} {message}", wall_time()) {
                        eprintln!("Could not write database error log: {error}");
                    }
                }
                Err(error) => eprintln!("Could not open database error log: {error}"),
            }
        }
    }
    pub fn add_time(&mut self, name: &str, metrics: Metrics) {
        self.totals.entry(name.into()).or_default().add(metrics);
        self.increments.entry(name.into()).or_default().add(metrics);
    }
    pub fn finish_run(&mut self, at: i64, reason: &str) {
        self.run_end = Some((at, reason.into()));
    }
    pub fn observe(&mut self, interval: ObservationInterval) {
        let rows = self
            .observations
            .entry(interval.activity.clone())
            .or_default();
        // Merge only contiguous intervals with identical foreground and instance
        // state. A checkpoint bounds their size; no gaps are bridged.
        if let Some(last) = rows.last_mut()
            && last.end_ms == interval.start_ms
            && last.session_id == interval.session_id
            && (last.metrics.active_ms == 0) == (interval.metrics.active_ms == 0)
            && last.process_count == interval.process_count
            && last.window_count == interval.window_count
        {
            last.end_ms = interval.end_ms;
            last.metrics.add(interval.metrics);
        } else {
            rows.push(interval);
        }
    }
    fn connect(&mut self) -> Result<()> {
        if self.connection.is_some() {
            return Ok(());
        }
        let Some(path) = &self.path else {
            bail!("local data directory unavailable; persistence is buffered in memory");
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(path)?;
        initialize(&mut connection)?;
        let mut loaded = HashMap::new();
        {
            let mut stmt =
                connection.prepare("SELECT activity, open_ms, active_ms FROM activity_totals")?;
            for row in stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    Metrics {
                        open_ms: r.get::<_, i64>(1)?.max(0) as u64,
                        active_ms: r.get::<_, i64>(2)?.max(0) as u64,
                    },
                ))
            })? {
                let (name, metrics) = row?;
                loaded.insert(name, metrics);
            }
        }
        // Add the existing baseline once, including when startup initially failed.
        for (name, metrics) in loaded {
            self.totals.entry(name).or_default().add(metrics);
        }
        self.connection = Some(connection);
        Ok(())
    }
    pub fn flush(&mut self) -> Result<()> {
        self.connect()?;
        let tx = self.connection.as_mut().expect("connected").transaction()?;
        tx.execute("INSERT INTO dat_runs(id,started_at_ms,last_seen_ms,ended_at_ms,end_reason) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET last_seen_ms=excluded.last_seen_ms,ended_at_ms=excluded.ended_at_ms,end_reason=excluded.end_reason",
            params![self.run_id, self.run_started, self.run_end.as_ref().map_or_else(wall_time, |end| end.0), self.run_end.as_ref().map(|end| end.0), self.run_end.as_ref().map(|end| &end.1)])?;
        for (name, m) in &self.increments {
            tx.execute("INSERT INTO activity_totals(activity,open_ms,active_ms) VALUES (?1,?2,?3) ON CONFLICT(activity) DO UPDATE SET open_ms=open_ms+excluded.open_ms, active_ms=active_ms+excluded.active_ms", params![name, sql_ms(m.open_ms), sql_ms(m.active_ms)])?;
        }
        for r in self.sessions.values() {
            tx.execute("INSERT INTO activity_sessions(id,activity,start_ms,last_seen_ms,end_ms,open_ms,active_ms,peak_instances,end_reason,peak_window_instances,run_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(id) DO UPDATE SET last_seen_ms=excluded.last_seen_ms,end_ms=excluded.end_ms,open_ms=excluded.open_ms,active_ms=excluded.active_ms,peak_instances=excluded.peak_instances,end_reason=excluded.end_reason,peak_window_instances=excluded.peak_window_instances", params![r.id,r.activity,r.start,r.last_seen,r.end,sql_ms(r.metrics.open_ms),sql_ms(r.metrics.active_ms),r.peak_instances as i64,r.end_reason,r.peak_window_instances.map(|n|n as i64),self.run_id])?;
        }
        for e in &self.activity_events {
            tx.execute("INSERT INTO activity_events(activity,at_ms,kind,pids,foreground,window_count,run_id) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![e.activity,e.at,e.kind,e.pids,e.foreground,e.window_count as i64,self.run_id])?;
        }
        for r in self.reminders.values() {
            tx.execute("INSERT INTO reminder_occurrences(id,name,first_due_ms,done_ms,elapsed_ms,snooze_count,completion_interval_ms,status,run_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(id) DO UPDATE SET done_ms=excluded.done_ms,elapsed_ms=excluded.elapsed_ms,snooze_count=excluded.snooze_count,completion_interval_ms=excluded.completion_interval_ms,status=excluded.status", params![r.id,r.name,r.first_due,r.done,sql_ms(r.elapsed_ms),r.snoozes,r.interval_ms.map(sql_ms),r.status,self.run_id])?;
        }
        // Within an execution use monotonic completion intervals. Across DAT
        // restarts only wall-clock completion dates are available.
        for r in self
            .reminders
            .values()
            .filter(|r| r.done.is_some() && r.interval_ms.is_none())
        {
            tx.execute("UPDATE reminder_occurrences SET completion_interval_ms=done_ms-(SELECT MAX(previous.done_ms) FROM reminder_occurrences previous WHERE previous.name=reminder_occurrences.name AND previous.id<>reminder_occurrences.id AND previous.done_ms<=reminder_occurrences.done_ms) WHERE id=?1 AND completion_interval_ms IS NULL", [&r.id])?;
        }
        for e in &self.reminder_events {
            tx.execute("INSERT INTO reminder_events(occurrence_id,at_ms,elapsed_ms,kind,run_id) VALUES (?1,?2,?3,?4,?5)", params![e.occurrence,e.at,sql_ms(e.elapsed_ms),e.kind,self.run_id])?;
        }
        for interval in self.observations.values().flatten() {
            tx.execute("INSERT INTO activity_observations(run_id,activity,session_id,start_ms,end_ms,open_ms,active_ms,process_count,window_count) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![self.run_id,interval.activity,interval.session_id,interval.start_ms,interval.end_ms,sql_ms(interval.metrics.open_ms),sql_ms(interval.metrics.active_ms),interval.process_count as i64,interval.window_count as i64])?;
        }
        tx.commit()?;
        self.increments.clear();
        self.sessions.clear();
        self.activity_events.clear();
        self.reminders.clear();
        self.reminder_events.clear();
        self.observations.clear();
        Ok(())
    }
}
fn sql_ms(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}
fn initialize(connection: &mut Connection) -> Result<()> {
    connection.busy_timeout(Duration::from_millis(250))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    let version: u32 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > 3 {
        bail!("database schema {version} is newer than this DAT build supports");
    }
    let tx = connection.transaction()?;
    if version == 0 {
        tx.execute_batch("CREATE TABLE activity_totals(activity TEXT PRIMARY KEY, open_ms INTEGER NOT NULL, active_ms INTEGER NOT NULL);
        CREATE TABLE activity_sessions(id TEXT PRIMARY KEY, activity TEXT NOT NULL, start_ms INTEGER NOT NULL, last_seen_ms INTEGER NOT NULL, end_ms INTEGER, open_ms INTEGER NOT NULL, active_ms INTEGER NOT NULL, peak_instances INTEGER NOT NULL, end_reason TEXT);
        CREATE TABLE activity_events(id INTEGER PRIMARY KEY, activity TEXT NOT NULL, at_ms INTEGER NOT NULL, kind TEXT NOT NULL, pids TEXT NOT NULL, foreground INTEGER NOT NULL);
        CREATE TABLE reminder_occurrences(id TEXT PRIMARY KEY, name TEXT NOT NULL, first_due_ms INTEGER NOT NULL, done_ms INTEGER, elapsed_ms INTEGER NOT NULL, snooze_count INTEGER NOT NULL, completion_interval_ms INTEGER, status TEXT NOT NULL);
        CREATE TABLE reminder_events(id INTEGER PRIMARY KEY, occurrence_id TEXT NOT NULL REFERENCES reminder_occurrences(id), at_ms INTEGER NOT NULL, elapsed_ms INTEGER NOT NULL, kind TEXT NOT NULL);
        CREATE INDEX sessions_activity_start ON activity_sessions(activity,start_ms);
        CREATE INDEX reminder_name_due ON reminder_occurrences(name,first_due_ms);
        CREATE INDEX reminder_name_done ON reminder_occurrences(name,done_ms);
        PRAGMA user_version=1;")?;
    }
    if version < 2 {
        // Keep old process-based instance peaks intact; historical window counts
        // cannot be reconstructed and must remain NULL instead of being guessed.
        tx.execute_batch(
            "ALTER TABLE activity_sessions ADD COLUMN peak_window_instances INTEGER;
            ALTER TABLE activity_events ADD COLUMN window_count INTEGER;
            PRAGMA user_version=2;",
        )?;
    }
    if version < 3 {
        tx.execute_batch("CREATE TABLE dat_runs(id TEXT PRIMARY KEY, started_at_ms INTEGER NOT NULL, last_seen_ms INTEGER NOT NULL, ended_at_ms INTEGER, end_reason TEXT);
            ALTER TABLE activity_sessions ADD COLUMN run_id TEXT REFERENCES dat_runs(id);
            ALTER TABLE activity_events ADD COLUMN run_id TEXT REFERENCES dat_runs(id);
            ALTER TABLE reminder_occurrences ADD COLUMN run_id TEXT REFERENCES dat_runs(id);
            ALTER TABLE reminder_events ADD COLUMN run_id TEXT REFERENCES dat_runs(id);
            CREATE TABLE activity_observations(id INTEGER PRIMARY KEY,run_id TEXT NOT NULL REFERENCES dat_runs(id),activity TEXT NOT NULL,session_id TEXT REFERENCES activity_sessions(id),start_ms INTEGER NOT NULL,end_ms INTEGER NOT NULL,open_ms INTEGER NOT NULL,active_ms INTEGER NOT NULL,process_count INTEGER NOT NULL,window_count INTEGER NOT NULL);
            CREATE INDEX observations_range ON activity_observations(start_ms,end_ms);
            CREATE INDEX observations_run ON activity_observations(run_id,activity);
            CREATE INDEX observations_session ON activity_observations(session_id,start_ms,end_ms);
            CREATE INDEX sessions_run ON activity_sessions(run_id);
            CREATE INDEX events_run ON activity_events(run_id);
            CREATE INDEX reminder_events_occurrence_time ON reminder_events(occurrence_id,at_ms);
            PRAGMA user_version=3;")?;
    }
    tx.execute("UPDATE dat_runs SET ended_at_ms=last_seen_ms,end_reason='interrupted' WHERE ended_at_ms IS NULL", [])?;
    // An interrupted observation ends at its last durable checkpoint, not startup.
    tx.execute("UPDATE activity_sessions SET end_ms=last_seen_ms,end_reason='interrupted' WHERE end_ms IS NULL", [])?;
    tx.execute(
        "UPDATE reminder_occurrences SET status='interrupted' WHERE status='pending'",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn version_two_migration_preserves_all_rows_and_adds_unattributed_run_columns() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE activity_totals(activity TEXT PRIMARY KEY,open_ms INTEGER NOT NULL,active_ms INTEGER NOT NULL);
            CREATE TABLE activity_sessions(id TEXT PRIMARY KEY,activity TEXT NOT NULL,start_ms INTEGER NOT NULL,last_seen_ms INTEGER NOT NULL,end_ms INTEGER,open_ms INTEGER NOT NULL,active_ms INTEGER NOT NULL,peak_instances INTEGER NOT NULL,end_reason TEXT,peak_window_instances INTEGER);
            CREATE TABLE activity_events(id INTEGER PRIMARY KEY,activity TEXT NOT NULL,at_ms INTEGER NOT NULL,kind TEXT NOT NULL,pids TEXT NOT NULL,foreground INTEGER NOT NULL,window_count INTEGER);
            CREATE TABLE reminder_occurrences(id TEXT PRIMARY KEY,name TEXT NOT NULL,first_due_ms INTEGER NOT NULL,done_ms INTEGER,elapsed_ms INTEGER NOT NULL,snooze_count INTEGER NOT NULL,completion_interval_ms INTEGER,status TEXT NOT NULL);
            CREATE TABLE reminder_events(id INTEGER PRIMARY KEY,occurrence_id TEXT NOT NULL REFERENCES reminder_occurrences(id),at_ms INTEGER NOT NULL,elapsed_ms INTEGER NOT NULL,kind TEXT NOT NULL);
            INSERT INTO activity_totals VALUES ('Code',123456,12345);
            INSERT INTO activity_sessions VALUES ('s','Code',100,200,200,100,50,3,'closed',NULL);
            INSERT INTO activity_events VALUES (1,'Code',100,'opened','42',1,NULL);
            INSERT INTO reminder_occurrences VALUES ('r','Water',100,200,100,1,NULL,'done');
            INSERT INTO reminder_events VALUES (1,'r',200,100,'Done'); PRAGMA user_version=2;").unwrap();
        initialize(&mut c).unwrap();
        initialize(&mut c).unwrap();
        assert_eq!(
            c.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            3
        );
        for table in [
            "activity_totals",
            "activity_sessions",
            "activity_events",
            "reminder_occurrences",
            "reminder_events",
        ] {
            assert_eq!(
                c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
        for table in [
            "activity_sessions",
            "activity_events",
            "reminder_occurrences",
            "reminder_events",
        ] {
            assert_eq!(
                c.query_row(&format!("SELECT run_id FROM {table}"), [], |r| r
                    .get::<_, Option<String>>(0))
                    .unwrap(),
                None
            );
        }
        assert_eq!(
            c.query_row("SELECT open_ms,active_ms FROM activity_totals", [], |r| Ok(
                (r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)
            ))
            .unwrap(),
            (123456, 12345)
        );
        assert_eq!(
            c.query_row(
                "SELECT end_ms,end_reason,peak_window_instances FROM activity_sessions",
                [],
                |r| Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?
                ))
            )
            .unwrap(),
            (200, "closed".into(), None)
        );
        for table in ["dat_runs", "activity_observations"] {
            assert_eq!(
                c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn each_storage_execution_has_one_run_and_interruption_ends_at_last_checkpoint() {
        let path = std::env::temp_dir().join(format!("dat-runs-{}.db", unique_id()));
        let mut first = Storage::new(Some(path.clone()));
        first.add_time(
            "Code",
            Metrics {
                open_ms: 1234,
                active_ms: 345,
            },
        );
        first.flush().unwrap();
        let first_id = first.run_id.clone();
        first.finish_run(wall_time(), "dat_shutdown");
        first.flush().unwrap();
        drop(first);
        let mut second = Storage::new(Some(path.clone()));
        second.flush().unwrap();
        second.flush().unwrap();
        let second_id = second.run_id.clone();
        assert_ne!(first_id, second_id);
        let checkpoint: i64 = second
            .connection
            .as_ref()
            .unwrap()
            .query_row(
                "SELECT last_seen_ms FROM dat_runs WHERE id=?1",
                [&second_id],
                |r| r.get(0),
            )
            .unwrap();
        drop(second); // Simulated abrupt stop, without finish_run.
        let mut third = Storage::new(Some(path.clone()));
        third.flush().unwrap();
        let c = third.connection.as_ref().unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM dat_runs", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            c.query_row(
                "SELECT end_reason FROM dat_runs WHERE id=?1",
                [&first_id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "dat_shutdown"
        );
        assert_eq!(
            c.query_row(
                "SELECT ended_at_ms,end_reason FROM dat_runs WHERE id=?1",
                [&second_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            )
            .unwrap(),
            (checkpoint, "interrupted".into())
        );
        assert_eq!(
            c.query_row(
                "SELECT ended_at_ms FROM dat_runs WHERE id=?1",
                [&third.run_id],
                |r| r.get::<_, Option<i64>>(0)
            )
            .unwrap(),
            None
        );
        assert_eq!(
            third.totals["Code"],
            Metrics {
                open_ms: 1234,
                active_ms: 345
            }
        );
        drop(third);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn statistics_are_read_only_and_do_not_double_count_joined_rows() {
        let path = std::env::temp_dir().join(format!("dat-stats-{}.db", unique_id()));
        let mut store = Storage::new(Some(path.clone()));
        store.add_time(
            "Code",
            Metrics {
                open_ms: 900_000,
                active_ms: 300_000,
            },
        );
        store.flush().unwrap();
        let c = store.connection.as_ref().unwrap();
        c.execute_batch("INSERT INTO activity_sessions(id,activity,start_ms,last_seen_ms,open_ms,active_ms,peak_instances,peak_window_instances) VALUES ('a','Code',1000,2000,600000,200000,10,1),('b','Code',3000,4000,100000,50000,12,2),('c','Session only',5000,6000,1000,500,2,1);
        INSERT INTO reminder_occurrences(id,name,first_due_ms,done_ms,elapsed_ms,snooze_count,completion_interval_ms,status) VALUES ('one','Water',0,1000,1000,1,NULL,'done'),('two','Water',1000,4000,3000,2,NULL,'done'),('three','Water',5000,NULL,100000,3,NULL,'pending');") .unwrap();
        let stats = read_statistics(&path).unwrap();
        let row = &stats.activities[0];
        assert_eq!(row.name, "Code");
        assert_eq!(row.open_ms, 900_000);
        assert_eq!(row.active_ms, 300_000);
        assert_eq!(row.sessions, 2);
        assert_eq!(row.peak_processes, Some(12));
        assert_eq!(row.peak_window_instances, Some(2));
        assert!(!row.session_totals_only);
        assert!(stats.activities[1].session_totals_only);
        assert_eq!(stats.activities[1].open_ms, 1000);
        assert_eq!(stats.reminders[0].completed, 2);
        assert_eq!(stats.reminders[0].snoozes, 6);
        assert_eq!(stats.reminders[0].average_response_ms, Some(2000));
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM activity_sessions WHERE end_ms IS NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            3
        );
        assert_eq!(
            c.query_row(
                "SELECT status FROM reminder_occurrences WHERE id='three'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "pending"
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn version_one_migration_preserves_legacy_process_counts_and_statistics() {
        let path = std::env::temp_dir().join(format!("dat-migration-{}.db", unique_id()));
        let mut c = Connection::open(&path).unwrap();
        initialize(&mut c).unwrap();
        c.execute_batch("DROP TABLE activity_observations; DROP INDEX reminder_events_occurrence_time; DROP INDEX sessions_run; DROP INDEX events_run; ALTER TABLE activity_sessions DROP COLUMN run_id; ALTER TABLE activity_events DROP COLUMN run_id; ALTER TABLE reminder_occurrences DROP COLUMN run_id; ALTER TABLE reminder_events DROP COLUMN run_id; DROP TABLE dat_runs; ALTER TABLE activity_sessions DROP COLUMN peak_window_instances; ALTER TABLE activity_events DROP COLUMN window_count; PRAGMA user_version=1;
        INSERT INTO activity_totals VALUES ('Code',5000,2000);
        INSERT INTO activity_sessions(id,activity,start_ms,last_seen_ms,end_ms,open_ms,active_ms,peak_instances,end_reason) VALUES ('legacy','Code',1000,2000,2000,5000,2000,10,'closed');
        INSERT INTO reminder_occurrences(id,name,first_due_ms,done_ms,elapsed_ms,snooze_count,completion_interval_ms,status) VALUES ('legacy-reminder','Water',1000,2000,1000,1,NULL,'done');
        INSERT INTO reminder_events(id,occurrence_id,at_ms,elapsed_ms,kind) VALUES (1,'legacy-reminder',2000,1000,'Done');") .unwrap();
        let old = read_statistics(&path).unwrap();
        assert_eq!(old.activities[0].peak_processes, Some(10));
        assert_eq!(old.activities[0].peak_window_instances, None);
        assert_eq!(
            c.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            1
        );
        initialize(&mut c).unwrap();
        initialize(&mut c).unwrap();
        let new = read_statistics(&path).unwrap();
        assert_eq!(new.activities[0].peak_processes, Some(10));
        assert_eq!(new.activities[0].peak_window_instances, None);
        assert_eq!(new.activities[0].open_ms, 5000);
        assert_eq!(new.activities[0].sessions, 1);
        assert_eq!(new.reminders[0].completed, 1);
        assert_eq!(
            c.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM reminder_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(c);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn statistics_missing_database_does_not_create_a_file() {
        let path = std::env::temp_dir().join(format!("dat-missing-{}.db", unique_id()));
        assert!(read_statistics(&path).unwrap().activities.is_empty());
        assert!(!path.exists());
    }

    use super::*;
    #[test]
    fn restart_schema_totals_and_no_duplicate_sessions() {
        let path = std::env::temp_dir().join(format!("dat-test-{}.db", unique_id()));
        let mut store = Storage::new(Some(path.clone()));
        store.add_time(
            "Game",
            Metrics {
                open_ms: 600_000,
                active_ms: 300_000,
            },
        );
        store.sessions.insert(
            "one".into(),
            SessionRecord {
                id: "one".into(),
                activity: "Game".into(),
                start: 1,
                last_seen: 2,
                end: None,
                metrics: Metrics::default(),
                peak_instances: 2,
                peak_window_instances: Some(1),
                end_reason: None,
            },
        );
        store.flush().unwrap();
        store.flush().unwrap();
        drop(store);
        let mut store = Storage::new(Some(path.clone()));
        store.flush().unwrap();
        assert_eq!(store.totals["Game"].open_ms, 600_000);
        let c = store.connection.as_ref().unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM activity_sessions", [], |r| r
                .get::<_, u32>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            c.query_row("SELECT end_reason FROM activity_sessions", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "interrupted"
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn failed_flush_retains_data_for_retry() {
        let mut store = Storage::new(None);
        store.add_time(
            "Game",
            Metrics {
                open_ms: 10,
                active_ms: 5,
            },
        );
        assert!(store.flush().is_err());
        assert_eq!(store.increments["Game"].open_ms, 10);
    }

    #[test]
    fn transaction_failure_rolls_back_then_retries_once() {
        let path = std::env::temp_dir().join(format!("dat-retry-{}.db", unique_id()));
        let mut store = Storage::new(Some(path.clone()));
        store.flush().unwrap();
        store.add_time(
            "Game",
            Metrics {
                open_ms: 100,
                active_ms: 50,
            },
        );
        // Force a late foreign-key failure after totals were updated in the transaction.
        store.reminder_events.push(ReminderEvent {
            occurrence: "missing".into(),
            at: 100,
            elapsed_ms: 0,
            kind: "Triggered",
        });
        assert!(store.flush().is_err());
        let count: i32 = store
            .connection
            .as_ref()
            .unwrap()
            .query_row("SELECT count(*) FROM activity_totals", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(store.increments["Game"].open_ms, 100);
        store.reminder_events.clear();
        store.flush().unwrap();
        store.flush().unwrap();
        drop(store);
        let mut reopened = Storage::new(Some(path.clone()));
        reopened.flush().unwrap();
        assert_eq!(reopened.totals["Game"].open_ms, 100);
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn reminders_survive_restarts_with_completion_intervals() {
        let path = std::env::temp_dir().join(format!("dat-reminders-{}.db", unique_id()));
        let mut store = Storage::new(Some(path.clone()));
        let first = ReminderRecord {
            id: "first".into(),
            name: "Water".into(),
            first_due: 1000,
            done: Some(2000),
            elapsed_ms: 1000,
            snoozes: 1,
            interval_ms: None,
            status: "done".into(),
        };
        store.reminders.insert(first.id.clone(), first.clone());
        store.reminder_events.push(ReminderEvent {
            occurrence: first.id.clone(),
            at: 2000,
            elapsed_ms: 1000,
            kind: "Done",
        });
        store.flush().unwrap();
        drop(store);
        let mut store = Storage::new(Some(path.clone()));
        let second = ReminderRecord {
            id: "second".into(),
            first_due: 5000,
            done: Some(6000),
            ..first
        };
        store.reminders.insert(second.id.clone(), second);
        store.flush().unwrap();
        let c = store.connection.as_ref().unwrap();
        assert_eq!(
            c.query_row(
                "SELECT completion_interval_ms FROM reminder_occurrences WHERE id='second'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            4000
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM reminder_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn newer_schema_is_rejected_without_modification() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.pragma_update(None, "user_version", 4).unwrap();
        assert!(initialize(&mut connection).is_err());
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            4
        );
    }
}

#[derive(Debug, Default)]
pub struct Statistics {
    pub activities: Vec<ActivitySummary>,
    pub reminders: Vec<ReminderSummary>,
}
#[derive(Debug)]
pub struct ActivitySummary {
    pub name: String,
    pub open_ms: u64,
    pub active_ms: u64,
    pub sessions: u64,
    pub events: u64,
    pub peak_processes: Option<i64>,
    pub peak_window_instances: Option<i64>,
    pub last_session: Option<String>,
    pub session_totals_only: bool,
}
#[derive(Debug)]
pub struct ReminderSummary {
    pub name: String,
    pub completed: u64,
    pub snoozes: u64,
    pub average_response_ms: Option<u64>,
}

/// UI readers never initialize/recover sessions or create a missing database.
/// A separate read transaction gives the window a consistent committed snapshot.
pub fn read_statistics(path: &std::path::Path) -> Result<Statistics> {
    if !path.try_exists()? {
        return Ok(Statistics::default());
    }
    let mut connection =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_millis(250))?;
    let tx = connection.transaction()?;
    let version: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if !(1..=3).contains(&version) {
        bail!("Unsupported statistics schema {version}; start a compatible DAT build first");
    }
    let window_peak = if version >= 2 {
        "MAX(peak_window_instances)"
    } else {
        "NULL"
    };
    let query = format!(
        "WITH sessions AS (
        SELECT activity, SUM(open_ms) AS open_ms, SUM(active_ms) AS active_ms,
            COUNT(*) AS count, MAX(peak_instances) AS processes, {window_peak} AS windows,
            MAX(start_ms) AS last_start FROM activity_sessions GROUP BY activity
        ), names AS (SELECT activity FROM activity_totals UNION SELECT activity FROM sessions)
        SELECT names.activity, COALESCE(t.open_ms,s.open_ms,0), COALESCE(t.active_ms,s.active_ms,0),
            COALESCE(s.count,0), s.processes, s.windows,
            strftime('%Y-%m-%d %H:%M:%S UTC',s.last_start/1000,'unixepoch'), t.activity IS NULL,
            (SELECT COUNT(*) FROM activity_events e WHERE e.activity=names.activity)
        FROM names LEFT JOIN activity_totals t ON t.activity=names.activity
        LEFT JOIN sessions s ON s.activity=names.activity ORDER BY names.activity COLLATE NOCASE"
    );
    let activities = tx
        .prepare(&query)?
        .query_map([], |r| {
            Ok(ActivitySummary {
                name: r.get(0)?,
                open_ms: r.get::<_, i64>(1)?.max(0) as u64,
                active_ms: r.get::<_, i64>(2)?.max(0) as u64,
                sessions: r.get::<_, i64>(3)?.max(0) as u64,
                peak_processes: r.get(4)?,
                peak_window_instances: r.get(5)?,
                last_session: r.get(6)?,
                session_totals_only: r.get(7)?,
                events: r.get::<_, i64>(8)?.max(0) as u64,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let reminders = tx
        .prepare(
            "SELECT name,
        SUM(CASE WHEN status='done' AND done_ms IS NOT NULL THEN 1 ELSE 0 END),
        SUM(snooze_count), AVG(CASE WHEN status='done' AND done_ms IS NOT NULL THEN elapsed_ms END)
        FROM reminder_occurrences GROUP BY name ORDER BY name COLLATE NOCASE",
        )?
        .query_map([], |r| {
            Ok(ReminderSummary {
                name: r.get(0)?,
                completed: r.get::<_, i64>(1)?.max(0) as u64,
                snoozes: r.get::<_, i64>(2)?.max(0) as u64,
                average_response_ms: r
                    .get::<_, Option<f64>>(3)?
                    .map(|n| n.max(0.0).round() as u64),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    tx.commit()?;
    Ok(Statistics {
        activities,
        reminders,
    })
}
