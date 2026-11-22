//! Third-party harness sessions (docs/gaps/harnesses.md): the harnesses a
//! node advertises, starting one (the dispatch with its key mode), the owner
//! as an A2A participant, the peer directory, and the transcript labels.
//!
//! Every path here checks the kill switch first ([`crate::control`]), A2A
//! paths the blackout too, and every action or refusal writes an audit entry
//! with its origin. No path bypasses them for the owner.

use crate::control;
use crate::gateway::DispatchSpec;
use crate::{Core, board::A2aSend};
use anyhow::{Result, bail};
use kls_store::audit::LedgerDraft;
use kls_store::board::A2aMessage;
use kls_store::sessions::{AuditLine, Entry, Session, SessionQuery};
use kls_store::vault::SecretMeta;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub mod kinds {
    pub const HARNESS_DISPATCHED: &str = "harness.dispatched";
    pub const HARNESS_REFUSED: &str = "harness.refused";
    /// The key-mode setting changed. Data: the new policy and who set it.
    pub const HARNESS_KEYS_SET: &str = "harness.keys.set";
    /// A vault key goes with a harness dispatch. Data: key id, harness, node, mode.
    pub const HARNESS_KEY_HANDOFF: &str = "harness.keys.handoff";
    pub const HARNESS_KEY_SENT: &str = "harness.key.sent";
    pub const HARNESS_KEY_REFUSED: &str = "harness.key.refused";
    pub const A2A_OWNER_SENT: &str = "a2a.owner.sent";
    pub const A2A_OWNER_REFUSED: &str = "a2a.owner.refused";
    pub const SCREEN_ATTACHED: &str = "screen.attached";
    /// A prompt step belai recorded for a harness session (its host audit
    /// line). Data: node, session, line, harness, prompt hash or id, origin,
    /// verdict, reason. Never the prompt's text.
    pub const HARNESS_PROMPT_QUEUED: &str = "harness.prompt.queued";
    pub const HARNESS_PROMPT_TYPED: &str = "harness.prompt.typed";
    pub const HARNESS_PROMPT_FAILED: &str = "harness.prompt.failed";
    pub const HARNESS_PROMPT_REFUSED: &str = "harness.prompt.refused";
    /// A harness session ended. Data: reason, cause (owner, kill_switch,
    /// grant_expiry, refused or exit), exit code, termination.
    pub const HARNESS_STOPPED: &str = "harness.stopped";
    pub const PREFIXES: [&str; 3] = ["harness.", "a2a.", "screen."];
}

/// The owner's reserved A2A id. Only the UI and CLI send as it; a node never
/// can (kls-node refuses a `from` outside the caller's node).
pub const OWNER_AGENT: &str = "keleustes:owner";

/// Settings key of the key-mode policy (JSON [`KeyPolicy`]).
pub const KEY_POLICY_SETTING: &str = "harness.key_policy";

/// One harness a node advertises (`HostAdvert.harnesses`); kept with the
/// node in the store.
pub use kls_store::HarnessInfo;

/// The picker's two lists: harnesses that can start, and the rest that were
/// detected (greyed, with their detail).
pub fn picker(list: &[HarnessInfo]) -> (Vec<HarnessInfo>, Vec<HarnessInfo>) {
    list.iter().cloned().partition(HarnessInfo::launchable)
}

// ---- key modes -----------------------------------------------------------------

/// How a vault key may reach a third-party harness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyMode {
    /// The harness uses its own login; no key leaves the vault.
    #[default]
    OwnLogin,
    /// The owner picks a key for one session at start.
    Handoff,
    /// The provider's key goes to every session of the harness.
    Always,
}

impl KeyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyMode::OwnLogin => "own_login",
            KeyMode::Handoff => "handoff",
            KeyMode::Always => "always",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "own_login" | "" => KeyMode::OwnLogin,
            "handoff" => KeyMode::Handoff,
            "always" => KeyMode::Always,
            _ => return None,
        })
    }
}

/// The owner's key-mode setting: a global mode with per-harness and per-key
/// overrides (a key override wins over a harness one).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct KeyPolicy {
    pub global: KeyMode,
    pub harnesses: BTreeMap<String, KeyMode>,
    pub keys: BTreeMap<String, KeyMode>,
    pub updated_by: String,
    pub updated_at: i64,
}

impl KeyPolicy {
    /// The mode for `harness` with key `key_id` (or none).
    pub fn resolve(&self, harness: &str, key_id: Option<&str>) -> KeyMode {
        if let Some(k) = key_id.and_then(|k| self.keys.get(k)) {
            return *k;
        }
        self.harnesses.get(harness).copied().unwrap_or(self.global)
    }
}

/// A request to start a harness session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HarnessRequest {
    pub node_id: String,
    pub harness: String,
    pub cwd: String,
    pub prompt: String,
    pub mode: String,
    pub model: String,
    pub resume: String,
    /// A vault key the owner picked for this session (hand-off).
    pub key_id: Option<String>,
    /// Who asked: "owner", a run or card id.
    pub actor: String,
    pub run_id: String,
    pub card_id: String,
}

