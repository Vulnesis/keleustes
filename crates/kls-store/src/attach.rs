//! Files an agent profile carries (migration 0252, agent B): one row per
//! (agent, path) naming the content-addressed blob that holds the bytes.

use crate::{Store, now_ms};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::Row;

/// One file of an agent profile.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentFile {
    pub agent_id: String,
    /// Where the profile lists it (relative, `~/...` or absolute).
    pub path: String,
    pub sha256: String,
    pub size_bytes: i64,
    /// Who attached it (provenance).
    pub added_by: String,
    pub added_at: i64,
}

impl Store {
    /// An agent's files, by path.
    pub async fn agent_files(&self, agent_id: &str) -> Result<Vec<AgentFile>> {
        let rows = sqlx::query(self.sql(
            "SELECT agent_id, path, sha256, size_bytes, added_by, added_at FROM agent_files \
             WHERE agent_id = ? ORDER BY path",
        ))
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(AgentFile {
                    agent_id: r.try_get("agent_id")?,
                    path: r.try_get("path")?,
                    sha256: r.try_get("sha256")?,
                    size_bytes: r.try_get("size_bytes")?,
                    added_by: r.try_get("added_by")?,
                    added_at: r.try_get("added_at")?,
                })
            })
            .collect()
    }

    /// Adds or replaces the file at (agent, path).
    pub async fn put_agent_file(&self, f: &AgentFile) -> Result<AgentFile> {
        let at = if f.added_at > 0 { f.added_at } else { now_ms() };
        let mut tx = self.begin_write().await?;
        sqlx::query(self.sql("DELETE FROM agent_files WHERE agent_id = ? AND path = ?"))
            .bind(&f.agent_id)
            .bind(&f.path)
            .execute(&mut *tx)
            .await?;
        sqlx::query(self.sql(
            "INSERT INTO agent_files (agent_id, path, sha256, size_bytes, added_by, added_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        ))
        .bind(&f.agent_id)
        .bind(&f.path)
        .bind(&f.sha256)
        .bind(f.size_bytes)
        .bind(&f.added_by)
        .bind(at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(AgentFile {
            added_at: at,
            ..f.clone()
        })
    }

    /// Removes one file; true when it was there.
    pub async fn delete_agent_file(&self, agent_id: &str, path: &str) -> Result<bool> {
        let res = sqlx::query(self.sql("DELETE FROM agent_files WHERE agent_id = ? AND path = ?"))
            .bind(agent_id)
            .bind(path)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Whether any agent file still names the blob.
    pub async fn agent_blob_in_use(&self, sha256: &str) -> Result<bool> {
        let row = sqlx::query(self.sql("SELECT COUNT(*) AS n FROM agent_files WHERE sha256 = ?"))
            .bind(sha256)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get::<i64, _>("n")? > 0)
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;

    #[tokio::test]
    async fn agent_files_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_sqlite(&dir.path().join("t.db")).await.unwrap();
        let f = super::AgentFile {
            agent_id: "a1".into(),
            path: "docs/guide.md".into(),
            sha256: "ab".repeat(32),
            size_bytes: 12,
            added_by: "owner".into(),
            added_at: 0,
        };
        let saved = s.put_agent_file(&f).await.unwrap();
        assert!(saved.added_at > 0);
        s.put_agent_file(&super::AgentFile {
            size_bytes: 13,
            ..f.clone()
        })
        .await
        .unwrap();
        let got = s.agent_files("a1").await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].size_bytes, 13);
        assert!(s.agent_blob_in_use(&f.sha256).await.unwrap());
        assert!(s.delete_agent_file("a1", "docs/guide.md").await.unwrap());
        assert!(!s.delete_agent_file("a1", "docs/guide.md").await.unwrap());
        assert!(s.agent_files("a1").await.unwrap().is_empty());
        assert!(!s.agent_blob_in_use(&f.sha256).await.unwrap());
    }
}
