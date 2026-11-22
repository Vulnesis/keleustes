//! Attention requests (WP-J): a host with nobody at it asks the owner for
//! something from a closed list (a gh sign-in, provider credentials, an
//! approval). Identifiers only, so a notification never carries model or
//! tool text. The owner's decision goes back to the host as a replayable
//! `AttentionDecided` frame (the node server sends it on `attention.decided`).

use crate::Core;
use anyhow::{Result, bail};
use kls_store::audit::LedgerDraft;
use serde_json::json;

pub use kls_store::belai::Attention;

pub mod kinds {
    /// Subject: "<nodeId>:<id>". Data: `{kind, reason, subject}`.
    pub const ATTENTION_REQUESTED: &str = "attention.requested";
    /// Subject: "<nodeId>:<id>". Data: `{nodeId, id, state}`.
    pub const ATTENTION_DECIDED: &str = "attention.decided";
}

pub const KINDS: [&str; 4] = ["gh_login", "provider_credentials", "approve", "action"];
pub const REASONS: [&str; 4] = [
    "credential_expiring",
    "reauth_needed",
    "approval_needed",
    "action_needed",
];
/// An open request lapses after this long.
pub const LAPSE_MS: i64 = 24 * 60 * 60 * 1000;

fn ident(s: &str, n: usize) -> bool {
    !s.is_empty()
        && s.len() <= n
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '@'))
}

impl Core {
    /// A host files a request. Unknown kinds or reasons, and a subject that
    /// is not an identifier, are refused. A repeat of the same id is fine.
    pub async fn attention_file(
        &self,
        node_id: &str,
        id: &str,
        kind: &str,
        reason: &str,
        subject: &str,
    ) -> Result<()> {
        if !ident(id, 64) {
            bail!("the request id is not an identifier");
        }
        if !KINDS.contains(&kind) || !REASONS.contains(&reason) {
            bail!("unknown attention kind or reason");
        }
        if !subject.is_empty() && !ident(subject, 128) {
            bail!("the subject is not an identifier");
        }
        let a = Attention {
            node_id: node_id.into(),
            id: id.into(),
            kind: kind.into(),
            reason: reason.into(),
            subject: subject.into(),
            state: "open".into(),
            created_at: kls_store::now_ms(),
            decided_at: None,
        };
        if !self.store().insert_attention(&a).await? {
            return Ok(());
        }
        let subj = format!("{node_id}:{id}");
        self.emit(
            kinds::ATTENTION_REQUESTED,
            &subj,
            &json!({ "nodeId": node_id, "kind": kind, "reason": reason, "subject": subject }),
        )
        .await?;
        self.audit_record(LedgerDraft {
            kind: "attention".into(),
            actor: format!("node:{node_id}"),
            actor_kind: "host".into(),
            msg: format!("host asks: {kind} ({reason})"),
            subject: json!({ "request": id, "subject": subject }),
            payload: json!({ "kind": kind, "reason": reason, "outcome": "open" }),
            origin: format!("host:{node_id}"),
            node_id: node_id.into(),
            ..Default::default()
        })
        .await?;
        Ok(())
    }

    /// The owner decides an open request ("approved" or "declined").
    pub async fn attention_decide(&self, node_id: &str, id: &str, approve: bool) -> Result<()> {
        let state = if approve { "approved" } else { "declined" };
        self.settle_attention(node_id, id, state, "owner").await
    }

    /// Lapses open requests older than [`LAPSE_MS`] at `now`.
    pub async fn attention_lapse(&self, now: i64) -> Result<usize> {
        let mut n = 0;
        for a in self.store().attention("open", 1000).await? {
            if now - a.created_at >= LAPSE_MS {
                self.settle_attention(&a.node_id, &a.id, "lapsed", "system")
                    .await?;
                n += 1;
            }
        }
        Ok(n)
    }

    async fn settle_attention(&self, node_id: &str, id: &str, state: &str, by: &str) -> Result<()> {
        if !self.store().decide_attention(node_id, id, state).await? {
            bail!("that request is not open");
        }
        self.emit(
            kinds::ATTENTION_DECIDED,
            &format!("{node_id}:{id}"),
            &json!({ "nodeId": node_id, "id": id, "state": state }),
        )
        .await?;
        self.audit_record(LedgerDraft {
            kind: "attention".into(),
            actor: by.into(),
            actor_kind: if by == "owner" { "user" } else { "system" }.into(),
            msg: format!("host request {state}"),
            subject: json!({ "request": id }),
            payload: json!({ "outcome": state }),
            origin: "factory".into(),
            node_id: node_id.into(),
            ..Default::default()
        })
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn file_decide_lapse() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        assert!(
            c.attention_file("n", "r1", "login", "reauth_needed", "")
                .await
                .is_err()
        );
        assert!(
            c.attention_file("n", "r1", "gh_login", "because", "")
                .await
                .is_err()
        );
        assert!(
            c.attention_file("n", "r1", "gh_login", "reauth_needed", "a b")
                .await
                .is_err()
        );
        c.attention_file("n", "r1", "gh_login", "reauth_needed", "github.com")
            .await
            .unwrap();
        c.attention_file("n", "r1", "gh_login", "reauth_needed", "github.com")
            .await
            .unwrap();
        c.attention_decide("n", "r1", true).await.unwrap();
        assert!(c.attention_decide("n", "r1", false).await.is_err());
        c.attention_file("n", "r2", "approve", "approval_needed", "")
            .await
            .unwrap();
        let now = kls_store::now_ms() + LAPSE_MS + 1;
        assert_eq!(c.attention_lapse(now).await.unwrap(), 1);
        let audited = c
            .audit_entries(&Default::default())
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "attention")
            .count();
        assert_eq!(audited, 4);
    }
}
