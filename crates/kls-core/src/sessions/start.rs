//! Start fields of a `session`, `worker` or `crew` dispatch (P3 dispatch
//! fidelity, belai-counterparts #7, #8, #26): extra directories, workspace,
//! effort, git sync, ask, crew fill, stay, clone and MCP bindings. The New
//! session dialog and the engine's steps fill them through [`StartFields`];
//! the dialog sends through [`dispatch`], which checks the kill switch,
//! stamps the provenance and audits the outcome.
//!
//! MCP bindings name a vault env secret and the (server, key) it feeds on the
//! host. Only names are stored and logged here; the node gateway resolves the
//! value into `VaultGrant.mcp` at send time.

use crate::Core;
use crate::control;
use crate::gateway::{AdhocWorker, CloneSpec, DispatchSpec, McpBinding, Trigger};
use crate::hosts::Origin;
use anyhow::{Result, bail};
use kls_store::audit::LedgerDraft;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Extra directories a session may take (belai `MaxExtraDirs`).
pub const MAX_DIRS: usize = 16;
/// MCP bindings one dispatch may carry (the node takes 32).
pub const MAX_MCP: usize = 32;
/// The longest idle wait belai lets a staying worker have (24 h).
pub const MAX_STAY_IDLE_S: u32 = 86_400;
/// Effort levels belai profiles take.
pub const EFFORTS: [&str; 4] = ["low", "medium", "high", "none"];
/// The settings key the stored MCP bindings live under (names only).
pub const MCP_KEY: &str = "mcp.bindings";

/// Event kinds of this module.
pub mod kinds {
    /// A start dispatch left the console. Data: `{nodeId, kind, fields, origin, dispatchId}`.
    pub const START_DISPATCHED: &str = "dispatch.start";
    /// A start dispatch was refused here. Data: `{nodeId, kind, reason, origin}`.
    pub const START_REFUSED: &str = "dispatch.start_refused";
    /// An MCP binding was added or removed. Data: `{server, key, secret, op, origin}`.
    pub const MCP_BINDING: &str = "vault.mcp_binding";
}

/// The optional start fields of a dispatch. Empty values send nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StartFields {
    pub dirs: Vec<String>,
    pub workspace: String,
    pub effort: String,
    pub git_sync: Option<bool>,
    pub ask: Option<bool>,
    pub fill: bool,
    pub stay: bool,
    pub stay_idle_max_s: u32,
    pub clone: Option<CloneSpec>,
    pub mcp: Vec<McpBinding>,
    /// #52: scripted answers for a session (belai `-answers` JSON).
    #[serde(default)]
    pub answers_json: String,
    /// #51: harness gates for a session, merged into `Dispatch.options`.
    #[serde(default)]
    pub gates: BTreeMap<String, String>,
    /// #55: a worker made from a prompt.
    #[serde(default)]
    pub adhoc: Option<AdhocWorker>,
}

fn plain(s: &str, extra: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// An env-var name as the vault stores it.
fn env_name(s: &str) -> bool {
    plain(s, "_", 128)
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars().all(|c| !c.is_ascii_lowercase())
}

/// A workspace as belai takes it: one to four plain segments.
fn workspace_ok(ws: &str) -> bool {
    let segs: Vec<&str> = ws.split('/').collect();
    ws.len() <= 200
        && (1..=4).contains(&segs.len())
        && segs
            .iter()
            .all(|s| plain(s, "._-", 200) && *s != "." && *s != "..")
}

/// Largest scripted answers list (belai `maxAnswersBytes`).
pub const MAX_ANSWERS: usize = 64 * 1024;
/// Largest ad hoc worker prompt.
pub const MAX_ADHOC_PROMPT: usize = 16 * 1024;
/// Most tools an ad hoc worker may list.
pub const MAX_ADHOC_TOOLS: usize = 64;
/// Harness gate keys (#51) and the values each takes. Every value only
/// tightens a gate; belai ignores (and audits) one that would loosen it.
pub const GATES: [(&str, &[&str]); 5] = [
    ("finish_review", &["on"]),
    ("lint_loop", &["on"]),
    ("checkpoints", &["on"]),
    ("code_mode", &["off"]),
    (
        "commit_mode",
        &["off", "ask", "auto", "none", "end", "per_task"],
    ),
];
/// Ad hoc worker publish modes ("" = agent).
pub const PUBLISH: [&str; 4] = ["", "none", "draft_pr", "agent"];

/// The hex SHA-256 of `s` (audit lines carry this, never the text).
pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// The optional dispatch fields `spec` uses, as the host advertises them in
/// `Capabilities.dispatch_features`. The gateway refuses a node lacking one.
pub fn features_needed(spec: &DispatchSpec) -> Vec<&'static str> {
    let mut out = Vec::new();
    if !spec.resume_session.is_empty() {
        out.push("resume");
    }
    if !spec.fork_entry.is_empty() {
        out.push("fork");
    }
    if !spec.answers_json.is_empty() {
        out.push("answers");
    }
    if spec.adhoc.is_some() {
        out.push("adhoc");
    }
    if GATES.iter().any(|(k, _)| spec.options.contains_key(*k)) {
        out.push("gates");
    }
    out
}

