//! Project overrides: belai's per-project preferences (`config.ProjectPrefs`)
//! for one directory on one node, set from Settings > Project overrides.
//!
//! A change goes to the node as a `project_prefs` dispatch through the
//! gateway (the host must run with `--web-project-settings`). It is checked
//! against the kill switch first, and every send, refusal and
//! acknowledgement is an audited `settings.project_prefs` event. The node
//! acknowledges by reporting an inventory whose repository prefs match.
//! Until then the change is pending; [`project_overrides`] says which.

use crate::{Core, DispatchSpec};
use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub mod kinds {
    /// A project prefs change was sent, refused or acknowledged:
    /// `outcome` "sent" | "refused" | "applied" | "not_applied".
    pub const PROJECT_PREFS: &str = "settings.project_prefs";
}

const KEY_MAX: usize = 64;
const VALUE_MAX: usize = 512;

/// A prefs key: lower-case letters, digits, `_`, `.`, `-`.
pub fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= KEY_MAX
        && k.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.-".contains(c))
}

/// Checks a change before it is sent.
pub fn validate(set: &BTreeMap<String, String>, unset: &[String]) -> Result<()> {
    if set.is_empty() && unset.is_empty() {
        bail!("nothing to change");
    }
    for k in set.keys().chain(unset.iter()) {
        if !valid_key(k) {
            bail!("{k:?} is not a project setting key");
        }
    }
    for (k, v) in set {
        if v.chars().count() > VALUE_MAX || v.contains(['\n', '\r']) {
            bail!("the value of {k} must be one line of at most {VALUE_MAX} characters");
        }
        if unset.contains(k) {
            bail!("{k} is both set and cleared");
        }
    }
    Ok(())
}

/// Whether `prefs` reflect the change.
pub fn applied(
    prefs: &BTreeMap<String, String>,
    set: &BTreeMap<String, String>,
    unset: &[String],
) -> bool {
    set.iter().all(|(k, v)| prefs.get(k) == Some(v)) && unset.iter().all(|k| !prefs.contains_key(k))
}

/// The state of the last change to a project.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub at: i64,
    /// "sent" (waiting for the node), "applied", "not_applied", "refused".
    pub state: String,
    pub set: BTreeMap<String, String>,
    pub unset: Vec<String>,
    pub dispatch_id: String,
    pub reason: String,
}

/// One project directory a node offers, with its prefs as last reported.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRow {
    pub node_id: String,
    pub node_name: String,
    pub online: bool,
    /// The host allows web edits (`--web-project-settings`).
    pub editable: bool,
    pub path: String,
    /// `owner/name` when it is a forge repository.
    pub repo: String,
    pub prefs: BTreeMap<String, String>,
    /// When the node last reported (Unix ms).
    pub reported_at: i64,
    pub last_change: Option<Change>,
}

fn change_of(d: &Value) -> Change {
    Change {
        at: 0,
        state: d["outcome"].as_str().unwrap_or("").to_string(),
        set: serde_json::from_value(d["set"].clone()).unwrap_or_default(),
        unset: serde_json::from_value(d["unset"].clone()).unwrap_or_default(),
        dispatch_id: d["dispatchId"].as_str().unwrap_or("").to_string(),
        reason: d["reason"].as_str().unwrap_or("").to_string(),
    }
}

/// The last change per (node, path), newest event wins.
async fn last_changes(core: &Core) -> Result<BTreeMap<(String, String), Change>> {
    let mut out = BTreeMap::new();
    for ev in core
        .store()
        .events_with_prefix(kinds::PROJECT_PREFS, None, 2000)
        .await?
    {
        let d: Value = serde_json::from_str(&ev.data_json).unwrap_or_default();
        let key = (
            d["nodeId"].as_str().unwrap_or("").to_string(),
            d["dir"].as_str().unwrap_or("").to_string(),
        );
        out.entry(key).or_insert_with(|| Change {
            at: ev.at,
            ..change_of(&d)
        });
    }
    Ok(out)
}

/// Every project every joined node reports, with its prefs and the state of
/// its last change.
pub async fn project_overrides(core: &Core) -> Result<Vec<ProjectRow>> {
    let online = match core.gateway() {
        Ok(g) => g.online().await,
        Err(_) => Vec::new(),
    };
    let changes = last_changes(core).await?;
    let mut out = Vec::new();
    for n in core
        .store()
        .nodes()
        .await?
        .iter()
        .filter(|n| n.revoked_at.is_none())
    {
        let Some((at, inv)) = crate::hosts::inventory(core, &n.id).await? else {
            continue;
        };
        for r in inv.repos {
            let repo = if r.owner.is_empty() {
                String::new()
            } else {
                format!("{}/{}", r.owner, r.name)
            };
            let last_change = changes.get(&(n.id.clone(), r.path.clone())).cloned();
            out.push(ProjectRow {
                node_id: n.id.clone(),
                node_name: n.name.clone(),
                online: online.contains(&n.id),
                editable: inv.gates.web_project_settings,
                path: r.path,
                repo,
                prefs: r.prefs,
                reported_at: at,
                last_change,
            });
        }
    }
    out.sort_by(|a, b| (&a.node_name, &a.path).cmp(&(&b.node_name, &b.path)));
    Ok(out)
}