/// The harness fields a dispatch carries (Dispatch.harness and friends,
/// pending in node.proto; see docs/gaps/harness-proto-request.md).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HarnessFields {
    pub harness: String,
    pub harness_mode: String,
    pub harness_resume: String,
    /// The resolved key mode ("own_login", "handoff", "always").
    pub harness_keys: String,
    /// The vault key handed off, if any (id only).
    pub key_id: String,
}

/// The planned dispatch: the spec, the harness fields, and the key that goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HarnessPlan {
    pub spec: DispatchSpec,
    pub fields: HarnessFields,
    pub key: Option<SecretMeta>,
}

fn id_shape(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Model ids: as belai's `harness.ModelShape`.
pub fn model_shape(s: &str) -> bool {
    s.len() <= 128
        && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/@-".contains(c))
}

/// Plans a harness dispatch against what the node advertises, the key
/// policy and the vault's keys (metadata only). Pure: no I/O.
pub fn plan(
    req: &HarnessRequest,
    advertised: &[HarnessInfo],
    policy: &KeyPolicy,
    secrets: &[SecretMeta],
) -> std::result::Result<HarnessPlan, String> {
    let Some(h) = advertised.iter().find(|h| h.id == req.harness) else {
        return Err("the node does not advertise that harness".into());
    };
    if !h.launchable() {
        return Err(format!("{} cannot be started ({})", h.name, h.state));
    }
    let mode = req.mode.trim();
    if !mode.is_empty() && mode != "default" && !h.modes.iter().any(|m| m == mode) {
        return Err(format!("{} has no mode {mode}", h.name));
    }
    if !req.model.is_empty() && !model_shape(&req.model) {
        return Err("the model is not a model id".into());
    }
    if !req.resume.is_empty() && (!h.resume || !id_shape(&req.resume, 64)) {
        return Err("that native session cannot be resumed".into());
    }
    if req.cwd.trim().is_empty() {
        return Err("pick a directory".into());
    }
    let usable = |s: &&SecretMeta| h.grant_env.contains(&s.env);
    let chosen = match &req.key_id {
        Some(id) => {
            let Some(s) = secrets.iter().find(|s| &s.id == id) else {
                return Err("that vault key does not exist".into());
            };
            if !usable(&s) {
                return Err(format!("{} does not read {}", h.name, s.env));
            }
            Some(s.clone())
        }
        None => None,
    };
    let mut km = policy.resolve(&h.id, chosen.as_ref().map(|s| s.id.as_str()));
    let key = match km {
        KeyMode::OwnLogin => None,
        // A hand-off needs the owner's pick for this session.
        KeyMode::Handoff => chosen,
        KeyMode::Always => chosen.or_else(|| {
            secrets
                .iter()
                .filter(usable)
                .find(|s| policy.resolve(&h.id, Some(&s.id)) == KeyMode::Always)
                .cloned()
        }),
    };
    if key.is_none() {
        km = KeyMode::OwnLogin;
    }
    let spec = DispatchSpec {
        kind: "session".into(),
        cwd: req.cwd.trim().into(),
        prompt: req.prompt.trim().into(),
        model: req.model.clone(),
        card_id: req.card_id.clone(),
        run_id: req.run_id.clone(),
        vault_providers: key.iter().map(|k| k.provider.clone()).collect(),
        ..Default::default()
    };
    let fields = HarnessFields {
        harness: h.id.clone(),
        harness_mode: mode.to_string(),
        harness_resume: req.resume.clone(),
        harness_keys: km.as_str().into(),
        key_id: key.as_ref().map(|k| k.id.clone()).unwrap_or_default(),
    };
    Ok(HarnessPlan { spec, fields, key })
}

/// Sends a planned harness dispatch to a node (the `NodeGateway` once the
/// harness fields are in the protocol).
#[async_trait::async_trait]
pub trait HarnessDispatcher: Send + Sync {
    async fn dispatch_harness(
        &self,
        node_id: &str,
        spec: DispatchSpec,
        fields: HarnessFields,
    ) -> Result<String>;
}

/// The A2A directory row (`Board.A2APeers`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aPeer {
    pub agent_id: String,
    pub node_name: String,
    pub title: String,
    pub repo: String,
    pub branch: String,
    pub harness: String,
    pub agent: String,
    pub updated_at: i64,
}

/// One line of another model's text, made safe to list: controls dropped,
/// whitespace collapsed, capped.
pub fn clean_line(s: &str, max: usize) -> String {
    let one: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| !matches!(*c as u32, 0x200b..=0x200f | 0x202a..=0x202e | 0x2066..=0x2069))
        .collect();
    one.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

impl Core {
    async fn harness_audit(
        &self,
        event: &str,
        action: &str,
        payload: Value,
        node: &str,
        session: &str,
        actor: &str,
    ) -> Result<()> {
        self.emit(
            event,
            if session.is_empty() { node } else { session },
            &payload,
        )
        .await?;
        self.audit_record(LedgerDraft {
            kind: "harness".into(),
            actor: if actor.is_empty() { "owner" } else { actor }.into(),
            actor_kind: "user".into(),
            msg: action.into(),
            subject: json!({"node": node, "session": session}),
            payload,
            node_id: node.into(),
            session_id: session.into(),
            ..Default::default()
        })
        .await?;
        Ok(())
    }

    // ---- key policy --------------------------------------------------------

    pub async fn harness_key_policy(&self) -> Result<KeyPolicy> {
        Ok(self
            .store()
            .setting(KEY_POLICY_SETTING)
            .await?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default())
    }

