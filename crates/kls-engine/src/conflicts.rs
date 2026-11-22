//! Conflict-fix automation (vdb-site `factory_engine_conflicts.go`,
//! `factory_checkpoints_conflicts.go`): when the GitHub plane reports an
//! open pull request turned conflicted (`github.conflict.detected`), file
//! one `merge-conflict` card per pull request for `belai:conflict-resolver`.
//! A fork's card is held for a person (`checkpoint:fork_code`).
//!
//! Gated like the `conflict-fix` default: `defaults.enabled`,
//! `defaults.conflict-fix.enabled`, the repo's `defaults_off`, and autonomy
//! `draft_pr`; refused under the kill switch.
//!
//! A workflow can file the same cards with the `conflict_cards` output
//! (`github` step): the run's pull request, or up to [`MAX_PULLS`] open
//! pull requests after a push, rechecking unknown mergeability at
//! [`RECHECK_MINUTES`] (the executor does the waiting).

use crate::guard::{self, Origin};
use anyhow::Result;
use kls_core::Core;
use kls_core::board::CardDraft;
use serde_json::{Value, json};

pub const EVENT: &str = "github.conflict.detected";
pub const LABEL: &str = "merge-conflict";
pub const FORK_HOLD: &str = "checkpoint:fork_code";
pub const AGENT: &str = "belai:conflict-resolver";
pub const DEFAULT_KEY: &str = "conflict-fix";

fn s<'a>(d: &'a Value, k: &str) -> &'a str {
    d.get(k).and_then(Value::as_str).unwrap_or("")
}

/// The card body: a fenced block the resolver reads.
pub fn card_body(d: &Value) -> String {
    format!(
        "Resolve the merge conflicts of this pull request so it merges cleanly into its base. \
         Keep both sides' intent; the result must build, lint and pass the tests.\n\n```conflict\n\
         repo: {}\npull: {}\nurl: {}\nhead: {}\nhead_sha: {}\nbase: {}\nfork: {}\n```\n",
        s(d, "repo"),
        d.get("number").and_then(Value::as_i64).unwrap_or(0),
        s(d, "url"),
        s(d, "head"),
        s(d, "headSha"),
        s(d, "base"),
        d.get("fork").and_then(Value::as_bool).unwrap_or(false),
    )
}

/// Files the card for a conflict event. `Ok(None)` when gated off or a card
/// for the pull request is already open.
pub async fn on_conflict(core: &Core, data: &Value) -> Result<Option<String>> {
    let repo = s(data, "repo").to_string();
    let number = data.get("number").and_then(Value::as_i64).unwrap_or(0);
    if repo.is_empty() || number <= 0 {
        return Ok(None);
    }
    let target = format!("{repo}#{number}");
    if guard::blocked(core, "trigger", &target, "rule:conflict-fix")
        .await
        .is_some()
    {
        return Ok(None);
    }
    let eff = kls_core::audit::config::Effective::load(core).await?;
    let gh = core.store().gh_repo(&repo).await?;
    let opted_out = gh
        .as_ref()
        .is_some_and(|r| r.settings.defaults_off.iter().any(|k| k == DEFAULT_KEY));
    let autonomy = guard::effective_autonomy(core, &repo).await;
    let origin = Origin::engine(json!({ "rule": DEFAULT_KEY, "repo": repo, "number": number }));
    let skip = if !eff.defaults_enabled {
        Some("defaults_off")
    } else if !crate::defaults::key_enabled(core, DEFAULT_KEY).await {
        Some("default_disabled")
    } else if opted_out {
        Some("repo_opted_out")
    } else if guard::autonomy_rank(&autonomy) < guard::autonomy_rank("draft_pr") {
        Some("autonomy_too_low")
    } else {
        None
    };
    if let Some(why) = skip {
        guard::audit(
            core,
            "card",
            "conflict.card",
            json!({ "pull": target }),
            &origin,
            &format!("skipped: {why}"),
            json!({}),
        )
        .await?;
        return Ok(None);
    }
    file_card(core, data, &origin).await
}

/// When a workflow's `conflict_cards` output looks at a pull request whose
/// mergeability is still unknown: now, then 1, 3 and 10 minutes after.
pub const RECHECK_MINUTES: [i64; 4] = [0, 1, 3, 10];
/// A push looks at up to this many open pull requests.
pub const MAX_PULLS: usize = 20;

