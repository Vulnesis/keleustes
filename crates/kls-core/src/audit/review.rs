//! The firewall / egress review strip on Trust: hosts belai refused to
//! reach, each waiting for a person to look. A sighting becomes a review
//! card on the board (with the host, nodes and counts as its origin), or is
//! dismissed. New sightings after a review bring the host back. Every
//! review is audited. Owner: WP1.

use crate::Core;
use anyhow::{Result, anyhow, bail};
use kls_store::audit::LedgerDraft;
use kls_store::board::Card;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

pub mod keys {
    /// JSON map host -> [`Review`].
    pub const REVIEWS: &str = "trust.egress_reviews";
}

pub mod kinds {
    /// An egress host was reviewed: `host`, `decision`, `cardId`.
    pub const EGRESS_REVIEWED: &str = "trust.egress_reviewed";
}

/// Label every egress review card carries.
pub const CARD_LABEL: &str = "egress-review";
/// Sightings looked at (days back).
pub const WINDOW_DAYS: i64 = 30;

/// What a person decided about a host.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    /// `card` (filed for review) or `dismissed`.
    pub decision: String,
    pub card_id: String,
    pub card_key: String,
    pub note: String,
    pub at: i64,
    /// Sightings counted when it was reviewed.
    pub count: i64,
}

/// One host on the strip.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressSighting {
    pub host: String,
    pub count: i64,
    pub nodes: Vec<String>,
    pub last_at: i64,
    pub review: Option<Review>,
    /// Waiting for a person: never reviewed, or seen again since.
    pub pending: bool,
}

async fn reviews(core: &Core) -> Result<BTreeMap<String, Review>> {
    Ok(core
        .store()
        .setting(keys::REVIEWS)
        .await?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default())
}

async fn save(core: &Core, m: &BTreeMap<String, Review>) -> Result<()> {
    core.store()
        .set_setting(keys::REVIEWS, &serde_json::to_string(m)?)
        .await
}

/// The strip: refused hosts of the last [`WINDOW_DAYS`] with their review,
/// pending first.
pub async fn egress_review(core: &Core) -> Result<Vec<EgressSighting>> {
    let since = kls_store::now_ms() - WINDOW_DAYS * 86_400_000;
    let hosts = core.egress_hosts(since).await?;
    let rv = reviews(core).await?;
    let mut out: Vec<EgressSighting> = hosts
        .into_iter()
        .map(|h| {
            let review = rv.get(&h.host).cloned();
            let pending = review
                .as_ref()
                .is_none_or(|r| h.last_at > r.at && h.count > r.count);
            EgressSighting {
                host: h.host,
                count: h.count,
                nodes: h.nodes,
                last_at: h.last_at,
                review,
                pending,
            }
        })
        .collect();
    out.sort_by(|a, b| b.pending.cmp(&a.pending).then(b.count.cmp(&a.count)));
    Ok(out)
}

async fn sighting(core: &Core, host: &str) -> Result<EgressSighting> {
    egress_review(core)
        .await?
        .into_iter()
        .find(|s| s.host == host)
        .ok_or_else(|| anyhow!("no refusal of {host} in {WINDOW_DAYS} days"))
}

async fn audit(core: &Core, host: &str, decision: &str, data: serde_json::Value) -> Result<()> {
    core.audit_record(LedgerDraft {
        at: 0,
        kind: "trust".into(),
        actor: "owner".into(),
        actor_kind: "user".into(),
        msg: format!("egress.review {host} {decision}"),
        subject: json!({ "action": "egress.review", "host": host }),
        payload: json!({ "outcome": decision, "data": data }),
        origin: "factory".into(),
        node_id: String::new(),
        session_id: String::new(),
        ref_key: None,
    })
    .await?;
    core.emit(
        kinds::EGRESS_REVIEWED,
        host,
        &json!({ "host": host, "decision": decision, "data": data, "by": "owner" }),
    )
    .await?;
    Ok(())
}

