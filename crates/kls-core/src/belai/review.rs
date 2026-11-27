//! Rubric code reviews as a node job (belai-counterparts row 47, belai
//! `/review` and `belai review`): a host that has the repository reviews a
//! working tree, a branch or a pull request (job `review`, read only; the
//! host posts nothing), Keleustes keeps the findings with their provenance,
//! and the `pr_review` output posts them (`kls_engine::forge`).
//!
//! A review is a host's model-judged text: every field is checked against
//! the closed lists belai uses (verdicts, severities, rubric criteria), paths
//! and text are cleaned, and anything that does not fit is dropped and
//! counted, never repaired. Requesting a review is refused under the kill
//! switch; each review kept is audited with the node, the run and who asked.
//! The job goes from Keleustes to one host, never between hosts.

use crate::Core;
use crate::gateway::JobNeeds;
use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The job kind.
pub const JOB: &str = "review";
/// Where the reviews are kept.
const KEY: &str = "review.results";
/// Reviews kept (oldest dropped first).
const MAX_REVIEWS: usize = 100;
/// The longest a review job runs (the host caps a job at ten minutes).
pub const JOB_TIMEOUT_MS: i64 = 600_000;

/// The closed lists (belai `internal/review`).
pub const VERDICTS: [&str; 3] = ["approve", "comment", "request_changes"];
pub const SEVERITIES: [&str; 4] = ["blocker", "major", "minor", "nit"];
pub const CRITERIA: [&str; 9] = [
    "correctness",
    "security",
    "tests",
    "errors",
    "concurrency",
    "performance",
    "compatibility",
    "maintainability",
    "docs",
];
pub const TARGETS: [&str; 3] = ["workdir", "branch", "pr"];
/// Findings kept per review (belai `review.MaxFindings`).
pub const MAX_FINDINGS: usize = 20;
const MAX_BODY: usize = 2048;
const MAX_FIX: usize = 1024;
const MAX_PATH: usize = 512;

pub mod kinds {
    /// A review's findings arrived. Subject: review id.
    pub const REVIEW: &str = "review.findings";
}

/// One finding, as the host validated it and Keleustes checked it again.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub path: String,
    pub line: i64,
    pub severity: String,
    pub criterion: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fix: String,
}

/// A review and where it came from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    pub id: String,
    pub node_id: String,
    pub node_name: String,
    pub repo: String,
    pub cwd: String,
    /// "workdir", "branch" or "pr".
    pub target: String,
    /// The branch or the pull request number asked for.
    pub reference: String,
    pub base: String,
    pub head_sha: String,
    pub pr: i64,
    /// "approve", "comment" or "request_changes". Advisory.
    pub verdict: String,
    pub findings: Vec<Finding>,
    pub files: i64,
    pub skipped: i64,
    /// Finding lines the host or Keleustes dropped.
    pub dropped: i64,
    pub truncated: bool,
    pub incomplete: bool,
    pub cached: bool,
    /// The workflow run and step that asked, when one did.
    pub run_id: String,
    pub step_id: String,
    pub requested_by: String,
    pub at: i64,
}

/// What to review, and where.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// A node, or empty for any online node that runs reviews.
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub target: String,
    #[serde(default, rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub step_id: String,
}

impl Request {
    /// The job's payload (a `ReviewJobRequest`, proto3 JSON).
    pub fn payload(&self) -> String {
        let mut p = json!({ "target": self.target_word() });
        for (k, v) in [
            ("cwd", &self.cwd),
            ("repo", &self.repo),
            ("ref", &self.reference),
            ("base", &self.base),
            ("provider", &self.provider),
            ("model", &self.model),
        ] {
            if !v.is_empty() {
                p[k] = json!(v);
            }
        }
        if self.force {
            p["force"] = json!(true);
        }
        p.to_string()
    }

    fn target_word(&self) -> &str {
        if self.target.is_empty() {
            "workdir"
        } else {
            &self.target
        }
    }

