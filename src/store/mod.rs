mod config;
mod rule_status;
mod sessions;
mod tasks;
use crate::{
    crypto::Crypto,
    error::{Error, Result},
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Serialize, de::DeserializeOwned};
pub use sessions::Session;
use std::{path::PathBuf, time::Duration};

#[derive(Clone)]
pub struct Store {
    pub(crate) path: PathBuf,
    pub crypto: Crypto,
}
pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
pub(crate) fn json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}
pub(crate) fn decode<T: DeserializeOwned>(value: String) -> Result<T> {
    Ok(serde_json::from_str(&value)?)
}
pub(crate) fn read<T: DeserializeOwned>(conn: &Connection, table: &str, id: &str) -> Result<T> {
    // table 只由内部固定调用点提供，用户值始终使用参数绑定。
    let sql = format!("SELECT data FROM {table} WHERE id=?1");
    let data: Option<String> = conn.query_row(&sql, [id], |r| r.get(0)).optional()?;
    decode(data.ok_or_else(|| Error::NotFound("记录不存在".into()))?)
}
impl Store {
    pub fn open(path: PathBuf, crypto: Crypto) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let value = Self { path, crypto };
        let mut conn = value.conn()?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 1 {
            return Err(Error::Internal("数据库版本高于当前程序支持版本"));
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!("../../migrations/001_initial.sql"))?;
        tx.commit()?;
        Ok(value)
    }
    pub(crate) fn conn(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        Ok(conn)
    }
    pub(crate) fn tx<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }
}
