//! A scored pair is a receipt, not permission to dismiss changed source state.

use super::*;
use rusqlite::OptionalExtension;

pub(in crate::commands::prune) struct Pair {
    category: Category,
    a: (&'static str, i64),
    b: (&'static str, i64),
}

pub(in crate::commands::prune) fn prepare(
    conn: &Connection,
    root: &Path,
    id: &str,
) -> Result<Option<Pair>> {
    let mut pair = None;
    visit_matching(conn, root, None, Some(id), &[], &mut |item, a, b| {
        pair = Some(Pair {
            category: item.category,
            a: (a.excerpt.source_type, a.excerpt.source_id),
            b: (b.excerpt.source_type, b.excerpt.source_id),
        });
    })?;
    Ok(pair)
}

impl Pair {
    /// Re-score only the named pair, outside the writer transaction. The
    /// scan's display order is not necessarily the model's input order.
    pub(in crate::commands::prune) fn prepare_targeted(
        conn: &Connection,
        root: &Path,
        category: Category,
        first: (&'static str, i64),
        second: (&'static str, i64),
        id: &str,
        protected_files: &[FileIdentity],
    ) -> Result<Option<Self>> {
        let eligible = match category {
            Category::DuplicateFacts => first.0 == "fact" && second.0 == "fact",
            Category::UnlinkedDecisions => first.0 == "decision" && second.0 == "decision",
            Category::CorrectionSweep => {
                matches!(first.0, "fact" | "decision")
                    && matches!(second.0, "fact" | "arch_section" | "doc_chunk" | "task")
            }
            _ => false,
        };
        if !eligible || first == second {
            return Ok(None);
        }
        let (Some(a), Some(b)) = (
            load(conn, root, first, protected_files)?,
            load(conn, root, second, protected_files)?,
        ) else {
            return Ok(None);
        };
        let mut found = None;
        let pair = Self {
            category,
            a: first,
            b: second,
        };
        if category == Category::CorrectionSweep {
            if pair.is_current_rows(conn, id, &a, &b)?
                && !matches(&a.match_text(), &[&b])?.is_empty()
            {
                found = Some(pair);
            }
        } else {
            pairs(conn, &[a, b], category, Some(id), &mut |_, a, b| {
                found = Some(Self {
                    category,
                    a: (a.excerpt.source_type, a.excerpt.source_id),
                    b: (b.excerpt.source_type, b.excerpt.source_id),
                });
            })?;
        }
        Ok(found)
    }

    pub(in crate::commands::prune) fn is_current(
        &self,
        conn: &Connection,
        root: &Path,
        id: &str,
        protected_files: &[FileIdentity],
    ) -> Result<bool> {
        // Only these two rows, their parents and (for fact replacements) their
        // replacement chain contribute. No all-pair traversal or model work.
        let Some(a) = load(conn, root, self.a, protected_files)? else {
            return Ok(false);
        };
        let Some(b) = load(conn, root, self.b, protected_files)? else {
            return Ok(false);
        };
        self.is_current_rows(conn, id, &a, &b)
    }

    fn is_current_rows(&self, conn: &Connection, id: &str, a: &Row, b: &Row) -> Result<bool> {
        if identifier(self.category, (&a.state, &b.state))? != id {
            return Ok(false);
        }
        if self.category == Category::CorrectionSweep {
            if a.active || !b.active {
                return Ok(false);
            }
            if self.a.0 == "fact" && self.b.0 == "fact" {
                let replaced = conn
                    .query_row(
                        "WITH RECURSIVE replacements(id) AS (
                         SELECT superseded_by FROM facts WHERE project_id = 1 AND id = ?1
                         UNION
                         SELECT f.superseded_by FROM facts f JOIN replacements r ON f.id = r.id
                         WHERE f.project_id = 1
                     ) SELECT 1 FROM replacements WHERE id = ?2 LIMIT 1",
                        [self.a.1, self.b.1],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                return Ok(self.a.1 != self.b.1 && !replaced);
            }
            return Ok(true);
        }
        Ok(a.active && b.active)
    }
}

fn load(
    conn: &Connection,
    root: &Path,
    (kind, id): (&str, i64),
    protected_files: &[FileIdentity],
) -> Result<Option<Row>> {
    let mut rows = match kind {
        "fact" => facts(conn, Some(id)),
        "decision" => decisions(conn, Some(id)),
        _ => correction_targets(conn, root, Some((kind, id)), protected_files),
    }?;
    Ok(rows.pop())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::TransactionBehavior;

    #[test]
    fn dismissal_scores_without_a_writer_lock_and_revalidates_only_contributing_state() {
        let temp = tempfile::tempdir().unwrap();
        crate::commands::init::run(temp.path()).unwrap();
        let paths = crate::db::discover_paths(temp.path()).unwrap();
        let mut reader = Connection::open(&paths.db_path).unwrap();
        reader
            .execute_batch(
                "INSERT INTO facts(key,value) VALUES
             ('datastore','We will run a Postgres server to hold project memory.'),
             ('database','We will run a Postgres server to hold project memory.'),
             ('animal','Cats are domesticated felines kept as household pets.');",
            )
            .unwrap();
        let rows = facts(&reader, None).unwrap();
        let duplicate =
            identifier(Category::DuplicateFacts, (&rows[0].state, &rows[1].state)).unwrap();
        let unrelated =
            identifier(Category::DuplicateFacts, (&rows[0].state, &rows[2].state)).unwrap();
        let snapshot = reader.transaction().unwrap();
        let mut writer = Connection::open(&paths.db_path).unwrap();
        let write = writer
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let pair = prepare(&snapshot, temp.path(), &duplicate)
            .unwrap()
            .unwrap();
        assert!(
            prepare(&snapshot, temp.path(), &unrelated)
                .unwrap()
                .is_none()
        );
        write
            .execute("UPDATE facts SET value='new value' WHERE id=2", [])
            .unwrap();
        write.commit().unwrap();
        assert!(
            pair.is_current(&snapshot, temp.path(), &duplicate, &[])
                .unwrap()
        );
        drop(snapshot);
        let commit = reader
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(
            !pair
                .is_current(&commit, temp.path(), &duplicate, &[])
                .unwrap()
        );
        commit
            .execute(
                "UPDATE facts SET value=?1 WHERE id=2",
                ["We will run a Postgres server to hold project memory."],
            )
            .unwrap();
        // Unrelated tables are deliberately unavailable. Revalidation must not
        // read documents, backups, decisions, tasks or all other fact pairs.
        commit
            .execute_batch(
                "DROP TABLE doc_chunks; DROP TABLE documents; DROP TABLE arch_sections;
             DROP TABLE project_arch; DROP TABLE decisions; DROP TABLE tasks;",
            )
            .unwrap();
        assert!(
            pair.is_current(&commit, temp.path(), &duplicate, &[])
                .unwrap()
        );
    }

    #[test]
    fn correction_revalidation_checks_replacement_chains_and_current_architecture() {
        let temp = tempfile::tempdir().unwrap();
        crate::commands::init::run(temp.path()).unwrap();
        let paths = crate::db::discover_paths(temp.path()).unwrap();
        let mut conn = Connection::open(&paths.db_path).unwrap();
        conn.execute_batch(
            "INSERT INTO facts(id,key,value,superseded_by) VALUES
             (1,'old','claim',2),(2,'intermediate','claim',NULL),(3,'active','claim',NULL);
             INSERT INTO project_arch(id,body,actor,actor_raw,created_at)
             VALUES (1,'# v1','fixture','fixture','2025-01-01');
             INSERT INTO arch_sections(id,arch_id,ord,heading_path,body)
             VALUES (1,1,0,'v1','# v1');",
        )
        .unwrap();
        let source = facts(&conn, Some(1)).unwrap().pop().unwrap();
        let target = facts(&conn, Some(3)).unwrap().pop().unwrap();
        let fact_pair = Pair {
            category: Category::CorrectionSweep,
            a: ("fact", 1),
            b: ("fact", 3),
        };
        let fact_id =
            identifier(Category::CorrectionSweep, (&source.state, &target.state)).unwrap();
        let target = correction_targets(&conn, temp.path(), Some(("arch_section", 1)), &[])
            .unwrap()
            .pop()
            .unwrap();
        let arch_pair = Pair {
            category: Category::CorrectionSweep,
            a: ("fact", 1),
            b: ("arch_section", 1),
        };
        let arch_id =
            identifier(Category::CorrectionSweep, (&source.state, &target.state)).unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(
            fact_pair
                .is_current(&tx, temp.path(), &fact_id, &[])
                .unwrap()
        );
        assert!(
            arch_pair
                .is_current(&tx, temp.path(), &arch_id, &[])
                .unwrap()
        );
        tx.execute("UPDATE facts SET superseded_by=3 WHERE id=2", [])
            .unwrap();
        assert!(
            !fact_pair
                .is_current(&tx, temp.path(), &fact_id, &[])
                .unwrap()
        );
        tx.execute_batch(
            "INSERT INTO project_arch(id,body,actor,actor_raw,created_at)
             VALUES (2,'# v2','fixture','fixture','2025-01-02');",
        )
        .unwrap();
        assert!(
            !arch_pair
                .is_current(&tx, temp.path(), &arch_id, &[])
                .unwrap()
        );
    }
}