    /// Saves the key-mode policy; the change is audited with who set it.
    pub async fn set_harness_key_policy(&self, mut p: KeyPolicy, actor: &str) -> Result<KeyPolicy> {
        let before = self.harness_key_policy().await?;
        p.updated_by = if actor.is_empty() { "owner" } else { actor }.into();
        p.updated_at = kls_store::now_ms();
        self.store()
            .set_setting(KEY_POLICY_SETTING, &serde_json::to_string(&p)?)
            .await?;
        self.harness_audit(
            kinds::HARNESS_KEYS_SET,
            "key policy set",
            json!({"before": before, "after": p, "by": p.updated_by}),
            "",
            "",
            actor,
        )
        .await?;
        Ok(p)
    }

    // ---- dispatch ----------------------------------------------------------

    /// Starts a harness session: kill switch first, then the plan, the
    /// audited key hand-off and the dispatch. Every refusal is emitted and
    /// audited with the request's origin.
    pub async fn dispatch_harness(
        &self,
        req: &HarnessRequest,
        advertised: &[HarnessInfo],
        secrets: &[SecretMeta],
        via: &dyn HarnessDispatcher,
    ) -> Result<String> {
        let origin = json!({"actor": req.actor, "runId": req.run_id, "cardId": req.card_id});
        let refuse = |why: String| json!({"harness": req.harness, "node": req.node_id, "origin": origin, "reason": why, "outcome": "refused"});
        if let Err(e) = control::check(self, "dispatch", &req.node_id, &req.actor).await {
            self.harness_audit(
                kinds::HARNESS_REFUSED,
                "harness dispatch refused",
                refuse(e.to_string()),
                &req.node_id,
                "",
                &req.actor,
            )
            .await?;
            return Err(e);
        }
        let policy = self.harness_key_policy().await?;
        let p = match plan(req, advertised, &policy, secrets) {
            Ok(p) => p,
            Err(why) => {
                self.harness_audit(
                    kinds::HARNESS_REFUSED,
                    "harness dispatch refused",
                    refuse(why.clone()),
                    &req.node_id,
                    "",
                    &req.actor,
                )
                .await?;
                bail!(why);
            }
        };
        let version = advertised
            .iter()
            .find(|h| h.id == p.fields.harness)
            .map(|h| h.version.clone())
            .unwrap_or_default();
        if let Some(k) = &p.key {
            self.harness_audit(
                kinds::HARNESS_KEY_HANDOFF,
                "vault key handed to a harness",
                json!({"keyId": k.id, "provider": k.provider, "env": k.env, "harness": p.fields.harness,
                       "node": req.node_id, "mode": p.fields.harness_keys, "origin": origin,
                       "modeSetBy": policy.updated_by}),
                &req.node_id,
                "",
                &req.actor,
            )
            .await?;
        }
        let id = via
            .dispatch_harness(&req.node_id, p.spec.clone(), p.fields.clone())
            .await?;
        self.harness_audit(
            kinds::HARNESS_DISPATCHED,
            "harness dispatched",
            json!({"dispatchId": id, "harness": p.fields.harness, "version": version, "node": req.node_id,
                   "mode": p.fields.harness_mode, "keyMode": p.fields.harness_keys, "keyId": p.fields.key_id,
                   "resume": p.fields.harness_resume, "origin": origin, "outcome": "queued"}),
            &req.node_id,
            "",
            &req.actor,
        )
        .await?;
        Ok(id)
    }

    /// Sends one allowlisted key to a harness session's screen. Refused under
    /// the kill switch and for a key the harness does not allow.
    pub async fn harness_key(
        &self,
        session_id: &str,
        key: &str,
        allow: &[String],
        actor: &str,
    ) -> Result<()> {
        let s = self.store().session(session_id).await?;
        let node = s.as_ref().map(|s| s.node_id.clone()).unwrap_or_default();
        let blocked = control::check(self, "harness_key", session_id, actor)
            .await
            .err();
        let why = blocked.map(|e| e.to_string()).or_else(|| {
            (!allow.iter().any(|a| a == key))
                .then(|| format!("{key} is not a key this harness takes"))
        });
        if let Some(why) = why {
            self.harness_audit(
                kinds::HARNESS_KEY_REFUSED,
                "harness key refused",
                json!({"key": key, "reason": why, "by": actor, "outcome": "refused"}),
                &node,
                session_id,
                actor,
            )
            .await?;
            bail!(why);
        }
        self.queue_command(session_id, "key", &[key.to_string()])
            .await?;
        self.harness_audit(
            kinds::HARNESS_KEY_SENT,
            "harness key sent",
            json!({"key": key, "by": actor, "outcome": "queued"}),
            &node,
            session_id,
            actor,
        )
        .await
    }

    // ---- A2A ---------------------------------------------------------------