    /// Why the request cannot be sent, if it cannot.
    pub fn check(&self) -> Result<()> {
        if !TARGETS.contains(&self.target_word()) {
            bail!("review a working tree, a branch or a pull request");
        }
        if self.cwd.is_empty() && self.repo.is_empty() {
            bail!("pick a directory or a repository");
        }
        if self.cwd.len() > 1024 || self.repo.len() > 256 || self.cwd.contains('\0') {
            bail!("that directory or repository name is too long");
        }
        if self.target_word() == "pr" && !is_number(&self.reference) {
            bail!("a pull request review needs the pull request number");
        }
        if self.reference.len() > 200 || self.base.len() > 200 {
            bail!("that ref is too long");
        }
        Ok(())
    }
}

fn is_number(s: &str) -> bool {
    !s.is_empty() && s.len() <= 7 && s.bytes().all(|b| b.is_ascii_digit()) && !s.starts_with('0')
}

/// The host's answer (a `ReviewResult`, proto3 JSON; both spellings taken).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Wire {
    verdict: String,
    findings: Vec<WireFinding>,
    target: String,
    #[serde(alias = "head_sha")]
    head_sha: String,
    pr: i64,
    files: i64,
    skipped: i64,
    dropped: i64,
    truncated: bool,
    incomplete: bool,
    cached: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireFinding {
    path: String,
    line: i64,
    severity: String,
    criterion: String,
    body: String,
    fix: String,
}

/// Host text cleaned for display and for a forge: one line, no control
/// characters, no bidi overrides, at most `max` bytes.
pub fn clean_text(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200b}'..='\u{200f}')
        {
            continue;
        }
        let c = if c.is_control() { ' ' } else { c };
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A repository-relative path a finding may name (belai's reviewable path
/// alphabet: letters, digits and `. _ - / @ +`; no `..`, nothing under `.git`).
pub fn valid_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= MAX_PATH
        && !p.starts_with('/')
        && !p.starts_with(".git/")
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/@+".contains(&b))
        && p.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
}

/// Reads and checks a host's `ReviewResult`. A finding outside the closed
/// lists, off the path alphabet or over a cap is dropped and counted.
pub fn parse(output_json: &str) -> Result<Review> {
    let w: Wire = serde_json::from_str(output_json)
        .map_err(|e| anyhow!("the host's review does not parse: {e}"))?;
    let mut r = Review {
        verdict: w.verdict.clone(),
        target: if TARGETS.contains(&w.target.as_str()) {
            w.target
        } else {
            String::new()
        },
        head_sha: w
            .head_sha
            .to_ascii_lowercase()
            .chars()
            .filter(char::is_ascii_hexdigit)
            .take(64)
            .collect(),
        pr: w.pr.clamp(0, 10_000_000),
        files: w.files.max(0),
        skipped: w.skipped.max(0),
        dropped: w.dropped.max(0),
        truncated: w.truncated,
        incomplete: w.incomplete,
        cached: w.cached,
        ..Default::default()
    };
    if !VERDICTS.contains(&r.verdict.as_str()) {
        r.verdict = "comment".into();
        r.incomplete = true;
    }
    for f in w.findings {
        let body = clean_text(&f.body, MAX_BODY);
        let ok = r.findings.len() < MAX_FINDINGS
            && valid_path(&f.path)
            && f.line > 0
            && f.line <= 10_000_000
            && SEVERITIES.contains(&f.severity.as_str())
            && CRITERIA.contains(&f.criterion.as_str())
            && !body.is_empty()
            && f.body.len() <= MAX_BODY
            && f.fix.len() <= MAX_FIX;
        if !ok {
            r.dropped += 1;
            continue;
        }
        r.findings.push(Finding {
            path: f.path,
            line: f.line,
            severity: f.severity,
            criterion: f.criterion,
            body,
            fix: clean_text(&f.fix, MAX_FIX),
        });
    }
    // The verdict the findings still back (belai's rule): request_changes
    // needs a blocker or major finding.
    if r.verdict == "request_changes"
        && !r
            .findings
            .iter()
            .any(|f| matches!(f.severity.as_str(), "blocker" | "major"))
    {
        r.verdict = "comment".into();
    }
    Ok(r)
}

