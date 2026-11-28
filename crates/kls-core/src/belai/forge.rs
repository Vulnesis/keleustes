//! Forge relay (WP-H). A worker that cannot publish its branch itself (no
//! push credential, a rate limit) files a request through its node. The
//! request is identifiers, shas and enums plus the item title (untrusted);
//! Keleustes keeps it and never unpacks or runs git on another host's data.
//! The forge coordinator (kls-forge) decides, and answers the worker with a
//! `steer` dispatch ([`Core::forge_steer`]): took over (with the pull request
//! it opened), waiting until a time, or failed. Filing and steering are
//! audited; the kill switch refuses a steer like any dispatch.

use crate::Core;
use crate::gateway::DispatchSpec;
use crate::sessions::Refusal;
use anyhow::Result;
use kls_store::audit::LedgerDraft;
use serde_json::json;

pub use kls_store::belai::ForgeRequest;

pub mod kinds {
    /// A worker filed a forge request. Subject: "<node>:<request>".
    pub const FORGE_REQUEST: &str = "forge.request";
    /// The coordinator answered one. Subject: "<node>:<request>".
    pub const FORGE_STEERED: &str = "forge.steered";
}

pub const KINDS: [&str; 4] = ["push", "pull_request", "issue_sync", "commit"];
pub const EVENTS: [&str; 3] = ["took_over", "waiting", "failed"];

fn hex_of(s: &str, lens: &[usize]) -> bool {
    lens.contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn ident(s: &str, n: usize) -> bool {
    !s.is_empty()
        && s.len() <= n
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn invalid(why: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Refusal::Invalid(why.into()))
}

/// Checks a filed request's shape (belai's `fleet.ValidForgeRequest` again).
pub fn check(f: &ForgeRequest) -> Result<()> {
    if !hex_of(&f.request_id, &[16]) {
        return Err(invalid("the request id is 16 hex characters"));
    }
    if !KINDS.contains(&f.kind.as_str()) {
        return Err(invalid("unknown forge request kind"));
    }
    if !ident(&f.worker_id, 64) || !ident(&f.repo_owner, 100) || !ident(&f.repo_name, 100) {
        return Err(invalid("the worker or repository is not an identifier"));
    }
    if !f.branch.starts_with("belai/")
        || f.branch.len() > 200
        || f.branch.contains("..")
        || !f
            .branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        return Err(invalid(
            "a forge request names an item's attempt branch (belai/...)",
        ));
    }
    for sha in [&f.base_sha, &f.head_sha] {
        if !sha.is_empty() && !hex_of(sha, &[40, 64]) {
            return Err(invalid("a sha is 40 or 64 hex characters"));
        }
    }
    if !f.bundle_sha256.is_empty() && !hex_of(&f.bundle_sha256, &[64]) {
        return Err(invalid("the bundle hash is 64 hex characters"));
    }
    if !f.failure.is_empty() && !ident(&f.failure, 40) {
        return Err(invalid("the failure is a word"));
    }
    Ok(())
}

impl Core {
    /// A node files a worker's request. A repeat is a duplicate (true).
    pub async fn forge_file(&self, node_id: &str, mut f: ForgeRequest) -> Result<bool> {
        f.node_id = node_id.into();
        f.title = f
            .title
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        check(&f)?;
        let fresh = self.store().insert_forge_request(&f).await?;
        if fresh {
            let subject = format!("{node_id}:{}", f.request_id);
            self.emit(
                kinds::FORGE_REQUEST,
                &subject,
                &json!({ "nodeId": node_id, "request": f.request_id, "kind": f.kind,
                         "repo": format!("{}/{}", f.repo_owner, f.repo_name), "worker": f.worker_id }),
            )
            .await?;
            self.audit_record(LedgerDraft {
                kind: "forge".into(),
                actor: format!("node:{node_id}"),
                actor_kind: "host".into(),
                msg: format!("forge request {} filed ({})", f.request_id, f.kind),
                subject: json!({ "request": f.request_id, "item": f.item_id, "worker": f.worker_id }),
                payload: json!({ "kind": f.kind, "repo": format!("{}/{}", f.repo_owner, f.repo_name),
                                 "branch": f.branch, "failure": f.failure }),
                origin: format!("host:{node_id}"),
                node_id: node_id.into(),
                ..Default::default()
            })
            .await?;
        }
        Ok(!fresh)
    }