    /// The owner sends an A2A message as [`OWNER_AGENT`]. The kill switch and
    /// the blackout refuse it like any agent's; the receiving belai classifies
    /// it as `KindPeer` like any peer text.
    pub async fn send_owner_a2a(&self, to: &str, text: &str, thread_id: &str) -> Result<A2aSend> {
        let to = to.trim();
        if to.is_empty() || to == OWNER_AGENT {
            bail!("name the session to message");
        }
        if let Err(e) = control::check_a2a(self, OWNER_AGENT, to, "owner").await {
            self.harness_audit(
                kinds::A2A_OWNER_REFUSED,
                "owner A2A refused",
                json!({"from": OWNER_AGENT, "to": to, "reason": e.to_string(), "surface": "owner", "outcome": "refused"}),
                "",
                "",
                "owner",
            )
            .await?;
            return Err(e);
        }
        let sent = self
            .send_a2a(A2aMessage {
                from: OWNER_AGENT.into(),
                to: to.into(),
                thread_id: thread_id.into(),
                text: text.into(),
                meta_json: json!({"surface": "owner"}).to_string(),
                ..Default::default()
            })
            .await?;
        let (kind, outcome) = if sent.delivered {
            (kinds::A2A_OWNER_SENT, "sent")
        } else {
            (kinds::A2A_OWNER_REFUSED, "refused")
        };
        self.harness_audit(
            kind,
            "owner A2A",
            json!({"from": OWNER_AGENT, "to": to, "id": sent.message.as_ref().map(|m| m.id.clone()),
                   "surface": "owner", "reason": sent.refused, "outcome": outcome}),
            "",
            "",
            "owner",
        )
        .await?;
        Ok(sent)
    }

    /// Live sessions any node may message, newest first (`Board.A2APeers`).
    /// Titles are other models' text: one cleaned line, at most 120 chars.
    pub async fn a2a_peers(&self, repo: &str, limit: i64) -> Result<Vec<A2aPeer>> {
        let sessions = self
            .store()
            .sessions(&SessionQuery {
                open_only: true,
                limit: if limit <= 0 { 200 } else { limit },
                ..Default::default()
            })
            .await?;
        let nodes = self.store().nodes().await?;
        let name = |id: &str| {
            nodes
                .iter()
                .find(|n| n.id == id)
                .map(|n| n.name.clone())
                .unwrap_or_default()
        };
        let mut out: Vec<A2aPeer> = sessions
            .iter()
            .filter(|s| s.is_open() && (repo.is_empty() || s.repo == repo))
            .map(|s| peer_of(s, &name(&s.node_id)))
            .collect();
        out.sort_by_key(|p| std::cmp::Reverse(p.updated_at));
        Ok(out)
    }

    /// Records that someone watched a harness session's screen.
    pub async fn screen_attached(&self, session_id: &str, actor: &str) -> Result<()> {
        self.harness_audit(
            kinds::SCREEN_ATTACHED,
            "screen attached",
            json!({"by": actor}),
            "",
            session_id,
            actor,
        )
        .await
    }
}

fn peer_of(s: &Session, node_name: &str) -> A2aPeer {
    A2aPeer {
        agent_id: format!("{}:{}", s.node_id, s.id),
        node_name: node_name.into(),
        title: clean_line(&s.title, 120),
        repo: s.repo.clone(),
        branch: s.branch.clone(),
        harness: if s.kind == "harness" {
            s.provider.clone()
        } else {
            "belai".into()
        },
        agent: s.agent.clone(),
        updated_at: s.updated_at,
    }
}

// ---- host audit lines ----------------------------------------------------------

/// Why a harness session stopped, from its `stopped` line's reason.
fn stop_cause(reason: &str) -> &'static str {
    let r = reason.to_ascii_lowercase();
    if r.contains("grant expired") {
        "grant_expiry"
    } else if r.contains("factory stopped") {
        "kill_switch"
    } else if r.contains("owner") {
        "owner"
    } else if r.contains("refused") || r.contains("continuity") {
        "refused"
    } else {
        "exit"
    }
}

/// The Keleustes event a belai harness host audit line stands for (C8 rule
/// 2): a `host.dispatch` fact of a harness session whose status is a prompt
/// step or the stop. The payload carries ids, the prompt's hash, the origin,
/// the classifier verdict and a cleaned reason; never prompt text, and no
/// field the line has beyond those.
pub fn line_event(l: &AuditLine) -> Option<(&'static str, Value)> {
    if l.kind != "host.dispatch" {
        return None;
    }
    let v: Value = serde_json::from_str(&l.json).ok()?;
    let d = v.get("data")?;
    let field = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("");
    if field("dispatch") != "harness" {
        return None;
    }
    let kind = match field("status") {
        "prompt_queued" => kinds::HARNESS_PROMPT_QUEUED,
        "prompt_typed" => kinds::HARNESS_PROMPT_TYPED,
        "prompt_failed" => kinds::HARNESS_PROMPT_FAILED,
        "prompt_refused" => kinds::HARNESS_PROMPT_REFUSED,
        "stopped" => kinds::HARNESS_STOPPED,
        _ => return None,
    };
    let id = |k: &str| clean_line(field(k), 64);
    let reason = clean_line(field("reason"), 200);
    let mut p = json!({
        "node": l.node_id, "session": l.session_id, "line": l.idx,
        "harness": id("kind"), "reason": reason,
    });
    if kind == kinds::HARNESS_STOPPED {
        p["cause"] = stop_cause(&reason).into();
        p["exit"] = id("exit").into();
        p["termination"] = id("state").into();
    } else {
        // The prompt as belai recorded it: the hash of its text, or the
        // remote prompt's id.
        p["prompt"] = id("id").into();
        p["from"] = id("from").into();
        p["verdict"] = id("decision").into();
    }
    Some((kind, p))
}

