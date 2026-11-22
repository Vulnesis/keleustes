//! Knowledge indexes across hosts (belai-counterparts row 49): search every
//! online host's index at once (job `knowledge_search`, fanned out), and
//! ask one host to re-run its own ingestion (dispatch `knowledge_reindex`).
//!
//! A search calls no model: each host answers from its index, whose chunks
//! its classifier admitted when they were ingested. The snippets are still
//! host text, so they are capped and stripped of control characters here,
//! and shown as data only. Every hit names the node it came from
//! (provenance); every search and reindex is audited and refused under the
//! kill switch. A search goes from Keleustes to each host and back to the
//! person, never from one host to another, so the A2A blackout does not
//! apply; a host's reindex re-classifies on that host.

use crate::Core;
use crate::gateway::{DispatchSpec, JobNeeds, Trigger};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

/// The job kind a host runs searches as.
pub const SEARCH_JOB: &str = "knowledge_search";
/// The dispatch kind of a reindex.
pub const REINDEX_DISPATCH: &str = "knowledge_reindex";
/// Most hits one search keeps.
pub const MAX_HITS: usize = 100;
/// How long a host may take to answer.
const SEARCH_TIMEOUT_MS: i64 = 20_000;

/// One hit, with the node it came from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub node_id: String,
    pub node_name: String,
    /// The knowledge address (kb://...).
    pub address: String,
    pub index: String,
    pub score: f64,
    /// A short passage from the host's index (host text; data only).
    pub snippet: String,
    pub sha256: String,
}

/// What a search found, and how each node answered.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchReport {
    pub hits: Vec<Hit>,
    /// Some node answered only part of the query, or not at all.
    pub partial: bool,
    /// (node name, "ok" / why it gave nothing).
    pub nodes: Vec<(String, String)>,
    /// The kill switch's refusal, when it refused.
    pub refused: Option<String>,
}

/// The wire shape of a host's answer (`KnowledgeSearchResult`, proto3 JSON).
#[derive(Deserialize, Default)]
struct WireResult {
    #[serde(default)]
    hits: Vec<WireHit>,
    #[serde(default)]
    partial: bool,
}

#[derive(Deserialize, Default)]
struct WireHit {
    #[serde(default)]
    address: String,
    #[serde(default)]
    index: String,
    #[serde(default)]
    score: f64,
    #[serde(default)]
    snippet: String,
    #[serde(default)]
    sha256: String,
}

fn line(s: &str, n: usize) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        .take(n)
        .collect::<String>()
        .trim()
        .to_string()
}

fn short_hash(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))[..16].to_string()
}

/// Whether a node runs knowledge searches (it lists the job kind).
pub fn searches(node: &kls_store::Node) -> bool {
    node.revoked_at.is_none()
        && node
            .host
            .capabilities
            .job_kinds
            .iter()
            .any(|k| k == SEARCH_JOB)
}

/// The knowledge indexes a node advertises: (scope, key or root, docs).
pub fn indexes(node: &kls_store::Node) -> Vec<(String, String, i64)> {
    node.host
        .capabilities
        .inventory
        .iter()
        .filter(|r| r.kind == "knowledge")
        .filter_map(|r| {
            let (scope, key) = r.id.split_once(':')?;
            Some((scope.to_string(), key.to_string(), r.version))
        })
        .collect()
}

/// Searches every online node that runs searches. `scope` is "" (every
/// index), "profile" or "project"; `key` narrows to one index.
pub async fn search(
    core: &Core,
    query: &str,
    scope: &str,
    key: &str,
    limit: u32,
    origin: &str,
) -> Result<SearchReport> {
    search_on(core, &[], query, scope, key, limit, origin).await
}

