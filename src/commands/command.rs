use std::path::Path;

use crate::Result;
use crate::db;
use crate::models::CommandRecord;

pub fn list(start: &Path) -> Result<Vec<CommandRecord>> {
    let ctx = db::open_project(start)?;
    let mut stmt = ctx.conn.prepare(
        "SELECT id, kind, cmdline, last_exit_code, last_run_at, success_count, fail_count
         FROM commands
         ORDER BY COALESCE(last_run_at, '') DESC, id DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(CommandRecord {
            id: row.get(0)?,
            kind: row.get(1)?,
            cmdline: row.get(2)?,
            last_exit_code: row.get(3)?,
            last_run_at: row.get(4)?,
            success_count: row.get(5)?,
            fail_count: row.get(6)?,
        })
    })?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}