impl Core {
    /// Emits the `harness.prompt.*` and `harness.stopped` events for a batch
    /// of host audit lines the store took as new. The ledger entry for each
    /// is the folded line itself (`audit::fold_line`), so the events add no
    /// second ledger row. Returns how many were emitted.
    pub async fn harness_lines(
        &self,
        node_id: &str,
        session_id: &str,
        lines: &[AuditLine],
    ) -> Result<usize> {
        let mut n = 0;
        for l in lines {
            if l.node_id != node_id || l.session_id != session_id {
                continue;
            }
            if let Some((kind, payload)) = line_event(l) {
                self.emit(kind, session_id, &payload).await?;
                n += 1;
            }
        }
        Ok(n)
    }
}

// ---- transcript labels ------------------------------------------------------------

/// Whether a session runs a third-party harness, and which.
pub fn session_harness(s: &Session) -> Option<&str> {
    (s.kind == "harness" && !s.provider.is_empty()).then_some(s.provider.as_str())
}

/// The harness fields an entry's meta carries (belai `harnessrun`):
/// `{"type","id","meta":{"harness","native":{"type"},"tool","own","origin"}}`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntryHarness {
    pub harness: String,
    pub native_type: String,
    pub tool: String,
    pub own: String,
    pub origin: String,
    pub subagent: bool,
}

