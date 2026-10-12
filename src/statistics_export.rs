//! Local, streaming export of a committed SQLite snapshot. Never initializes the DB.
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, types::ValueRef};
use serde::{
    Serialize, Serializer,
    ser::{Error, SerializeMap, SerializeSeq, SerializeStruct},
};
use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{OnceLock, mpsc},
    time::Duration,
};

pub type CheckpointRequest = mpsc::Sender<std::result::Result<(), String>>;
static CHECKPOINTS: OnceLock<mpsc::Sender<CheckpointRequest>> = OnceLock::new();
const TABLES: [&str; 5] = [
    "activity_totals",
    "activity_sessions",
    "activity_events",
    "reminder_occurrences",
    "reminder_events",
];

pub fn set_checkpoint_sender(sender: mpsc::Sender<CheckpointRequest>) {
    let _ = CHECKPOINTS.set(sender);
}
pub fn checkpoint_available() -> bool {
    CHECKPOINTS.get().is_some()
}

pub fn checkpoint() -> Result<()> {
    let sender = CHECKPOINTS
        .get()
        .context("Tracker checkpoint service unavailable")?;
    let (reply, receiver) = mpsc::channel();
    sender.send(reply).context("Tracker has stopped")?;
    receiver
        .recv_timeout(Duration::from_secs(10))
        .context("Timed out waiting for tracker checkpoint")?
        .map_err(anyhow::Error::msg)
}

/// UTC filenames avoid locale-specific punctuation and are deterministic for QA.
pub fn filename(at_ms: i64) -> Result<String> {
    let connection = Connection::open_in_memory()?;
    Ok(connection.query_row(
        "SELECT strftime('DAT-statistics-%Y-%m-%d_%H-%M-%S.json',?1/1000,'unixepoch')",
        [at_ms],
        |row| row.get(0),
    )?)
}

fn parse_pids(raw: &str) -> Result<Vec<u32>> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    raw.split(',')
        .map(|pid| {
            if pid.is_empty() || !pid.bytes().all(|c| c.is_ascii_digit()) {
                bail!("Invalid activity event PID list");
            }
            pid.parse()
                .context("Activity event PID is outside the u32 range")
        })
        .collect()
}

struct Table<'a> {
    connection: &'a Connection,
    name: &'a str,
}
impl Serialize for Table<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        // Names come only from TABLES, never user input. Export actual columns,
        // including legacy schema-1 rows, without coercing NULL or integer values.
        let key = if self.name == "activity_totals" {
            "activity"
        } else {
            "id"
        };
        let mut statement = self
            .connection
            .prepare(&format!("SELECT * FROM {} ORDER BY {key}", self.name))
            .map_err(S::Error::custom)?;
        let columns: Vec<String> = statement
            .column_names()
            .iter()
            .map(|name| (*name).into())
            .collect();
        let mut rows = statement.query([]).map_err(S::Error::custom)?;
        let mut sequence = serializer.serialize_seq(None)?;
        while let Some(row) = rows.next().map_err(S::Error::custom)? {
            let mut fields = Vec::with_capacity(columns.len());
            for (index, name) in columns.iter().enumerate() {
                let value = match row.get_ref(index).map_err(S::Error::custom)? {
                    ValueRef::Null => serde_json::Value::Null,
                    ValueRef::Integer(value) => value.into(),
                    ValueRef::Text(value) => {
                        let text = std::str::from_utf8(value).map_err(S::Error::custom)?;
                        if self.name == "activity_events" && name == "pids" {
                            serde_json::to_value(parse_pids(text).map_err(S::Error::custom)?)
                                .map_err(S::Error::custom)?
                        } else {
                            text.into()
                        }
                    }
                    _ => {
                        return Err(S::Error::custom(format!(
                            "Unexpected SQLite type in {}.{name}",
                            self.name
                        )));
                    }
                };
                fields.push((name, value));
            }
            sequence.serialize_element(&Fields(fields))?;
        }
        sequence.end()
    }
}
struct Fields<'a>(Vec<(&'a String, serde_json::Value)>);
impl Serialize for Fields<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in &self.0 {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}
struct Snapshot<'a> {
    connection: &'a Connection,
    version: u32,
    at: String,
}
impl Serialize for Snapshot<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut object = serializer.serialize_struct(
            "DatStatisticsExport",
            if self.version >= 3 { 11 } else { 9 },
        )?;
        object.serialize_field("format", "dat-statistics-export")?;
        object.serialize_field("format_version", &1u32)?;
        object.serialize_field("exported_at_utc", &self.at)?;
        object.serialize_field("database_schema_version", &self.version)?;
        for name in TABLES {
            object.serialize_field(
                name,
                &Table {
                    connection: self.connection,
                    name,
                },
            )?;
        }
        if self.version >= 3 {
            for name in ["dat_runs", "activity_observations"] {
                object.serialize_field(
                    name,
                    &Table {
                        connection: self.connection,
                        name,
                    },
                )?;
            }
        }
        object.end()
    }
}

