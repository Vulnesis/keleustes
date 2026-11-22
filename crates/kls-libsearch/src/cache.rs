//! HTTP cache (ETag / Last-Modified / max-age) and the content-addressed
//! blob store under `<data>/libsearch/blobs/ab/abcdef…`. The rows live in
//! the index database (`http_cache`); deleting both is safe.

use anyhow::Result;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use sqlx::Row;
use sqlx::sqlite::SqlitePool;
use std::path::PathBuf;

#[derive(Clone)]
pub struct Cache {
    pool: SqlitePool,
    dir: PathBuf,
}

/// A cached response.
#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub etag: String,
    pub last_modified: String,
    /// Seconds.
    pub max_age: i64,
    pub fetched_at: i64,
    /// `None` when the blob is gone.
    pub body: Option<Bytes>,
}

impl Entry {
    pub fn fresh(&self, now: i64) -> bool {
        self.max_age > 0 && self.fetched_at + self.max_age * 1000 > now
    }
}

pub fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

impl Cache {
    pub(crate) fn new(pool: SqlitePool, dir: PathBuf) -> Self {
        Self { pool, dir }
    }

    fn blob_path(&self, sha: &str) -> PathBuf {
        self.dir.join(&sha[..2.min(sha.len())]).join(sha)
    }

    /// Writes bytes once; returns their sha256.
    pub async fn put_blob(&self, data: &[u8]) -> Result<String> {
        let sha = sha256_hex(data);
        let p = self.blob_path(&sha);
        if tokio::fs::metadata(&p).await.is_err() {
            if let Some(d) = p.parent() {
                tokio::fs::create_dir_all(d).await?;
            }
            let tmp = p.with_extension("tmp");
            tokio::fs::write(&tmp, data).await?;
            tokio::fs::rename(&tmp, &p).await?;
        }
        Ok(sha)
    }

    /// The bytes for a sha, when present and intact.
    pub async fn blob(&self, sha: &str) -> Option<Bytes> {
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let data = tokio::fs::read(self.blob_path(sha)).await.ok()?;
        (sha256_hex(&data) == sha).then(|| Bytes::from(data))
    }

    pub async fn lookup(&self, url: &str) -> Result<Option<Entry>> {
        let row = sqlx::query(
            "SELECT etag, last_modified, max_age, fetched_at, blob FROM http_cache WHERE url = ?",
        )
        .bind(url)
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else {
            return Ok(None);
        };
        let blob: String = r.try_get("blob")?;
        Ok(Some(Entry {
            etag: r.try_get("etag")?,
            last_modified: r.try_get("last_modified")?,
            max_age: r.try_get("max_age")?,
            fetched_at: r.try_get("fetched_at")?,
            body: self.blob(&blob).await,
        }))
    }

    pub async fn store(
        &self,
        url: &str,
        etag: &str,
        last_modified: &str,
        max_age: i64,
        now: i64,
        body: &[u8],
    ) -> Result<()> {
        let sha = self.put_blob(body).await?;
        sqlx::query(
            "INSERT INTO http_cache (url, etag, last_modified, max_age, fetched_at, blob) \
             VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (url) DO UPDATE SET etag = excluded.etag, \
             last_modified = excluded.last_modified, max_age = excluded.max_age, \
             fetched_at = excluded.fetched_at, blob = excluded.blob",
        )
        .bind(url)
        .bind(etag)
        .bind(last_modified)
        .bind(max_age)
        .bind(now)
        .bind(sha)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// A 304: the entry is fresh again.
    pub async fn touch(&self, url: &str, now: i64, max_age: i64) -> Result<()> {
        sqlx::query(
            "UPDATE http_cache SET fetched_at = ?, max_age = CASE WHEN ? > 0 THEN ? ELSE max_age END \
             WHERE url = ?",
        )
        .bind(now)
        .bind(max_age)
        .bind(max_age)
        .bind(url)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// When a URL was last fetched (for the `⌂ cached 2d` chip).
    pub async fn fetched_at(&self, url: &str) -> Option<i64> {
        self.lookup(url).await.ok().flatten().map(|e| e.fetched_at)
    }
}
