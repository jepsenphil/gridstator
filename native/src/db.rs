//! A dedicated SQLite owner serializes mutations without blocking async I/O threads.
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::{
    path::Path,
    sync::{Arc, mpsc},
};

type Operation = Box<dyn FnOnce(&mut Connection) + Send>;
#[derive(Clone)]
pub struct Db {
    sender: Arc<mpsc::Sender<Operation>>,
}
impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_guarded(path, None)
    }
    /// Retain an instance lease until the owner thread drains queued work and closes SQLite.
    pub fn open_with_guard(path: &Path, guard: Arc<dyn Send + Sync>) -> Result<Self> {
        Self::open_guarded(path, Some(guard))
    }
    fn open_guarded(path: &Path, guard: Option<Arc<dyn Send + Sync>>) -> Result<Self> {
        if let Some(parent) = path.parent() {
            crate::state::private_dir(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(10))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS records(kind TEXT NOT NULL,id TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(kind,id)); CREATE TABLE IF NOT EXISTS logs(job_id TEXT NOT NULL,seq INTEGER NOT NULL,data TEXT NOT NULL,PRIMARY KEY(job_id,seq));")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        let (sender, receiver) = mpsc::channel::<Operation>();
        std::thread::Builder::new()
            .name("gridstator-sqlite".into())
            .spawn(move || {
                let _guard = guard;
                let mut connection = connection;
                for op in receiver {
                    op(&mut connection);
                }
            })?;
        Ok(Self {
            sender: Arc::new(sender),
        })
    }
    pub async fn call<T: Send + 'static>(
        &self,
        op: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.sender
            .send(Box::new(move |connection| {
                let _ = tx.send(op(connection));
            }))
            .map_err(|_| anyhow::anyhow!("database owner stopped"))?;
        rx.await.context("database operation cancelled")?
    }
    pub async fn write<T: Send + 'static>(
        &self,
        op: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.call(move |connection| {
            // Local administration and an embedded worker can open independent owners. Acquire
            // the writer reservation before reading so WAL read->write upgrades cannot fail
            // with SQLITE_BUSY_SNAPSHOT despite the configured busy timeout.
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let result = op(&transaction)?;
            transaction.commit()?;
            Ok(result)
        })
        .await
    }
}
pub fn get(c: &Connection, kind: &str, id: &str) -> Result<Option<Value>> {
    let raw: Option<String> = c
        .query_row(
            "SELECT data FROM records WHERE kind=?1 AND id=?2",
            params![kind, id],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
}
pub fn list(c: &Connection, kind: &str) -> Result<Vec<Value>> {
    let mut stmt = c.prepare("SELECT data FROM records WHERE kind=?1 ORDER BY id")?;
    let rows = stmt.query_map([kind], |r| r.get::<_, String>(0))?;
    rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
}
pub fn put(c: &Connection, kind: &str, id: &str, value: &Value) -> Result<()> {
    c.execute("INSERT INTO records(kind,id,data)VALUES(?1,?2,?3)ON CONFLICT(kind,id)DO UPDATE SET data=excluded.data",params![kind,id,serde_json::to_string(value)?])?;
    Ok(())
}
pub fn delete(c: &Connection, kind: &str, id: &str) -> Result<()> {
    c.execute(
        "DELETE FROM records WHERE kind=?1 AND id=?2",
        params![kind, id],
    )?;
    Ok(())
}
pub fn logs(c: &Connection, id: &str, after: i64, limit: usize) -> Result<Vec<Value>> {
    let mut stmt =
        c.prepare("SELECT data FROM logs WHERE job_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3")?;
    let rows = stmt.query_map(params![id, after, limit as i64], |r| r.get::<_, String>(0))?;
    rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
}
