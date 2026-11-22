//! AGENTS.md drafts as a node job (belai-counterparts row 54, belai `/init`):
//! a host that has the repository drafts the file from its harness-computed
//! repository facts (job `init_draft`; nothing is written there), Keleustes
//! keeps the draft for a person to review, and accepting it opens a pull
//! request through the forge plane (`kls_github::forge::propose`).
//!
//! The draft is model text from a host: it is stored and shown as data, its
//! hash checked against the host's, and it reaches a repository only after
//! a person accepts it. Each draft keeps its provenance (node, directory,
//! repository, who asked, when, the host's hashes); requesting, accepting
//! and rejecting are audited and refused under the kill switch. The job goes
//! from Keleustes to one host, never between hosts.

use crate::Core;
use crate::gateway::JobNeeds;
use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

/// The job kind.
pub const JOB: &str = "init_draft";
/// Where the drafts are kept.
const KEY: &str = "init_draft.drafts";
/// Drafts kept (oldest dropped first).
const MAX_DRAFTS: usize = 50;
/// Largest draft taken.
pub const MAX_BYTES: usize = 64 * 1024;
const JOB_TIMEOUT_MS: i64 = 180_000;

pub mod kinds {
    /// A draft arrived, or a person accepted or rejected one. Subject: draft id.
    pub const INIT_DRAFT: &str = "init_draft.changed";
}

/// One draft and where it came from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    pub id: String,
    pub node_id: String,
    pub node_name: String,
    pub cwd: String,
    pub repo: String,
    /// "AGENTS.md" or the instruction file the repository already uses.
    pub path: String,
    pub markdown: String,
    pub sha256: String,
    /// The existing file's hash when the draft replaces one.
    pub base_sha256: String,
    pub requested_by: String,
    pub requested_at: i64,
    /// "pending", "accepted", "rejected".
    pub status: String,
    pub decided_by: String,
    pub decided_at: i64,
    /// The pull request an accepted draft opened.
    pub pr_url: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Wire {
    #[serde(default)]
    path: String,
    #[serde(default)]
    markdown: String,
    #[serde(default)]
    sha256: String,
    #[serde(default, alias = "base_sha256")]
    base_sha256: String,
}

