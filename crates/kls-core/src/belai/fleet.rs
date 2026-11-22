//! Cross-host dispatch (WP-I): a node asks Keleustes to run work on another
//! node, and the factory's node policy decides. The policy is off by
//! default; a request a model made (`initiated_by = "agent"`) always waits
//! for a person; the kill switch refuses every request; every outcome is
//! audited with its origin; the target only ever sees a dispatch with
//! `trigger.source = "node"` naming who asked.

use crate::Core;
use crate::gateway::{DispatchSpec, Trigger};
use anyhow::{Result, bail};
use kls_store::audit::LedgerDraft;
use kls_store::belai::NodeRequest;
use kls_store::now_ms;
use kls_store::sessions::SessionQuery;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub use kls_store::belai::NodeRequest as Request;

pub mod kinds {
    /// A node request was filed, decided or settled. Subject: request id.
    pub const FLEET_REQUEST: &str = "fleet.request";
    pub const FLEET_POLICY: &str = "fleet.policy";
}

/// Settings key holding the [`Policy`] JSON.
pub const POLICY_SETTING: &str = "fleet.policy";

/// What a rule decides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Queue it at once (never for an agent's request).
    Auto,
    /// Park it until a person approves.
    #[default]
    Ask,
    Deny,
}

/// One rule; `"*"` or empty matches anything.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub dir: String,
    pub action: Action,
}

/// The factory's node policy. Off by default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub enabled: bool,
    /// `Nodes` lists other hosts' directories.
    #[serde(default)]
    pub share_dirs: bool,
    /// Nodes may prompt sessions on other nodes.
    #[serde(default)]
    pub prompts: bool,
    /// First match wins; none matching means [`Action::Ask`].
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Requests one node may make per hour.
    #[serde(default = "default_rate")]
    pub rate_per_hour: i64,
}

fn default_rate() -> i64 {
    20
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            share_dirs: false,
            prompts: false,
            rules: Vec::new(),
            rate_per_hour: default_rate(),
        }
    }
}

fn matches(pat: &str, v: &str) -> bool {
    pat.is_empty() || pat == "*" || pat == v
}

impl Policy {
    /// The decision for one request. An agent's request is never automatic.
    pub fn decide(&self, origin: &str, target: &str, kind: &str, dir: &str, agent: bool) -> Action {
        let a = self
            .rules
            .iter()
            .find(|r| {
                matches(&r.origin, origin)
                    && matches(&r.target, target)
                    && matches(&r.kind, kind)
                    && matches(&r.dir, dir)
            })
            .map(|r| r.action)
            .unwrap_or_default();
        if agent && a == Action::Auto {
            Action::Ask
        } else {
            a
        }
    }
}

/// What a node asked for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ask {
    /// Node id or name; empty: any eligible node.
    pub target: String,
    pub spec: DispatchSpec,
    pub origin_session: String,
    pub initiated_by: String,
}

/// The answer to a request (the protocol's `DispatchTicket`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ticket {
    pub request_id: String,
    pub state: String,
    pub reason: String,
    pub dispatch_id: String,
    pub target_node: String,
}

/// A request's progress (the protocol's `DispatchStatus`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub request_id: String,
    pub dispatch_id: String,
    pub state: String,
    pub session_id: String,
    pub reason: String,
    pub target_node: String,
}

/// A node another node may address (the protocol's `NodeSummary`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub node_id: String,
    pub name: String,
    pub online: bool,
    pub remote_control: bool,
    pub trusted_dirs: Vec<String>,
    pub agents: Vec<String>,
    pub crews: Vec<String>,
    pub providers: Vec<String>,
    pub running_sessions: u32,
    pub max_sessions: u32,
    pub agent_ids: Vec<String>,
}

/// Kinds a node may ask for on another.
const KINDS: [&str; 3] = ["session", "worker", "crew"];

fn initiator(s: &str) -> &'static str {
    if s == "user" { "user" } else { "agent" }
}

impl Core {
    pub async fn fleet_policy(&self) -> Result<Policy> {
        Ok(self
            .store()
            .setting(POLICY_SETTING)
            .await?
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default())
    }

    /// Saves the node policy (the owner's action, audited).
    pub async fn set_fleet_policy(&self, p: &Policy) -> Result<()> {
        let v = serde_json::to_string(p)?;
        self.store().set_setting(POLICY_SETTING, &v).await?;
        self.emit(
            kinds::FLEET_POLICY,
            "factory",
            &json!({ "enabled": p.enabled }),
        )
        .await?;
        self.audit_record(LedgerDraft {
            kind: "fleet_policy".into(),
            actor: "owner".into(),
            actor_kind: "user".into(),
            msg: format!("node policy {}", if p.enabled { "on" } else { "off" }),
            subject: json!({ "setting": POLICY_SETTING }),
            payload: serde_json::to_value(p)?,
            origin: "factory".into(),
            ..Default::default()
        })
        .await?;
        Ok(())
    }

