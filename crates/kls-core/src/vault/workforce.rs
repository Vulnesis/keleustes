//! Workforce (WP5): saved agents and crews reach nodes, crew checks in
//! core, the avatar model job, and worker controls.
//!
//! **Reaching nodes (P0 #14).** Saving an agent or crew backs it up to the
//! library and sends the existing `library_install` dispatch to its target
//! nodes (no proto change): the nodes named, or else every node that
//! already holds it. Each dispatch goes through
//! [`crate::hosts::library_dispatch`], so it is console-only, refused under
//! the kill switch and the A2A blackout, and audited with its origin. A
//! crew's saved members travel in the same dispatch.
//!
//! **Worker controls.** Pause, resume, start, stop and crew pause/resume
//! all. Winding down (pause, stop) always goes out; starting or resuming is
//! new work and is refused under the kill switch, and resuming a crew
//! restarts agent-to-agent traffic, so it is also refused under the A2A
//! blackout. Every control is audited, sent or refused.

use super::{
    AGENT_KIND, AgentProfile, CREW_KIND, Crew, agent_to_library, sanitize_svg, save_agent,
    save_crew, save_library,
};
use crate::Core;
use crate::control;
use crate::gateway::{CrewControl, DispatchSpec, JobNeeds, LibraryRef, Trigger};
use crate::hosts::{self, Origin, Refusal};
use anyhow::{Context, Result, anyhow, bail};
use kls_store::audit::LedgerDraft;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::OnceLock;

pub mod kinds {
    /// An agent or crew was sent to its target nodes. Subject: library item.
    pub const WORKFORCE_PUBLISHED: &str = "workforce.published";
    /// A worker or crew control went out. Subject: node id.
    pub const WORKFORCE_CONTROL: &str = "workforce.control";
    /// A worker or crew control was refused. Subject: node id.
    pub const WORKFORCE_REFUSED: &str = "workforce.refused";
    /// An avatar was drawn for a profile. Subject: profile name.
    pub const AVATAR_DRAWN: &str = "agent.avatar_drawn";
}

/// Most replicas one crew member may run (the website's cap).
pub const MAX_REPLICAS: i64 = 8;
/// Avatar drawings per profile per hour.
pub const AVATAR_PER_HOUR: usize = 5;
/// Avatar candidates kept per profile.
pub const AVATAR_CANDIDATES: usize = 5;

// ---- crew checks ----------------------------------------------------------------

/// Builtin belai profiles (the engine's catalogue), read here so core can
/// check a crew of builtins without depending on the engine.
fn builtin_profiles() -> &'static [Value] {
    static P: OnceLock<Vec<Value>> = OnceLock::new();
    P.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../../kls-engine/src/builtins/profiles.json"
        ))
        .unwrap_or_default()
    })
}

