//! Library search persistence (migration 0012): provenance of imported
//! versions, update checks, hook bundle files and blobs, and the search
//! sources a person added or toggled.

use crate::{Store, now_ms};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::any::AnyRow;

/// Where one imported version came from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    /// Library kind, or `agent`.
    pub item_kind: String,
    pub item_id: String,
    pub version: i64,
    pub source: String,
    pub ecosystem: String,
    /// JSON `Locator`.
    pub locator: String,
    pub upstream_commit: String,
    pub upstream_version: String,
    pub source_urls_json: String,
    pub upstream_sha256: String,
    pub doc_sha256: String,
    pub files_json: String,
    pub licence: String,
    pub notes_json: String,
    pub flags_json: String,
    pub ack_json: String,
    pub actor: String,
    pub converted_by: String,
    pub audit_seq: i64,
    pub reviewed_at: i64,
}

impl Provenance {
    /// The URLs fetched for this version.
    pub fn urls(&self) -> Vec<String> {
        serde_json::from_str(&self.source_urls_json).unwrap_or_default()
    }

    /// `abc1234` of the commit, or the version.
    pub fn short_ref(&self) -> String {
        if !self.upstream_commit.is_empty() {
            self.upstream_commit.chars().take(7).collect()
        } else {
            self.upstream_version.clone()
        }
    }
}

/// The last update check of an imported item.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateState {
    pub item_kind: String,
    pub item_id: String,
    pub latest_commit: String,
    pub latest_version: String,
    pub latest_sha256: String,
    pub checked_at: i64,
    /// `current`, `behind`, `gone` or `error`.
    pub state: String,
}

/// One file of a hook bundle version.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BundleEntry {
    pub item_id: String,
    pub version: i64,
    pub path: String,
    pub sha256: String,
    pub size_bytes: i64,
}

/// A search source row (a tap, a well-known site, or a toggled built-in).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceRow {
    pub id: String,
    /// `builtin`, `tap`, `well_known` or `setting`.
    pub kind: String,
    pub spec_json: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

const PROV_COLS: &str = "item_kind, item_id, version, source, ecosystem, locator, upstream_commit, \
     upstream_version, source_urls_json, upstream_sha256, doc_sha256, files_json, licence, \
     notes_json, flags_json, ack_json, actor, converted_by, audit_seq, reviewed_at";

fn prov(r: &AnyRow) -> Result<Provenance> {
    Ok(Provenance {
        item_kind: r.try_get("item_kind")?,
        item_id: r.try_get("item_id")?,
        version: r.try_get("version")?,
        source: r.try_get("source")?,
        ecosystem: r.try_get("ecosystem")?,
        locator: r.try_get("locator")?,
        upstream_commit: r.try_get("upstream_commit")?,
        upstream_version: r.try_get("upstream_version")?,
        source_urls_json: r.try_get("source_urls_json")?,
        upstream_sha256: r.try_get("upstream_sha256")?,
        doc_sha256: r.try_get("doc_sha256")?,
        files_json: r.try_get("files_json")?,
        licence: r.try_get("licence")?,
        notes_json: r.try_get("notes_json")?,
        flags_json: r.try_get("flags_json")?,
        ack_json: r.try_get("ack_json")?,
        actor: r.try_get("actor")?,
        converted_by: r.try_get("converted_by")?,
        audit_seq: r.try_get("audit_seq")?,
        reviewed_at: r.try_get("reviewed_at")?,
    })
}

fn update(r: &AnyRow) -> Result<UpdateState> {
    Ok(UpdateState {
        item_kind: r.try_get("item_kind")?,
        item_id: r.try_get("item_id")?,
        latest_commit: r.try_get("latest_commit")?,
        latest_version: r.try_get("latest_version")?,
        latest_sha256: r.try_get("latest_sha256")?,
        checked_at: r.try_get("checked_at")?,
        state: r.try_get("state")?,
    })
}