/// [`search`] on the named nodes (empty: every online node that runs
/// searches). A process with no node links of its own (a driver beside
/// the app) names them; the jobs still go through the app's outbox.
pub async fn search_on(
    core: &Core,
    only: &[String],
    query: &str,
    scope: &str,
    key: &str,
    limit: u32,
    origin: &str,
) -> Result<SearchReport> {
    let query = line(query, 512);
    if query.is_empty() {
        bail!("type what to search for");
    }
    if !matches!(scope, "" | "profile" | "project") {
        bail!("unknown knowledge scope {scope:?}");
    }
    let limit = limit.clamp(1, 50);
    let mut report = SearchReport::default();
    if let Err(e) = crate::control::check(core, "job", SEARCH_JOB, origin).await {
        report.refused = Some(e.to_string());
        return Ok(report);
    }
    let gw = core.gateway()?;
    let online = gw.online().await;
    let mut nodes = Vec::new();
    for n in core.store().nodes().await? {
        let picked = if only.is_empty() {
            online.contains(&n.id)
        } else {
            only.contains(&n.id)
        };
        if picked && searches(&n) {
            nodes.push(n);
        }
    }
    let payload = json!({ "query": query, "scope": scope, "key": line(key, 256), "limit": limit })
        .to_string();
    let asks: Vec<_> = nodes
        .iter()
        .map(|n| {
            let gw = gw.clone();
            let payload = payload.clone();
            let id = n.id.clone();
            tokio::spawn(async move {
                gw.job(
                    Some(&id),
                    SEARCH_JOB,
                    &payload,
                    JobNeeds::default(),
                    SEARCH_TIMEOUT_MS,
                )
                .await
            })
        })
        .collect();
    let mut answers = Vec::new();
    for a in asks {
        answers.push(
            a.await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("search task: {e}"))),
        );
    }
    for (n, a) in nodes.iter().zip(answers) {
        match a {
            Ok(o) if o.status == "ok" => {
                let r: WireResult = serde_json::from_str(&o.output_json).unwrap_or_default();
                report.partial |= r.partial;
                for h in r.hits.into_iter().take(limit as usize) {
                    let address = line(&h.address, 300);
                    if address.is_empty() || !h.score.is_finite() {
                        continue;
                    }
                    report.hits.push(Hit {
                        node_id: n.id.clone(),
                        node_name: n.name.clone(),
                        address,
                        index: line(&h.index, 128),
                        score: h.score,
                        snippet: line(&h.snippet, 320),
                        sha256: line(&h.sha256, 64),
                    });
                }
                report.nodes.push((n.name.clone(), "ok".into()));
            }
            Ok(o) => {
                report.partial = true;
                report.nodes.push((
                    n.name.clone(),
                    line(&format!("{}: {}", o.status, o.error), 200),
                ));
            }
            Err(e) => {
                report.partial = true;
                report
                    .nodes
                    .push((n.name.clone(), line(&format!("{e:#}"), 200)));
            }
        }
    }
    report
        .hits
        .sort_by(|a, b| b.score.total_cmp(&a.score).then(a.address.cmp(&b.address)));
    report.hits.truncate(MAX_HITS);
    core.audit_record(kls_store::audit::LedgerDraft {
        kind: "knowledge_search".into(),
        actor: origin.into(),
        actor_kind: "user".into(),
        msg: format!(
            "knowledge search across {} node(s): {} hit(s)",
            nodes.len(),
            report.hits.len()
        ),
        subject: json!({ "scope": scope, "key": key }),
        // The query's hash and size, not its text.
        payload: json!({
            "query": short_hash(&query), "queryChars": query.chars().count(),
            "nodes": report.nodes,
            "nodeIds": nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), "hits": report.hits.len(), "partial": report.partial,
            "origin": origin,
        }),
        origin: "factory".into(),
        ..Default::default()
    })
    .await?;
    Ok(report)
}