/// A member's claim rules: (lists, labels, handoff labels, success move).
#[derive(Clone, Debug, Default)]
struct Claims {
    lists: Vec<String>,
    labels: Vec<String>,
    handoff: Vec<String>,
    success: Option<(String, Vec<String>)>,
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The kanban rules of a worker profile, `None` when it claims nothing.
fn claims_of(spec: &Value) -> Option<Claims> {
    let k = spec.get("kanban").filter(|k| k.is_object())?;
    let success = k.get("on_success").and_then(|m| {
        let list = m
            .get("list")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let labels = strings(m.get("labels"));
        (!labels.is_empty() && list != "done").then_some((list, labels))
    });
    Some(Claims {
        lists: strings(k.get("lists")),
        labels: strings(k.get("labels")),
        handoff: strings(k.get("handoff_labels")),
        success,
    })
}

/// Problems with a crew (the website's `validateCrew` / `claimConflicts`):
/// no member, a replica count past [`MAX_REPLICAS`], two members that claim
/// the same cards, a handoff label no member claims. Members resolve to a
/// saved profile first, then a builtin; a member Keleustes does not know
/// (it may live on a node) is not checked for claims.
pub async fn crew_problems(core: &Core, c: &Crew) -> Result<Vec<String>> {
    let mut out = Vec::new();
    if c.members.is_empty() {
        out.push("a crew needs at least one member".into());
    }
    let mut specs: Vec<(String, Claims)> = Vec::new();
    for m in &c.members {
        if !(1..=MAX_REPLICAS).contains(&m.count) {
            out.push(format!("{}: 1 to {MAX_REPLICAS} replicas", m.agent));
        }
        let saved = core
            .store()
            .agent_profile_by_name(&m.agent)
            .await?
            .and_then(|a| serde_json::from_str::<Value>(&a.meta_json).ok())
            .and_then(|v| claims_of(&v));
        let claims = saved.or_else(|| {
            builtin_profiles()
                .iter()
                .find(|p| p.get("name").and_then(Value::as_str) == Some(m.agent.as_str()))
                .and_then(claims_of)
        });
        if let Some(k) = claims
            && !specs.iter().any(|(n, _)| n == &m.agent)
        {
            specs.push((m.agent.clone(), k));
        }
    }
    let claims = |k: &Claims, list: &str, labels: &[String]| {
        k.lists.iter().any(|l| l == list)
            && k.labels
                .iter()
                .all(|need| labels.iter().any(|l| l.eq_ignore_ascii_case(need)))
    };
    for (i, (a, ka)) in specs.iter().enumerate() {
        for (b, kb) in specs.iter().skip(i + 1) {
            if ka.lists.iter().any(|l| kb.lists.contains(l))
                && !ka.labels.is_empty()
                && ka.labels == kb.labels
            {
                out.push(format!("{a} and {b} claim the same cards"));
            }
        }
        let mut hand: Vec<(String, Vec<String>)> = Vec::new();
        if !ka.handoff.is_empty() {
            hand.push(("backlog".into(), ka.handoff.clone()));
        }
        if let Some(s) = &ka.success {
            hand.push(s.clone());
        }
        for (list, labels) in hand {
            let taken = specs.iter().any(|(_, k)| claims(k, &list, &labels));
            if !taken && !labels.iter().all(|l| l == "ticket" || l == "reviewed") {
                out.push(format!(
                    "{a} hands off {} to {list}, which no member claims",
                    labels.join(", ")
                ));
            }
        }
    }
    Ok(out)
}

// ---- reaching nodes -------------------------------------------------------------

/// One target node's dispatch: its id, or why it was refused.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sent {
    pub node_id: String,
    pub node: String,
    pub dispatch_id: String,
    /// The refusal code ("kill_switch", "a2a_blackout", "not_offered"...),
    /// empty when it went out.
    pub refused: String,
}

/// What a save sent: the library item and version, and each node.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Published {
    pub item_id: String,
    pub version: i64,
    pub sent: Vec<Sent>,
}

impl Published {
    pub fn refused(&self) -> usize {
        self.sent.iter().filter(|s| !s.refused.is_empty()).count()
    }
}

/// Nodes that already hold `name` (inventory kind `profile`/`agent` or
/// `crew`, or the host's agent list) and take `library_install`.
pub async fn install_targets(core: &Core, kind: &str, name: &str) -> Result<Vec<String>> {
    let kinds: &[&str] = if kind == CREW_KIND {
        &["crew"]
    } else {
        &["profile", "agent"]
    };
    Ok(core
        .store()
        .nodes()
        .await?
        .into_iter()
        .filter(|n| n.revoked_at.is_none())
        .filter(|n| n.host.capabilities.takes_dispatch("library_install"))
        .filter(|n| {
            n.host
                .capabilities
                .inventory
                .iter()
                .any(|i| kinds.contains(&i.kind.as_str()) && i.name == name)
                || (kind != CREW_KIND && n.host.agents.iter().any(|a| a == name))
        })
        .map(|n| n.id)
        .collect())
}

/// Sends `refs` to each target with `library_install`. A refusal is kept
/// per node (already audited by `library_dispatch`), never an error.
async fn send(
    core: &Core,
    targets: &[String],
    refs: Vec<LibraryRef>,
    origin: &Origin,
) -> Vec<Sent> {
    let mut out = Vec::new();
    for node_id in targets {
        let node = core
            .store()
            .node(node_id)
            .await
            .ok()
            .flatten()
            .map(|n| n.name)
            .unwrap_or_default();
        let r = hosts::library_dispatch(
            core,
            node_id,
            "library_install",
            refs.clone(),
            false,
            origin.clone(),
        )
        .await;
        out.push(match r {
            Ok(id) => Sent {
                node_id: node_id.clone(),
                node,
                dispatch_id: id,
                refused: String::new(),
            },
            Err(e) => Sent {
                node_id: node_id.clone(),
                node,
                dispatch_id: String::new(),
                refused: hosts::refusal_of(&e)
                    .map(|r| r.code().to_string())
                    .unwrap_or_else(|| format!("{e:#}")),
            },
        });
    }
    out
}

