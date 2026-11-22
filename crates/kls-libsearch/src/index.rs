//! The local search index: SQLite FTS5 in its own file
//! (`<data>/libsearch.db`). It is a rebuildable cache: a corrupt file is
//! deleted and rebuilt, and `Store` (which may be Postgres) never holds it.
//!
//! Ranking: exact name match first, then a name prefix match, then
//! `-bm25(hits_fts, 8, 6, 2, 3)` plus the trust boost (Official 2,
//! Trusted 1) plus `0.3·ln(1+popularity)`. The last query token matches as
//! a prefix.

use crate::cache::Cache;
use crate::licence::{Licence, LicencePolicy};
use crate::scan::Flag;
use crate::source::{Ecosystem, Hit, ItemType, Locator, Query, Trust};
use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, Row};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS src_state (source TEXT PRIMARY KEY, cursor TEXT, etag TEXT, \
     last_modified TEXT, refreshed_at INTEGER, ok_at INTEGER, error TEXT, items INTEGER, \
     parked_until INTEGER, commit_sha TEXT)",
    "CREATE TABLE IF NOT EXISTS hits (rowid INTEGER PRIMARY KEY, id TEXT UNIQUE, source TEXT, \
     item_type TEXT, ecosystem TEXT, name TEXT, title TEXT, description TEXT, tags TEXT, \
     licence TEXT, licence_class TEXT, trust TEXT, popularity INTEGER, updated_at INTEGER, \
     locator TEXT, flags TEXT, seen_at INTEGER)",
    "CREATE INDEX IF NOT EXISTS hits_source ON hits (source, seen_at)",
    "CREATE VIRTUAL TABLE IF NOT EXISTS hits_fts USING fts5(name, title, description, tags, \
     content='hits', content_rowid='rowid', tokenize='porter unicode61 remove_diacritics 2', \
     prefix='2 3')",
    "CREATE TRIGGER IF NOT EXISTS hits_ai AFTER INSERT ON hits BEGIN \
     INSERT INTO hits_fts(rowid, name, title, description, tags) \
     VALUES (new.rowid, new.name, new.title, new.description, new.tags); END",
    "CREATE TRIGGER IF NOT EXISTS hits_ad AFTER DELETE ON hits BEGIN \
     INSERT INTO hits_fts(hits_fts, rowid, name, title, description, tags) \
     VALUES ('delete', old.rowid, old.name, old.title, old.description, old.tags); END",
    "CREATE TRIGGER IF NOT EXISTS hits_au AFTER UPDATE ON hits BEGIN \
     INSERT INTO hits_fts(hits_fts, rowid, name, title, description, tags) \
     VALUES ('delete', old.rowid, old.name, old.title, old.description, old.tags); \
     INSERT INTO hits_fts(rowid, name, title, description, tags) \
     VALUES (new.rowid, new.name, new.title, new.description, new.tags); END",
    "CREATE TABLE IF NOT EXISTS http_cache (url TEXT PRIMARY KEY, etag TEXT, last_modified TEXT, \
     max_age INTEGER, fetched_at INTEGER, blob TEXT)",
    "CREATE TABLE IF NOT EXISTS git_refs (repo TEXT, ref TEXT, sha TEXT, checked_at INTEGER, \
     PRIMARY KEY (repo, ref))",
];

/// The index file and its blob store.
#[derive(Clone)]
pub struct Index {
    pool: SqlitePool,
    path: PathBuf,
}

/// Counts per type / ecosystem / source under a query.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facets {
    pub types: BTreeMap<ItemType, u64>,
    pub ecosystems: BTreeMap<Ecosystem, u64>,
    pub sources: BTreeMap<String, u64>,
    pub total: u64,
}

/// One source's refresh state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceState {
    pub source: String,
    pub cursor: Option<String>,
    pub refreshed_at: Option<i64>,
    pub ok_at: Option<i64>,
    pub error: Option<String>,
    pub items: i64,
    pub parked_until: Option<i64>,
    /// The commit last crawled (git-backed sources).
    pub commit: Option<String>,
}

