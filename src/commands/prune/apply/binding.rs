use rusqlite::ffi;

use super::{
    Connection, FileIdentity, ProjectPaths, Result, fs, invalid, is_link_or_reparse_point,
    sha256_hex,
};
use crate::fs_identity::{FileInfo, file_identity};

pub(super) struct Binding {
    pub(super) fingerprint: String,
    database: FileInfo,
}

impl Binding {
    pub(super) fn read(paths: &ProjectPaths) -> Result<Self> {
        let meta = fs::symlink_metadata(&paths.memhub_dir)?;
        if !meta.is_dir()
            || is_link_or_reparse_point(&meta)
            || fs::canonicalize(&paths.memhub_dir)? != paths.memhub_dir
        {
            return Err(invalid(
                "wrap-up prune needs a direct .memhub directory at the project root",
            ));
        }
        let db_path = &paths.db_path;
        let meta = fs::symlink_metadata(db_path)?;
        if !meta.is_file() || is_link_or_reparse_point(&meta) {
            return Err(invalid("wrap-up prune needs a direct project.sqlite"));
        }
        // POSIX releases this process's SQLite locks if *any* descriptor for
        // the same inode is closed. Never independently open the DB on Unix.
        #[cfg(unix)]
        let database = crate::fs_identity::file_info_from_metadata(&meta);
        #[cfg(not(unix))]
        let database = {
            use crate::fs_identity::{file_info_from_handle, open_direct_file};
            file_info_from_handle(&open_direct_file(db_path, &meta)?)?
        };
        if database.links != 1 || fs::canonicalize(db_path)? != *db_path {
            return Err(invalid(
                "wrap-up prune needs a direct, unshared project.sqlite",
            ));
        }
        let fingerprint = sha256_hex(&serde_json::to_vec(&(
            paths.repo_root.as_os_str().as_encoded_bytes(),
            file_identity(&paths.memhub_dir)?,
            &database.identity,
        ))?);
        Ok(Self {
            database,
            fingerprint,
        })
    }

    pub(super) fn protected_file(&self) -> Option<&FileIdentity> {
        // A recorded document path (or a moved backup entry) must not cause
        // a raw content read of the SQLite inode while it may hold locks.
        cfg!(unix).then_some(&self.database.identity)
    }

    pub(super) fn require_connection(&self, conn: &Connection, paths: &ProjectPaths) -> Result<()> {
        let changed = || invalid("wrap-up prune project or database changed; scan again");
        if Self::read(paths)?.fingerprint != self.fingerprint
            || !matches_database(conn, &self.database)?
        {
            return Err(changed());
        }
        // Unix's file control compares with a pathname stat, not a returned
        // identity. Bracket it with the path checks; this is not a namespace lock.
        #[cfg(unix)]
        if Self::read(paths)?.fingerprint != self.fingerprint {
            return Err(changed());
        }
        Ok(())
    }
}

#[cfg(unix)]
fn matches_database(conn: &Connection, _database: &FileInfo) -> Result<bool> {
    let mut file: *mut ffi::sqlite3_file = std::ptr::null_mut();
    // SAFETY: conn outlives the call and FILE_POINTER takes a sqlite3_file**.
    let rc = unsafe {
        ffi::sqlite3_file_control(
            conn.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_FILE_POINTER,
            std::ptr::from_mut(&mut file).cast(),
        )
    };
    // Bundled Unix SQLite only supplies xShmMap for its POSIX I/O methods.
    // Other methods can report "not moved" without tracking an open inode.
    // SAFETY: these public VFS structs belong to the still-open connection.
    let posix = rc == ffi::SQLITE_OK
        && unsafe { file.as_ref().and_then(|f| f.pMethods.as_ref()) }
            .is_some_and(|methods| methods.iVersion >= 2 && methods.xShmMap.is_some());
    if !posix {
        return Err(invalid(
            "wrap-up prune cannot verify SQLite's open database without POSIX VFS locking",
        ));
    }
    let mut moved = -1;
    // SAFETY: conn outlives the call and the main-db opcode takes an int*.
    let rc = unsafe {
        ffi::sqlite3_file_control(
            conn.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_HAS_MOVED,
            std::ptr::from_mut(&mut moved).cast(),
        )
    };
    if rc != ffi::SQLITE_OK || !matches!(moved, 0 | 1) {
        return Err(invalid(format!(
            "wrap-up prune cannot verify SQLite's open database (HAS_MOVED: {rc})"
        )));
    }
    Ok(moved == 0)
}