    /// Filed requests not answered yet, newest first (the Delivery screen's list).
    pub async fn forge_pending(&self, limit: i64) -> Result<Vec<ForgeRequest>> {
        self.store()
            .forge_requests("filed", limit.clamp(1, 500))
            .await
    }

    /// The coordinator's answer to a filed request: a `steer` dispatch to
    /// the node that filed it. Returns the dispatch id.
    #[allow(clippy::too_many_arguments)]
    pub async fn forge_steer(
        &self,
        node_id: &str,
        request_id: &str,
        event: &str,
        pr: i32,
        pr_url: &str,
        until_ms: i64,
        reason: &str,
    ) -> Result<String> {
        self.forge_steer_by(
            "coordinator",
            node_id,
            request_id,
            event,
            pr,
            pr_url,
            until_ms,
            reason,
        )
        .await
    }

    /// [`Core::forge_steer`] on behalf of `caller` (`user` from the console,
    /// `coordinator` for the forge coordinator), recorded as the steer's
    /// actor. The kill switch is checked first and refuses (audited
    /// `factory.refused`) before anything reaches the node.
    #[allow(clippy::too_many_arguments)]
    pub async fn forge_steer_by(
        &self,
        caller: &str,
        node_id: &str,
        request_id: &str,
        event: &str,
        pr: i32,
        pr_url: &str,
        until_ms: i64,
        reason: &str,
    ) -> Result<String> {
        let subject = format!("{node_id}:{request_id}");
        crate::control::check(self, "dispatch", &subject, caller).await?;
        let Some(f) = self.store().forge_request(node_id, request_id).await? else {
            return Err(anyhow::Error::new(Refusal::NotFound("forge request")));
        };
        if !EVENTS.contains(&event) {
            return Err(invalid("a steer is took_over, waiting or failed"));
        }
        if !pr_url.is_empty() && (!pr_url.starts_with("https://") || pr_url.len() > 300) {
            return Err(invalid("the pull request link is one https URL"));
        }
        let id = self
            .gateway()?
            .dispatch(
                node_id,
                DispatchSpec {
                    kind: "steer".into(),
                    worker_id: f.worker_id.clone(),
                    card_id: f.item_id.clone(),
                    steer_request: f.request_id.clone(),
                    steer_event: event.into(),
                    steer_pr: pr.max(0),
                    steer_pr_url: pr_url.into(),
                    steer_until_ms: until_ms.max(0),
                    steer_reason: reason
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .take(40)
                        .collect(),
                    ..Default::default()
                },
            )
            .await?;
        self.store()
            .steer_forge_request(node_id, request_id, event, pr_url, &id)
            .await?;
        self.emit(
            kinds::FORGE_STEERED,
            &format!("{node_id}:{request_id}"),
            &json!({ "event": event, "dispatchId": id }),
        )
        .await?;
        let person = caller == "user";
        self.audit_record(LedgerDraft {
            kind: "forge".into(),
            actor: caller.into(),
            actor_kind: if person { "user" } else { "system" }.into(),
            msg: format!("forge request {request_id} steered: {event}"),
            subject: json!({ "request": request_id, "worker": f.worker_id }),
            payload: json!({ "event": event, "pr": pr, "prUrl": pr_url, "dispatch": id,
                             "caller": caller }),
            origin: if person { "console" } else { "factory" }.into(),
            node_id: node_id.into(),
            ..Default::default()
        })
        .await?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{JobNeeds, JobOutcome, NodeGateway};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Gw {
        sent: Mutex<Vec<(String, DispatchSpec)>>,
    }

    #[async_trait::async_trait]
    impl NodeGateway for Gw {
        async fn online(&self) -> Vec<String> {
            vec!["n1".into()]
        }
        async fn dispatch(&self, node: &str, spec: DispatchSpec) -> Result<String> {
            self.sent.lock().unwrap().push((node.into(), spec));
            Ok("d-1".into())
        }
        async fn job(
            &self,
            _: Option<&str>,
            _: &str,
            _: &str,
            _: JobNeeds,
            _: i64,
        ) -> Result<JobOutcome> {
            anyhow::bail!("no jobs")
        }
    }

    #[tokio::test]
    async fn person_steers_a_pending_request_audited_and_kill_switch_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        let gw = Arc::new(Gw::default());
        c.set_gateway(gw.clone());
        c.forge_file("n1", req()).await.unwrap();
        let pending = c.forge_pending(50).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].request_id, "0123456789abcdef");

        // Stopped: refused before any dispatch, and the refusal is audited.
        crate::control::stop(&c, "drill").await.unwrap();
        let e = c
            .forge_steer_by("user", "n1", "0123456789abcdef", "failed", 0, "", 0, "auth")
            .await
            .unwrap_err();
        assert!(crate::control::is_blocked(&e), "{e:#}");
        assert!(gw.sent.lock().unwrap().is_empty());
        c.audit_sync().await.unwrap();
        assert!(
            c.audit_entries(&Default::default())
                .await
                .unwrap()
                .iter()
                .any(|e| e.msg.contains("factory.refused"))
        );
        crate::control::resume(&c).await.unwrap();

        let id = c
            .forge_steer_by(
                "user",
                "n1",
                "0123456789abcdef",
                "waiting",
                0,
                "",
                kls_store::now_ms() + 3_600_000,
                "rate limit!",
            )
            .await
            .unwrap();
        assert_eq!(id, "d-1");
        let sent = gw.sent.lock().unwrap().clone();
        assert_eq!(sent[0].0, "n1");
        assert_eq!(
            (sent[0].1.kind.as_str(), sent[0].1.steer_event.as_str()),
            ("steer", "waiting")
        );
        assert_eq!(sent[0].1.steer_reason, "ratelimit");
        assert!(c.forge_pending(50).await.unwrap().is_empty());
        // Provenance: the person is the actor, from the console.
        let steered = c
            .audit_entries(&Default::default())
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "forge" && e.msg.contains("steered"))
            .unwrap();
        assert_eq!(
            (steered.actor.as_str(), steered.origin.as_str()),
            ("user", "console")
        );
        // A bad event is refused.
        assert!(
            c.forge_steer_by("user", "n1", "0123456789abcdef", "merge", 0, "", 0, "")
                .await
                .is_err()
        );
    }

    fn req() -> ForgeRequest {
        ForgeRequest {
            request_id: "0123456789abcdef".into(),
            item_id: "card-1".into(),
            worker_id: "w-1".into(),
            kind: "pull_request".into(),
            repo_owner: "acme".into(),
            repo_name: "api".into(),
            branch: "belai/K-1/a1".into(),
            head_sha: "a".repeat(40),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn file_checks_shape_dedupes_and_audits() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        assert!(!c.forge_file("n1", req()).await.unwrap());
        assert!(
            c.forge_file("n1", req()).await.unwrap(),
            "a repeat is a duplicate"
        );
        for bad in [
            ForgeRequest {
                branch: "main".into(),
                ..req()
            },
            ForgeRequest {
                kind: "force_push".into(),
                ..req()
            },
            ForgeRequest {
                request_id: "short".into(),
                ..req()
            },
            ForgeRequest {
                head_sha: "zz".into(),
                ..req()
            },
        ] {
            assert!(c.forge_file("n1", bad).await.is_err());
        }
        assert_eq!(
            c.audit_entries(&Default::default())
                .await
                .unwrap()
                .iter()
                .filter(|e| e.kind == "forge")
                .count(),
            1
        );
    }
}
