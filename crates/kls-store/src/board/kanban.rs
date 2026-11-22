//! Store: the external kanban binding's links and last pass (agent A,
//! migration 0243). Logic lives in `kls-core` (`board::kanban`).

use crate::*;

/// One board card mirrored to one item on the bound outside board.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KanbanLink {
    pub provider: String,
    pub board: String,
    pub card_id: String,
    pub remote_id: String,
    pub remote_url: String,
    /// The last agreed value per field, JSON `{title, body, column}`.
    pub base_json: String,
    /// Open conflict fields, JSON; "" when none.
    pub conflict_json: String,
    /// `linked` or `gone`.
    pub state: String,
    pub source_sha256: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// The last sync pass of one binding.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KanbanRun {
    pub provider: String,
    pub board: String,
    pub at: i64,
    pub outcome: String,
    pub report_json: String,
    pub last_ok_at: i64,
}

const LINK_COLS: &str = "provider, board, card_id, remote_id, remote_url, base_json, conflict_json, state, source_sha256, created_at, updated_at";

fn link_from_row(r: &AnyRow) -> Result<KanbanLink> {
    Ok(KanbanLink {
        provider: r.try_get("provider")?,
        board: r.try_get("board")?,
        card_id: r.try_get("card_id")?,
        remote_id: r.try_get("remote_id")?,
        remote_url: r.try_get("remote_url")?,
        base_json: r.try_get("base_json")?,
        conflict_json: r.try_get("conflict_json")?,
        state: r.try_get("state")?,
        source_sha256: r.try_get("source_sha256")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

impl Store {
    /// Every link of one binding.
    pub async fn kanban_links(&self, provider: &str, board: &str) -> Result<Vec<KanbanLink>> {
        let q = format!(
            "SELECT {LINK_COLS} FROM kanban_links WHERE provider = ? AND board = ? ORDER BY created_at, card_id"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(provider)
            .bind(board)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(link_from_row).collect()
    }

    /// The links of one card, any binding.
    pub async fn kanban_links_of_card(&self, card_id: &str) -> Result<Vec<KanbanLink>> {
        let q = format!(
            "SELECT {LINK_COLS} FROM kanban_links WHERE card_id = ? ORDER BY provider, board"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(card_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(link_from_row).collect()
    }

    /// Inserts or replaces a link (keyed by provider, board and card).
    /// `created_at` is kept on replace; `updated_at` is now.
    pub async fn kanban_link_put(&self, l: &KanbanLink) -> Result<()> {
        let now = now_ms();
        let q = format!(
            "INSERT INTO kanban_links ({LINK_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (provider, board, card_id) DO UPDATE SET remote_id = excluded.remote_id, \
             remote_url = excluded.remote_url, base_json = excluded.base_json, \
             conflict_json = excluded.conflict_json, state = excluded.state, \
             source_sha256 = excluded.source_sha256, updated_at = excluded.updated_at"
        );
        sqlx::query(self.sql(&q))
            .bind(&l.provider)
            .bind(&l.board)
            .bind(&l.card_id)
            .bind(&l.remote_id)
            .bind(&l.remote_url)
            .bind(if l.base_json.is_empty() {
                "{}"
            } else {
                &l.base_json
            })
            .bind(&l.conflict_json)
            .bind(if l.state.is_empty() {
                "linked"
            } else {
                &l.state
            })
            .bind(&l.source_sha256)
            .bind(if l.created_at > 0 { l.created_at } else { now })
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Removes one link.
    pub async fn kanban_link_delete(
        &self,
        provider: &str,
        board: &str,
        card_id: &str,
    ) -> Result<bool> {
        let r = sqlx::query(
            self.sql("DELETE FROM kanban_links WHERE provider = ? AND board = ? AND card_id = ?"),
        )
        .bind(provider)
        .bind(board)
        .bind(card_id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Records a pass. `last_ok_at` moves only when `ok`.
    pub async fn kanban_run_put(
        &self,
        provider: &str,
        board: &str,
        outcome: &str,
        report_json: &str,
        ok: bool,
    ) -> Result<()> {
        let now = now_ms();
        sqlx::query(self.sql(
            "INSERT INTO kanban_runs (provider, board, at, outcome, report_json, last_ok_at) VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT (provider, board) DO UPDATE SET at = excluded.at, outcome = excluded.outcome, \
             report_json = excluded.report_json, \
             last_ok_at = CASE WHEN excluded.last_ok_at > 0 THEN excluded.last_ok_at ELSE kanban_runs.last_ok_at END",
        ))
        .bind(provider)
        .bind(board)
        .bind(now)
        .bind(outcome)
        .bind(report_json)
        .bind(if ok { now } else { 0 })
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The last pass of one binding.
    pub async fn kanban_run(&self, provider: &str, board: &str) -> Result<Option<KanbanRun>> {
        let row = sqlx::query(self.sql(
            "SELECT provider, board, at, outcome, report_json, last_ok_at FROM kanban_runs WHERE provider = ? AND board = ?",
        ))
        .bind(provider)
        .bind(board)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            Ok(KanbanRun {
                provider: r.try_get("provider")?,
                board: r.try_get("board")?,
                at: r.try_get("at")?,
                outcome: r.try_get("outcome")?,
                report_json: r.try_get("report_json")?,
                last_ok_at: r.try_get("last_ok_at")?,
            })
        })
        .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn links_and_runs_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_sqlite(&dir.path().join("t.db")).await.unwrap();
        let mut l = KanbanLink {
            provider: "linear".into(),
            board: "ENG".into(),
            card_id: "c1".into(),
            remote_id: "r1".into(),
            base_json: r#"{"title":"a"}"#.into(),
            ..Default::default()
        };
        s.kanban_link_put(&l).await.unwrap();
        let got = s.kanban_links("linear", "ENG").await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].state, "linked");
        let created = got[0].created_at;
        l.conflict_json = r#"{"title":1}"#.into();
        l.created_at = created;
        s.kanban_link_put(&l).await.unwrap();
        let got = s.kanban_links_of_card("c1").await.unwrap();
        assert_eq!(got[0].conflict_json, r#"{"title":1}"#);
        assert_eq!(got[0].created_at, created);
        // One remote item maps to one card per binding.
        let dup = KanbanLink {
            card_id: "c2".into(),
            ..l.clone()
        };
        assert!(s.kanban_link_put(&dup).await.is_err());
        assert!(s.kanban_link_delete("linear", "ENG", "c1").await.unwrap());
        assert!(s.kanban_links("linear", "ENG").await.unwrap().is_empty());

        s.kanban_run_put("linear", "ENG", "ok", "{}", true)
            .await
            .unwrap();
        let ok_at = s
            .kanban_run("linear", "ENG")
            .await
            .unwrap()
            .unwrap()
            .last_ok_at;
        assert!(ok_at > 0);
        s.kanban_run_put("linear", "ENG", "failed", "{}", false)
            .await
            .unwrap();
        let r = s.kanban_run("linear", "ENG").await.unwrap().unwrap();
        assert_eq!((r.outcome.as_str(), r.last_ok_at), ("failed", ok_at));
    }
}