async fn record_publish(
    core: &Core,
    kind: &str,
    name: &str,
    p: &Published,
    origin: &Origin,
) -> Result<()> {
    let data = json!({
        "kind": kind, "name": name, "itemId": p.item_id, "version": p.version,
        "sent": p.sent, "origin": origin,
    });
    core.emit(kinds::WORKFORCE_PUBLISHED, &p.item_id, &data)
        .await?;
    core.audit_record(LedgerDraft {
        kind: "workforce".into(),
        actor: if origin.actor.is_empty() {
            "keleustes".into()
        } else {
            origin.actor.clone()
        },
        actor_kind: "user".into(),
        msg: format!(
            "{kind} {name} v{} saved by {}: {} node(s), {} refused",
            p.version,
            origin.label(),
            p.sent.len(),
            p.refused()
        ),
        subject: json!({ "item": p.item_id, "name": name }),
        payload: data,
        ..Default::default()
    })
    .await?;
    Ok(())
}

/// Backs agent `agent_id` up to the library and installs it on `targets`
/// (every node that holds it when `None`).
pub async fn publish_agent(
    core: &Core,
    agent_id: &str,
    targets: Option<&[String]>,
    origin: Origin,
) -> Result<Published> {
    let a = core
        .store()
        .agent_profile(agent_id)
        .await?
        .ok_or_else(|| anyhow!("no such agent"))?;
    let (item_id, version) = agent_to_library(core, agent_id, "saved").await?;
    let targets = match targets {
        Some(t) => t.to_vec(),
        None => install_targets(core, AGENT_KIND, &a.name).await?,
    };
    let refs = vec![LibraryRef {
        kind: "profile".into(),
        id: item_id.clone(),
        name: a.name.clone(),
        version,
        ..Default::default()
    }];
    let p = Published {
        item_id,
        version,
        sent: send(core, &targets, refs, &origin).await,
    };
    record_publish(core, "agent", &a.name, &p, &origin).await?;
    Ok(p)
}

/// [`save_agent`], then [`publish_agent`].
pub async fn save_and_publish_agent(
    core: &Core,
    a: &AgentProfile,
    targets: Option<&[String]>,
    origin: Origin,
) -> Result<(AgentProfile, Published)> {
    let saved = save_agent(core, a).await?;
    let p = publish_agent(core, &saved.id, targets, origin).await?;
    Ok((saved, p))
}

/// Backs a crew up to the library (kind `crew`): its JSON with members.
pub async fn crew_to_library(core: &Core, crew_id: &str, note: &str) -> Result<(String, i64)> {
    let c = core
        .store()
        .crews()
        .await?
        .into_iter()
        .find(|c| c.id == crew_id)
        .ok_or_else(|| anyhow!("no such crew"))?;
    let body = serde_json::to_string_pretty(&json!({
        "name": c.name, "description": c.description, "process": c.process,
        "members": c.members, "crewVersion": c.version,
    }))?;
    save_library(core, "", CREW_KIND, &c.name, &c.description, &body, note).await
}