async fn connect(path: &Path) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(opts)
        .await?;
    for s in SCHEMA {
        sqlx::query(*s).execute(&pool).await?;
    }
    let ok: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&pool)
        .await?;
    if ok != "ok" {
        anyhow::bail!("index check: {ok}");
    }
    Ok(pool)
}

/// The FTS5 MATCH expression for free text: every token quoted, the last
/// one a prefix. `None` for empty text.
pub fn fts_query(text: &str) -> Option<String> {
    let toks: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();
    if toks.is_empty() {
        return None;
    }
    let n = toks.len();
    Some(
        toks.iter()
            .enumerate()
            .map(|(i, t)| {
                if i + 1 == n {
                    format!("\"{t}\"*")
                } else {
                    format!("\"{t}\"")
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

/// WHERE clauses for the filters (and their binds), leaving one facet out.
fn filters(q: &Query, skip: Option<&str>) -> (Vec<String>, Vec<String>) {
    let mut w = Vec::new();
    let mut b = Vec::new();
    if skip != Some("type") && !q.types.is_empty() {
        w.push(format!("h.item_type IN ({})", placeholders(q.types.len())));
        b.extend(q.types.iter().map(|t| t.as_str().to_string()));
    }
    if skip != Some("eco") && !q.ecosystems.is_empty() {
        w.push(format!(
            "h.ecosystem IN ({})",
            placeholders(q.ecosystems.len())
        ));
        b.extend(q.ecosystems.iter().map(|t| t.as_str().to_string()));
    }
    if skip != Some("source") && !q.sources.is_empty() {
        w.push(format!("h.source IN ({})", placeholders(q.sources.len())));
        b.extend(q.sources.iter().cloned());
    }
    if !q.trust.is_empty() {
        w.push(format!("h.trust IN ({})", placeholders(q.trust.len())));
        b.extend(q.trust.iter().map(|t| t.as_str().to_string()));
    }
    match q.licence {
        LicencePolicy::Any => {}
        LicencePolicy::Permissive => w.push("h.licence_class = 'Permissive'".into()),
        LicencePolicy::KnownOnly => w.push("h.licence_class <> 'Unknown'".into()),
    }
    if q.hide_flagged {
        w.push("h.flags = '[]'".into());
    }
    (w, b)
}

fn hit_from_row(r: &sqlx::sqlite::SqliteRow) -> Result<Hit> {
    let tags: String = r.try_get("tags")?;
    let licence: String = r.try_get("licence")?;
    let flags: String = r.try_get("flags")?;
    let locator: String = r.try_get("locator")?;
    let item_type: String = r.try_get("item_type")?;
    let eco: String = r.try_get("ecosystem")?;
    let pop: Option<i64> = r.try_get("popularity")?;
    Ok(Hit {
        source: r.try_get("source")?,
        locator: serde_json::from_str::<Locator>(&locator).context("index locator")?,
        item_type: ItemType::parse(&item_type).unwrap_or(ItemType::Skill),
        ecosystem: Ecosystem::parse(&eco).unwrap_or(Ecosystem::Generic),
        name: r.try_get("name")?,
        title: r.try_get("title")?,
        description: r.try_get("description")?,
        tags: tags.split_whitespace().map(str::to_string).collect(),
        licence: serde_json::from_str::<Licence>(&licence).unwrap_or_default(),
        trust: Trust::parse(&r.try_get::<String, _>("trust")?),
        popularity: pop.map(|p| p.max(0) as u64),
        updated_at: r.try_get("updated_at")?,
        flags: serde_json::from_str::<Vec<Flag>>(&flags).unwrap_or_default(),
    })
}

/// The rank of a hit for `text` (higher first): exact name 1e6, name
/// prefix 1e5, then `bm25` (already negated) + trust + popularity.
pub fn rank(h: &Hit, text: &str, bm25: f64) -> f64 {
    let t = text.trim().to_lowercase();
    let name = h.name.to_lowercase();
    let mut s = bm25 + h.trust.boost() + 0.3 * (1.0 + h.popularity.unwrap_or(0) as f64).ln();
    if !t.is_empty() {
        if name == t {
            s += 1e6;
        } else if name.starts_with(&t) {
            s += 1e5;
        }
    }
    s
}

impl Index {
    /// Opens (or creates) the index; a corrupt file is deleted and rebuilt.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let pool = match connect(path).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "library index {} unusable ({e:#}); rebuilding",
                    path.display()
                );
                for ext in ["", "-wal", "-shm"] {
                    let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
                }
                connect(path).await?
            }
        };
        Ok(Self {
            pool,
            path: path.to_path_buf(),
        })
    }

    /// The HTTP cache sharing this file; blobs under `<dir>/blobs`.
    pub fn cache(&self) -> Cache {
        let dir = self
            .path
            .parent()
            .map(|p| p.join("libsearch").join("blobs"))
            .unwrap_or_else(|| PathBuf::from("libsearch/blobs"));
        Cache::new(self.pool.clone(), dir)
    }

    /// Inserts or updates hits (by `Hit::id`) and marks them seen now.
    pub async fn upsert(&self, source: &str, hits: &[Hit]) -> Result<()> {
        self.upsert_at(source, hits, kls_store::now_ms()).await
    }

    pub async fn upsert_at(&self, source: &str, hits: &[Hit], seen_at: i64) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for h in hits {
            sqlx::query(
                "INSERT INTO hits (id, source, item_type, ecosystem, name, title, description, tags, \
                 licence, licence_class, trust, popularity, updated_at, locator, flags, seen_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (id) DO UPDATE SET item_type = excluded.item_type, \
                 ecosystem = excluded.ecosystem, name = excluded.name, title = excluded.title, \
                 description = excluded.description, tags = excluded.tags, licence = excluded.licence, \
                 licence_class = excluded.licence_class, trust = excluded.trust, \
                 popularity = excluded.popularity, updated_at = excluded.updated_at, \
                 locator = excluded.locator, flags = excluded.flags, seen_at = excluded.seen_at",
            )
            .bind(h.id())
            .bind(source)
            .bind(h.item_type.as_str())
            .bind(h.ecosystem.as_str())
            .bind(&h.name)
            .bind(&h.title)
            .bind(&h.description)
            .bind(h.tags.join(" "))
            .bind(serde_json::to_string(&h.licence)?)
            .bind(format!("{:?}", h.licence.class))
            .bind(h.trust.as_str())
            .bind(h.popularity.map(|p| p.min(i64::MAX as u64) as i64))
            .bind(h.updated_at)
            .bind(h.locator.to_json())
            .bind(serde_json::to_string(&h.flags)?)
            .bind(seen_at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Deletes a source's rows not seen since `seen_before`. Returns how many.
    pub async fn retire(&self, source: &str, seen_before: i64) -> Result<u64> {
        let r = sqlx::query("DELETE FROM hits WHERE source = ? AND seen_at < ?")
            .bind(source)
            .bind(seen_before)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// Drops every row of a source (a tap removed).
    pub async fn forget(&self, source: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM hits WHERE source = ?")
            .bind(source)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM src_state WHERE source = ?")
            .bind(source)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    pub async fn count(&self, source: &str) -> Result<i64> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM hits WHERE source = ?")
                .bind(source)
                .fetch_one(&self.pool)
                .await?,
        )
    }

    /// Ranked hits for a query.
    pub async fn search(&self, q: &Query) -> Result<Vec<Hit>> {
        let (mut w, mut b) = filters(q, None);
        let fts = fts_query(&q.text);
        let sql = if let Some(m) = &fts {
            w.insert(0, "hits_fts MATCH ?".into());
            b.insert(0, m.clone());
            format!(
                "SELECT h.*, -bm25(hits_fts, 8.0, 6.0, 2.0, 3.0) AS score FROM hits_fts \
                 JOIN hits h ON h.rowid = hits_fts.rowid WHERE {} ORDER BY score DESC LIMIT ?",
                w.join(" AND ")
            )
        } else {
            let wh = if w.is_empty() {
                "1 = 1".to_string()
            } else {
                w.join(" AND ")
            };
            format!(
                "SELECT h.*, 0.0 AS score FROM hits h WHERE {wh} \
                 ORDER BY CASE h.trust WHEN 'official' THEN 0 WHEN 'trusted' THEN 1 ELSE 2 END, \
                 COALESCE(h.popularity, 0) DESC, h.name LIMIT ?"
            )
        };
        // Over-fetch so the re-rank (trust, popularity, exact name) has room.
        let lim = (q.limit.max(1) * 4).min(5000) as i64;
        let mut query = sqlx::query(AssertSqlSafe(sql));
        for v in &b {
            query = query.bind(v.clone());
        }
        let rows = query.bind(lim).fetch_all(&self.pool).await?;
        let mut out: Vec<(f64, Hit)> = Vec::with_capacity(rows.len());
        for r in &rows {
            let h = hit_from_row(r)?;
            let s: f64 = r.try_get("score").unwrap_or(0.0);
            out.push((
                if fts.is_some() {
                    rank(&h, &q.text, s)
                } else {
                    -(out.len() as f64)
                },
                h,
            ));
        }
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(out
            .into_iter()
            .take(q.limit.max(1))
            .map(|(_, h)| h)
            .collect())
    }

    /// Counts per type (ignoring the type filter), ecosystem (ignoring
    /// the ecosystem filter) and source.
    pub async fn facets(&self, q: &Query) -> Result<Facets> {
        let mut f = Facets::default();
        let fts = fts_query(&q.text);
        for (col, skip) in [
            ("item_type", "type"),
            ("ecosystem", "eco"),
            ("source", "source"),
        ] {
            let (mut w, mut b) = filters(q, Some(skip));
            let from = if let Some(m) = &fts {
                w.insert(0, "hits_fts MATCH ?".into());
                b.insert(0, m.clone());
                "hits_fts JOIN hits h ON h.rowid = hits_fts.rowid"
            } else {
                "hits h"
            };
            let wh = if w.is_empty() {
                "1 = 1".to_string()
            } else {
                w.join(" AND ")
            };
            let sql = format!(
                "SELECT h.{col} AS k, COUNT(*) AS n FROM {from} WHERE {wh} GROUP BY h.{col}"
            );
            let mut query = sqlx::query(AssertSqlSafe(sql));
            for v in &b {
                query = query.bind(v.clone());
            }
            for r in query.fetch_all(&self.pool).await? {
                let k: String = r.try_get("k")?;
                let n: i64 = r.try_get("n")?;
                match col {
                    "item_type" => {
                        if let Some(t) = ItemType::parse(&k) {
                            f.types.insert(t, n as u64);
                        }
                    }
                    "ecosystem" => {
                        if let Some(e) = Ecosystem::parse(&k) {
                            f.ecosystems.insert(e, n as u64);
                        }
                    }
                    _ => {
                        f.total += n as u64;
                        f.sources.insert(k, n as u64);
                    }
                }
            }
        }
        Ok(f)
    }

    pub async fn state(&self) -> Result<Vec<SourceState>> {
        let rows = sqlx::query(
            "SELECT source, cursor, refreshed_at, ok_at, error, items, parked_until, commit_sha \
             FROM src_state ORDER BY source",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(SourceState {
                    source: r.try_get("source")?,
                    cursor: r.try_get("cursor")?,
                    refreshed_at: r.try_get("refreshed_at")?,
                    ok_at: r.try_get("ok_at")?,
                    error: r.try_get("error")?,
                    items: r.try_get::<Option<i64>, _>("items")?.unwrap_or(0),
                    parked_until: r.try_get("parked_until")?,
                    commit: r.try_get("commit_sha")?,
                })
            })
            .collect()
    }

    pub async fn source_state(&self, source: &str) -> Result<Option<SourceState>> {
        Ok(self.state().await?.into_iter().find(|s| s.source == source))
    }

    /// Records a refresh outcome. `error` empty means it worked.
    pub async fn set_state(
        &self,
        source: &str,
        cursor: Option<&str>,
        commit: Option<&str>,
        now: i64,
        error: Option<&str>,
        parked_until: Option<i64>,
    ) -> Result<()> {
        let items = self.count(source).await?;
        let ok_at = if error.is_none() { Some(now) } else { None };
        sqlx::query(
            "INSERT INTO src_state (source, cursor, refreshed_at, ok_at, error, items, parked_until, commit_sha) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (source) DO UPDATE SET \
             cursor = COALESCE(excluded.cursor, src_state.cursor), refreshed_at = excluded.refreshed_at, \
             ok_at = COALESCE(excluded.ok_at, src_state.ok_at), error = excluded.error, \
             items = excluded.items, parked_until = excluded.parked_until, \
             commit_sha = COALESCE(excluded.commit_sha, src_state.commit_sha)",
        )
        .bind(source)
        .bind(cursor)
        .bind(now)
        .bind(ok_at)
        .bind(error)
        .bind(items)
        .bind(parked_until)
        .bind(commit)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The last commit seen for `repo` at `git_ref`.
    pub async fn git_ref(&self, repo: &str, git_ref: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT sha FROM git_refs WHERE repo = ? AND ref = ?")
                .bind(repo)
                .bind(git_ref)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn set_git_ref(&self, repo: &str, git_ref: &str, sha: &str, now: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO git_refs (repo, ref, sha, checked_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (repo, ref) DO UPDATE SET sha = excluded.sha, checked_at = excluded.checked_at",
        )
        .bind(repo)
        .bind(git_ref)
        .bind(sha)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(source: &str, name: &str, desc: &str, trust: Trust, pop: u64) -> Hit {
        Hit {
            source: source.into(),
            locator: Locator::Url {
                url: format!("https://x/{name}"),
            },
            item_type: ItemType::Skill,
            ecosystem: Ecosystem::ClaudeCode,
            name: name.into(),
            title: name.into(),
            description: desc.into(),
            tags: vec!["review".into()],
            licence: Licence::from_id("MIT", "index"),
            trust,
            popularity: Some(pop),
            updated_at: None,
            flags: vec![],
        }
    }

    async fn index() -> (Index, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        (
            Index::open(&d.path().join("libsearch.db")).await.unwrap(),
            d,
        )
    }

    #[tokio::test]
    async fn ranking_exact_then_prefix_then_bm25() {
        let (ix, _d) = index().await;
        ix.upsert(
            "s",
            &[
                hit(
                    "s",
                    "pr-helper",
                    "code review of pull requests",
                    Trust::Community,
                    10,
                ),
                hit("s", "code-review", "reviews code", Trust::Community, 0),
                hit("s", "code-reviewer-pro", "an agent", Trust::Community, 0),
                hit("s", "lint", "nothing about it", Trust::Official, 99999),
            ],
        )
        .await
        .unwrap();
        let got = ix.search(&Query::text("code-review")).await.unwrap();
        let names: Vec<_> = got.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names[0], "code-review");
        assert_eq!(names[1], "code-reviewer-pro");
        assert!(names.contains(&"pr-helper"));
        assert!(!names.contains(&"lint"));
        // Prefix on the last token.
        let got = ix.search(&Query::text("revi")).await.unwrap();
        assert!(got.len() >= 3);
        // Empty text: trust then popularity.
        let got = ix.search(&Query::default()).await.unwrap();
        assert_eq!(got[0].name, "lint");
    }

    #[tokio::test]
    async fn upsert_is_idempotent_and_retire_drops_stale() {
        let (ix, _d) = index().await;
        let a = hit("s", "a", "alpha", Trust::Trusted, 1);
        ix.upsert_at("s", std::slice::from_ref(&a), 10)
            .await
            .unwrap();
        ix.upsert_at("s", std::slice::from_ref(&a), 20)
            .await
            .unwrap();
        assert_eq!(ix.count("s").await.unwrap(), 1);
        let got = ix.search(&Query::text("alpha")).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], a);
        ix.upsert_at("s", &[hit("s", "b", "beta", Trust::Trusted, 1)], 30)
            .await
            .unwrap();
        assert_eq!(ix.retire("s", 30).await.unwrap(), 1);
        assert!(ix.search(&Query::text("alpha")).await.unwrap().is_empty());
        assert_eq!(ix.search(&Query::text("beta")).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn facets_and_filters() {
        let (ix, _d) = index().await;
        let mut m = hit(
            "mcp-registry",
            "github",
            "GitHub MCP server",
            Trust::Official,
            0,
        );
        m.item_type = ItemType::Mcp;
        m.ecosystem = Ecosystem::McpRegistry;
        m.licence = Licence::unknown();
        let mut f = hit("s", "gh-skill", "GitHub helper", Trust::Community, 0);
        f.flags = vec![Flag::SecretLike];
        ix.upsert("mcp-registry", &[m]).await.unwrap();
        ix.upsert("s", &[f]).await.unwrap();
        let mut q = Query::text("github");
        q.types = vec![ItemType::Mcp];
        let fc = ix.facets(&q).await.unwrap();
        assert_eq!(fc.types.get(&ItemType::Mcp), Some(&1));
        assert_eq!(
            fc.types.get(&ItemType::Skill),
            Some(&1),
            "type facet ignores the type filter"
        );
        assert_eq!(ix.search(&q).await.unwrap().len(), 1);
        let mut q = Query::text("github");
        q.licence = LicencePolicy::Permissive;
        assert_eq!(ix.search(&q).await.unwrap()[0].name, "gh-skill");
        q.hide_flagged = true;
        assert!(ix.search(&q).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn corrupt_file_rebuilds() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("libsearch.db");
        std::fs::write(
            &p,
            b"this is not a database, just junk bytes that fill a page",
        )
        .unwrap();
        let ix = Index::open(&p).await.unwrap();
        ix.upsert("s", &[hit("s", "a", "alpha", Trust::Trusted, 1)])
            .await
            .unwrap();
        assert_eq!(ix.count("s").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn cache_round_trip_and_etag_not_modified() {
        use crate::fetch::{FakeFetcher, Fetcher, HttpFetcher, Req};
        let (ix, _d) = index().await;
        let fake = FakeFetcher::new();
        fake.serve_with(
            "https://geminicli.com/extensions.json",
            200,
            vec![("etag", "\"v1\"")],
            "[1]",
        );
        let f = HttpFetcher::with_transport(fake.clone(), Some(ix.cache()));
        let r = f
            .get(Req::new("https://geminicli.com/extensions.json"))
            .await
            .unwrap();
        assert!(!r.not_modified);
        let r = f
            .get(Req::new("https://geminicli.com/extensions.json"))
            .await
            .unwrap();
        assert!(r.not_modified, "304 from the matching ETag");
        assert_eq!(&r.body[..], b"[1]");
        assert_eq!(fake.requests().len(), 2);
        // Fresh max-age: no request at all.
        fake.serve_with(
            "https://www.aitmpl.com/components.json",
            200,
            vec![("cache-control", "max-age=300")],
            "{}",
        );
        f.get(Req::new("https://www.aitmpl.com/components.json"))
            .await
            .unwrap();
        let r = f
            .get(Req::new("https://www.aitmpl.com/components.json"))
            .await
            .unwrap();
        assert!(r.not_modified);
        assert_eq!(fake.requests().len(), 3);
        // Offline: the stale body answers.
        let r = f
            .get(Req::new("https://geminicli.com/extensions.json"))
            .await
            .unwrap();
        assert!(r.not_modified);
        let fake2 = FakeFetcher::new();
        let f2 = HttpFetcher::with_transport(fake2, Some(ix.cache()));
        let r = f2
            .get(Req::new("https://geminicli.com/extensions.json"))
            .await
            .unwrap();
        assert!(r.header("x-kls-stale").is_some());
    }

    #[test]
    fn fts_expr() {
        assert_eq!(fts_query("code review").unwrap(), "\"code\" \"review\"*");
        assert!(fts_query("  -- ").is_none());
        assert_eq!(fts_query("a\"b").unwrap(), "\"a\" \"b\"*");
    }
}