/// Files a review card for a refused host. The card's origin is the
/// sighting (host, nodes, count, last seen).
pub async fn file_card(core: &Core, host: &str) -> Result<Card> {
    let s = sighting(core, host).await?;
    if let Some(r) = s
        .review
        .as_ref()
        .filter(|r| r.decision == "card" && !s.pending)
    {
        bail!("{host} is already under review as {}", r.card_key);
    }
    let origin = json!({
        "source": "trust.egress", "host": s.host, "nodes": s.nodes,
        "count": s.count, "lastAt": s.last_at, "by": "owner",
    });
    let body = format!(
        "belai refused egress to `{}` {} time(s) in {} days, on {}.\n\nAllow it in the host's egress policy, or leave it refused and close this card.",
        s.host,
        s.count,
        WINDOW_DAYS,
        s.nodes.join(", ")
    );
    let card = core
        .create_card(
            crate::board::CardDraft {
                title: format!("Review egress: {}", s.host),
                body,
                status: "triage".into(),
                labels: vec![CARD_LABEL.into(), "security".into()],
                meta_json: json!({ "origin": origin }).to_string(),
                ..Default::default()
            },
            "owner",
        )
        .await?;
    let mut rv = reviews(core).await?;
    rv.insert(
        s.host.clone(),
        Review {
            decision: "card".into(),
            card_id: card.id.clone(),
            card_key: card.key.clone(),
            at: kls_store::now_ms(),
            count: s.count,
            ..Default::default()
        },
    );
    save(core, &rv).await?;
    audit(
        core,
        &s.host,
        "card",
        json!({ "cardId": card.id, "cardKey": card.key, "count": s.count, "nodes": s.nodes }),
    )
    .await?;
    Ok(card)
}

/// Dismisses a host until it is refused again.
pub async fn dismiss(core: &Core, host: &str, note: &str) -> Result<()> {
    let s = sighting(core, host).await?;
    let note: String = note.trim().chars().take(300).collect();
    let mut rv = reviews(core).await?;
    rv.insert(
        s.host.clone(),
        Review {
            decision: "dismissed".into(),
            note: note.clone(),
            at: kls_store::now_ms(),
            count: s.count,
            ..Default::default()
        },
    );
    save(core, &rv).await?;
    audit(
        core,
        &s.host,
        "dismissed",
        json!({ "note": note, "count": s.count }),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use kls_store::sessions::{AuditLine, Session};

    async fn seen(c: &Core, idx: i64, host: &str) {
        let s = Session {
            id: "s1".into(),
            node_id: "n1".into(),
            ..Default::default()
        };
        let _ = c.store().upsert_session(&s).await;
        c.store()
            .append_audit_lines(
                "n1",
                "s1",
                &[AuditLine {
                    idx,
                    at: kls_store::now_ms(),
                    kind: "egress.denied".into(),
                    json: json!({ "host": host }).to_string(),
                    ..Default::default()
                }],
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn sightings_become_cards_or_are_dismissed_and_return_when_seen_again() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        seen(&c, 1, "evil.example").await;
        seen(&c, 2, "evil.example").await;
        seen(&c, 3, "pypi.org").await;
        let strip = egress_review(&c).await.unwrap();
        assert_eq!(strip.len(), 2);
        assert!(strip.iter().all(|s| s.pending));
        let card = file_card(&c, "evil.example").await.unwrap();
        assert!(card.labels.contains(&CARD_LABEL.to_string()));
        assert!(card.meta_json.contains("trust.egress"));
        assert!(
            file_card(&c, "evil.example").await.is_err(),
            "already filed"
        );
        dismiss(&c, "pypi.org", "mirror is fine").await.unwrap();
        let strip = egress_review(&c).await.unwrap();
        assert!(strip.iter().all(|s| !s.pending));
        // Seen again later: back on the strip.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        seen(&c, 4, "pypi.org").await;
        let strip = egress_review(&c).await.unwrap();
        assert!(strip.iter().find(|s| s.host == "pypi.org").unwrap().pending);
        assert!(file_card(&c, "nowhere.example").await.is_err());
        let n = c
            .audit_entries(&kls_store::audit::LedgerQuery {
                kinds: vec!["trust".into()],
                ..Default::default()
            })
            .await
            .unwrap()
            .len();
        assert_eq!(n, 2);
    }
}