/// Backs a crew up and installs it, with its saved members, on `targets`
/// (every node that holds the crew when `None`).
pub async fn publish_crew(
    core: &Core,
    crew_id: &str,
    targets: Option<&[String]>,
    origin: Origin,
) -> Result<Published> {
    let c = core
        .store()
        .crews()
        .await?
        .into_iter()
        .find(|c| c.id == crew_id)
        .ok_or_else(|| anyhow!("no such crew"))?;
    let (item_id, version) = crew_to_library(core, crew_id, "saved").await?;
    let mut refs = vec![LibraryRef {
        kind: CREW_KIND.into(),
        id: item_id.clone(),
        name: c.name.clone(),
        version,
        ..Default::default()
    }];
    for m in &c.members {
        if refs.iter().any(|r| r.name == m.agent) {
            continue;
        }
        if let Some(a) = core.store().agent_profile_by_name(&m.agent).await? {
            let (id, v) = agent_to_library(core, &a.id, "crew saved").await?;
            refs.push(LibraryRef {
                kind: "profile".into(),
                id,
                name: a.name,
                version: v,
                ..Default::default()
            });
        }
    }
    refs.truncate(64);
    let targets = match targets {
        Some(t) => t.to_vec(),
        None => install_targets(core, CREW_KIND, &c.name).await?,
    };
    let p = Published {
        item_id,
        version,
        sent: send(core, &targets, refs, &origin).await,
    };
    record_publish(core, "crew", &c.name, &p, &origin).await?;
    Ok(p)
}

/// [`save_crew`] (checked), then [`publish_crew`].
pub async fn save_and_publish_crew(
    core: &Core,
    c: &Crew,
    targets: Option<&[String]>,
    origin: Origin,
) -> Result<(Crew, Published)> {
    let saved = save_crew(core, c).await?;
    let p = publish_crew(core, &saved.id, targets, origin).await?;
    Ok((saved, p))
}

// ---- avatar job -----------------------------------------------------------------

/// What the avatar job is asked to draw from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AvatarBrief {
    pub display_name: String,
    /// Up to four `#rrggbb` colours.
    pub palette: Vec<String>,
    pub report_style: String,
    pub focus: String,
    pub vocabulary: Vec<String>,
}

fn candidates_key(name: &str) -> String {
    format!("avatar.candidates.{name}")
}

/// The avatars drawn for `name` so far, newest first.
pub async fn avatar_candidates(core: &Core, name: &str) -> Result<Vec<String>> {
    Ok(core
        .store()
        .setting(&candidates_key(name))
        .await?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default())
}

/// Draws an avatar for profile `name` with the model job `avatar` on a
/// node (`{displayName, palette, reportStyle, focus, vocabulary}` ->
/// `{svg}`). Refused under the kill switch and past
/// [`AVATAR_PER_HOUR`] per profile; the SVG is sanitised and kept with the
/// last [`AVATAR_CANDIDATES`]. Audited.
pub async fn draw_avatar(
    core: &Core,
    name: &str,
    brief: &AvatarBrief,
    provider: Option<&str>,
    origin: &str,
    timeout_ms: i64,
) -> Result<String> {
    if !super::valid_name(name) {
        bail!("name the agent first");
    }
    control::check(core, "job", name, origin).await?;
    let hour_ago = kls_store::now_ms() - 3_600_000;
    let recent = core
        .store()
        .recent_events(2000)
        .await?
        .into_iter()
        .filter(|e| e.kind == kinds::AVATAR_DRAWN && e.subject == name && e.at >= hour_ago)
        .count();
    if recent >= AVATAR_PER_HOUR {
        bail!("{AVATAR_PER_HOUR} avatars an hour per agent; try later");
    }
    let palette: Vec<&String> = brief.palette.iter().take(4).collect();
    let payload = json!({
        "task": "avatar",
        "displayName": brief.display_name,
        "palette": palette,
        "reportStyle": brief.report_style,
        "focus": brief.focus,
        "vocabulary": brief.vocabulary.iter().take(12).collect::<Vec<_>>(),
    });
    let out = core
        .gateway()?
        .job(
            None,
            "model",
            &payload.to_string(),
            JobNeeds {
                provider: provider.filter(|p| !p.is_empty()).map(str::to_string),
                ..Default::default()
            },
            timeout_ms,
        )
        .await?;
    if out.status != "ok" {
        bail!(
            "the node could not draw the avatar ({}): {}",
            out.status,
            out.error
        );
    }
    let v: Value = serde_json::from_str(&out.output_json).context("the avatar is not JSON")?;
    let svg = sanitize_svg(v.get("svg").and_then(Value::as_str).unwrap_or(""));
    if svg.is_empty() {
        bail!("the node's avatar is not plain SVG");
    }
    let mut kept = avatar_candidates(core, name).await?;
    kept.retain(|s| s != &svg);
    kept.insert(0, svg.clone());
    kept.truncate(AVATAR_CANDIDATES);
    core.store()
        .set_setting(&candidates_key(name), &serde_json::to_string(&kept)?)
        .await?;
    core.emit(
        kinds::AVATAR_DRAWN,
        name,
        &json!({"name": name, "node": out.node_id, "origin": origin, "bytes": svg.len()}),
    )
    .await?;
    core.audit_record(LedgerDraft {
        kind: "workforce".into(),
        actor: origin.into(),
        actor_kind: "user".into(),
        msg: format!("avatar drawn for {name} on {}", out.node_id),
        subject: json!({ "agent": name }),
        payload: json!({ "node": out.node_id, "origin": origin }),
        node_id: out.node_id.clone(),
        ..Default::default()
    })
    .await?;
    Ok(svg)
}