pub fn entry_harness(e: &Entry) -> Option<EntryHarness> {
    let v: Value = serde_json::from_str(&e.meta_json).ok()?;
    let m = v.get("meta").unwrap_or(&v);
    let s = |k: &str| {
        m.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let harness = s("harness");
    if harness.is_empty() {
        return None;
    }
    Some(EntryHarness {
        native_type: m
            .pointer("/native/type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        tool: s("tool"),
        own: s("own"),
        origin: s("origin"),
        subagent: m.get("subagent").and_then(Value::as_bool).unwrap_or(false),
        harness,
    })
}

/// The role column of a harness entry: "tool Bash", "subagent", "you", the
/// harness's own word for a system line, or None to use the default.
pub fn harness_role_title(e: &Entry) -> Option<String> {
    let h = entry_harness(e)?;
    if e.role == "tool" && !h.tool.is_empty() {
        return Some(format!("tool {}", h.tool));
    }
    if h.subagent {
        return Some("subagent".into());
    }
    if !h.own.is_empty() && e.role == "system" {
        return Some(match h.own.as_str() {
            "harness_start" | "harness_end" | "harness_native" => "harness".into(),
            "harness_keys" => "keys".into(),
            "harness_control" => "control".into(),
            "harness_snapshot" | "harness_screen" => "screen".into(),
            "harness_prompt" => "typed".into(),
            "harness_waiting" => "waiting".into(),
            _ => return None,
        });
    }
    None
}

// ---- measured startup (O-027, O-048) ----------------------------------------------

/// Startup overhead (spawn to first output) of one harness on one node, from
/// the end facts Keleustes stored for its sessions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StartupStats {
    pub p50_ms: i64,
    pub p95_ms: i64,
    pub samples: i64,
    pub measured_at_ms: i64,
}

/// Recent sessions a figure covers.
pub const STARTUP_SAMPLES: usize = 50;

/// p50/p95 over the newest [`STARTUP_SAMPLES`] of `(ended_at_ms, startup_ms)`.
pub fn startup_stats(mut samples: Vec<(i64, i64)>) -> Option<StartupStats> {
    samples.retain(|(_, ms)| *ms > 0);
    if samples.is_empty() {
        return None;
    }
    samples.sort_by_key(|(at, _)| *at);
    let recent = &samples[samples.len().saturating_sub(STARTUP_SAMPLES)..];
    let mut ms: Vec<i64> = recent.iter().map(|(_, m)| *m).collect();
    ms.sort_unstable();
    let pick = |q: f64| ms[((q * ms.len() as f64) as usize).min(ms.len() - 1)];
    Some(StartupStats {
        p50_ms: pick(0.5),
        p95_ms: pick(0.95),
        samples: ms.len() as i64,
        measured_at_ms: recent.last().map(|(at, _)| *at).unwrap_or_default(),
    })
}

/// Startup figures keyed by (node id, harness id).
pub type StartupTable = std::collections::HashMap<(String, String), StartupStats>;

/// Puts the stored figures on a node's advertised harnesses; the node's own
/// advert figures stay where Keleustes has no sessions yet.
pub fn apply_startup(list: &mut [HarnessInfo], node_id: &str, t: &StartupTable) {
    for h in list.iter_mut() {
        if let Some(s) = t.get(&(node_id.to_string(), h.id.clone())) {
            h.startup_p50_ms = s.p50_ms;
            h.startup_p95_ms = s.p95_ms;
            h.startup_samples = s.samples;
            h.startup_measured_at_ms = s.measured_at_ms;
        }
    }
}

impl Core {
    /// Startup figures from the end facts of the last ended sessions.
    pub async fn harness_startup(&self) -> Result<StartupTable> {
        let ended = self
            .store()
            .sessions(&SessionQuery {
                limit: 500,
                ..Default::default()
            })
            .await?;
        let mut by: std::collections::HashMap<(String, String), Vec<(i64, i64)>> =
            Default::default();
        for s in ended.iter().filter(|s| !s.is_open()) {
            if let Some(h) = self
                .store()
                .session_extras(&s.id)
                .await?
                .and_then(|x| x.harness)
                && let Some(end) = h.end
            {
                by.entry((s.node_id.clone(), h.harness))
                    .or_default()
                    .push((s.ended_at.unwrap_or(s.updated_at), end.startup_ms));
            }
        }
        Ok(by
            .into_iter()
            .filter_map(|(k, v)| startup_stats(v).map(|s| (k, s)))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude() -> HarnessInfo {
        HarnessInfo {
            id: "claude-code".into(),
            name: "Claude Code".into(),
            version: "2.1".into(),
            state: "launch".into(),
            resume: true,
            a2a: true,
            grant_env: vec!["ANTHROPIC_API_KEY".into()],
            modes: vec!["default".into(), "plan".into()],
            allow_keys: vec!["enter".into(), "1".into()],
            ..Default::default()
        }
    }

    fn key(id: &str, env: &str) -> SecretMeta {
        SecretMeta {
            id: id.into(),
            provider: "anthropic".into(),
            env: env.into(),
            ..Default::default()
        }
    }

    fn req() -> HarnessRequest {
        HarnessRequest {
            node_id: "n1".into(),
            harness: "claude-code".into(),
            cwd: "/w".into(),
            prompt: "hi".into(),
            actor: "owner".into(),
            ..Default::default()
        }
    }

    #[test]
    fn picker_splits_launchable() {
        let mut gui = claude();
        gui.id = "cursor".into();
        gui.state = "gui".into();
        let (a, b) = picker(&[claude(), gui]);
        assert_eq!(a.len(), 1);
        assert_eq!(b[0].id, "cursor");
    }

    #[test]
    fn own_login_is_the_default_and_hands_nothing() {
        let keys = [key("k1", "ANTHROPIC_API_KEY")];
        let mut r = req();
        r.key_id = Some("k1".into());
        let p = plan(&r, &[claude()], &KeyPolicy::default(), &keys).unwrap();
        assert_eq!(p.fields.harness_keys, "own_login");
        assert!(p.key.is_none() && p.spec.vault_providers.is_empty());
    }

    #[test]
    fn handoff_needs_the_owners_pick() {
        let keys = [key("k1", "ANTHROPIC_API_KEY")];
        let pol = KeyPolicy {
            global: KeyMode::Handoff,
            ..Default::default()
        };
        let p = plan(&req(), &[claude()], &pol, &keys).unwrap();
        assert_eq!(p.fields.harness_keys, "own_login", "no pick, no key");
        let mut r = req();
        r.key_id = Some("k1".into());
        let p = plan(&r, &[claude()], &pol, &keys).unwrap();
        assert_eq!(p.fields.harness_keys, "handoff");
        assert_eq!(p.spec.vault_providers, vec!["anthropic".to_string()]);
        assert_eq!(p.fields.key_id, "k1");
    }

    #[test]
    fn always_picks_the_providers_key_and_overrides_resolve() {
        let keys = [key("k0", "OTHER_KEY"), key("k1", "ANTHROPIC_API_KEY")];
        let mut pol = KeyPolicy::default();
        pol.harnesses.insert("claude-code".into(), KeyMode::Always);
        let p = plan(&req(), &[claude()], &pol, &keys).unwrap();
        assert_eq!(p.fields.harness_keys, "always");
        assert_eq!(p.fields.key_id, "k1");
        // A per-key override wins over the harness one.
        pol.keys.insert("k1".into(), KeyMode::OwnLogin);
        let p = plan(&req(), &[claude()], &pol, &keys).unwrap();
        assert_eq!(p.fields.harness_keys, "own_login");
        assert!(p.key.is_none());
    }

    #[test]
    fn plan_refuses() {
        let keys = [key("k9", "OPENAI_API_KEY")];
        type Tweak = Box<dyn Fn(&mut HarnessRequest)>;
        let cases: Vec<(&str, Tweak)> = vec![
            ("unadvertised", Box::new(|r| r.harness = "codex".into())),
            ("mode", Box::new(|r| r.mode = "bypassPermissions".into())),
            (
                "model",
                Box::new(|r| r.model = "--dangerously-skip-permissions".into()),
            ),
            ("resume", Box::new(|r| r.resume = "../x".into())),
            ("key env", Box::new(|r| r.key_id = Some("k9".into()))),
            ("no key", Box::new(|r| r.key_id = Some("nope".into()))),
        ];
        for (name, f) in cases {
            let mut r = req();
            f(&mut r);
            assert!(
                plan(&r, &[claude()], &KeyPolicy::default(), &keys).is_err(),
                "{name}"
            );
        }
        let mut broken = claude();
        broken.state = "broken".into();
        assert!(plan(&req(), &[broken], &KeyPolicy::default(), &keys).is_err());
    }

    #[test]
    fn labels() {
        let e = Entry {
            role: "tool".into(),
            meta_json: r#"{"type":"tool","meta":{"harness":"claude-code","tool":"Bash","native":{"type":"assistant"}}}"#.into(),
            ..Default::default()
        };
        assert_eq!(harness_role_title(&e).as_deref(), Some("tool Bash"));
        let e = Entry {
            role: "system".into(),
            meta_json: r#"{"meta":{"harness":"x","own":"harness_keys"}}"#.into(),
            ..Default::default()
        };
        assert_eq!(harness_role_title(&e).as_deref(), Some("keys"));
        assert!(harness_role_title(&Entry::default()).is_none());
    }

    struct FakeDispatch(std::sync::Mutex<Vec<(DispatchSpec, HarnessFields)>>);

    #[async_trait::async_trait]
    impl HarnessDispatcher for FakeDispatch {
        async fn dispatch_harness(
            &self,
            _: &str,
            s: DispatchSpec,
            f: HarnessFields,
        ) -> Result<String> {
            self.0.lock().unwrap().push((s, f));
            Ok("d1".into())
        }
    }

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Core::open(dir.path(), None).await.unwrap(), dir)
    }

    async fn ledger_has(c: &Core, msg: &str) -> bool {
        c.audit_entries(&kls_store::audit::LedgerQuery::default())
            .await
            .unwrap()
            .iter()
            .any(|e| e.msg == msg)
    }

    #[tokio::test]
    async fn dispatch_audits_provenance_and_handoff() {
        let (c, _d) = core().await;
        let mut pol = KeyPolicy::default();
        pol.harnesses.insert("claude-code".into(), KeyMode::Always);
        c.set_harness_key_policy(pol, "owner").await.unwrap();
        assert!(ledger_has(&c, "key policy set").await);
        let f = FakeDispatch(Default::default());
        let keys = [key("k1", "ANTHROPIC_API_KEY")];
        let id = c
            .dispatch_harness(&req(), &[claude()], &keys, &f)
            .await
            .unwrap();
        assert_eq!(id, "d1");
        let sent = f.0.lock().unwrap().clone();
        assert_eq!(sent[0].1.harness_keys, "always");
        assert!(ledger_has(&c, "vault key handed to a harness").await);
        assert!(ledger_has(&c, "harness dispatched").await);
        let ev = c.store().recent_events(50).await.unwrap();
        let d = ev
            .iter()
            .find(|e| e.kind == kinds::HARNESS_DISPATCHED)
            .unwrap();
        assert!(
            d.data_json.contains("\"actor\":\"owner\"")
                && d.data_json.contains("\"version\":\"2.1\""),
            "{}",
            d.data_json
        );
    }

    #[tokio::test]
    async fn kill_switch_refuses_dispatch_key_and_handoff() {
        let (c, _d) = core().await;
        control::set_mode(&c, control::Mode::Stop, "incident", "owner")
            .await
            .unwrap();
        let f = FakeDispatch(Default::default());
        let mut r = req();
        r.key_id = Some("k1".into());
        let keys = [key("k1", "ANTHROPIC_API_KEY")];
        assert!(
            c.dispatch_harness(&r, &[claude()], &keys, &f)
                .await
                .is_err()
        );
        assert!(
            f.0.lock().unwrap().is_empty(),
            "nothing sent, no key handed off"
        );
        assert!(ledger_has(&c, "harness dispatch refused").await);
        assert!(!ledger_has(&c, "vault key handed to a harness").await);
        assert!(
            c.harness_key("s1", "1", &["1".into()], "owner")
                .await
                .is_err()
        );
        assert!(ledger_has(&c, "harness key refused").await);
        assert!(c.send_owner_a2a("n2:s9", "hi", "").await.is_err());
        assert!(ledger_has(&c, "owner A2A refused").await);
    }

    #[tokio::test]
    async fn blackout_refuses_owner_a2a_and_open_sends() {
        let (c, _d) = core().await;
        let ok = c.send_owner_a2a("n2:s9", "hello", "").await.unwrap();
        assert!(ok.delivered);
        assert_eq!(ok.message.unwrap().from, OWNER_AGENT);
        assert!(ledger_has(&c, "owner A2A").await);
        c.set_a2a_blackout(true, "drill").await.unwrap();
        assert!(c.send_owner_a2a("n2:s9", "hello", "").await.is_err());
        assert!(ledger_has(&c, "owner A2A refused").await);
        assert!(c.send_owner_a2a(OWNER_AGENT, "x", "").await.is_err());
    }

    #[tokio::test]
    async fn unadvertised_harness_refused_and_audited() {
        let (c, _d) = core().await;
        let f = FakeDispatch(Default::default());
        let mut r = req();
        r.harness = "codex".into();
        assert!(c.dispatch_harness(&r, &[claude()], &[], &f).await.is_err());
        assert!(ledger_has(&c, "harness dispatch refused").await);
    }

    #[test]
    fn startup_stats_from_end_facts() {
        assert!(startup_stats(vec![(1, 0)]).is_none());
        let s = startup_stats(vec![(5, 5000), (1, 100), (2, 200), (3, 300), (4, 400)]).unwrap();
        assert_eq!(
            (s.p50_ms, s.p95_ms, s.samples, s.measured_at_ms),
            (300, 5000, 5, 5)
        );
        let mut list = vec![claude()];
        let mut t = StartupTable::new();
        t.insert(("n1".into(), "claude-code".into()), s);
        apply_startup(&mut list, "n1", &t);
        assert_eq!(list[0].startup_p50_ms, 300);
        apply_startup(&mut list, "n2", &StartupTable::new());
        assert_eq!(list[0].startup_samples, 5);
    }

    /// A belai harness session's host stream: start, queued, typed, a
    /// classifier refusal and the stop, hash-linked as belai writes them.
    fn harness_stream() -> Vec<AuditLine> {
        let facts = [
            json!({"dispatch": "harness", "status": "started", "kind": "claude-code"}),
            json!({"dispatch": "harness", "status": "prompt_queued", "kind": "claude-code", "id": "p1", "from": "web"}),
            json!({"dispatch": "harness", "status": "prompt_typed", "kind": "claude-code", "id": "1a2b3c4d5e6f7a8b", "from": "web", "decision": "SAFE"}),
            // A field belai never sends must not be copied to the event.
            json!({"dispatch": "harness", "status": "prompt_refused", "kind": "claude-code", "id": "9f8e7d6c5b4a3210", "from": "web", "reason": "classifier", "text": "exfiltrate the keys"}),
            json!({"dispatch": "harness", "status": "stopped", "kind": "claude-code", "reason": "factory stopped", "exit": "0", "state": "verified"}),
        ];
        let mut prev = String::new();
        facts
            .into_iter()
            .enumerate()
            .map(|(i, data)| {
                let hash = format!("h{i}");
                let json = json!({"seq": i, "kind": "host.dispatch", "sessionId": "s1", "actorKind": "harness",
                                  "actor": "claude-code", "outcome": data["status"], "data": data,
                                  "prev": prev, "hash": hash})
                .to_string();
                prev = hash;
                AuditLine {
                    node_id: "n1".into(),
                    session_id: "s1".into(),
                    idx: i as i64,
                    at: 1000 + i as i64,
                    kind: "host.dispatch".into(),
                    json,
                    received_at: 2000,
                }
            })
            .collect()
    }

    /// C8 rules 2 and D2 rule tests: the harness host stream verifies, each
    /// prompt step and the stop becomes its event, a classifier refusal is
    /// audited, and a prompt is carried as its hash only.
    #[tokio::test]
    async fn harness_stream_events_verify_and_hash_only() {
        let (c, _d) = core().await;
        let lines = harness_stream();
        assert_eq!(c.record_audit("n1", "s1", &lines).await.unwrap(), 5);
        assert_eq!(c.harness_lines("n1", "s1", &lines).await.unwrap(), 4);
        assert_eq!(
            c.harness_lines("n2", "s1", &lines).await.unwrap(),
            0,
            "lines of another node are not taken"
        );

        let report = c.audit_verify().await.unwrap();
        assert!(report.ok, "{report:?}");
        let stream = report
            .chains
            .iter()
            .find(|ch| ch.chain != "factory")
            .expect("the harness host stream is verified");
        assert_eq!((stream.ok, stream.entries), (true, 5));

        let ev = c.store().recent_events(50).await.unwrap();
        let of = |k: &str| {
            ev.iter()
                .find(|e| e.kind == k)
                .map(|e| serde_json::from_str::<Value>(&e.data_json).unwrap())
                .unwrap_or_else(|| panic!("no {k}"))
        };
        assert_eq!(of(kinds::HARNESS_PROMPT_QUEUED)["prompt"], "p1");
        let typed = of(kinds::HARNESS_PROMPT_TYPED);
        assert_eq!(
            (typed["prompt"].as_str(), typed["verdict"].as_str()),
            (Some("1a2b3c4d5e6f7a8b"), Some("SAFE"))
        );
        let refused = of(kinds::HARNESS_PROMPT_REFUSED);
        assert_eq!(refused["reason"], "classifier");
        let stopped = of(kinds::HARNESS_STOPPED);
        assert_eq!(
            (stopped["cause"].as_str(), stopped["exit"].as_str()),
            (Some("kill_switch"), Some("0"))
        );
        for e in ev.iter().filter(|e| e.kind.starts_with("harness.")) {
            assert!(
                !e.data_json.contains("exfiltrate"),
                "prompt text in {}",
                e.data_json
            );
        }

        // The refusal is in the ledger, from the host stream, by hash.
        let ledger = c
            .audit_entries(&kls_store::audit::LedgerQuery::default())
            .await
            .unwrap();
        let row = ledger
            .iter()
            .find(|e| e.payload["data"]["status"] == "prompt_refused")
            .expect("classifier refusal audited");
        assert_eq!(row.payload["data"]["id"], "9f8e7d6c5b4a3210");
        assert_eq!(row.origin, "host:n1");
    }

    #[test]
    fn stop_causes() {
        for (r, want) in [
            ("vault grant expired", "grant_expiry"),
            ("factory stopped", "kill_switch"),
            ("stopped by the owner", "owner"),
            ("harness exited", "exit"),
        ] {
            assert_eq!(stop_cause(r), want, "{r}");
        }
        let mut l = harness_stream().remove(0);
        assert!(line_event(&l).is_none(), "start has its own event");
        l.kind = "worker.started".into();
        assert!(line_event(&l).is_none());
    }

    #[test]
    fn clean_line_strips_controls_and_caps() {
        assert_eq!(clean_line("a\u{202e}b\n\tc\x1b", 120), "ab c");
        assert_eq!(clean_line(&"x".repeat(300), 120).len(), 120);
    }
}