/// The event data [`on_conflict`] reads, from a mirrored pull request.
pub fn pull_data(p: &kls_store::github::GhPull) -> Value {
    json!({
        "repo": p.repo, "number": p.number, "url": p.html_url, "head": p.head_ref,
        "headSha": p.head_sha, "base": p.base_ref, "fork": p.is_fork(), "author": p.author,
    })
}

/// Files the merge-conflict card for `data` (one open card per pull
/// request), audited with `origin`. `Ok(None)` when one is already open.
/// The caller has checked the kill switch and the gates.
pub async fn file_card(core: &Core, data: &Value, origin: &Origin) -> Result<Option<String>> {
    let repo = s(data, "repo").to_string();
    let url = s(data, "url").to_string();
    let number = data.get("number").and_then(Value::as_i64).unwrap_or(0);
    let target = format!("{repo}#{number}");
    let open = core.cards().await?.into_iter().any(|c| {
        c.status != "done"
            && c.labels.iter().any(|l| l == LABEL)
            && (c.pr_url == url || c.title.contains(&target))
    });
    if open {
        return Ok(None);
    }
    let fork = data.get("fork").and_then(Value::as_bool).unwrap_or(false);
    let mut labels = vec![LABEL.to_string()];
    if fork {
        labels.push(FORK_HOLD.to_string());
    }
    let meta = json!({
        "belai": { "list": "backlog", "agent": { "labels": labels, "branch": s(data, "head"), "pr": url } },
        "keleustes": { "origin": "conflict", "agent": AGENT, "event": EVENT },
    });
    let card = core
        .create_card(
            CardDraft {
                title: format!("Resolve merge conflicts in {target}"),
                body: card_body(data),
                status: "backlog".into(),
                labels: labels.clone(),
                repo: repo.clone(),
                branch: s(data, "head").into(),
                pr_url: url,
                meta_json: meta.to_string(),
                ..Default::default()
            },
            "engine",
        )
        .await?;
    guard::audit(
        core,
        "card",
        "conflict.card",
        json!({ "cardId": card.id, "key": card.key, "pull": target }),
        origin,
        if fork {
            "filed, held for fork code"
        } else {
            "filed"
        },
        json!({ "labels": labels }),
    )
    .await?;
    Ok(Some(card.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Core::open(dir.path(), None).await.unwrap(), dir)
    }

    fn ev(n: i64, fork: bool) -> Value {
        json!({"repo":"acme/api","number":n,"url":format!("https://github.com/acme/api/pull/{n}"),
               "head":"feature/x","headSha":"abc","base":"main","fork":fork})
    }

    async fn enable(core: &Core) {
        let doc = core.settings_doc().await.unwrap();
        let mut ch = std::collections::BTreeMap::new();
        ch.insert("defaults.enabled".to_string(), json!(true));
        ch.insert("autonomy.default".to_string(), json!("draft_pr"));
        core.settings_save(doc.revision, ch).await.unwrap();
    }

    #[tokio::test]
    async fn one_card_per_pull_and_fork_hold() {
        let (core, _d) = core().await;
        enable(&core).await;
        let id = on_conflict(&core, &ev(7, false)).await.unwrap().unwrap();
        assert!(
            on_conflict(&core, &ev(7, false)).await.unwrap().is_none(),
            "one open card per pull"
        );
        let c = core.card(&id).await.unwrap().unwrap();
        assert_eq!(c.labels, [LABEL]);
        assert!(c.body.contains("```conflict") && c.body.contains("pull: 7"));
        let f = on_conflict(&core, &ev(8, true)).await.unwrap().unwrap();
        let f = core.card(&f).await.unwrap().unwrap();
        assert!(kls_core::board::cards::is_held(&f));
        assert!(
            crate::guard::audited(&core, "card")
                .await
                .iter()
                .any(|e| e.subject["action"] == "conflict.card")
        );
    }

    #[tokio::test]
    async fn gated_and_refused() {
        let (core, _d) = core().await;
        // Defaults off by default: audited skip, no card.
        assert!(on_conflict(&core, &ev(1, false)).await.unwrap().is_none());
        assert!(core.cards().await.unwrap().is_empty());
        enable(&core).await;
        core.set_factory_stopped(true, "").await.unwrap();
        assert!(on_conflict(&core, &ev(1, false)).await.unwrap().is_none());
        assert!(core.cards().await.unwrap().is_empty());
    }
}