// ---- worker controls ------------------------------------------------------------

/// A worker or crew control the Workforce screen sends.
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    /// Pause one worker.
    Pause {
        worker: String,
    },
    Resume {
        worker: String,
    },
    /// End a worker's session.
    Stop {
        session: String,
    },
    /// Start a worker of `profile` in `cwd`.
    Start {
        profile: String,
        cwd: String,
    },
    /// Pause or resume every worker of crew run `run`.
    Crew {
        run: String,
        crew: String,
        workers: Vec<String>,
        paused: bool,
    },
    /// Stop crew run `run`.
    CrewStop {
        run: String,
        crew: String,
    },
    /// Change crew run `run`'s scope or a member's replicas (#10).
    CrewEdit {
        run: String,
        crew: String,
        edit: CrewEdit,
    },
}

/// Weight bounds of a crew run's directory (belai `fleet.MinWeight`/`MaxWeight`).
pub const CREW_WEIGHT: std::ops::RangeInclusive<i32> = 1..=10;

/// One crew run edit; the host applies it at the next claim.
#[derive(Clone, Debug, PartialEq)]
pub enum CrewEdit {
    /// Set a scope directory's weight.
    Weight { dir: String, weight: i32 },
    /// Pause or resume claiming from one directory.
    PauseDir { dir: String, paused: bool },
    /// Add a trusted directory to the scope (weight 0: the minimum).
    AddDir { dir: String, weight: i32 },
    /// Drop a directory from the scope.
    RemoveDir { dir: String },
    /// Set a member profile's replicas.
    Replicas { profile: String, replicas: u32 },
}

impl CrewEdit {
    fn op(&self) -> &'static str {
        match self {
            CrewEdit::Weight { .. } => "scope_weight",
            CrewEdit::PauseDir { .. } => "scope_pause",
            CrewEdit::AddDir { .. } => "scope_add",
            CrewEdit::RemoveDir { .. } => "scope_remove",
            CrewEdit::Replicas { .. } => "replicas",
        }
    }

    /// Checks the values against the host's bounds.
    pub fn validate(&self) -> std::result::Result<(), String> {
        let dir_ok = |d: &str| d.starts_with('/') && !d.chars().any(char::is_control);
        match self {
            CrewEdit::Weight { dir, weight } => {
                if !CREW_WEIGHT.contains(weight) {
                    return Err("a weight runs 1 to 10".into());
                }
                if !dir_ok(dir) {
                    return Err("that is not a directory".into());
                }
            }
            CrewEdit::AddDir { dir, weight } => {
                if *weight != 0 && !CREW_WEIGHT.contains(weight) {
                    return Err("a weight runs 1 to 10".into());
                }
                if !dir_ok(dir) {
                    return Err("that is not a directory".into());
                }
            }
            CrewEdit::PauseDir { dir, .. } | CrewEdit::RemoveDir { dir } => {
                if !dir_ok(dir) {
                    return Err("that is not a directory".into());
                }
            }
            CrewEdit::Replicas { profile, replicas } => {
                if i64::from(*replicas) > MAX_REPLICAS {
                    return Err(format!("replicas run 0 to {MAX_REPLICAS}"));
                }
                if profile.is_empty()
                    || profile.len() > 64
                    || !profile
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c))
                {
                    return Err("that is not a profile name".into());
                }
            }
        }
        Ok(())
    }

    fn control(&self, run: &str) -> CrewControl {
        let mut c = CrewControl {
            run: run.into(),
            op: self.op().into(),
            ..Default::default()
        };
        match self {
            CrewEdit::Weight { dir, weight } | CrewEdit::AddDir { dir, weight } => {
                c.dir = dir.clone();
                c.weight = *weight;
            }
            CrewEdit::PauseDir { dir, paused } => {
                c.dir = dir.clone();
                c.paused = *paused;
            }
            CrewEdit::RemoveDir { dir } => c.dir = dir.clone(),
            CrewEdit::Replicas { profile, replicas } => {
                c.profile = profile.clone();
                c.replicas = *replicas;
            }
        }
        c
    }

    /// Only holds or narrows work.
    fn winds_down(&self) -> bool {
        matches!(
            self,
            CrewEdit::PauseDir { paused: true, .. }
                | CrewEdit::RemoveDir { .. }
                | CrewEdit::Replicas { replicas: 0, .. }
        )
    }
}

