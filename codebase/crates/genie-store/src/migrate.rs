//! Migration runner (04 §5).
//!
//! * Migrations are numbered SQL files under `migrations/` plus optional Rust steps,
//!   journaled in `migration(id, completed_at)`.
//! * A fresh DB is bootstrapped from [`CURRENT_SCHEMA`] in one transaction and every
//!   migration id is marked complete.
//! * An existing DB gets an encrypted backup (`<db>.bak-<version>`, last two kept) and
//!   then each pending migration in its own transaction.
//! * A DB with a migration id this binary doesn't know refuses to open
//!   ([`StoreError::NewerSchema`]): downgrades are not supported.
//!
//! Long data migrations (resumable background jobs with a cursor in `kv`) are not
//! needed yet; see the README's known gaps.

use rusqlite::{Connection, Transaction, params};

use crate::error::{Result, StoreError};
use crate::util::now_ms;

pub enum Step {
    Sql(&'static str),
    Rust(fn(&Transaction<'_>) -> Result<()>),
}

pub struct Migration {
    pub id: i64,
    pub name: &'static str,
    pub step: Step,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        id: 1,
        name: "init",
        step: Step::Sql(include_str!("../migrations/0001_init.sql")),
    },
    Migration {
        id: 2,
        name: "session_meta",
        step: Step::Sql(include_str!("../migrations/0002_session_meta.sql")),
    },
];

/// The full current schema used to bootstrap a fresh DB: the SQL migrations in order
/// (a test checks that bootstrap and the migration chain agree). Regenerate it from a
/// migrated DB once a migration needs a Rust step.
pub const CURRENT_SCHEMA: &str = concat!(
    include_str!("../migrations/0001_init.sql"),
    "\n",
    include_str!("../migrations/0002_session_meta.sql"),
);

const JOURNAL: &str =
    "CREATE TABLE IF NOT EXISTS migration (id INTEGER PRIMARY KEY, completed_at INTEGER NOT NULL)";

pub fn latest() -> i64 {
    MIGRATIONS.iter().map(|m| m.id).max().unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Bootstrapped,
    UpToDate,
    Migrated { from: i64, to: i64 },
}

/// The schema version of an open DB (0 = fresh).
pub fn current_version(conn: &Connection) -> Result<i64> {
    let has_journal: bool = conn.query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'migration'",
        [],
        |r| r.get(0),
    )?;
    if !has_journal {
        return Ok(0);
    }
    Ok(
        conn.query_row("SELECT coalesce(max(id), 0) FROM migration", [], |r| {
            r.get(0)
        })?,
    )
}

/// Bring the DB to [`latest`]. `backup(version)` runs before any migration of an
/// existing DB.
pub fn run(
    conn: &mut Connection,
    backup: &dyn Fn(&Connection, i64) -> Result<()>,
) -> Result<Outcome> {
    let version = current_version(conn)?;
    let known = latest();
    if version > known {
        return Err(StoreError::NewerSchema {
            found: version,
            known,
        });
    }
    if version == 0 {
        let tables: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |r| r.get(0),
        )?;
        if tables > 0 {
            return Err(StoreError::Format(
                "database has tables but no migration journal".into(),
            ));
        }
        bootstrap(conn)?;
        return Ok(Outcome::Bootstrapped);
    }
    if version == known {
        return Ok(Outcome::UpToDate);
    }
    backup(conn, version)?;
    apply_from(conn, version)?;
    Ok(Outcome::Migrated {
        from: version,
        to: known,
    })
}

fn bootstrap(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(JOURNAL)?;
    tx.execute_batch(CURRENT_SCHEMA)?;
    let now = now_ms();
    for m in MIGRATIONS {
        tx.execute(
            "INSERT INTO migration (id, completed_at) VALUES (?1, ?2)",
            params![m.id, now],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Apply every migration with id > `version`, each in its own transaction.
pub(crate) fn apply_from(conn: &mut Connection, version: i64) -> Result<()> {
    conn.execute_batch(JOURNAL)?;
    for m in MIGRATIONS.iter().filter(|m| m.id > version) {
        let tx = conn.transaction()?;
        match m.step {
            Step::Sql(sql) => tx.execute_batch(sql)?,
            Step::Rust(f) => f(&tx)?,
        }
        tx.execute(
            "INSERT INTO migration (id, completed_at) VALUES (?1, ?2)",
            params![m.id, now_ms()],
        )?;
        tx.commit()?;
        tracing::info!(id = m.id, name = m.name, "migration applied");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(conn: &Connection) -> Vec<(String, String)> {
        let mut st = conn
            .prepare(
                "SELECT name, coalesce(sql, '') FROM sqlite_master
                 WHERE name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn bootstrap_matches_migration_chain() {
        let mut a = Connection::open_in_memory().unwrap();
        assert_eq!(run(&mut a, &|_, _| Ok(())).unwrap(), Outcome::Bootstrapped);
        let mut b = Connection::open_in_memory().unwrap();
        apply_from(&mut b, 0).unwrap();
        assert_eq!(schema(&a), schema(&b));
        assert_eq!(current_version(&a).unwrap(), latest());
        assert_eq!(run(&mut a, &|_, _| Ok(())).unwrap(), Outcome::UpToDate);
    }

    #[test]
    fn newer_db_is_refused() {
        let mut c = Connection::open_in_memory().unwrap();
        run(&mut c, &|_, _| Ok(())).unwrap();
        c.execute("INSERT INTO migration VALUES (9999, 0)", [])
            .unwrap();
        let err = run(&mut c, &|_, _| Ok(())).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("Update Genie to open this data")
        );
    }
}