fn write_snapshot(database: &Path, writer: impl Write) -> Result<()> {
    let mut connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .context("Opening statistics database")?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let transaction = connection.transaction()?;
    let version =
        transaction.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))?;
    if !(1..=3).contains(&version) {
        bail!("Unsupported statistics database schema {version}");
    }
    let at = transaction.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
        row.get(0)
    })?;
    serde_json::to_writer_pretty(
        writer,
        &Snapshot {
            connection: &transaction,
            version,
            at,
        },
    )
    .context("Writing statistics JSON")?;
    transaction.commit()?;
    Ok(())
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::storage::Storage::new(
                crate::settings::data_directory()
                    .ok()
                    .map(|dir| dir.join("dat.db")),
            )
            .log_error(&format!(
                "Could not remove incomplete statistics export: {error}"
            ));
        }
    }
}

/// Replace permission is captured after the Save As overwrite confirmation.
/// Without it, a destination created concurrently is preserved.
pub fn export(database: &Path, destination: &Path, replace: bool) -> Result<()> {
    if destination.exists()
        && std::fs::canonicalize(database)? == std::fs::canonicalize(destination)?
    {
        bail!("Choose a JSON filename separate from the DAT database");
    }
    let mut name = destination.as_os_str().to_os_string();
    name.push(format!(".{}.tmp", crate::storage::unique_id()));
    let path = PathBuf::from(name);
    let file: File = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .context("Creating temporary export file")?;
    let temporary = Temporary(path);
    {
        let mut writer = BufWriter::new(file);
        write_snapshot(database, &mut writer)?;
        writer.write_all(b"\n")?;
        writer.flush().context("Flushing statistics export")?;
        writer
            .get_ref()
            .sync_all()
            .context("Syncing statistics export")?;
    }
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let from: Vec<u16> = temporary
        .0
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let to: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) } == 0 {
        return Err(std::io::Error::last_os_error()).context("Replacing statistics export file");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    struct Fixture {
        directory: PathBuf,
        database: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let directory =
                std::env::temp_dir().join(format!("dat-export-{}", crate::storage::unique_id()));
            std::fs::create_dir(&directory).unwrap();
            let database = directory.join("dat.db");
            crate::storage::Storage::new(Some(database.clone()))
                .flush()
                .unwrap();
            Self {
                directory,
                database,
            }
        }
        fn connection(&self) -> Connection {
            Connection::open(&self.database).unwrap()
        }
        fn destination(&self) -> PathBuf {
            self.directory.join("statistics.json")
        }
        fn json(&self) -> Value {
            export(&self.database, &self.destination(), true).unwrap();
            let bytes = std::fs::read(self.destination()).unwrap();
            assert!(String::from_utf8_lossy(&bytes).contains("\n  \"format\""));
            serde_json::from_slice(&bytes).unwrap()
        }
        fn seed(&self) {
            self.connection().execute_batch("INSERT INTO activity_totals VALUES ('Code',9223372036854775807,1234);
                INSERT INTO activity_sessions(id,activity,start_ms,last_seen_ms,end_ms,open_ms,active_ms,peak_instances,end_reason,peak_window_instances) VALUES ('s','Code',1000,2345,NULL,1345,345,2,NULL,NULL);
                INSERT INTO activity_events(id,activity,at_ms,kind,pids,foreground,window_count) VALUES (7,'Code',2345,'opened','10240,11488',1,NULL);
                INSERT INTO reminder_occurrences(id,name,first_due_ms,done_ms,elapsed_ms,snooze_count,completion_interval_ms,status) VALUES ('r','Water',1234,NULL,5678,2,NULL,'pending');
                INSERT INTO reminder_events(id,occurrence_id,at_ms,elapsed_ms,kind) VALUES (9,'r',2345,1111,'Snooze');").unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    #[test]
    fn empty_database_has_metadata_and_all_five_empty_arrays() {
        let fixture = Fixture::new();
        let document = fixture.json();
        assert_eq!(document["format"], "dat-statistics-export");
        assert_eq!(document["format_version"], 1);
        assert_eq!(document["database_schema_version"], 3);
        let timestamp = document["exported_at_utc"].as_str().unwrap();
        assert!(timestamp.contains('T') && timestamp.ends_with('Z'));
        assert_eq!(document.as_object().unwrap().len(), 11);
        for table in TABLES {
            assert_eq!(document[table], json!([]));
        }
    }

    #[test]
    fn complete_rows_integers_nulls_pids_and_database_content_are_preserved() {
        let fixture = Fixture::new();
        fixture.seed();
        let before = std::fs::read(&fixture.database).unwrap();
        let document = fixture.json();
        assert_eq!(
            document["activity_totals"],
            json!([{"activity":"Code","open_ms":i64::MAX,"active_ms":1234}])
        );
        assert_eq!(
            document["activity_sessions"],
            json!([{"id":"s","activity":"Code","start_ms":1000,"last_seen_ms":2345,"end_ms":null,"open_ms":1345,"active_ms":345,"peak_instances":2,"end_reason":null,"peak_window_instances":null,"run_id":null}])
        );
        assert_eq!(
            document["activity_events"],
            json!([{"id":7,"activity":"Code","at_ms":2345,"kind":"opened","pids":[10240,11488],"foreground":1,"window_count":null,"run_id":null}])
        );
        assert_eq!(
            document["reminder_occurrences"],
            json!([{"id":"r","name":"Water","first_due_ms":1234,"done_ms":null,"elapsed_ms":5678,"snooze_count":2,"completion_interval_ms":null,"status":"pending","run_id":null}])
        );
        assert_eq!(
            document["reminder_events"],
            json!([{"id":9,"occurrence_id":"r","at_ms":2345,"elapsed_ms":1111,"kind":"Snooze","run_id":null}])
        );
        assert_eq!(
            before,
            std::fs::read(&fixture.database).unwrap(),
            "Read-only export must leave DB bytes unchanged"
        );
        for table in TABLES {
            let count: i64 = fixture
                .connection()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 1);
        }
    }

    #[test]
    fn schema_version_is_read_from_database_and_legacy_columns_are_lossless() {
        let fixture = Fixture::new();
        fixture.seed();
        fixture.connection().execute_batch("ALTER TABLE activity_sessions DROP COLUMN peak_window_instances; ALTER TABLE activity_events DROP COLUMN window_count; PRAGMA user_version=1;").unwrap();
        let document = fixture.json();
        assert_eq!(document["database_schema_version"], 1);
        assert_eq!(document["format_version"], 1);
        assert!(
            document["activity_sessions"][0]
                .get("peak_window_instances")
                .is_none()
        );
        assert_eq!(
            fixture
                .connection()
                .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn pid_parsing_is_strict_and_preserves_order_and_duplicates() {
        assert_eq!(parse_pids("10240,11488").unwrap(), vec![10240, 11488]);
        assert_eq!(
            parse_pids("42,42,0,4294967295").unwrap(),
            vec![42, 42, 0, u32::MAX]
        );
        assert!(parse_pids("").unwrap().is_empty());
        for invalid in ["1,", "1,,2", "-1", "abc", "4294967296", " 42", "+42"] {
            assert!(parse_pids(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn snapshot_stays_consistent_during_concurrent_commits() {
        struct ConcurrentWriter {
            database: PathBuf,
            bytes: Vec<u8>,
            committed: bool,
        }
        impl Write for ConcurrentWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if !self.committed {
                    Connection::open(&self.database).unwrap()
                        .execute_batch("INSERT INTO activity_totals VALUES ('New',100,50); INSERT INTO reminder_occurrences(id,name,first_due_ms,done_ms,elapsed_ms,snooze_count,completion_interval_ms,status) VALUES ('new','New',10,NULL,20,0,NULL,'pending');").unwrap();
                    self.committed = true;
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let fixture = Fixture::new();
        fixture.seed();
        fixture
            .connection()
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        let mut writer = ConcurrentWriter {
            database: fixture.database.clone(),
            bytes: Vec::new(),
            committed: false,
        };
        write_snapshot(&fixture.database, &mut writer).unwrap();
        let document: Value = serde_json::from_slice(&writer.bytes).unwrap();
        assert!(writer.committed);
        assert_eq!(document["activity_totals"].as_array().unwrap().len(), 1);
        assert_eq!(
            document["reminder_occurrences"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            fixture
                .connection()
                .query_row("SELECT count(*) FROM activity_totals", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn normal_checkpoint_exports_live_sessions_without_closing_or_duplicating_them() {
        use crate::{
            resolver::ActivityObservation, settings::Settings, storage::Storage,
            tracking::ActivityTracker,
        };
        use std::time::Instant;
        let fixture = Fixture::new();
        let mut store = Storage::new(Some(fixture.database.clone()));
        store.flush().unwrap();
        let start = Instant::now();
        let mut tracker = ActivityTracker::new(start, Duration::from_secs(1));
        let settings = Settings {
            detailed_statistics: true,
            ..Settings::default()
        };
        let observations = [ActivityObservation {
            name: "Code".into(),
            pids: vec![10240],
            foreground: true,
            window_count: 1,
        }];
        tracker.sample(&observations, start, 1000, &settings, &mut store);
        tracker.sample(
            &observations,
            start + Duration::from_secs(1),
            2000,
            &settings,
            &mut store,
        );
        store.flush().unwrap();
        let first = fixture.json();
        assert_eq!(first["activity_sessions"].as_array().unwrap().len(), 1);
        assert_eq!(first["activity_sessions"][0]["end_ms"], Value::Null);
        assert_eq!(first["activity_totals"][0]["open_ms"], 1000);
        tracker.sample(
            &observations,
            start + Duration::from_secs(2),
            3000,
            &settings,
            &mut store,
        );
        store.flush().unwrap();
        let second = fixture.json();
        assert_eq!(second["activity_sessions"].as_array().unwrap().len(), 1);
        assert_eq!(
            second["activity_sessions"][0]["id"],
            first["activity_sessions"][0]["id"]
        );
        assert_eq!(second["activity_sessions"][0]["end_ms"], Value::Null);
        assert_eq!(second["activity_totals"][0]["open_ms"], 2000);
        assert_eq!(second["activity_events"].as_array().unwrap().len(), 1);
        drop(store);
    }

    #[test]
    fn malformed_pids_and_unsupported_schema_preserve_existing_file_and_clean_temporary() {
        let fixture = Fixture::new();
        fixture.seed();
        std::fs::write(fixture.destination(), "previous export").unwrap();
        fixture
            .connection()
            .execute("UPDATE activity_events SET pids='1,broken'", [])
            .unwrap();
        assert!(export(&fixture.database, &fixture.destination(), true).is_err());
        assert_eq!(
            std::fs::read_to_string(fixture.destination()).unwrap(),
            "previous export"
        );
        fixture
            .connection()
            .pragma_update(None, "user_version", 99)
            .unwrap();
        assert!(
            export(&fixture.database, &fixture.destination(), true)
                .unwrap_err()
                .to_string()
                .contains("Unsupported")
        );
        assert_eq!(
            std::fs::read_to_string(fixture.destination()).unwrap(),
            "previous export"
        );
        assert_eq!(std::fs::read_dir(&fixture.directory).unwrap().count(), 2);
    }

    #[test]
    fn missing_database_invalid_destination_and_unconfirmed_overwrite_fail_safely() {
        let fixture = Fixture::new();
        let missing = fixture.directory.join("missing.db");
        assert!(export(&missing, &fixture.destination(), false).is_err());
        assert!(!missing.exists() && !fixture.destination().exists());
        assert!(
            export(
                &fixture.database,
                &fixture.directory.join("absent/file.json"),
                false
            )
            .is_err()
        );
        std::fs::write(fixture.destination(), "previous").unwrap();
        assert!(export(&fixture.database, &fixture.destination(), false).is_err());
        assert_eq!(
            std::fs::read_to_string(fixture.destination()).unwrap(),
            "previous"
        );
        assert!(export(&fixture.database, &fixture.database, true).is_err());
        assert_eq!(std::fs::read_dir(&fixture.directory).unwrap().count(), 2);
    }

    #[test]
    fn serialization_writer_failure_is_reported() {
        struct FullDisk;
        impl Write for FullDisk {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk full"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let fixture = Fixture::new();
        assert!(
            format!(
                "{:#}",
                write_snapshot(&fixture.database, FullDisk).unwrap_err()
            )
            .contains("disk full")
        );
    }

    #[test]
    fn filenames_have_safe_utc_timestamps_and_json_extension() {
        assert_eq!(
            filename(0).unwrap(),
            "DAT-statistics-1970-01-01_00-00-00.json"
        );
        let name = filename(1_791_690_322_000).unwrap();
        assert_eq!(name, "DAT-statistics-2026-10-11_03-45-22.json");
        assert!(!name.contains([':', '/', '\\', '<', '>', '?', '*', '"', '|']));
    }
}