impl Control {
    fn name(&self) -> &'static str {
        match self {
            Control::Pause { .. } => "pause",
            Control::Resume { .. } => "resume",
            Control::Stop { .. } => "stop",
            Control::Start { .. } => "start",
            Control::Crew { paused: true, .. } => "crew_pause",
            Control::Crew { paused: false, .. } => "crew_resume",
            Control::CrewStop { .. } => "crew_stop",
            Control::CrewEdit { edit, .. } => match edit {
                CrewEdit::Weight { .. } => "crew_scope_weight",
                CrewEdit::PauseDir { .. } => "crew_scope_pause",
                CrewEdit::AddDir { .. } => "crew_scope_add",
                CrewEdit::RemoveDir { .. } => "crew_scope_remove",
                CrewEdit::Replicas { .. } => "crew_replicas",
            },
        }
    }

    /// Only holds or ends work.
    fn winds_down(&self) -> bool {
        match self {
            Control::CrewEdit { edit, .. } => edit.winds_down(),
            _ => matches!(
                self,
                Control::Pause { .. }
                    | Control::Stop { .. }
                    | Control::CrewStop { .. }
                    | Control::Crew { paused: true, .. }
            ),
        }
    }

    fn specs(&self, origin: &Origin) -> Vec<DispatchSpec> {
        let trigger = Some(Trigger {
            source: "console".into(),
            subject: origin.surface.clone(),
            ..Default::default()
        });
        match self {
            Control::Pause { worker } | Control::Resume { worker } => vec![DispatchSpec {
                kind: self.name().into(),
                worker_id: worker.clone(),
                trigger,
                ..Default::default()
            }],
            Control::Stop { session } => vec![DispatchSpec {
                kind: "stop".into(),
                session_id: session.clone(),
                trigger,
                ..Default::default()
            }],
            Control::Start { profile, cwd } => vec![DispatchSpec {
                kind: "worker".into(),
                agent: profile.clone(),
                cwd: cwd.clone(),
                trigger,
                ..Default::default()
            }],
            Control::Crew {
                run,
                workers,
                paused,
                ..
            } => workers
                .iter()
                .map(|w| DispatchSpec {
                    kind: "crew_control".into(),
                    control: Some(CrewControl {
                        run: run.clone(),
                        op: if *paused {
                            "worker_pause"
                        } else {
                            "worker_resume"
                        }
                        .into(),
                        worker: w.clone(),
                        paused: *paused,
                        ..Default::default()
                    }),
                    trigger: trigger.clone(),
                    ..Default::default()
                })
                .collect(),
            Control::CrewStop { run, .. } => vec![DispatchSpec {
                kind: "crew_control".into(),
                control: Some(CrewControl {
                    run: run.clone(),
                    op: "stop".into(),
                    ..Default::default()
                }),
                trigger,
                ..Default::default()
            }],
            Control::CrewEdit { run, edit, .. } => vec![DispatchSpec {
                kind: "crew_control".into(),
                control: Some(edit.control(run)),
                trigger,
                ..Default::default()
            }],
        }
    }

    fn target(&self) -> String {
        match self {
            Control::Pause { worker } | Control::Resume { worker } => worker.clone(),
            Control::Stop { session } => session.clone(),
            Control::Start { profile, .. } => profile.clone(),
            Control::Crew { crew, run, .. }
            | Control::CrewStop { crew, run }
            | Control::CrewEdit { crew, run, .. } => {
                format!("crew:{crew}/{run}")
            }
        }
    }
}