/// Sends a prefs change for `dir` on `node_id`. Refused (and audited) under
/// the kill switch, for a directory the node does not report, or a host
/// without `--web-project-settings`. Returns the dispatch id.
pub async fn set_project_prefs(
    core: &Core,
    node_id: &str,
    dir: &str,
    set: &BTreeMap<String, String>,
    unset: &[String],
    by: &str,
) -> Result<String> {
    validate(set, unset)?;
    let base = json!({"nodeId": node_id, "dir": dir, "set": set, "unset": unset, "by": by});
    let refuse = |reason: String| {
        let mut d = base.clone();
        d["outcome"] = json!("refused");
        d["reason"] = json!(reason);
        d
    };
    if let Err(e) = crate::control::check(core, "dispatch", node_id, by).await {
        core.emit(kinds::PROJECT_PREFS, node_id, &refuse(e.to_string()))
            .await?;
        return Err(e);
    }
    let (_, inv) = crate::hosts::inventory(core, node_id)
        .await?
        .ok_or_else(|| anyhow!("the node has not reported its projects yet"))?;
    let why = if !inv.gates.web_project_settings {
        Some("the host does not allow project settings (start it with --web-project-settings)")
    } else if !inv.repos.iter().any(|r| r.path == dir) {
        Some("the node does not offer that directory")
    } else {
        None
    };
    if let Some(w) = why {
        core.emit(kinds::PROJECT_PREFS, node_id, &refuse(w.into()))
            .await?;
        bail!("{w}");
    }
    let spec = DispatchSpec {
        kind: "project_prefs".into(),
        cwd: dir.into(),
        prefs_set: set.clone(),
        prefs_unset: unset.to_vec(),
        trigger: Some(crate::gateway::Trigger {
            source: "console".into(),
            subject: dir.into(),
            ..Default::default()
        }),
        origin_json: json!({"actor": by, "from": "settings.project_overrides"}).to_string(),
        ..Default::default()
    };
    let gw = core.gateway()?;
    let id = match gw.dispatch(node_id, spec).await {
        Ok(id) => id,
        Err(e) => {
            core.emit(kinds::PROJECT_PREFS, node_id, &refuse(format!("{e:#}")))
                .await?;
            return Err(e);
        }
    };
    let mut d = base;
    d["outcome"] = json!("sent");
    d["dispatchId"] = json!(id);
    core.emit(kinds::PROJECT_PREFS, node_id, &d).await?;
    Ok(id)
}