    /// Nodes `origin` may address, as the policy shares them.
    pub async fn fleet_nodes(&self, origin: &str, online: &[String]) -> Result<Vec<Summary>> {
        let p = self.fleet_policy().await?;
        if !p.enabled {
            bail!("cross-host dispatch is off in Keleustes");
        }
        let blackout = crate::control::state(self).await?.a2a_blackout;
        let open = self
            .store()
            .sessions(&SessionQuery {
                open_only: true,
                limit: 10_000,
                ..Default::default()
            })
            .await?;
        let mut out = Vec::new();
        for n in self.store().nodes().await? {
            if n.revoked_at.is_some() || n.id == origin {
                continue;
            }
            let live: Vec<&kls_store::sessions::Session> =
                open.iter().filter(|s| s.node_id == n.id).collect();
            out.push(Summary {
                online: online.contains(&n.id),
                remote_control: n.host.capabilities.remote_control,
                trusted_dirs: if p.share_dirs {
                    n.host.trusted_dirs.clone()
                } else {
                    Vec::new()
                },
                agents: n.host.agents.clone(),
                crews: n.host.crews.clone(),
                providers: n.host.capabilities.providers.clone(),
                running_sessions: live.len() as u32,
                max_sessions: n.host.capabilities.max_sessions,
                // The A2A blackout hides who could be addressed.
                agent_ids: if blackout {
                    Vec::new()
                } else {
                    live.iter().map(|s| format!("{}:{}", n.id, s.id)).collect()
                },
                node_id: n.id,
                name: n.name,
            });
        }
        Ok(out)
    }

    /// Files a node's request for work on another node.
    pub async fn fleet_request(&self, origin: &str, ask: Ask) -> Result<Ticket> {
        let by = initiator(&ask.initiated_by);
        let mut spec = ask.spec;
        if spec.kind.is_empty() {
            spec.kind = "session".into();
        }
        let origin_name = self
            .store()
            .node(origin)
            .await?
            .map(|n| n.name)
            .unwrap_or_default();
        // The target sees who asked, never a vault grant chosen by a node.
        spec.vault_providers.clear();
        spec.trigger = Some(Trigger {
            source: "node".into(),
            origin_node: origin.into(),
            origin_session: ask.origin_session.chars().take(64).collect(),
            origin_name: origin_name.clone(),
            ..Default::default()
        });
        let mut req = NodeRequest {
            id: uuid::Uuid::now_v7().to_string(),
            origin_node: origin.into(),
            origin_session: ask.origin_session.chars().take(64).collect(),
            initiated_by: by.into(),
            kind: spec.kind.clone(),
            cwd: spec.cwd.clone(),
            created_at: now_ms(),
            ..Default::default()
        };
        let verdict = self.screen(origin, &ask.target, &spec, by == "agent").await;
        let (state, reason, target) = match verdict {
            Ok((target, Action::Auto)) => ("queued", String::new(), target),
            Ok((target, Action::Ask)) => ("awaiting_approval", String::new(), target),
            Ok((target, Action::Deny)) => ("refused", "the node policy denies it".into(), target),
            Err(e) => ("refused", e.to_string(), ask.target.clone()),
        };
        req.target_node = target;
        req.spec_json = serde_json::to_string(&spec)?;
        req.state = if state == "queued" {
            "awaiting_approval".into()
        } else {
            state.into()
        };
        req.reason = reason.clone();
        self.store().insert_node_request(&req).await?;
        self.audit_request(&req, "filed", &reason).await?;
        let mut ticket = Ticket {
            request_id: req.id.clone(),
            state: state.into(),
            reason,
            target_node: req.target_node.clone(),
            ..Default::default()
        };
        if state == "queued" {
            match self.release(&req, "policy").await {
                Ok(id) => ticket.dispatch_id = id,
                Err(e) => {
                    ticket.state = "refused".into();
                    ticket.reason = e.to_string();
                }
            }
        } else {
            self.emit_request(&req.id, state).await?;
        }
        Ok(ticket)
    }