/// Checks a scripted answers list; returns how many answers it holds.
pub fn check_answers(json: &str) -> Result<usize, String> {
    if json.len() > MAX_ANSWERS {
        return Err(format!("the answers are larger than {MAX_ANSWERS} bytes"));
    }
    let v: Value =
        serde_json::from_str(json).map_err(|_| "the answers are not JSON".to_string())?;
    let list = v
        .as_array()
        .ok_or("the answers are a JSON array of {choose, note, skip, match}")?;
    for (i, a) in list.iter().enumerate() {
        let o = a
            .as_object()
            .ok_or(format!("answer {i} is not an object"))?;
        for (k, v) in o {
            let ok = match k.as_str() {
                "choose" => v.as_u64().is_some(),
                "note" | "match" => v.is_string(),
                "skip" => v.is_boolean(),
                _ => return Err(format!("answer {i}: unknown key {k:?}")),
            };
            if !ok {
                return Err(format!("answer {i}: {k} has the wrong type"));
            }
        }
    }
    Ok(list.len())
}

/// Checks a gate key and value (#51).
pub fn check_gate(key: &str, value: &str) -> Result<(), String> {
    match GATES.iter().find(|(k, _)| *k == key) {
        None => Err(format!("{key:?} is not a gate")),
        Some((_, vals)) if !vals.contains(&value) => {
            Err(format!("{key} takes {}", vals.join(", ")))
        }
        Some(_) => Ok(()),
    }
}

/// Checks an ad hoc worker spec (#55).
pub fn check_adhoc(a: &AdhocWorker) -> Result<(), String> {
    if a.prompt.trim().is_empty() {
        return Err("a one-off worker needs a prompt".into());
    }
    if a.prompt.len() > MAX_ADHOC_PROMPT {
        return Err(format!(
            "the prompt is larger than {MAX_ADHOC_PROMPT} bytes"
        ));
    }
    if a.tools.len() > MAX_ADHOC_TOOLS {
        return Err(format!("at most {MAX_ADHOC_TOOLS} tools"));
    }
    if let Some(t) = a.tools.iter().find(|t| !plain(t, "_-.:", 64)) {
        return Err(format!("{t:?} is not a tool name"));
    }
    if a.claim_lists.len() > 1 {
        return Err("a one-off worker claims from one list".into());
    }
    if let Some(l) = a
        .claim_lists
        .iter()
        .chain(&a.claim_labels)
        .chain(std::iter::once(&a.to).filter(|t| !t.is_empty()))
        .find(|l| !plain(l, "_-.:/ ", 64))
    {
        return Err(format!("{l:?} is not a list or label name"));
    }
    if a.claim_labels.len() > 16 {
        return Err("at most 16 labels".into());
    }
    if !PUBLISH.contains(&a.publish.as_str()) {
        return Err("publish is none, draft_pr or agent".into());
    }
    Ok(())
}

/// An ad hoc worker for an audit line: the prompt's hash and size, never
/// its text.
pub fn adhoc_audit(a: &AdhocWorker) -> Value {
    json!({
        "promptSha256": sha256_hex(&a.prompt), "promptBytes": a.prompt.len(),
        "tools": a.tools, "claimLists": a.claim_lists, "claimLabels": a.claim_labels,
        "to": a.to, "publish": a.publish, "readOnly": a.read_only,
    })
}