fn source_row(r: &AnyRow) -> Result<SourceRow> {
    Ok(SourceRow {
        id: r.try_get("id")?,
        kind: r.try_get("kind")?,
        spec_json: r.try_get("spec_json")?,
        enabled: r.try_get::<i64, _>("enabled")? != 0,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

impl Store {
    // ---- provenance ---------------------------------------------------------

    /// Records one imported version (replacing a row for the same version).
    pub async fn save_provenance(&self, p: &Provenance) -> Result<()> {
        let q = format!(
            "INSERT INTO library_provenance ({PROV_COLS}) VALUES \
             (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (item_kind, item_id, version) DO UPDATE SET source = excluded.source, \
             ecosystem = excluded.ecosystem, locator = excluded.locator, \
             upstream_commit = excluded.upstream_commit, upstream_version = excluded.upstream_version, \
             source_urls_json = excluded.source_urls_json, upstream_sha256 = excluded.upstream_sha256, \
             doc_sha256 = excluded.doc_sha256, files_json = excluded.files_json, \
             licence = excluded.licence, notes_json = excluded.notes_json, \
             flags_json = excluded.flags_json, ack_json = excluded.ack_json, actor = excluded.actor, \
             converted_by = excluded.converted_by, audit_seq = excluded.audit_seq, \
             reviewed_at = excluded.reviewed_at"
        );
        sqlx::query(self.sql(&q))
            .bind(&p.item_kind)
            .bind(&p.item_id)
            .bind(p.version)
            .bind(&p.source)
            .bind(&p.ecosystem)
            .bind(&p.locator)
            .bind(&p.upstream_commit)
            .bind(&p.upstream_version)
            .bind(&p.source_urls_json)
            .bind(&p.upstream_sha256)
            .bind(&p.doc_sha256)
            .bind(&p.files_json)
            .bind(&p.licence)
            .bind(&p.notes_json)
            .bind(&p.flags_json)
            .bind(&p.ack_json)
            .bind(&p.actor)
            .bind(&p.converted_by)
            .bind(p.audit_seq)
            .bind(p.reviewed_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Every recorded version of an item, newest first.
    pub async fn provenance_of(&self, item_kind: &str, item_id: &str) -> Result<Vec<Provenance>> {
        let q = format!(
            "SELECT {PROV_COLS} FROM library_provenance WHERE item_kind = ? AND item_id = ? \
             ORDER BY version DESC"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(item_kind)
            .bind(item_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(prov).collect()
    }

    /// The newest provenance row of an item, if it was imported.
    pub async fn latest_provenance(
        &self,
        item_kind: &str,
        item_id: &str,
    ) -> Result<Option<Provenance>> {
        Ok(self
            .provenance_of(item_kind, item_id)
            .await?
            .into_iter()
            .next())
    }

    /// The newest provenance row per imported item.
    pub async fn provenance_latest_all(&self) -> Result<Vec<Provenance>> {
        let q = format!(
            "SELECT {PROV_COLS} FROM library_provenance p WHERE version = \
             (SELECT MAX(version) FROM library_provenance q \
              WHERE q.item_kind = p.item_kind AND q.item_id = p.item_id) \
             ORDER BY item_kind, item_id"
        );
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(prov).collect()
    }

    pub async fn delete_provenance(&self, item_kind: &str, item_id: &str) -> Result<u64> {
        let r = sqlx::query(
            self.sql("DELETE FROM library_provenance WHERE item_kind = ? AND item_id = ?"),
        )
        .bind(item_kind)
        .bind(item_id)
        .execute(&self.pool)
        .await?;
        sqlx::query(self.sql("DELETE FROM library_updates WHERE item_kind = ? AND item_id = ?"))
            .bind(item_kind)
            .bind(item_id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    // ---- updates --------------------------------------------------------------

    pub async fn save_update_state(&self, u: &UpdateState) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO library_updates (item_kind, item_id, latest_commit, latest_version, \
             latest_sha256, checked_at, state) VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (item_kind, item_id) DO UPDATE SET latest_commit = excluded.latest_commit, \
             latest_version = excluded.latest_version, latest_sha256 = excluded.latest_sha256, \
             checked_at = excluded.checked_at, state = excluded.state",
        ))
        .bind(&u.item_kind)
        .bind(&u.item_id)
        .bind(&u.latest_commit)
        .bind(&u.latest_version)
        .bind(&u.latest_sha256)
        .bind(u.checked_at)
        .bind(&u.state)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_state(
        &self,
        item_kind: &str,
        item_id: &str,
    ) -> Result<Option<UpdateState>> {
        let row = sqlx::query(self.sql(
            "SELECT item_kind, item_id, latest_commit, latest_version, latest_sha256, checked_at, \
             state FROM library_updates WHERE item_kind = ? AND item_id = ?",
        ))
        .bind(item_kind)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(update).transpose()
    }

    pub async fn update_states(&self) -> Result<Vec<UpdateState>> {
        let rows = sqlx::query(self.sql(
            "SELECT item_kind, item_id, latest_commit, latest_version, latest_sha256, checked_at, \
             state FROM library_updates ORDER BY item_kind, item_id",
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(update).collect()
    }

    // ---- bundle files -----------------------------------------------------------

    /// Stores a bundle version's files: blobs once by hash, the manifest
    /// per version. All or nothing.
    pub async fn save_bundle_files(
        &self,
        item_id: &str,
        version: i64,
        files: &[(String, Vec<u8>)],
    ) -> Result<Vec<BundleEntry>> {
        use sha2::{Digest, Sha256};
        let mut tx = self.begin_write().await?;
        let mut out = Vec::with_capacity(files.len());
        sqlx::query(self.sql("DELETE FROM library_files WHERE item_id = ? AND version = ?"))
            .bind(item_id)
            .bind(version)
            .execute(&mut *tx)
            .await?;
        for (path, data) in files {
            let sha = hex::encode(Sha256::digest(data));
            sqlx::query(self.sql(
                "INSERT INTO library_blobs (sha256, size_bytes, data) VALUES (?, ?, ?) \
                 ON CONFLICT (sha256) DO NOTHING",
            ))
            .bind(&sha)
            .bind(data.len() as i64)
            .bind(data.clone())
            .execute(&mut *tx)
            .await?;
            sqlx::query(self.sql(
                "INSERT INTO library_files (item_id, version, path, sha256, size_bytes) \
                 VALUES (?, ?, ?, ?, ?)",
            ))
            .bind(item_id)
            .bind(version)
            .bind(path)
            .bind(&sha)
            .bind(data.len() as i64)
            .execute(&mut *tx)
            .await?;
            out.push(BundleEntry {
                item_id: item_id.to_string(),
                version,
                path: path.clone(),
                sha256: sha,
                size_bytes: data.len() as i64,
            });
        }
        tx.commit().await?;
        Ok(out)
    }

    /// The manifest of a bundle version, by path.
    pub async fn bundle_files(&self, item_id: &str, version: i64) -> Result<Vec<BundleEntry>> {
        let rows = sqlx::query(self.sql(
            "SELECT item_id, version, path, sha256, size_bytes FROM library_files \
             WHERE item_id = ? AND version = ? ORDER BY path",
        ))
        .bind(item_id)
        .bind(version)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(BundleEntry {
                    item_id: r.try_get("item_id")?,
                    version: r.try_get("version")?,
                    path: r.try_get("path")?,
                    sha256: r.try_get("sha256")?,
                    size_bytes: r.try_get("size_bytes")?,
                })
            })
            .collect()
    }

    pub async fn library_blob(&self, sha256: &str) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query(self.sql("SELECT data FROM library_blobs WHERE sha256 = ?"))
            .bind(sha256)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<Vec<u8>, _>("data")))
    }

    /// Removes an item's manifests, and blobs no manifest uses any more.
    pub async fn delete_bundle_files(&self, item_id: &str) -> Result<()> {
        let mut tx = self.begin_write().await?;
        sqlx::query(self.sql("DELETE FROM library_files WHERE item_id = ?"))
            .bind(item_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(self.sql(
            "DELETE FROM library_blobs WHERE sha256 NOT IN (SELECT sha256 FROM library_files)",
        ))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    // ---- sources ------------------------------------------------------------------

    pub async fn library_sources(&self) -> Result<Vec<SourceRow>> {
        let rows = sqlx::query(self.sql(
            "SELECT id, kind, spec_json, enabled, created_at, updated_at FROM library_sources \
             ORDER BY created_at, id",
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(source_row).collect()
    }

    /// Adds or updates a source row (enabled, spec).
    pub async fn save_library_source(
        &self,
        id: &str,
        kind: &str,
        spec_json: &str,
        enabled: bool,
    ) -> Result<()> {
        let now = now_ms();
        sqlx::query(self.sql(
            "INSERT INTO library_sources (id, kind, spec_json, enabled, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (id) DO UPDATE SET kind = excluded.kind, \
             spec_json = excluded.spec_json, enabled = excluded.enabled, \
             updated_at = excluded.updated_at",
        ))
        .bind(id)
        .bind(kind)
        .bind(spec_json)
        .bind(enabled as i64)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_library_source(&self, id: &str) -> Result<bool> {
        let r = sqlx::query(self.sql("DELETE FROM library_sources WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::AssertSqlSafe;

    async fn stores() -> Vec<(Store, Option<tempfile::TempDir>)> {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_sqlite(&dir.path().join("t.db")).await.unwrap();
        let mut out = vec![(s, Some(dir))];
        if let Ok(url) = std::env::var("KLS_TEST_POSTGRES_URL") {
            let s = Store::open(&url).await.unwrap();
            for t in [
                "library_provenance",
                "library_updates",
                "library_files",
                "library_blobs",
                "library_sources",
            ] {
                sqlx::query(AssertSqlSafe(format!("DELETE FROM {t}")))
                    .execute(&s.pool)
                    .await
                    .unwrap();
            }
            out.push((s, None));
        }
        out
    }

    fn p(version: i64) -> Provenance {
        Provenance {
            item_kind: "skill".into(),
            item_id: "i1".into(),
            version,
            source: "gh:anthropics/skills".into(),
            ecosystem: "claude-code".into(),
            locator: "{}".into(),
            upstream_commit: "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2".into(),
            source_urls_json: "[\"https://codeload.github.com/o/r/tar.gz/a1b2\"]".into(),
            upstream_sha256: "u".into(),
            doc_sha256: "d".into(),
            actor: "user".into(),
            converted_by: "kls".into(),
            audit_seq: 7,
            reviewed_at: 1,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn provenance_crud() {
        for (s, _d) in stores().await {
            s.save_provenance(&p(1)).await.unwrap();
            s.save_provenance(&p(2)).await.unwrap();
            s.save_provenance(&p(2)).await.unwrap();
            let all = s.provenance_of("skill", "i1").await.unwrap();
            assert_eq!(all.len(), 2);
            assert_eq!(all[0].version, 2);
            assert_eq!(all[0].short_ref(), "a1b2c3d");
            assert_eq!(all[0].urls().len(), 1);
            let latest = s.provenance_latest_all().await.unwrap();
            assert_eq!(latest.len(), 1);
            assert_eq!(latest[0].version, 2);
            assert_eq!(
                s.latest_provenance("skill", "i1").await.unwrap().unwrap(),
                all[0]
            );
            let u = UpdateState {
                item_kind: "skill".into(),
                item_id: "i1".into(),
                latest_commit: "b7e".into(),
                checked_at: 5,
                state: "behind".into(),
                ..Default::default()
            };
            s.save_update_state(&u).await.unwrap();
            assert_eq!(s.update_state("skill", "i1").await.unwrap().unwrap(), u);
            assert_eq!(s.update_states().await.unwrap().len(), 1);
            assert_eq!(s.delete_provenance("skill", "i1").await.unwrap(), 2);
            assert!(s.update_state("skill", "i1").await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn bundle_files_and_blobs() {
        for (s, _d) in stores().await {
            let files = vec![
                ("guard.sh".to_string(), b"#!/bin/sh\necho ok\n".to_vec()),
                ("lib/x.py".to_string(), b"print(1)\n".to_vec()),
            ];
            let m = s.save_bundle_files("h1", 1, &files).await.unwrap();
            assert_eq!(m.len(), 2);
            s.save_bundle_files("h1", 2, &files[..1]).await.unwrap();
            let got = s.bundle_files("h1", 1).await.unwrap();
            assert_eq!(got, m);
            assert_eq!(s.bundle_files("h1", 2).await.unwrap().len(), 1);
            assert_eq!(
                s.library_blob(&m[0].sha256).await.unwrap().unwrap(),
                files[0].1
            );
            s.delete_bundle_files("h1").await.unwrap();
            assert!(s.bundle_files("h1", 1).await.unwrap().is_empty());
            assert!(s.library_blob(&m[0].sha256).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn sources_crud() {
        for (s, _d) in stores().await {
            s.save_library_source("gh:o/r", "tap", "{\"spec\":\"o/r\"}", true)
                .await
                .unwrap();
            s.save_library_source("hermes-index", "builtin", "{}", true)
                .await
                .unwrap();
            s.save_library_source("gh:o/r", "tap", "{\"spec\":\"o/r@v1\"}", false)
                .await
                .unwrap();
            let all = s.library_sources().await.unwrap();
            assert_eq!(all.len(), 2);
            let t = all.iter().find(|r| r.id == "gh:o/r").unwrap();
            assert!(!t.enabled);
            assert!(t.spec_json.contains("v1"));
            assert!(s.delete_library_source("gh:o/r").await.unwrap());
            assert_eq!(s.library_sources().await.unwrap().len(), 1);
        }
    }
}
