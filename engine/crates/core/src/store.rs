//! SQLite, shared by every module. One connection behind a mutex: Stuntman is a test double, not a
//! database server. ponytail: single connection; a pool when concurrent writes measurably queue.

use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::ApiError;

#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

impl Db {
    pub fn open(path: &std::path::Path) -> rusqlite::Result<Self> {
        let c = Connection::open(path)?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
        Ok(Self(Arc::new(Mutex::new(c))))
    }

    pub fn memory() -> Self {
        Self(Arc::new(Mutex::new(Connection::open_in_memory().unwrap())))
    }

    pub fn migrate(&self, sql: &str) -> rusqlite::Result<()> {
        self.0.lock().unwrap().execute_batch(sql)
    }

    /// Run blocking SQL off the async threads.
    pub async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> Result<T, ApiError> {
        let db = self.0.clone();
        tokio::task::spawn_blocking(move || f(&mut db.lock().unwrap()))
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?
            .map_err(|e| ApiError::internal(e.to_string()))
    }

    /// Empties every table, keeping the schema (reset to seeds). FTS shadow tables are left to
    /// their virtual table.
    pub fn wipe(&self, keep: &[&str]) -> rusqlite::Result<()> {
        let c = self.0.lock().unwrap();
        let tables: Vec<(String, String)> = c
            .prepare("SELECT name, coalesce(sql,'') FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let virt: Vec<&String> = tables.iter().filter(|(_, sql)| sql.to_uppercase().starts_with("CREATE VIRTUAL")).map(|(n, _)| n).collect();
        for (n, _) in &tables {
            if keep.contains(&n.as_str()) || virt.iter().any(|v| n != *v && n.starts_with(&format!("{v}_"))) {
                continue;
            }
            c.execute_batch(&format!("DELETE FROM \"{n}\""))?;
        }
        Ok(())
    }

    /// A consistent copy of the database into a new file (clone with state).
    pub fn copy_to(&self, path: &std::path::Path) -> rusqlite::Result<()> {
        self.0.lock().unwrap().execute("VACUUM INTO ?1", [path.to_string_lossy()]).map(|_| ())
    }

    pub fn size(&self) -> i64 {
        self.0.lock().unwrap().query_row("SELECT page_count * page_size FROM pragma_page_count, pragma_page_size", [], |r| r.get(0)).unwrap_or(0)
    }
}

#[cfg(test)]
mod t {
    #[test]
    fn wipe_keeps_schema_and_fts() {
        let d = super::Db::memory();
        d.migrate("CREATE TABLE a(x); INSERT INTO a VALUES(1); CREATE VIRTUAL TABLE f USING fts5(b); INSERT INTO f VALUES('hello');").unwrap();
        d.wipe(&[]).unwrap();
        let c = d.0.lock().unwrap();
        let n: i64 = c.query_row("SELECT (SELECT count(*) FROM a) + (SELECT count(*) FROM f)", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        c.execute("INSERT INTO f VALUES('again')", []).unwrap();
    }
}