async fn audit_control(
    core: &Core,
    node_id: &str,
    c: &Control,
    origin: &Origin,
    outcome: &str,
    reason: &str,
    ids: &[String],
) -> Result<()> {
    let mut data = json!({
        "nodeId": node_id, "control": c.name(), "target": c.target(), "origin": origin,
        "outcome": outcome, "reason": reason, "dispatches": ids,
    });
    if let Control::CrewEdit { run, edit, .. } = c {
        let cc = edit.control(run);
        data["edit"] = json!({
            "op": cc.op, "dir": cc.dir, "weight": cc.weight, "paused": cc.paused,
            "profile": cc.profile, "replicas": cc.replicas,
        });
    }
    let kind = if outcome == "refused" {
        kinds::WORKFORCE_REFUSED
    } else {
        kinds::WORKFORCE_CONTROL
    };
    core.emit(kind, node_id, &data).await?;
    core.audit_record(LedgerDraft {
        kind: "workforce".into(),
        actor: if origin.actor.is_empty() {
            "keleustes".into()
        } else {
            origin.actor.clone()
        },
        actor_kind: if outcome == "refused" { "rule" } else { "user" }.into(),
        msg: format!(
            "{} {} on {node_id} by {} outcome={outcome}{}",
            c.name(),
            c.target(),
            origin.label(),
            if reason.is_empty() {
                String::new()
            } else {
                format!(" ({reason})")
            }
        ),
        subject: json!({ "node": node_id, "target": c.target() }),
        payload: data,
        node_id: node_id.into(),
        ..Default::default()
    })
    .await?;
    Ok(())
}

/// Sends a worker or crew control to `node_id`. Returns the dispatch ids.
/// Refusals (kill switch, A2A blackout, a node that does not take the
/// kind) are audited and returned as a [`Refusal`].
pub async fn worker_control(
    core: &Core,
    node_id: &str,
    c: Control,
    origin: Origin,
) -> Result<Vec<String>> {
    let refuse = |r: Refusal| {
        let c = c.clone();
        let origin = origin.clone();
        async move {
            if let Err(e) =
                audit_control(core, node_id, &c, &origin, "refused", r.code(), &[]).await
            {
                tracing::warn!("recording a workforce refusal: {e:#}");
            }
            anyhow::Error::new(r)
        }
    };
    if !c.winds_down() {
        if let Err(e) = control::check(core, "dispatch", node_id, &origin.label()).await {
            let Some(b) = e.downcast_ref::<control::Blocked>() else {
                return Err(e);
            };
            let why = if b.window {
                "held".to_string()
            } else {
                match b.mode {
                    control::Mode::Stop => "stopped".to_string(),
                    _ => "paused".to_string(),
                }
            };
            return Err(refuse(Refusal::KillSwitch(why)).await);
        }
        if matches!(c, Control::Crew { .. } | Control::CrewEdit { .. })
            && control::check_a2a(core, "keleustes", &c.target(), &origin.label())
                .await
                .is_err()
        {
            return Err(refuse(Refusal::A2aBlackout).await);
        }
    }
    if let Control::CrewEdit { edit, .. } = &c
        && let Err(why) = edit.validate()
    {
        return Err(refuse(Refusal::Invalid(why)).await);
    }
    let specs = c.specs(&origin);
    if specs.is_empty() {
        return Err(refuse(Refusal::Invalid("no workers to control".into())).await);
    }
    let node = core.store().node(node_id).await?;
    let takes = |k: &str| {
        node.as_ref()
            .is_some_and(|n| n.revoked_at.is_none() && n.host.capabilities.takes_dispatch(k))
    };
    if !specs.iter().all(|s| takes(&s.kind)) {
        return Err(refuse(Refusal::NotOffered).await);
    }
    let gw = core.gateway()?;
    let mut ids = Vec::new();
    for s in specs {
        ids.push(gw.dispatch(node_id, s).await?);
    }
    audit_control(core, node_id, &c, &origin, "dispatched", "", &ids).await?;
    Ok(ids)
}

#[cfg(test)]
mod tests;