    /// The owner approves or declines a parked request.
    pub async fn fleet_decide(&self, request_id: &str, approve: bool) -> Result<Ticket> {
        let Some(req) = self.store().node_request(request_id).await? else {
            bail!("unknown request");
        };
        if req.state != "awaiting_approval" {
            bail!("the request is {}", req.state);
        }
        if !approve {
            self.store()
                .settle_node_request(
                    &req.id,
                    "awaiting_approval",
                    "refused",
                    "declined",
                    "",
                    "",
                    "owner",
                )
                .await?;
            self.audit_request(&req, "declined", "declined").await?;
            self.emit_request(&req.id, "refused").await?;
            return Ok(Ticket {
                request_id: req.id,
                state: "refused".into(),
                reason: "declined".into(),
                target_node: req.target_node,
                ..Default::default()
            });
        }
        match self.release(&req, "owner").await {
            Ok(id) => Ok(Ticket {
                request_id: req.id,
                state: "queued".into(),
                dispatch_id: id,
                target_node: req.target_node,
                ..Default::default()
            }),
            Err(e) => Ok(Ticket {
                request_id: req.id,
                state: "refused".into(),
                reason: e.to_string(),
                target_node: req.target_node,
                ..Default::default()
            }),
        }
    }

    /// Queues an approved request through the gateway (which checks the kill
    /// switch and the target again).
    async fn release(&self, req: &NodeRequest, by: &str) -> Result<String> {
        let spec: DispatchSpec = serde_json::from_str(&req.spec_json)?;
        // A blackout that began while an agent's request waited for the
        // owner still holds it: approval is no bypass. (A prompt checks in
        // queue_peer_prompt.)
        if req.initiated_by != "user"
            && req.kind != "prompt"
            && let Err(e) = crate::control::check_a2a(
                self,
                &req.origin_node,
                &req.target_node,
                &format!("node:{}", req.origin_node),
            )
            .await
        {
            self.refuse_request(req, by, &e.to_string()).await?;
            return Err(e);
        }
        if req.kind == "prompt" {
            let (sid, text) = (req.target_node.clone(), spec.prompt.clone());
            return match self.queue_peer_prompt(&req.origin_node, &sid, &text).await {
                Ok(item) => {
                    self.store()
                        .settle_node_request(
                            &req.id,
                            "awaiting_approval",
                            "queued",
                            "",
                            &item,
                            "",
                            by,
                        )
                        .await?;
                    self.audit_request(req, "queued", "").await?;
                    self.emit_request(&req.id, "queued").await?;
                    Ok(item)
                }
                Err(e) => {
                    self.refuse_request(req, by, &e.to_string()).await?;
                    Err(e)
                }
            };
        }
        let out = match self.gateway() {
            Ok(g) => g.dispatch(&req.target_node, spec).await,
            Err(e) => Err(e),
        };
        match out {
            Ok(id) => {
                self.store()
                    .settle_node_request(&req.id, "awaiting_approval", "queued", "", &id, "", by)
                    .await?;
                self.audit_request(req, "queued", "").await?;
                self.emit_request(&req.id, "queued").await?;
                Ok(id)
            }
            Err(e) => {
                self.refuse_request(req, by, &e.to_string()).await?;
                Err(e)
            }
        }
    }

    async fn refuse_request(&self, req: &NodeRequest, by: &str, why: &str) -> Result<()> {
        self.store()
            .settle_node_request(&req.id, "awaiting_approval", "refused", why, "", "", by)
            .await?;
        self.audit_request(req, "refused", why).await?;
        self.emit_request(&req.id, "refused").await
    }