#[cfg(windows)]
fn matches_database(conn: &Connection, database: &FileInfo) -> Result<bool> {
    use std::os::windows::io::{BorrowedHandle, RawHandle};

    let mut handle: RawHandle = std::ptr::null_mut();
    // SAFETY: conn outlives the call and the main-db opcode takes a HANDLE*.
    let rc = unsafe {
        ffi::sqlite3_file_control(
            conn.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_WIN32_GET_HANDLE,
            std::ptr::from_mut(&mut handle).cast(),
        )
    };
    if rc != ffi::SQLITE_OK || handle.is_null() || handle as isize == -1 {
        return Err(invalid(format!(
            "wrap-up prune cannot verify SQLite's open database (WIN32_GET_HANDLE: {rc})"
        )));
    }
    // SAFETY: SQLite owns the live handle; borrowing it does not close it.
    let actual = crate::fs_identity::file_info_from_borrowed_handle(unsafe {
        BorrowedHandle::borrow_raw(handle)
    })?;
    Ok(actual == *database)
}

#[cfg(not(any(unix, windows)))]
fn matches_database(_conn: &Connection, _database: &FileInfo) -> Result<bool> {
    Err(invalid(
        "wrap-up prune cannot verify SQLite's open database on this platform",
    ))
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use rusqlite::OpenFlags;

    fn fixture(root: &std::path::Path) -> ProjectPaths {
        let paths = ProjectPaths::for_repo_root(&fs::canonicalize(root).unwrap());
        fs::create_dir(&paths.memhub_dir).unwrap();
        Connection::open(&paths.db_path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE fixture (id INTEGER PRIMARY KEY); PRAGMA journal_mode = DELETE;",
            )
            .unwrap();
        paths
    }

    #[test]
    fn a_copy_opened_instead_of_the_reviewed_database_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let paths = fixture(temp.path());
        let binding = Binding::read(&paths).unwrap();
        let copy = paths.memhub_dir.join("copy.sqlite");
        fs::copy(&paths.db_path, &copy).unwrap();
        let open = |path: &std::path::Path| {
            Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .unwrap()
        };
        #[cfg(unix)]
        let wrong = {
            let parked = paths.memhub_dir.join("parked.sqlite");
            fs::rename(&paths.db_path, &parked).unwrap();
            fs::rename(&copy, &paths.db_path).unwrap();
            let wrong = open(&paths.db_path);
            fs::rename(&paths.db_path, &copy).unwrap();
            fs::rename(&parked, &paths.db_path).unwrap();
            wrong
        };
        // SQLite's Windows VFS omits FILE_SHARE_DELETE, so the open file
        // cannot be swapped back. Compare against an open copy directly.
        #[cfg(windows)]
        let wrong = {
            let wrong = open(&copy);
            assert!(fs::rename(&copy, copy.with_extension("parked")).is_err());
            let mut moved = -1_i32;
            // SAFETY: the live connection and output match the file-control API.
            let rc = unsafe {
                ffi::sqlite3_file_control(
                    wrong.handle(),
                    c"main".as_ptr(),
                    ffi::SQLITE_FCNTL_HAS_MOVED,
                    std::ptr::from_mut(&mut moved).cast(),
                )
            };
            assert_eq!(rc, ffi::SQLITE_NOTFOUND);
            wrong
        };
        assert_eq!(
            Binding::read(&paths).unwrap().fingerprint,
            binding.fingerprint
        );
        let err = binding.require_connection(&wrong, &paths).unwrap_err();
        assert!(err.to_string().contains("database changed"), "{err}");
        drop(wrong);
        let current = open(&paths.db_path);
        binding.require_connection(&current, &paths).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn an_untracked_unix_connection_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let paths = fixture(temp.path());
        let binding = Binding::read(&paths).unwrap();
        let conn = Connection::open_with_flags_and_vfs(
            &paths.db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY,
            "unix-none",
        )
        .unwrap();
        let err = binding.require_connection(&conn, &paths).unwrap_err();
        assert!(
            err.to_string().contains("without POSIX VFS locking"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn checking_identity_keeps_the_reserved_lock_visible_to_a_separate_process() {
        use std::process::Command;
        use std::time::Duration;

        const CHILD: &str = "MEMHUB_TEST_PRUNE_BINDING_CONTENDER";
        if std::env::var_os(CHILD).is_some() {
            let paths = ProjectPaths::for_repo_root(&std::env::current_dir().unwrap());
            let conn =
                Connection::open_with_flags(&paths.db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
                    .unwrap();
            conn.busy_timeout(Duration::ZERO).unwrap();
            let err = conn.execute_batch("BEGIN IMMEDIATE").unwrap_err();
            assert_eq!(
                err.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy)
            );
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let paths = fixture(temp.path());
        let binding = Binding::read(&paths).unwrap();
        let conn = Connection::open(&paths.db_path).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        binding.require_connection(&conn, &paths).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                concat!(
                    module_path!(),
                    "::checking_identity_keeps_the_reserved_lock_visible_to_a_separate_process"
                )
                .trim_start_matches("memhub::"),
            ])
            .env(CHILD, "1")
            .current_dir(&paths.repo_root)
            .output()
            .unwrap();
        assert!(child.status.success(), "{child:?}");
        assert!(
            String::from_utf8_lossy(&child.stdout).contains("1 passed"),
            "{child:?}"
        );
        conn.execute_batch("INSERT INTO fixture VALUES (1); COMMIT")
            .unwrap();
    }
}