/// Asks `node_id` to re-run its own ingestion of one index: a profile's
/// (`key` = profile id) or a project's (`key` = the offered root). The host
/// classifies what it ingests as always. Returns the dispatch id.
pub async fn reindex(
    core: &Core,
    node_id: &str,
    scope: &str,
    key: &str,
    origin: &str,
) -> Result<String> {
    if !matches!(scope, "profile" | "project") {
        bail!("reindex a profile or a project index");
    }
    let key = key.trim();
    if key.is_empty() || key.len() > 512 || key.chars().any(char::is_control) {
        bail!("name the index to reindex");
    }
    crate::control::check(core, "dispatch", node_id, origin).await?;
    let mut options = std::collections::BTreeMap::new();
    options.insert("scope".to_string(), scope.to_string());
    options.insert(
        // A project names its offered root (a path) or its project key.
        if scope == "project" && (key.starts_with('/') || key.contains('\\')) {
            "root"
        } else {
            "key"
        }
        .to_string(),
        key.to_string(),
    );
    let spec = DispatchSpec {
        kind: REINDEX_DISPATCH.into(),
        options,
        trigger: Some(Trigger {
            source: "console".into(),
            subject: format!("knowledge:{scope}:{key}"),
            ..Default::default()
        }),
        origin_json: json!({ "actor": origin }).to_string(),
        ..Default::default()
    };
    let id = core.gateway()?.dispatch(node_id, spec).await?;
    core.audit_record(kls_store::audit::LedgerDraft {
        kind: "knowledge_reindex".into(),
        actor: origin.into(),
        actor_kind: "user".into(),
        msg: format!("knowledge reindex {scope}:{key} on {node_id}"),
        subject: json!({ "node": node_id, "scope": scope, "key": key, "dispatch": id }),
        payload: json!({ "dispatch": id, "origin": origin }),
        origin: "factory".into(),
        node_id: node_id.into(),
        ..Default::default()
    })
    .await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{JobOutcome, NodeGateway};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Gw {
        jobs: Mutex<Vec<(String, String)>>,
        dispatched: Mutex<Vec<(String, DispatchSpec)>>,
    }

    #[async_trait::async_trait]
    impl NodeGateway for Gw {
        async fn online(&self) -> Vec<String> {
            vec!["n1".into(), "n2".into(), "n3".into()]
        }
        async fn dispatch(&self, node_id: &str, spec: DispatchSpec) -> Result<String> {
            self.dispatched.lock().unwrap().push((node_id.into(), spec));
            Ok("d1".into())
        }
        async fn job(
            &self,
            node_id: Option<&str>,
            kind: &str,
            payload: &str,
            _: JobNeeds,
            _: i64,
        ) -> Result<JobOutcome> {
            let n = node_id.unwrap_or("").to_string();
            assert_eq!(kind, SEARCH_JOB);
            self.jobs.lock().unwrap().push((n.clone(), payload.into()));
            Ok(match n.as_str() {
                "n1" => JobOutcome {
                    status: "ok".into(),
                    output_json: json!({ "hits": [
                        { "address": "kb://p/doc.md#1", "index": "patcher", "score": 0.4,
                          "snippet": "alpha\u{1b}[31m\u{202e}beta", "sha256": "ab" },
                        { "address": "", "score": 0.9 }
                    ] })
                    .to_string(),
                    ..Default::default()
                },
                _ => JobOutcome {
                    status: "ok".into(),
                    output_json: json!({ "hits": [
                        { "address": "kb://q/x.md#2", "index": "project", "score": 0.8, "snippet": "x" }
                    ], "partial": true })
                    .to_string(),
                    ..Default::default()
                },
            })
        }
    }

    async fn setup() -> (Core, Arc<Gw>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        let gw = Arc::new(Gw::default());
        c.set_gateway(gw.clone());
        let mut h = kls_store::HostInfo::default();
        h.capabilities.job_kinds = vec![SEARCH_JOB.into()];
        h.capabilities.inventory = vec![kls_store::InventoryRef {
            kind: "knowledge".into(),
            id: "profile:p-1".into(),
            version: 3,
            ..Default::default()
        }];
        c.store()
            .insert_node("n1", "one", &h, "s1", "t")
            .await
            .unwrap();
        c.store()
            .insert_node("n2", "two", &h, "s2", "t")
            .await
            .unwrap();
        // n3 does not run searches.
        c.store()
            .insert_node("n3", "three", &kls_store::HostInfo::default(), "s3", "t")
            .await
            .unwrap();
        (c, gw, dir)
    }

    #[tokio::test]
    async fn search_fans_out_merges_cleans_and_audits() {
        let (c, gw, _d) = setup().await;
        let r = search(&c, "how to deploy", "", "", 10, "owner")
            .await
            .unwrap();
        let asked: Vec<String> = gw
            .jobs
            .lock()
            .unwrap()
            .iter()
            .map(|j| j.0.clone())
            .collect();
        assert_eq!(asked.len(), 2, "only nodes that run searches: {asked:?}");
        assert!(
            gw.jobs.lock().unwrap()[0]
                .1
                .contains("\"query\":\"how to deploy\"")
        );
        assert_eq!(r.hits.len(), 2, "the hit with no address is dropped");
        assert_eq!(r.hits[0].node_id, "n2");
        assert_eq!(r.hits[1].snippet, "alpha [31mbeta");
        assert!(r.partial);
        let e = c
            .store()
            .ledger(&kls_store::audit::LedgerQuery {
                kinds: vec!["knowledge_search".into()],
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(e.len(), 1);
        assert!(
            !e[0].payload.to_string().contains("how to deploy"),
            "the query text is not kept"
        );
        assert_eq!(
            indexes(&c.store().node("n1").await.unwrap().unwrap())[0].1,
            "p-1"
        );
    }

    #[tokio::test]
    async fn search_and_reindex_are_refused_under_the_kill_switch() {
        let (c, gw, _d) = setup().await;
        crate::control::pause(&c, "t").await.unwrap();
        let r = search(&c, "x", "", "", 5, "owner").await.unwrap();
        assert!(r.refused.is_some());
        assert!(gw.jobs.lock().unwrap().is_empty());
        assert!(reindex(&c, "n1", "profile", "p-1", "owner").await.is_err());
        assert!(gw.dispatched.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reindex_dispatches_with_provenance_and_audit() {
        let (c, gw, _d) = setup().await;
        assert!(reindex(&c, "n1", "other", "x", "owner").await.is_err());
        let id = reindex(&c, "n1", "project", "/src/app", "owner")
            .await
            .unwrap();
        assert_eq!(id, "d1");
        let d = gw.dispatched.lock().unwrap().clone();
        assert_eq!(d[0].1.kind, REINDEX_DISPATCH);
        assert_eq!(d[0].1.options["root"], "/src/app");
        assert_eq!(
            d[0].1.trigger.as_ref().unwrap().subject,
            "knowledge:project:/src/app"
        );
        let e = c
            .store()
            .ledger(&kls_store::audit::LedgerQuery {
                kinds: vec!["knowledge_reindex".into()],
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(e.len(), 1);
    }
}
