use std::{
    ffi::OsString,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use rusqlite::{Connection, ErrorCode, ffi};

use super::schema;

pub(crate) const DATABASE_FILE: &str = "library.db";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DatabaseOpenState {
    Current,
    Created,
    Recreated,
}

pub(crate) struct OpenDatabase {
    pub(crate) connection: Connection,
    pub(crate) state: DatabaseOpenState,
}

pub(crate) fn open_conn(db_path: &Path) -> Result<Connection> {
    register_sqlite_vec()?;
    let conn = Connection::open(db_path)
        .with_context(|| format!("无法打开数据库：{}", db_path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))
        .context("无法设置数据库超时")?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("无法启用数据库外键约束")?;
    Ok(conn)
}

/// Registers the statically linked sqlite-vec entry point for every SQLite
/// connection opened after this call. Registration is process-global, so keep
/// the result and never race multiple writes to SQLite's extension registry.
fn register_sqlite_vec() -> Result<()> {
    static REGISTRATION_RESULT: OnceLock<i32> = OnceLock::new();

    let result = *REGISTRATION_RESULT.get_or_init(|| {
        // SAFETY: sqlite-vec exports the standard three-argument SQLite
        // extension entry point, but its 0.1.8 Rust declaration omits those
        // arguments. This is the registration pattern shipped in that crate's
        // own rusqlite test. The function is statically linked and lives for
        // the duration of the process.
        unsafe {
            ffi::sqlite3_auto_extension(Some(std::mem::transmute(
                sqlite_vec::sqlite3_vec_init as *const (),
            )))
        }
    });
    if result != ffi::SQLITE_OK {
        bail!("无法注册 sqlite-vec 扩展，SQLite 错误码：{result}");
    }
    Ok(())
}

/// Opens the current development schema, discarding an incompatible or corrupt
/// database instead of attempting a migration or data repair.
pub(crate) fn open_or_recreate(db_path: &Path) -> Result<Connection> {
    Ok(open_or_recreate_with_state(db_path)?.connection)
}

/// Opens the current development schema and reports whether callers must clear
/// the external managed-object store. A missing database is `Created`; only an
/// existing incompatible or corrupt SQLite file is `Recreated`.
pub(crate) fn open_or_recreate_with_state(db_path: &Path) -> Result<OpenDatabase> {
    if !database_exists(db_path)? {
        remove_sidecars(db_path)?;
        return Ok(OpenDatabase {
            connection: create_fresh(db_path)?,
            state: DatabaseOpenState::Created,
        });
    }

    let conn = match open_conn(db_path) {
        Ok(conn) => conn,
        Err(error) if is_corrupt_database_error(&error) => {
            tracing::warn!(path = %db_path.display(), %error, "数据库已损坏，将重新创建");
            remove_database_files(db_path)?;
            return Ok(OpenDatabase {
                connection: create_fresh(db_path)?,
                state: DatabaseOpenState::Recreated,
            });
        }
        Err(error) => return Err(error),
    };

    match schema::is_current(&conn) {
        Ok(true) => Ok(OpenDatabase {
            connection: conn,
            state: DatabaseOpenState::Current,
        }),
        Ok(false) => {
            close_before_reset(conn, db_path)?;
            tracing::warn!(
                path = %db_path.display(),
                "数据库版本、结构或数据关系不符合当前开发版本，将重新创建"
            );
            remove_database_files(db_path)?;
            Ok(OpenDatabase {
                connection: create_fresh(db_path)?,
                state: DatabaseOpenState::Recreated,
            })
        }
        Err(error) if is_corrupt_database_error(&error) => {
            close_before_reset(conn, db_path)?;
            tracing::warn!(path = %db_path.display(), %error, "数据库已损坏，将重新创建");
            remove_database_files(db_path)?;
            Ok(OpenDatabase {
                connection: create_fresh(db_path)?,
                state: DatabaseOpenState::Recreated,
            })
        }
        Err(error) => Err(error),
    }
}

/// Closes and removes a database that was freshly created as part of a reset.
///
/// Callers use this when a later reset step (for example clearing the managed
/// object directory) fails. Removing the already-current empty database keeps
/// the reset retriable on the next startup instead of making the incomplete
/// reset look successfully committed.
pub(crate) fn discard_opened_database(conn: Connection, db_path: &Path) -> Result<()> {
    close_before_reset(conn, db_path)?;
    remove_database_files(db_path)
}

fn create_fresh(db_path: &Path) -> Result<Connection> {
    let mut conn = open_conn(db_path)?;
    schema::create(&mut conn)?;
    Ok(conn)
}

fn database_exists(db_path: &Path) -> Result<bool> {
    match fs::metadata(db_path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("无法检查数据库文件：{}", db_path.display()))
        }
    }
}

