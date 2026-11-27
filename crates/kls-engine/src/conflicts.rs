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

// ---- hand-off from the Conflicts screen --------------------------------------

/// The node `belai:conflict-resolver` would run on for `repo`, or why it
/// cannot start now (the Conflicts screen's disabled reason). Revoked nodes
/// never qualify.
pub fn resolver_host(
    nodes: &[kls_store::Node],
    online: &[String],
    stopped: bool,
    repo: &str,
) -> std::result::Result<String, String> {
    if stopped {
        return Err("Factory stopped".into());
    }
    let live: Vec<String> = online
        .iter()
        .filter(|id| {
            nodes
                .iter()
                .find(|n| &n.id == *id)
                .is_none_or(|n| n.revoked_at.is_none())
        })
        .cloned()
        .collect();
    crate::route::pick_node(nodes, &live, AGENT, "", repo)
}

/// What [`dispatch_resolver`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct Handed {
    pub card_id: String,
    pub key: String,
    /// The card for the pull request was already open.
    pub existing: bool,
    pub node: String,
    /// The dispatch id; `None` when the card is held (fork code).
    pub dispatch: Option<String>,
}

/// "Resolve with belai:conflict-resolver": files (or reuses) the pull
/// request's `merge-conflict` card and starts the builtin worker on a node
/// that offers it, so it claims the card. Refused, and audited, under the
/// kill switch or a maintenance window and when no node qualifies; a fork's
/// card is filed held for a person and nothing is dispatched. Every outcome
/// is audited (`conflict_resolver.dispatch`) with the surface it came from.
pub async fn dispatch_resolver(
    core: &Core,
    set: &kls_forge::conflicts::ConflictSet,
    url: &str,
    caller: &str,
) -> Result<Handed> {
    let target = format!("{}#{}", set.repo, set.number);
    let origin =
        Origin::person(json!({ "surface": "conflicts", "caller": caller, "pull": target }));
    let note = |outcome: String, data: Value| {
        let target = target.clone();
        let origin = &origin;
        async move {
            guard::audit(
                core,
                "dispatch",
                "conflict_resolver.dispatch",
                json!({ "pull": target, "agent": AGENT }),
                origin,
                &outcome,
                data,
            )
            .await
        }
    };
    // The kill switch first (audited `factory.refused` by the check).
    if let Err(e) = kls_core::control::check(core, "dispatch", &target, "conflicts").await {
        note(
            format!("refused: {e:#}"),
            json!({ "reason": "kill_switch" }),
        )
        .await?;
        return Err(e);
    }
    if let Some(why) = guard::window(core, "dispatch").await {
        note(format!("refused: {why}"), json!({ "reason": "window" })).await?;
        anyhow::bail!("{why}");
    }
    let gw = core.gateway()?;
    let online = gw.online().await;
    let nodes = core.store().nodes().await?;
    let node = match resolver_host(&nodes, &online, false, &set.repo) {
        Ok(n) => n,
        Err(why) => {
            note(format!("refused: {why}"), json!({ "reason": "no_node" })).await?;
            anyhow::bail!("{why}");
        }
    };
    let card = kls_forge::conflicts::file_resolver_card(core, set, url, caller).await?;
    if set.is_fork() {
        note(
            "held for fork code".into(),
            json!({ "card": card.key, "nodeId": node }),
        )
        .await?;
        return Ok(Handed {
            card_id: card.card_id,
            key: card.key,
            existing: card.existing,
            node,
            dispatch: None,
        });
    }
    let spec = kls_core::DispatchSpec {
        kind: "worker".into(),
        repo: set.repo.clone(),
        branch: set.head_ref.clone(),
        prompt: format!(
            "Resolve the merge conflicts of {target} on card {} (head {}, base {}).",
            card.key, set.head_ref, set.base_ref
        ),
        agent: AGENT.into(),
        card_id: card.card_id.clone(),
        mode: "agent".into(),
        trigger: Some(kls_core::gateway::Trigger {
            source: "console".into(),
            card_id: card.card_id.clone(),
            subject: format!("conflicts:{target}"),
            ..Default::default()
        }),
        ..Default::default()
    };
    let id = match gw.dispatch(&node, spec).await {
        Ok(id) => id,
        Err(e) => {
            note(
                format!("failed: {e:#}"),
                json!({ "card": card.key, "nodeId": node }),
            )
            .await?;
            return Err(e);
        }
    };
    if let Some(c) = core.card(&card.card_id).await? {
        core.note_card_dispatch(&c, &node, &id).await?;
    }
    note(
        "dispatched".into(),
        json!({ "card": card.key, "nodeId": node, "dispatchId": id, "existing": card.existing }),
    )
    .await?;
    Ok(Handed {
        card_id: card.card_id,
        key: card.key,
        existing: card.existing,
        node,
        dispatch: Some(id),
    })
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

    fn node(id: &str, agents: &[&str], revoked: bool) -> kls_store::Node {
        kls_store::Node {
            id: id.into(),
            name: id.into(),
            host: kls_store::HostInfo {
                agents: agents.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            cert_serial: String::new(),
            created_at: 0,
            last_seen_at: None,
            revoked_at: revoked.then_some(1),
            revoked_reason: None,
            node_seq: 0,
            server_acked: 0,
        }
    }

    #[test]
    fn resolver_host_needs_a_live_node_offering_the_worker() {
        let nodes = vec![
            node("a", &["belai:builder"], false),
            node("b", &[AGENT], false),
            node("c", &[AGENT], true),
        ];
        let on = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            resolver_host(&nodes, &on(&["b"]), true, "acme/api").unwrap_err(),
            "Factory stopped"
        );
        assert_eq!(
            resolver_host(&nodes, &[], false, "acme/api").unwrap_err(),
            "no node is online"
        );
        assert_eq!(
            resolver_host(&nodes, &on(&["a"]), false, "acme/api").unwrap_err(),
            format!("no online node offers {AGENT}")
        );
        // A revoked node never qualifies, even while linked.
        assert!(resolver_host(&nodes, &on(&["a", "c"]), false, "acme/api").is_err());
        assert_eq!(
            resolver_host(&nodes, &on(&["a", "b"]), false, "acme/api").unwrap(),
            "b"
        );
    }

    #[derive(Default)]
    struct Gw {
        sent: std::sync::Mutex<Vec<(String, kls_core::DispatchSpec)>>,
    }

    #[async_trait::async_trait]
    impl kls_core::NodeGateway for Gw {
        async fn online(&self) -> Vec<String> {
            vec!["n1".into()]
        }
        async fn dispatch(&self, node: &str, spec: kls_core::DispatchSpec) -> Result<String> {
            self.sent.lock().unwrap().push((node.into(), spec));
            Ok("d1".into())
        }
        async fn job(
            &self,
            _: Option<&str>,
            _: &str,
            _: &str,
            _: kls_core::JobNeeds,
            _: i64,
        ) -> Result<kls_core::JobOutcome> {
            anyhow::bail!("no jobs")
        }
    }

    fn set(head_repo: &str) -> kls_forge::conflicts::ConflictSet {
        kls_forge::conflicts::ConflictSet {
            repo: "acme/api".into(),
            number: 12,
            title: "Feature".into(),
            head_repo: head_repo.into(),
            head_ref: "feat".into(),
            head_sha: "1".repeat(40),
            base_ref: "main".into(),
            base_sha: "b".repeat(40),
            merge_base: "m".repeat(40),
            files: Vec::new(),
            base_only: Vec::new(),
            forge: kls_forge::ForgeKind::Github,
        }
    }

    async fn add_node(core: &Core, agent: &str) {
        let host = kls_store::HostInfo {
            agents: vec![agent.into()],
            ..Default::default()
        };
        core.store()
            .insert_node("n1", "n1", &host, "s", "")
            .await
            .unwrap();
    }

    async fn with_gateway(core: &Core) -> std::sync::Arc<Gw> {
        let gw = std::sync::Arc::new(Gw::default());
        core.set_gateway(gw.clone());
        gw
    }

    async fn dispatch_audit(core: &Core) -> Vec<String> {
        crate::guard::audited(core, "dispatch")
            .await
            .into_iter()
            .filter(|e| e.subject["action"] == "conflict_resolver.dispatch")
            .map(|e| e.payload["outcome"].as_str().unwrap_or("").to_string())
            .collect()
    }

    #[tokio::test]
    async fn resolver_dispatch_files_the_card_and_starts_the_worker() {
        let (core, _d) = core().await;
        add_node(&core, AGENT).await;
        let gw = with_gateway(&core).await;
        let url = "https://github.com/acme/api/pull/12";
        let h = dispatch_resolver(&core, &set("acme/api"), url, "user")
            .await
            .unwrap();
        assert_eq!((h.node.as_str(), h.dispatch.as_deref()), ("n1", Some("d1")));
        let sent = gw.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        let (n, spec) = &sent[0];
        assert_eq!(n, "n1");
        assert_eq!(
            (
                spec.kind.as_str(),
                spec.agent.as_str(),
                spec.branch.as_str()
            ),
            ("worker", AGENT, "feat")
        );
        assert_eq!(spec.card_id, h.card_id);
        assert_eq!(spec.trigger.as_ref().unwrap().source, "console");
        let c = core.card(&h.card_id).await.unwrap().unwrap();
        assert_eq!(c.labels, [LABEL]);
        assert_eq!(dispatch_audit(&core).await, ["dispatched"]);
    }

    #[tokio::test]
    async fn resolver_dispatch_is_refused_when_stopped_or_without_a_node() {
        let (core, _d) = core().await;
        // Linked, but the node offers only another worker.
        add_node(&core, "belai:builder").await;
        let gw = with_gateway(&core).await;
        let e = dispatch_resolver(&core, &set("acme/api"), "", "user")
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("offers"), "{e:#}");
        assert!(core.cards().await.unwrap().is_empty());
        kls_core::control::stop(&core, "drill").await.unwrap();
        let e = dispatch_resolver(&core, &set("acme/api"), "", "user")
            .await
            .unwrap_err();
        assert!(kls_core::control::is_blocked(&e), "{e:#}");
        assert!(core.cards().await.unwrap().is_empty());
        assert!(gw.sent.lock().unwrap().is_empty());
        let a = dispatch_audit(&core).await;
        assert_eq!(a.len(), 2, "{a:?}");
        assert!(a.iter().all(|o| o.starts_with("refused")), "{a:?}");
    }

    #[tokio::test]
    async fn resolver_dispatch_holds_a_fork() {
        let (core, _d) = core().await;
        add_node(&core, AGENT).await;
        let gw = with_gateway(&core).await;
        let h = dispatch_resolver(&core, &set("stranger/api"), "", "user")
            .await
            .unwrap();
        assert!(h.dispatch.is_none());
        assert!(gw.sent.lock().unwrap().is_empty());
        let c = core.card(&h.card_id).await.unwrap().unwrap();
        assert!(kls_core::board::cards::is_held(&c));
        assert_eq!(dispatch_audit(&core).await, ["held for fork code"]);
    }
}