    /// Checks a request before it is parked or queued and picks its target.
    async fn screen(
        &self,
        origin: &str,
        target: &str,
        spec: &DispatchSpec,
        agent: bool,
    ) -> Result<(String, Action)> {
        crate::control::check(self, "dispatch", target, &format!("node:{origin}")).await?;
        // An agent asking another host for work is agent-to-agent traffic:
        // the blackout refuses it (audited `a2a.withheld`), with no bypass.
        if agent {
            crate::control::check_a2a(self, origin, target, &format!("node:{origin}")).await?;
        }
        let p = self.fleet_policy().await?;
        if !p.enabled {
            bail!("cross-host dispatch is off in Keleustes");
        }
        let hour_ago = now_ms() - 3_600_000;
        if self
            .store()
            .count_node_requests_since(origin, hour_ago)
            .await?
            >= p.rate_per_hour.max(0)
        {
            bail!("this node made too many requests in the last hour");
        }
        if crate::hosts::is_host_kind(&spec.kind) {
            bail!("no node may manage another host");
        }
        if !KINDS.contains(&spec.kind.as_str()) {
            bail!("a node may ask for a session, worker or crew only");
        }
        if spec.kind == "session" && spec.prompt.trim().is_empty() {
            bail!("a session request carries a prompt");
        }
        let online = match self.gateway() {
            Ok(g) => g.online().await,
            Err(_) => Vec::new(),
        };
        let nodes = self.store().nodes().await?;
        let eligible = |n: &kls_store::Node| {
            n.revoked_at.is_none()
                && n.host.capabilities.remote_control
                && n.host.capabilities.takes_dispatch(&spec.kind)
                && (spec.cwd.is_empty() || n.host.trusted_dirs.contains(&spec.cwd))
                && (spec.agent.is_empty() || super::offers_agent(&n.host, &spec.agent))
                && (spec.crew.is_empty() || n.host.crews.contains(&spec.crew))
        };
        let node = if target.is_empty() || target == "any" {
            nodes
                .iter()
                .filter(|n| n.id != origin && online.contains(&n.id) && eligible(n))
                .min_by_key(|n| n.id.clone())
                .ok_or_else(|| anyhow::anyhow!("no online node can take this request"))?
        } else {
            let hit: Vec<&kls_store::Node> = nodes
                .iter()
                .filter(|n| n.revoked_at.is_none() && (n.id == target || n.name == target))
                .collect();
            let [n] = hit[..] else {
                bail!("no single active node named {target:?}");
            };
            if !n.host.capabilities.remote_control {
                bail!("node {} runs no belai rc", n.name);
            }
            if !spec.cwd.is_empty() && !n.host.trusted_dirs.contains(&spec.cwd) {
                bail!("node {} does not offer {}", n.name, spec.cwd);
            }
            if !eligible(n) {
                bail!("node {} cannot take this request", n.name);
            }
            n
        };
        let action = p.decide(origin, &node.id, &spec.kind, &spec.cwd, agent);
        Ok((node.id.clone(), action))
    }

    /// A node's prompt to a session on another node: the same policy, kill
    /// switch and A2A gate; queued as a remote prompt the target admits
    /// through its own CleanPrompt and classifier. An agent's prompt waits
    /// for approval.
    pub async fn fleet_prompt(
        &self,
        origin: &str,
        target_session: &str,
        text: &str,
        origin_session: &str,
        initiated_by: &str,
    ) -> Result<Ticket> {
        let by = initiator(initiated_by);
        let sid = target_session
            .split_once(':')
            .map(|(_, s)| s)
            .unwrap_or(target_session)
            .to_string();
        let mut req = NodeRequest {
            id: uuid::Uuid::now_v7().to_string(),
            origin_node: origin.into(),
            origin_session: origin_session.chars().take(64).collect(),
            initiated_by: by.into(),
            target_node: sid.clone(),
            kind: "prompt".into(),
            spec_json: serde_json::to_string(&DispatchSpec {
                prompt: text.chars().take(32 * 1024).collect(),
                ..Default::default()
            })?,
            created_at: now_ms(),
            state: "awaiting_approval".into(),
            ..Default::default()
        };
        let screened: Result<()> = async {
            crate::control::check(self, "prompt", &sid, &format!("node:{origin}")).await?;
            crate::control::check_a2a(self, origin, &sid, &format!("node:{origin}")).await?;
            let p = self.fleet_policy().await?;
            if !p.enabled || !p.prompts {
                bail!("cross-host prompts are off in Keleustes");
            }
            if text.trim().is_empty() {
                bail!("the prompt is empty");
            }
            self.peer_prompt_target(&sid).await?;
            Ok(())
        }
        .await;
        if let Err(e) = &screened {
            req.state = "refused".into();
            req.reason = e.to_string();
        }
        self.store().insert_node_request(&req).await?;
        self.audit_request(&req, "filed", &req.reason).await?;
        if screened.is_err() || by == "agent" {
            self.emit_request(&req.id, &req.state).await?;
            return Ok(Ticket {
                request_id: req.id,
                state: req.state,
                reason: req.reason,
                target_node: sid,
                ..Default::default()
            });
        }
        match self.release(&req, "policy").await {
            Ok(item) => Ok(Ticket {
                request_id: req.id,
                state: "queued".into(),
                dispatch_id: item,
                target_node: sid,
                ..Default::default()
            }),
            Err(e) => Ok(Ticket {
                request_id: req.id,
                state: "refused".into(),
                reason: e.to_string(),
                target_node: sid,
                ..Default::default()
            }),
        }
    }

    async fn peer_prompt_target(&self, sid: &str) -> Result<()> {
        let Some(s) = self.store().session(sid).await? else {
            bail!("unknown session");
        };
        if !s.is_open() {
            bail!("the session has ended");
        }
        if !self.session_accepts(sid).await?.is_some_and(|a| a.prompts) {
            bail!("the session does not take remote prompts");
        }
        Ok(())
    }