async fn load(core: &Core) -> Vec<Review> {
    core.store()
        .setting(KEY)
        .await
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

async fn save(core: &Core, mut all: Vec<Review>) -> Result<()> {
    if all.len() > MAX_REVIEWS {
        let drop = all.len() - MAX_REVIEWS;
        all.drain(..drop);
    }
    core.store()
        .set_setting(KEY, &serde_json::to_string(&all)?)
        .await
}

/// Whether a node runs review jobs (it lists the job kind).
pub fn reviews_on(node: &kls_store::Node) -> bool {
    node.revoked_at.is_none() && node.host.capabilities.job_kinds.iter().any(|k| k == JOB)
}

/// Whether a review job has somewhere to go: `node` when named, else any of
/// the `online` nodes, advertising `review` in `Capabilities.job_kinds`. A
/// node that lists no kinds predates the list, and so predates this job.
pub async fn offered(core: &Core, online: &[String], node: Option<&str>) -> bool {
    let Ok(nodes) = core.store().nodes().await else {
        return false;
    };
    nodes
        .iter()
        .filter(|n| online.contains(&n.id) && node.is_none_or(|id| id == n.id))
        .any(reviews_on)
}

/// Asks a node (or any online node that runs reviews) to review `req`,
/// waits for the findings, keeps them and audits them. Refused under the
/// kill switch.
pub async fn request(core: &Core, req: &Request, by: &str) -> Result<Review> {
    req.check()?;
    let target = if req.node_id.is_empty() {
        JOB
    } else {
        req.node_id.as_str()
    };
    crate::control::check(core, "job", target, by).await?;
    if !req.node_id.is_empty() {
        let node = core
            .store()
            .node(&req.node_id)
            .await?
            .ok_or_else(|| anyhow!("unknown node"))?;
        if !reviews_on(&node) {
            bail!(
                "node {} does not run code reviews (update Belai)",
                node.name
            );
        }
    }
    let needs = JobNeeds {
        provider: Some(req.provider.clone()).filter(|p| !p.is_empty()),
        ..Default::default()
    };
    let out = core
        .gateway()?
        .job(
            Some(req.node_id.as_str()).filter(|s| !s.is_empty()),
            JOB,
            &req.payload(),
            needs,
            JOB_TIMEOUT_MS,
        )
        .await?;
    if out.status != "ok" {
        bail!("review {}: {}", out.status, out.error);
    }
    let node_id = if out.node_id.is_empty() {
        req.node_id.clone()
    } else {
        out.node_id.clone()
    };
    record(core, req, &node_id, &out.output_json, by).await
}

/// Checks a host's answer to `req` and keeps it with its provenance.
pub async fn record(
    core: &Core,
    req: &Request,
    node_id: &str,
    output_json: &str,
    by: &str,
) -> Result<Review> {
    let mut r = parse(output_json)?;
    let node_name = match core.store().node(node_id).await {
        Ok(Some(n)) => n.name,
        _ => String::new(),
    };
    r.id = uuid::Uuid::now_v7().to_string();
    r.node_id = node_id.into();
    r.node_name = node_name;
    r.repo = req.repo.clone();
    r.cwd = req.cwd.clone();
    if r.target.is_empty() {
        r.target = req.target_word().into();
    }
    r.reference = req.reference.clone();
    r.base = req.base.clone();
    r.run_id = req.run_id.clone();
    r.step_id = req.step_id.clone();
    r.requested_by = by.into();
    r.at = kls_store::now_ms();
    let mut all = load(core).await;
    all.push(r.clone());
    save(core, all).await?;
    core.emit(
        kinds::REVIEW,
        &r.id,
        &json!({ "verdict": r.verdict, "findings": r.findings.len(), "repo": r.repo,
                 "pr": r.pr, "nodeId": r.node_id, "runId": r.run_id }),
    )
    .await?;
    core.audit_record(kls_store::audit::LedgerDraft {
        kind: "review".into(),
        actor: r.node_id.clone(),
        actor_kind: "host".into(),
        msg: format!(
            "code review of {} on {}: {}, {} finding(s)",
            r.subject(),
            if r.node_name.is_empty() {
                &r.node_id
            } else {
                &r.node_name
            },
            r.verdict,
            r.findings.len()
        ),
        subject: json!({ "review": r.id, "repo": r.repo, "pr": r.pr, "run": r.run_id,
                         "step": r.step_id, "node": r.node_id }),
        payload: json!({
            "verdict": r.verdict, "findings": r.findings.len(), "dropped": r.dropped,
            "target": r.target, "headSha": r.head_sha, "cwd": r.cwd, "files": r.files,
            "truncated": r.truncated, "incomplete": r.incomplete, "cached": r.cached,
            "requestedBy": r.requested_by,
        }),
        origin: format!("host:{}", r.node_id),
        node_id: r.node_id.clone(),
        ..Default::default()
    })
    .await?;
    Ok(r)
}

impl Review {
    /// "acme/api#7", "acme/api branch feature" or the directory.
    pub fn subject(&self) -> String {
        let place = if self.repo.is_empty() {
            &self.cwd
        } else {
            &self.repo
        };
        match self.target.as_str() {
            "pr" => format!("{place}#{}", self.pr.max(0)),
            "branch" if !self.reference.is_empty() => format!("{place} branch {}", self.reference),
            _ => place.to_string(),
        }
    }
}

/// Every review kept, newest first.
pub async fn list(core: &Core) -> Vec<Review> {
    let mut all = load(core).await;
    all.reverse();
    all
}

/// One review by id.
pub async fn get(core: &Core, id: &str) -> Option<Review> {
    load(core).await.into_iter().find(|r| r.id == id)
}

/// The newest review a workflow run asked for, for pull request `pr` when
/// given (a review of another pull request is not this one's).
pub async fn latest_for_run(core: &Core, run_id: &str, pr: Option<i64>) -> Option<Review> {
    if run_id.is_empty() {
        return None;
    }
    load(core)
        .await
        .into_iter()
        .rev()
        .find(|r| r.run_id == run_id && pr.is_none_or(|n| r.target != "pr" || r.pr == n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{DispatchSpec, JobOutcome, NodeGateway};
    use std::sync::{Arc, Mutex};

    struct Gw {
        answer: String,
        asked: Mutex<Vec<(Option<String>, String, String)>>,
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
            self.asked.lock().unwrap().push((
                node.map(str::to_string),
                kind.to_string(),
                payload.to_string(),
            ));
            Ok(JobOutcome {
                status: "ok".into(),
                output_json: self.answer.clone(),
                node_id: "n1".into(),
                ..Default::default()
            })
        }
    }

    fn answer() -> String {
        json!({
            "verdict": "request_changes", "target": "pr", "headSha": "AB".repeat(20), "pr": 7,
            "files": 3, "dropped": 1,
            "findings": [
                {"path": "src/a.rs", "line": 12, "severity": "major", "criterion": "correctness",
                 "body": "the lock is\u{202e} held\nacross the await", "fix": "drop it first"},
                {"path": "../etc/passwd", "line": 1, "severity": "major", "criterion": "security", "body": "x"},
                {"path": "b.rs", "line": 0, "severity": "minor", "criterion": "docs", "body": "x"},
                {"path": "b.rs", "line": 3, "severity": "urgent", "criterion": "docs", "body": "x"},
                {"path": "b.rs", "line": 3, "severity": "minor", "criterion": "vibes", "body": "x"},
            ],
        })
        .to_string()
    }

    async fn setup(answer: String) -> (Core, Arc<Gw>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        let gw = Arc::new(Gw {
            answer,
            asked: Mutex::new(Vec::new()),
        });
        c.set_gateway(gw.clone());
        let mut h = kls_store::HostInfo::default();
        h.capabilities.job_kinds = vec![JOB.into()];
        c.store()
            .insert_node("n1", "one", &h, "s", "t")
            .await
            .unwrap();
        c.store()
            .insert_node("n2", "old", &kls_store::HostInfo::default(), "s2", "t2")
            .await
            .unwrap();
        (c, gw, dir)
    }

    fn pr_request() -> Request {
        Request {
            node_id: "n1".into(),
            repo: "acme/api".into(),
            target: "pr".into(),
            reference: "7".into(),
            run_id: "run-1".into(),
            step_id: "step-1".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn findings_are_checked_kept_with_provenance_and_audited() {
        let (c, gw, _d) = setup(answer()).await;
        let r = request(&c, &pr_request(), "factory:wf").await.unwrap();
        let (node, kind, payload) = gw.asked.lock().unwrap()[0].clone();
        assert_eq!((node.as_deref(), kind.as_str()), (Some("n1"), JOB));
        let p: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(
            (p["target"].as_str(), p["ref"].as_str(), p["repo"].as_str()),
            (Some("pr"), Some("7"), Some("acme/api"))
        );
        assert_eq!(r.verdict, "request_changes");
        assert_eq!(r.findings.len(), 1);
        let f = &r.findings[0];
        assert_eq!((f.path.as_str(), f.line), ("src/a.rs", 12));
        assert_eq!(f.body, "the lock is held across the await");
        assert_eq!(r.dropped, 5, "1 from the host, 4 here");
        assert_eq!(r.head_sha, "ab".repeat(20));
        assert_eq!(
            (r.node_name.as_str(), r.run_id.as_str(), r.step_id.as_str()),
            ("one", "run-1", "step-1")
        );
        assert_eq!(r.requested_by, "factory:wf");
        assert_eq!(list(&c).await, vec![r.clone()]);
        assert_eq!(get(&c, &r.id).await.as_ref(), Some(&r));
        assert_eq!(
            latest_for_run(&c, "run-1", Some(7)).await.as_ref(),
            Some(&r)
        );
        assert!(latest_for_run(&c, "run-1", Some(8)).await.is_none());
        assert!(latest_for_run(&c, "run-2", None).await.is_none());
        let ledger = c
            .store()
            .ledger(&kls_store::audit::LedgerQuery {
                kinds: vec!["review".into()],
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(ledger.len(), 1);
        let e = &ledger[0];
        assert_eq!(
            (e.actor_kind.as_str(), e.origin.as_str()),
            ("host", "host:n1")
        );
        assert_eq!(e.subject["run"], "run-1");
        assert_eq!(e.payload["requestedBy"], "factory:wf");
    }

    #[tokio::test]
    async fn the_kill_switch_and_bad_requests_send_nothing() {
        let (c, gw, _d) = setup(answer()).await;
        for bad in [
            Request {
                target: "everything".into(),
                ..pr_request()
            },
            Request {
                reference: "7; rm".into(),
                ..pr_request()
            },
            Request {
                repo: String::new(),
                ..pr_request()
            },
            Request {
                node_id: "n2".into(),
                ..pr_request()
            },
        ] {
            assert!(request(&c, &bad, "owner").await.is_err(), "{bad:?}");
        }
        crate::control::stop(&c, "drill").await.unwrap();
        assert!(request(&c, &pr_request(), "owner").await.is_err());
        assert!(gw.asked.lock().unwrap().is_empty());
        assert!(list(&c).await.is_empty());
    }

    #[test]
    fn a_verdict_the_findings_do_not_back_is_narrowed() {
        let r = parse(
            &json!({"verdict": "request_changes", "findings": [
                {"path": "a.rs", "line": 1, "severity": "nit", "criterion": "docs", "body": "x"}]})
            .to_string(),
        )
        .unwrap();
        assert_eq!(r.verdict, "comment");
        let r = parse(r#"{"verdict":"merge"}"#).unwrap();
        assert_eq!((r.verdict.as_str(), r.incomplete), ("comment", true));
        assert!(parse("not json").is_err());
        let many: Vec<_> = (1..=25)
            .map(|n| json!({"path": "a.rs", "line": n, "severity": "minor", "criterion": "docs", "body": "x"}))
            .collect();
        let r = parse(&json!({"verdict": "comment", "findings": many}).to_string()).unwrap();
        assert_eq!((r.findings.len(), r.dropped), (MAX_FINDINGS, 5));
        assert!(!valid_path(".git/config") && !valid_path("/etc/x") && !valid_path("a//b"));
        assert!(valid_path("crates/kls-core/src/a_b.rs"));
    }
}