fn sha(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// The instruction file names a draft may target (belai
/// `repomap.InstructionFileNames`).
pub fn valid_path(p: &str) -> bool {
    matches!(
        p,
        "AGENTS.md" | "CLAUDE.md" | "GEMINI.md" | ".github/copilot-instructions.md"
    )
}

/// Model text from a host, cleaned for display: no control characters but
/// newlines and tabs, no bidi overrides.
pub fn clean_markdown(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .filter(|c| !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        .collect()
}

async fn load(core: &Core) -> Vec<Draft> {
    core.store()
        .setting(KEY)
        .await
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

async fn save(core: &Core, mut all: Vec<Draft>) -> Result<()> {
    if all.len() > MAX_DRAFTS {
        let drop = all.len() - MAX_DRAFTS;
        all.drain(..drop);
    }
    core.store()
        .set_setting(KEY, &serde_json::to_string(&all)?)
        .await
}

async fn audit(core: &Core, d: &Draft, what: &str, by: &str) -> Result<()> {
    core.emit(
        kinds::INIT_DRAFT,
        &d.id,
        &json!({ "status": d.status, "repo": d.repo, "nodeId": d.node_id, "by": by }),
    )
    .await?;
    core.audit_record(kls_store::audit::LedgerDraft {
        kind: "init_draft".into(),
        actor: by.into(),
        actor_kind: "user".into(),
        msg: format!(
            "AGENTS.md draft {what}: {} on {}",
            d.repo_or_cwd(),
            d.node_name
        ),
        subject: json!({ "draft": d.id, "repo": d.repo, "node": d.node_id, "path": d.path }),
        payload: json!({
            "status": d.status, "sha256": d.sha256, "baseSha256": d.base_sha256,
            "cwd": d.cwd, "requestedBy": d.requested_by, "requestedAt": d.requested_at,
            "prUrl": d.pr_url,
        }),
        origin: "factory".into(),
        node_id: d.node_id.clone(),
        ..Default::default()
    })
    .await?;
    Ok(())
}

impl Draft {
    pub fn repo_or_cwd(&self) -> &str {
        if self.repo.is_empty() {
            &self.cwd
        } else {
            &self.repo
        }
    }
}

/// Whether a node drafts AGENTS.md files (it lists the job kind).
pub fn drafts_on(node: &kls_store::Node) -> bool {
    node.revoked_at.is_none() && node.host.capabilities.job_kinds.iter().any(|k| k == JOB)
}

/// Asks `node_id` to draft an AGENTS.md for `cwd` (one of its trusted
/// directories) or `repo` (owner/name of a checkout it has). Waits for the
/// draft and keeps it pending review.
pub async fn request(core: &Core, node_id: &str, cwd: &str, repo: &str, by: &str) -> Result<Draft> {
    let (cwd, repo) = (cwd.trim(), repo.trim());
    if cwd.is_empty() && repo.is_empty() {
        bail!("pick a directory or a repository");
    }
    if cwd.len() > 1024 || repo.len() > 256 || cwd.contains('\0') {
        bail!("that directory or repository name is too long");
    }
    crate::control::check(core, "job", node_id, by).await?;
    let node = core
        .store()
        .node(node_id)
        .await?
        .ok_or_else(|| anyhow!("unknown node"))?;
    if !drafts_on(&node) {
        bail!(
            "node {} does not draft AGENTS.md files (update Belai)",
            node.name
        );
    }
    let payload = json!({ "cwd": cwd, "repo": repo }).to_string();
    let out = core
        .gateway()?
        .job(
            Some(node_id),
            JOB,
            &payload,
            JobNeeds::default(),
            JOB_TIMEOUT_MS,
        )
        .await?;
    if out.status != "ok" {
        bail!("{}: {}", out.status, out.error);
    }
    let w: Wire = serde_json::from_str(&out.output_json)
        .map_err(|e| anyhow!("the host's draft does not parse: {e}"))?;
    if w.markdown.len() > MAX_BYTES {
        bail!("the host's draft is larger than {MAX_BYTES} bytes");
    }
    if w.markdown.trim().is_empty() {
        bail!("the host returned an empty draft");
    }
    if sha(&w.markdown) != w.sha256 {
        bail!("the draft's hash does not match what the host said");
    }
    let path = if w.path.is_empty() {
        "AGENTS.md".to_string()
    } else {
        w.path
    };
    if !valid_path(&path) {
        bail!("the host proposed an unexpected file ({path})");
    }
    let markdown = clean_markdown(&w.markdown);
    let d = Draft {
        id: uuid::Uuid::now_v7().to_string(),
        node_id: node.id.clone(),
        node_name: node.name.clone(),
        cwd: cwd.into(),
        repo: repo.into(),
        path,
        sha256: sha(&markdown),
        markdown,
        base_sha256: w
            .base_sha256
            .chars()
            .filter(char::is_ascii_hexdigit)
            .take(64)
            .collect(),
        requested_by: by.into(),
        requested_at: kls_store::now_ms(),
        status: "pending".into(),
        ..Default::default()
    };
    let mut all = load(core).await;
    all.push(d.clone());
    save(core, all).await?;
    audit(core, &d, "drafted", by).await?;
    Ok(d)
}

/// Every draft kept, newest first.
pub async fn list(core: &Core) -> Vec<Draft> {
    let mut all = load(core).await;
    all.reverse();
    all
}

/// One draft.
pub async fn get(core: &Core, id: &str) -> Option<Draft> {
    load(core).await.into_iter().find(|d| d.id == id)
}

/// Records a person's decision on a pending draft: "accepted" (with the pull
/// request it opened, and the text that went out when they edited it) or
/// "rejected". The kill switch refuses an acceptance; a rejection only
/// winds down, so it goes through.
pub async fn decide(
    core: &Core,
    id: &str,
    status: &str,
    pr_url: &str,
    markdown: Option<&str>,
    by: &str,
) -> Result<Draft> {
    if !matches!(status, "accepted" | "rejected") {
        bail!("accept or reject a draft");
    }
    if status == "accepted" {
        crate::control::check(core, "forge", id, by).await?;
    }
    let mut all = load(core).await;
    let d = all
        .iter_mut()
        .find(|d| d.id == id)
        .ok_or_else(|| anyhow!("no such draft"))?;
    if d.status != "pending" {
        bail!("that draft was already {}", d.status);
    }
    if let Some(m) = markdown {
        let m = clean_markdown(m);
        if m.len() > MAX_BYTES {
            bail!("the draft is larger than {MAX_BYTES} bytes");
        }
        d.sha256 = sha(&m);
        d.markdown = m;
    }
    d.status = status.into();
    d.decided_by = by.into();
    d.decided_at = kls_store::now_ms();
    d.pr_url = pr_url.chars().take(512).collect();
    let d = d.clone();
    save(core, all).await?;
    audit(core, &d, status, by).await?;
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{DispatchSpec, JobOutcome, NodeGateway};
    use std::sync::{Arc, Mutex};

    struct Gw {
        answer: Mutex<String>,
    }

    #[async_trait::async_trait]
    impl NodeGateway for Gw {
        async fn online(&self) -> Vec<String> {
            vec!["n1".into()]
        }
        async fn dispatch(&self, _: &str, _: DispatchSpec) -> Result<String> {
            bail!("no dispatches")
        }
        async fn job(
            &self,
            node: Option<&str>,
            kind: &str,
            payload: &str,
            _: JobNeeds,
            _: i64,
        ) -> Result<JobOutcome> {
            assert_eq!((node, kind), (Some("n1"), JOB));
            assert!(payload.contains("\"repo\":\"acme/app\""));
            Ok(JobOutcome {
                status: "ok".into(),
                output_json: self.answer.lock().unwrap().clone(),
                ..Default::default()
            })
        }
    }

    async fn setup(md: &str, claimed: Option<&str>) -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        let answer = json!({
            "path": "AGENTS.md", "markdown": md,
            "sha256": claimed.map(str::to_string).unwrap_or_else(|| sha(md)),
        })
        .to_string();
        c.set_gateway(Arc::new(Gw {
            answer: Mutex::new(answer),
        }));
        let mut h = kls_store::HostInfo::default();
        h.capabilities.job_kinds = vec![JOB.into()];
        c.store()
            .insert_node("n1", "one", &h, "s", "t")
            .await
            .unwrap();
        (c, dir)
    }

    #[tokio::test]
    async fn a_draft_is_kept_pending_with_provenance_and_audited() {
        let (c, _d) = setup("# Agents\n\nRun `just check`.\u{202e}\n", None).await;
        let d = request(&c, "n1", "", "acme/app", "owner").await.unwrap();
        assert_eq!(d.status, "pending");
        assert_eq!(d.node_name, "one");
        assert_eq!(d.requested_by, "owner");
        assert!(!d.markdown.contains('\u{202e}'));
        assert_eq!(d.sha256, sha(&d.markdown));
        assert_eq!(list(&c).await.len(), 1);
        let edited = "# Agents\n\nRun `just check` first.\n";
        let a = decide(
            &c,
            &d.id,
            "accepted",
            "https://x/pull/1",
            Some(edited),
            "owner",
        )
        .await
        .unwrap();
        assert_eq!(a.markdown, edited);
        assert_eq!(a.pr_url, "https://x/pull/1");
        assert!(
            decide(&c, &d.id, "rejected", "", None, "owner")
                .await
                .is_err()
        );
        let e = c
            .store()
            .ledger(&kls_store::audit::LedgerQuery {
                kinds: vec!["init_draft".into()],
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(e.len(), 2);
    }

    #[tokio::test]
    async fn a_draft_whose_hash_does_not_match_is_refused() {
        let (c, _d) = setup("# x\n", Some("00")).await;
        assert!(request(&c, "n1", "", "acme/app", "owner").await.is_err());
        assert!(list(&c).await.is_empty());
    }

    #[tokio::test]
    async fn the_kill_switch_refuses_requests_and_acceptance_not_rejection() {
        let (c, _d) = setup("# x\n", None).await;
        let d = request(&c, "n1", "", "acme/app", "owner").await.unwrap();
        crate::control::pause(&c, "t").await.unwrap();
        assert!(request(&c, "n1", "", "acme/app", "owner").await.is_err());
        assert!(
            decide(&c, &d.id, "accepted", "u", None, "owner")
                .await
                .is_err()
        );
        assert_eq!(
            decide(&c, &d.id, "rejected", "", None, "owner")
                .await
                .unwrap()
                .status,
            "rejected"
        );
    }
}