/// After a node reports an inventory: each change still waiting on it is
/// acknowledged as applied or not applied (audited). Returns how many.
pub async fn settle_acks(core: &Core, node_id: &str) -> Result<usize> {
    let Some((at, inv)) = crate::hosts::inventory(core, node_id).await? else {
        return Ok(0);
    };
    let mut n = 0;
    for ((node, dir), c) in last_changes(core).await? {
        if node != node_id || c.state != "sent" || at < c.at {
            continue;
        }
        let Some(r) = inv.repos.iter().find(|r| r.path == dir) else {
            continue;
        };
        let ok = applied(&r.prefs, &c.set, &c.unset);
        core.emit(
            kinds::PROJECT_PREFS,
            node_id,
            &json!({
                "nodeId": node_id, "dir": dir, "set": c.set, "unset": c.unset,
                "dispatchId": c.dispatch_id, "by": "node:".to_string() + node_id,
                "outcome": if ok { "applied" } else { "not_applied" },
                "reason": if ok { "" } else { "the node reported other values" },
            }),
        )
        .await?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JobNeeds, JobOutcome, NodeGateway};
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
        async fn dispatch(&self, node_id: &str, spec: DispatchSpec) -> Result<String> {
            self.sent.lock().unwrap().push((node_id.into(), spec));
            Ok("d1".into())
        }
        async fn job(
            &self,
            _: Option<&str>,
            _: &str,
            _: &str,
            _: JobNeeds,
            _: i64,
        ) -> Result<JobOutcome> {
            bail!("no jobs")
        }
    }

    fn map(kv: &[(&str, &str)]) -> BTreeMap<String, String> {
        kv.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    async fn setup(editable: bool) -> (Core, Arc<Gw>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        let gw = Arc::new(Gw::default());
        c.set_gateway(gw.clone());
        let inv = json!({
            "at_ms": 1, "gates": {"web_project_settings": editable},
            "repos": [{"path": "~/src/api", "owner": "acme", "name": "api", "prefs": {"scan.sca": "true"}}],
        });
        c.store()
            .insert_node("n1", "box", &Default::default(), "serial", "tok")
            .await
            .unwrap();
        c.store()
            .upsert_host_inventory("n1", 1, &inv.to_string())
            .await
            .unwrap();
        (c, gw, dir)
    }

    #[test]
    fn validation_and_ack_rule() {
        assert!(validate(&map(&[("scan.sca", "false")]), &[]).is_ok());
        assert!(validate(&BTreeMap::new(), &[]).is_err());
        assert!(validate(&map(&[("Bad Key", "x")]), &[]).is_err());
        assert!(validate(&map(&[("a", "x\ny")]), &[]).is_err());
        assert!(validate(&map(&[("a", "1")]), &["a".into()]).is_err());
        let p = map(&[("a", "1")]);
        assert!(applied(&p, &map(&[("a", "1")]), &["b".into()]));
        assert!(!applied(&p, &BTreeMap::new(), &["a".into()]));
    }

    #[tokio::test]
    async fn sends_a_dispatch_audits_and_acks() {
        let (c, gw, _d) = setup(true).await;
        let rows = project_overrides(&c).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].repo, "acme/api");
        assert!(rows[0].editable && rows[0].last_change.is_none());
        let id = set_project_prefs(
            &c,
            "n1",
            "~/src/api",
            &map(&[("scan.sca", "false")]),
            &[],
            "owner",
        )
        .await
        .unwrap();
        assert_eq!(id, "d1");
        let (node, spec) = gw.sent.lock().unwrap()[0].clone();
        assert_eq!(
            (node.as_str(), spec.kind.as_str(), spec.cwd.as_str()),
            ("n1", "project_prefs", "~/src/api")
        );
        assert_eq!(
            spec.prefs_set.get("scan.sca").map(String::as_str),
            Some("false")
        );
        let rows = project_overrides(&c).await.unwrap();
        assert_eq!(rows[0].last_change.as_ref().unwrap().state, "sent");
        // The node reports the new value: acknowledged.
        let inv = json!({
            "at_ms": 2, "gates": {"web_project_settings": true},
            "repos": [{"path": "~/src/api", "owner": "acme", "name": "api", "prefs": {"scan.sca": "false"}}],
        });
        c.store()
            .upsert_host_inventory("n1", kls_store::now_ms() + 1, &inv.to_string())
            .await
            .unwrap();
        assert_eq!(settle_acks(&c, "n1").await.unwrap(), 1);
        let rows = project_overrides(&c).await.unwrap();
        assert_eq!(rows[0].last_change.as_ref().unwrap().state, "applied");
        assert_eq!(settle_acks(&c, "n1").await.unwrap(), 0, "acked once");
        c.audit_sync().await.unwrap();
        let led = c
            .audit_entries(&kls_store::audit::LedgerQuery::default())
            .await
            .unwrap();
        assert!(
            led.iter()
                .filter(|e| e.msg.contains("settings.project_prefs"))
                .count()
                >= 2
        );
    }

    #[tokio::test]
    async fn refused_under_the_kill_switch_and_without_the_host_gate() {
        let (c, gw, _d) = setup(false).await;
        assert!(
            set_project_prefs(&c, "n1", "~/src/api", &map(&[("a", "1")]), &[], "owner")
                .await
                .unwrap_err()
                .to_string()
                .contains("web-project-settings")
        );
        let (c2, gw2, _d2) = setup(true).await;
        assert!(
            set_project_prefs(&c2, "n1", "~/other", &map(&[("a", "1")]), &[], "owner")
                .await
                .is_err()
        );
        crate::control::pause(&c2, "freeze").await.unwrap();
        let e = set_project_prefs(&c2, "n1", "~/src/api", &map(&[("a", "1")]), &[], "owner")
            .await
            .unwrap_err();
        assert!(crate::control::is_blocked(&e));
        assert!(gw.sent.lock().unwrap().is_empty() && gw2.sent.lock().unwrap().is_empty());
        let refused = c2
            .store()
            .recent_events(1000)
            .await
            .unwrap()
            .iter()
            .filter(|e| {
                e.kind == kinds::PROJECT_PREFS && e.data_json.contains("\"outcome\":\"refused\"")
            })
            .count();
        assert_eq!(refused, 2);
    }
}