fn dir_ok(d: &str) -> bool {
    (d.starts_with('/') || d.get(1..3) == Some(":\\") || d.get(1..3) == Some(":/"))
        && d.len() <= 1024
        && !d.chars().any(char::is_control)
        && !d.split(['/', '\\']).any(|s| s == "..")
}

impl StartFields {
    /// Whether nothing is set.
    pub fn is_empty(&self) -> bool {
        *self == StartFields::default()
    }

    /// Reads a workflow step's config (camelCase keys): `dirs`, `workspace`,
    /// `effort`, `gitSync`, `ask`, `fill`, `stay`, `stayIdleMaxS`, `clone`
    /// (`"owner/name"` or `{owner, name, branch}`) and `mcp`
    /// (`[{server, key, secret}]`). A value of the wrong type is an error.
    pub fn from_config(cfg: &Map<String, Value>) -> Result<StartFields, String> {
        let mut f = StartFields::default();
        let s = |k: &str| -> Result<String, String> {
            match cfg.get(k) {
                None | Some(Value::Null) => Ok(String::new()),
                Some(Value::String(s)) => Ok(s.trim().to_string()),
                Some(_) => Err(format!("{k} is not text")),
            }
        };
        let b = |k: &str| -> Result<Option<bool>, String> {
            match cfg.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Bool(v)) => Ok(Some(*v)),
                Some(Value::String(v)) if v == "true" || v == "on" => Ok(Some(true)),
                Some(Value::String(v)) if v == "false" || v == "off" => Ok(Some(false)),
                Some(Value::String(v)) if v.is_empty() => Ok(None),
                Some(_) => Err(format!("{k} is not on or off")),
            }
        };
        f.dirs = match cfg.get("dirs") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(s)) => s
                .split([',', '\n'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            Some(Value::Array(a)) => a
                .iter()
                .map(|v| v.as_str().map(|s| s.trim().to_string()))
                .collect::<Option<Vec<_>>>()
                .ok_or("dirs holds a value that is not text")?,
            Some(_) => return Err("dirs is not a list".into()),
        };
        f.workspace = s("workspace")?;
        f.effort = s("effort")?.to_ascii_lowercase();
        f.git_sync = b("gitSync")?;
        f.ask = b("ask")?;
        f.fill = b("fill")?.unwrap_or(false);
        f.stay = b("stay")?.unwrap_or(false);
        f.stay_idle_max_s = match cfg.get("stayIdleMaxS") {
            None | Some(Value::Null) => 0,
            Some(v) => v
                .as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                .and_then(|n| u32::try_from(n).ok())
                .ok_or("stayIdleMaxS is not a number of seconds")?,
        };
        f.clone = match cfg.get("clone") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.trim().is_empty() => None,
            Some(Value::String(s)) => {
                let (repo, branch) = s.trim().split_once('@').unwrap_or((s.trim(), ""));
                let (owner, name) = repo.split_once('/').ok_or("clone is owner/name")?;
                Some(CloneSpec {
                    owner: owner.into(),
                    name: name.into(),
                    branch: branch.into(),
                })
            }
            Some(Value::Object(o)) => {
                let t = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").trim();
                Some(CloneSpec {
                    owner: t("owner").into(),
                    name: t("name").into(),
                    branch: t("branch").into(),
                })
            }
            Some(_) => return Err("clone is owner/name".into()),
        };
        f.mcp = match cfg.get("mcp") {
            None | Some(Value::Null) => Vec::new(),
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|_| "mcp is a list of {server, key, secret}")?,
        };
        f.answers_json = match cfg.get("answers") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.trim().to_string(),
            Some(v @ Value::Array(_)) => v.to_string(),
            Some(_) => return Err("answers is a JSON array".into()),
        };
        if let Some(g) = cfg.get("gates").filter(|g| !g.is_null()) {
            let o = g.as_object().ok_or("gates is {key: value}")?;
            for (k, v) in o {
                let v = match v {
                    Value::String(s) => s.clone(),
                    Value::Bool(true) => "on".into(),
                    Value::Bool(false) => "off".into(),
                    _ => return Err(format!("gate {k} is not text")),
                };
                f.gates.insert(k.clone(), v);
            }
        }
        f.adhoc = match cfg.get("adhoc") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                serde_json::from_value(v.clone())
                    .map_err(|_| "adhoc is {prompt, tools, claim_lists, ...}")?,
            ),
        };
        Ok(f)
    }

    /// Checks every set field against what belai takes for `kind`.
    pub fn validate(&self, kind: &str) -> Result<(), String> {
        let session = kind == "session";
        let workers = matches!(kind, "worker" | "crew");
        if !session && !workers {
            return Err(format!("start fields do not apply to a {kind} dispatch"));
        }
        if self.dirs.len() > MAX_DIRS {
            return Err(format!("at most {MAX_DIRS} extra directories"));
        }
        if let Some(d) = self.dirs.iter().find(|d| !dir_ok(d)) {
            return Err(format!("{d:?} is not an absolute directory"));
        }
        if !self.dirs.is_empty() && !session {
            return Err("extra directories apply to a session".into());
        }
        if !self.workspace.is_empty() {
            if !session {
                return Err("a workspace applies to a session".into());
            }
            if !workspace_ok(&self.workspace) {
                return Err("a workspace is one to four plain segments".into());
            }
        }
        if !self.effort.is_empty() && !EFFORTS.contains(&self.effort.as_str()) {
            return Err(format!("effort is one of {}", EFFORTS.join(", ")));
        }
        if self.git_sync.is_some() && !session {
            return Err("git sync applies to a session".into());
        }
        if self.fill && kind != "crew" {
            return Err("fill applies to a crew".into());
        }
        if (self.stay || self.stay_idle_max_s > 0) && !workers {
            return Err("stay applies to a worker or a crew".into());
        }
        if self.stay_idle_max_s > MAX_STAY_IDLE_S {
            return Err(format!("the idle wait is at most {MAX_STAY_IDLE_S} s"));
        }
        if self.stay_idle_max_s > 0 && !self.stay {
            return Err("an idle wait needs stay".into());
        }
        if let Some(c) = &self.clone {
            if !workers {
                return Err("a clone applies to a worker or a crew".into());
            }
            if !plain(&c.owner, "._-", 100) || !plain(&c.name, "._-", 100) {
                return Err("clone is owner/name".into());
            }
            if !c.branch.is_empty() && (!plain(&c.branch, "._-/", 200) || c.branch.contains("..")) {
                return Err("that is not a branch name".into());
            }
        }
        if self.mcp.len() > MAX_MCP {
            return Err(format!("at most {MAX_MCP} MCP bindings"));
        }
        for m in &self.mcp {
            check_binding(m)?;
        }
        if !self.answers_json.is_empty() {
            if !session {
                return Err("answers apply to a session".into());
            }
            check_answers(&self.answers_json)?;
        }
        if !self.gates.is_empty() && !session {
            return Err("gates apply to a session".into());
        }
        for (k, v) in &self.gates {
            check_gate(k, v)?;
        }
        if let Some(a) = &self.adhoc {
            if kind != "worker" {
                return Err("a one-off worker is a worker dispatch".into());
            }
            check_adhoc(a)?;
        }
        Ok(())
    }

    /// Writes the fields onto `spec`.
    pub fn apply(&self, spec: &mut DispatchSpec) {
        spec.dirs = self.dirs.clone();
        spec.workspace = self.workspace.clone();
        spec.effort = self.effort.clone();
        spec.git_sync = self.git_sync;
        spec.ask = self.ask;
        spec.fill = self.fill;
        spec.stay = self.stay;
        spec.stay_idle_max_s = self.stay_idle_max_s;
        spec.clone = self.clone.clone();
        spec.mcp = self.mcp.clone();
        spec.answers_json = self.answers_json.clone();
        spec.options
            .extend(self.gates.iter().map(|(k, v)| (k.clone(), v.clone())));
        if self.adhoc.is_some() {
            spec.adhoc = self.adhoc.clone();
            spec.agent.clear();
        }
    }

    /// The fields for an audit line: names and flags, never a value.
    pub fn audit_json(&self) -> Value {
        json!({
            "dirs": self.dirs, "workspace": self.workspace, "effort": self.effort,
            "gitSync": self.git_sync, "ask": self.ask, "fill": self.fill, "stay": self.stay,
            "stayIdleMaxS": self.stay_idle_max_s,
            "clone": self.clone.as_ref().map(|c| format!("{}/{}@{}", c.owner, c.name, c.branch)),
            "mcp": self.mcp.iter().map(|m| format!("{}:{}<-{}", m.server, m.key, m.secret)).collect::<Vec<_>>(),
            "answers": (!self.answers_json.is_empty()).then(|| json!({
                "count": check_answers(&self.answers_json).unwrap_or(0),
                "sha256": sha256_hex(&self.answers_json),
            })),
            "gates": self.gates,
            "adhoc": self.adhoc.as_ref().map(adhoc_audit),
        })
    }
}