fn close_before_reset(conn: Connection, db_path: &Path) -> Result<()> {
    conn.close()
        .map_err(|(_, error)| error)
        .with_context(|| format!("无法关闭待重建的数据库：{}", db_path.display()))
}

fn remove_database_files(db_path: &Path) -> Result<()> {
    // Delete the main file first so a lock/ACL/read-only failure cannot leave
    // an otherwise retained database with only some of its journals removed.
    remove_file_if_exists(db_path)?;
    remove_sidecars(db_path)
}

fn remove_sidecars(db_path: &Path) -> Result<()> {
    remove_file_if_exists(&sidecar_path(db_path, "-wal"))?;
    remove_file_if_exists(&sidecar_path(db_path, "-shm"))?;
    remove_file_if_exists(&sidecar_path(db_path, "-journal"))
}

fn sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut path = OsString::from(db_path.as_os_str());
    path.push(suffix);
    PathBuf::from(path)
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("无法删除旧数据库文件：{}", path.display()))
        }
    }
}

fn is_corrupt_database_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let Some(rusqlite::Error::SqliteFailure(failure, _)) =
            cause.downcast_ref::<rusqlite::Error>()
        else {
            return false;
        };
        matches!(
            failure.code,
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
        )
    })
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use rusqlite::ffi;

    use super::*;

    #[test]
    fn every_opened_connection_has_static_sqlite_vec_functions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(DATABASE_FILE);
        let conn = open_or_recreate(&path).unwrap();

        let version: String = conn
            .query_row("SELECT vec_version()", [], |row| row.get(0))
            .unwrap();

        assert_eq!(version, "v0.1.8");
    }

    #[test]
    fn creates_and_reopens_the_current_schema() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(DATABASE_FILE);

        let opened = open_or_recreate_with_state(&path).unwrap();
        assert_eq!(opened.state, DatabaseOpenState::Created);
        let conn = opened.connection;
        assert!(schema::is_current(&conn).unwrap());
        conn.execute(
            "INSERT INTO groups (id, name, parent_id, created_at) VALUES ('kept', '保留', NULL, 1)",
            [],
        )
        .unwrap();
        drop(conn);

        let reopened = open_or_recreate_with_state(&path).unwrap();
        assert_eq!(reopened.state, DatabaseOpenState::Current);
        let reopened = reopened.connection;
        assert!(schema::is_current(&reopened).unwrap());
        let count: i64 = reopened
            .query_row("SELECT COUNT(*) FROM groups WHERE id = 'kept'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(1, count);
    }

    #[test]
    fn version_mismatch_recreates_the_database() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(DATABASE_FILE);
        let conn = open_or_recreate(&path).unwrap();
        conn.execute(
            "INSERT INTO groups (id, name, parent_id, created_at) VALUES ('old', '旧数据', NULL, 1)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", schema::SCHEMA_VERSION + 1)
            .unwrap();
        drop(conn);

        let recreated = open_or_recreate_with_state(&path).unwrap();
        assert_eq!(recreated.state, DatabaseOpenState::Recreated);
        let recreated = recreated.connection;
        assert!(schema::is_current(&recreated).unwrap());
        let count: i64 = recreated
            .query_row("SELECT COUNT(*) FROM groups", [], |row| row.get(0))
            .unwrap();
        assert_eq!(0, count);
    }

    #[test]
    fn unversioned_database_is_recreated_instead_of_migrated() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(DATABASE_FILE);
        let legacy = Connection::open(&path).unwrap();
        legacy
            .execute_batch("CREATE TABLE legacy_payload (value TEXT); INSERT INTO legacy_payload VALUES ('old');")
            .unwrap();
        drop(legacy);

        let recreated = open_or_recreate_with_state(&path).unwrap();
        assert_eq!(recreated.state, DatabaseOpenState::Recreated);
        let recreated = recreated.connection;
        assert!(schema::is_current(&recreated).unwrap());
        let legacy_table: i64 = recreated
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = 'legacy_payload'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(0, legacy_table);
    }

    #[test]
    fn same_version_schema_mismatch_recreates_the_database() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(DATABASE_FILE);
        let conn = open_or_recreate(&path).unwrap();
        conn.execute_batch(
            "ALTER TABLE groups RENAME TO groups_old;
             CREATE TABLE groups (
                 id TEXT PRIMARY KEY,
                 name TEXT,
                 parent_id TEXT,
                 created_at INTEGER NOT NULL
             );
             INSERT INTO groups VALUES ('old', '旧数据', NULL, 1);
             DROP TABLE groups_old;",
        )
        .unwrap();
        drop(conn);

        let recreated = open_or_recreate_with_state(&path).unwrap();
        assert_eq!(recreated.state, DatabaseOpenState::Recreated);
        let recreated = recreated.connection;
        assert!(schema::is_current(&recreated).unwrap());
        let count: i64 = recreated
            .query_row("SELECT COUNT(*) FROM groups", [], |row| row.get(0))
            .unwrap();
        assert_eq!(0, count);
    }

    #[test]
    fn non_database_file_is_recreated() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(DATABASE_FILE);
        fs::write(&path, b"this is not sqlite").unwrap();

        let recreated = open_or_recreate_with_state(&path).unwrap();
        assert_eq!(recreated.state, DatabaseOpenState::Recreated);
        assert!(schema::is_current(&recreated.connection).unwrap());
    }

    #[test]
    fn invalid_group_relation_recreates_the_whole_database() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(DATABASE_FILE);
        let conn = open_or_recreate(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        conn.execute(
            "INSERT INTO groups (id, name, parent_id, created_at) VALUES ('child', '坏分组', 'missing', 1)",
            [],
        )
        .unwrap();
        drop(conn);

        let recreated = open_or_recreate(&path).unwrap();
        assert!(schema::is_current(&recreated).unwrap());
        let count: i64 = recreated
            .query_row("SELECT COUNT(*) FROM groups", [], |row| row.get(0))
            .unwrap();
        assert_eq!(0, count);
    }

    #[test]
    fn group_cycle_recreates_the_whole_database() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(DATABASE_FILE);
        let conn = open_or_recreate(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        conn.execute_batch(
            "INSERT INTO groups (id, name, parent_id, created_at) VALUES
                 ('a', 'A', 'b', 1),
                 ('b', 'B', 'a', 2);",
        )
        .unwrap();
        drop(conn);

        let recreated = open_or_recreate(&path).unwrap();
        assert!(schema::is_current(&recreated).unwrap());
        let count: i64 = recreated
            .query_row("SELECT COUNT(*) FROM groups", [], |row| row.get(0))
            .unwrap();
        assert_eq!(0, count);
    }

    #[test]
    fn missing_current_source_recreates_the_whole_database() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(DATABASE_FILE);
        let conn = open_or_recreate(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO blobs (object_key, media_type, byte_len, hash, created_at)
                 VALUES ('source', 'application/epub+zip', 1, 'hash', 1);
             INSERT INTO books
                 (id, title, author, format, revision, source_object_key,
                  cover_asset_id, added_at, updated_at, group_id)
                 VALUES ('book', '书名', '作者', 'epub', 1, 'source', NULL, 1, 1, NULL);",
        )
        .unwrap();
        drop(conn);

        let recreated = open_or_recreate(&path).unwrap();
        assert!(schema::is_current(&recreated).unwrap());
        let count: i64 = recreated
            .query_row("SELECT COUNT(*) FROM books", [], |row| row.get(0))
            .unwrap();
        assert_eq!(0, count);
    }

    #[test]
    fn ordinary_open_error_does_not_delete_the_target() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(DATABASE_FILE);
        fs::create_dir(&path).unwrap();
        let sentinel = path.join("keep-me");
        fs::write(&sentinel, b"preserved").unwrap();

        assert!(open_or_recreate(&path).is_err());
        assert_eq!(b"preserved", fs::read(sentinel).unwrap().as_slice());
    }

    #[test]
    fn database_reset_removes_all_journal_sidecars() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(DATABASE_FILE);
        let wal = sidecar_path(&path, "-wal");
        let shm = sidecar_path(&path, "-shm");
        let journal = sidecar_path(&path, "-journal");
        fs::write(&path, b"database").unwrap();
        fs::write(&wal, b"wal").unwrap();
        fs::write(&shm, b"shm").unwrap();
        fs::write(&journal, b"journal").unwrap();

        remove_database_files(&path).unwrap();

        assert!(!path.exists());
        assert!(!wal.exists());
        assert!(!shm.exists());
        assert!(!journal.exists());
    }

    #[test]
    fn failed_main_database_deletion_preserves_all_sidecars() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(DATABASE_FILE);
        let wal = sidecar_path(&path, "-wal");
        let shm = sidecar_path(&path, "-shm");
        let journal = sidecar_path(&path, "-journal");
        fs::create_dir(&path).unwrap();
        fs::write(&wal, b"wal").unwrap();
        fs::write(&shm, b"shm").unwrap();
        fs::write(&journal, b"journal").unwrap();

        assert!(remove_database_files(&path).is_err());

        assert!(path.is_dir());
        assert!(wal.exists());
        assert!(shm.exists());
        assert!(journal.exists());
    }

    #[test]
    fn only_corruption_errors_are_eligible_for_recreation() {
        let sqlite_error =
            |code| anyhow::Error::new(rusqlite::Error::SqliteFailure(ffi::Error::new(code), None));

        assert!(is_corrupt_database_error(&sqlite_error(
            ffi::SQLITE_CORRUPT
        )));
        assert!(is_corrupt_database_error(&sqlite_error(ffi::SQLITE_NOTADB)));
        assert!(!is_corrupt_database_error(&sqlite_error(ffi::SQLITE_BUSY)));
        assert!(!is_corrupt_database_error(&sqlite_error(
            ffi::SQLITE_LOCKED
        )));
        assert!(!is_corrupt_database_error(&sqlite_error(ffi::SQLITE_PERM)));
        assert!(!is_corrupt_database_error(&sqlite_error(ffi::SQLITE_IOERR)));
        assert!(!is_corrupt_database_error(&sqlite_error(
            ffi::SQLITE_READONLY
        )));
        assert!(!is_corrupt_database_error(&sqlite_error(ffi::SQLITE_FULL)));
        assert!(!is_corrupt_database_error(&sqlite_error(
            ffi::SQLITE_CANTOPEN
        )));
        assert!(!is_corrupt_database_error(&sqlite_error(
            ffi::SQLITE_PROTOCOL
        )));
        assert!(!is_corrupt_database_error(&sqlite_error(
            ffi::SQLITE_SCHEMA
        )));
        assert!(!is_corrupt_database_error(&anyhow!("普通文件错误")));
    }
}