    async fn queue_peer_prompt(&self, origin: &str, sid: &str, text: &str) -> Result<String> {
        crate::control::check_a2a(self, origin, sid, &format!("node:{origin}")).await?;
        self.peer_prompt_target(sid).await?;
        // The kill switch too (queue_prompt_from), with the node as origin.
        Ok(self
            .queue_prompt_from(sid, text, &format!("node:{origin}"))
            .await?
            .id)
    }

    /// The status of requests `origin` made.
    pub async fn fleet_status(&self, origin: &str, ids: &[String]) -> Result<Vec<Status>> {
        let mut out = Vec::new();
        for id in ids {
            let Some(r) = self.store().node_request(id).await? else {
                continue;
            };
            if r.origin_node != origin {
                continue;
            }
            let mut st = Status {
                request_id: r.id.clone(),
                dispatch_id: r.dispatch_id.clone(),
                state: r.state.clone(),
                reason: r.reason.clone(),
                target_node: r.target_node.clone(),
                ..Default::default()
            };
            if r.state == "queued"
                && r.kind != "prompt"
                && let Some(d) = self.store().dispatch(&r.dispatch_id).await?
            {
                st.session_id = d.session_id.clone();
                st.state = match d.status.as_str() {
                    "failed" | "refused" => "failed".into(),
                    "started" => "started".into(),
                    "delivered" => "delivered".into(),
                    _ => "queued".into(),
                };
                if !d.error.is_empty() {
                    st.reason = d.error.clone();
                }
                if !d.session_id.is_empty()
                    && let Some(s) = self.store().session(&d.session_id).await?
                    && !s.is_open()
                {
                    st.state = "finished".into();
                }
            }
            out.push(st);
        }
        Ok(out)
    }

    async fn emit_request(&self, id: &str, state: &str) -> Result<()> {
        self.emit(kinds::FLEET_REQUEST, id, &json!({ "state": state }))
            .await?;
        Ok(())
    }

    async fn audit_request(&self, r: &NodeRequest, what: &str, why: &str) -> Result<()> {
        self.audit_record(LedgerDraft {
            kind: "node_request".into(),
            actor: format!("node:{}", r.origin_node),
            actor_kind: if r.initiated_by == "user" { "host" } else { "agent" }.into(),
            msg: format!("{} request {what}", r.kind),
            subject: json!({ "request": r.id, "target": r.target_node, "kind": r.kind, "cwd": r.cwd }),
            payload: json!({ "outcome": what, "reason": why, "initiatedBy": r.initiated_by,
                             "originSession": r.origin_session }),
            origin: format!("host:{}", r.origin_node),
            node_id: r.origin_node.clone(),
            session_id: r.origin_session.clone(),
            ..Default::default()
        })
        .await?;
        Ok(())
    }
}

/// Where a detected harness came from (harnesses.md C8 rule 1, C7.3): the
/// binary's absolute path, the hash of its version output and when the host
/// probed it, one fact per line for the Fleet chip's hover. Facts the host
/// did not report are left out; an unknown probe time says so.
pub fn probe_provenance(h: &kls_store::HarnessInfo, now: i64) -> String {
    let mut out = vec![format!("{} {}", h.name, h.version).trim().to_string()];
    if !h.path.is_empty() {
        out.push(format!("path {}", h.path));
    }
    if !h.version_hash.is_empty() {
        out.push(format!(
            "version sha256 {}",
            h.version_hash.chars().take(12).collect::<String>()
        ));
    }
    out.push(if h.probed_at_ms > 0 {
        let mins = (now - h.probed_at_ms).max(0) / 60_000;
        match mins {
            0 => "probed just now".to_string(),
            m if m < 120 => format!("probed {m} min ago"),
            m => format!("probed {} h ago", m / 60),
        }
    } else {
        "probe time not reported".to_string()
    });
    out.join("\n")
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    fn probe_provenance_names_path_hash_and_time() {
        let h = kls_store::HarnessInfo {
            name: "Claude Code".into(),
            version: "2.3.1".into(),
            path: "/usr/bin/claude".into(),
            version_hash: "abcdef0123456789ffff".into(),
            probed_at_ms: 1_000,
            ..Default::default()
        };
        let s = probe_provenance(&h, 1_000 + 5 * 60_000);
        assert_eq!(
            s,
            "Claude Code 2.3.1\npath /usr/bin/claude\nversion sha256 abcdef012345\nprobed 5 min ago"
        );
        let bare = kls_store::HarnessInfo {
            name: "Aider".into(),
            ..Default::default()
        };
        assert_eq!(probe_provenance(&bare, 0), "Aider\nprobe time not reported");
    }
}