/// Checks one binding's names.
pub fn check_binding(m: &McpBinding) -> Result<(), String> {
    if !plain(&m.server, "._-", 64) {
        return Err(format!("{:?} is not an MCP server name", m.server));
    }
    if !plain(&m.key, "._-", 64) {
        return Err(format!("{:?} is not an MCP key name", m.key));
    }
    if !env_name(&m.secret) {
        return Err(format!("{:?} is not a vault env secret name", m.secret));
    }
    Ok(())
}

pub(crate) async fn audit(
    core: &Core,
    kind: &str,
    actor_kind: &str,
    msg: String,
    subject: Value,
    payload: Value,
    node_id: &str,
) {
    let draft = LedgerDraft {
        kind: kind.into(),
        actor: "owner".into(),
        actor_kind: actor_kind.into(),
        msg,
        subject,
        payload,
        node_id: node_id.into(),
        ..Default::default()
    };
    // A refusal is always audited: retry a write that raced another.
    for i in 1..=5u64 {
        match core.audit_record(draft.clone()).await {
            Ok(_) => break,
            Err(e) if i == 5 => tracing::warn!("audit of a start dispatch: {e:#}"),
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(40 * i)).await,
        }
    }
}

pub(crate) async fn refuse(
    core: &Core,
    node_id: &str,
    spec: &DispatchSpec,
    origin: &Origin,
    why: &str,
) {
    let _ = core
        .emit(
            kinds::START_REFUSED,
            node_id,
            &json!({ "nodeId": node_id, "kind": spec.kind, "reason": why, "origin": origin.label() }),
        )
        .await;
    audit(
        core,
        "dispatch",
        "rule",
        format!("{} dispatch refused: {why}", spec.kind),
        json!({ "node": node_id, "kind": spec.kind }),
        json!({ "outcome": "refused", "reason": why, "origin": origin.label() }),
        node_id,
    )
    .await;
}

/// Sends a start dispatch from the console: `spec` with `fields` on it.
/// Refused (audited, with an event) under the kill switch or when a field
/// does not fit the kind; otherwise the trigger names the console surface
/// and the audit line lists every field by name.
pub async fn dispatch(
    core: &Core,
    node_id: &str,
    mut spec: DispatchSpec,
    fields: &StartFields,
    origin: &Origin,
) -> Result<String> {
    if spec.kind.is_empty() {
        spec.kind = "session".into();
    }
    if let Err(e) = control::check(core, "dispatch", node_id, &origin.label()).await {
        let why = match e.downcast_ref::<control::Blocked>() {
            Some(b) if b.window => "held".to_string(),
            Some(b) => format!("kill switch: {}", b.mode.as_str()),
            None => return Err(e),
        };
        refuse(core, node_id, &spec, origin, &why).await;
        bail!("refused: {why}");
    }
    let why = fields.validate(&spec.kind).and_then(|()| {
        if fields.adhoc.is_some() && !spec.agent.is_empty() {
            Err("a one-off worker names no agent profile".to_string())
        } else {
            Ok(())
        }
    });
    if let Err(why) = why {
        refuse(core, node_id, &spec, origin, &why).await;
        bail!("{why}");
    }
    fields.apply(&mut spec);
    // The subject stays the caller's label (O-008); the surface goes to
    // the audit line and the event.
    if spec.trigger.is_none() {
        spec.trigger = Some(Trigger {
            source: "console".into(),
            ..Default::default()
        });
    }
    let kind = spec.kind.clone();
    let id = match core.gateway()?.dispatch(node_id, spec).await {
        Ok(id) => id,
        Err(e) => {
            refuse(
                core,
                node_id,
                &DispatchSpec {
                    kind,
                    ..Default::default()
                },
                origin,
                &format!("{e:#}"),
            )
            .await;
            return Err(e);
        }
    };
    let data = json!({
        "nodeId": node_id, "kind": kind, "fields": fields.audit_json(),
        "origin": origin, "dispatchId": id,
    });
    core.emit(kinds::START_DISPATCHED, &id, &data).await?;
    audit(
        core,
        "dispatch",
        "user",
        format!("{kind} dispatch {id} to {node_id} by {}", origin.label()),
        json!({ "node": node_id, "kind": kind, "dispatch": id }),
        json!({ "outcome": "dispatched", "fields": fields.audit_json(), "origin": origin }),
        node_id,
    )
    .await;
    Ok(id)
}

// ---- MCP bindings ----------------------------------------------------------------

/// A stored binding: the names plus who made it and when (provenance).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredBinding {
    #[serde(flatten)]
    pub binding: McpBinding,
    pub by: String,
    pub surface: String,
    pub at: i64,
}

/// The stored MCP bindings (names only).
pub async fn mcp_bindings(core: &Core) -> Result<Vec<StoredBinding>> {
    Ok(match core.store().setting(MCP_KEY).await? {
        Some(s) if !s.trim().is_empty() => serde_json::from_str(&s).unwrap_or_default(),
        _ => Vec::new(),
    })
}

async fn save(core: &Core, list: &[StoredBinding]) -> Result<()> {
    core.store()
        .set_setting(MCP_KEY, &serde_json::to_string(list)?)
        .await
}

async fn binding_audit(core: &Core, b: &McpBinding, op: &str, origin: &Origin, outcome: &str) {
    let data = json!({
        "server": b.server, "key": b.key, "secret": b.secret, "op": op,
        "origin": origin, "outcome": outcome,
    });
    let _ = core.emit(kinds::MCP_BINDING, &b.server, &data).await;
    audit(
        core,
        "vault",
        if outcome == "refused" { "rule" } else { "user" },
        format!(
            "mcp {op} {}:{} <- {} by {} outcome={outcome}",
            b.server,
            b.key,
            b.secret,
            origin.label()
        ),
        json!({ "server": b.server, "key": b.key, "secret": b.secret }),
        data,
        "",
    )
    .await;
}

/// Binds the vault env secret `b.secret` to (server, key). The secret must
/// exist; one binding per (server, key). Refused under the kill switch.
pub async fn mcp_bind(core: &Core, b: McpBinding, origin: &Origin) -> Result<StoredBinding> {
    let b = McpBinding {
        server: b.server.trim().into(),
        key: b.key.trim().into(),
        secret: b.secret.trim().to_ascii_uppercase(),
    };
    if let Err(e) = control::check(core, "vault.mcp", &b.server, &origin.label()).await {
        if e.downcast_ref::<control::Blocked>().is_none() {
            return Err(e);
        }
        binding_audit(core, &b, "bind", origin, "refused").await;
        bail!("refused: {e}");
    }
    if let Err(why) = check_binding(&b) {
        binding_audit(core, &b, "bind", origin, "refused").await;
        bail!("{why}");
    }
    let known =
        core.store().vault_secrets().await?.into_iter().any(|s| {
            s.provider == crate::vault::ENV_PROVIDER && s.env.eq_ignore_ascii_case(&b.secret)
        });
    if !known {
        binding_audit(core, &b, "bind", origin, "refused").await;
        bail!("no env secret {} in the vault", b.secret);
    }
    let mut list = mcp_bindings(core).await?;
    list.retain(|x| !(x.binding.server == b.server && x.binding.key == b.key));
    let stored = StoredBinding {
        binding: b.clone(),
        by: origin.actor.clone(),
        surface: origin.surface.clone(),
        at: kls_store::now_ms(),
    };
    list.push(stored.clone());
    save(core, &list).await?;
    binding_audit(core, &b, "bind", origin, "saved").await;
    Ok(stored)
}

/// Removes the binding of (server, key). Allowed under the kill switch: it
/// only takes access away.
pub async fn mcp_unbind(core: &Core, server: &str, key: &str, origin: &Origin) -> Result<bool> {
    let mut list = mcp_bindings(core).await?;
    let Some(i) = list
        .iter()
        .position(|x| x.binding.server == server && x.binding.key == key)
    else {
        return Ok(false);
    };
    let gone = list.remove(i);
    save(core, &list).await?;
    binding_audit(core, &gone.binding, "unbind", origin, "saved").await;
    Ok(true)
}

#[cfg(test)]
mod tests;
