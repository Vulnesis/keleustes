//! Engine operations over [`Core`]: workflows (draft, publish, versions,
//! restore), runs (start, cancel, pause, resume, retry), approvals,
//! checkpoints and schedules. The UI calls these directly; they write the
//! store and emit events, and the executor (if running in this process)
//! wakes on the events marked `"wake": true`.

use crate::cron;
use crate::dryrun::{self, DryRun};
use crate::graph::Graph;
use crate::validate::{self, Invalid, Mode};
use anyhow::{Context, Result, bail};
use kls_core::Core;
use kls_core::engine::kinds;
use kls_store::engine::{
    ACTIVE_RUN_STATUSES, Approval, Run, RunFilter, RunStep, Schedule, ScheduleSpec, StepUpdate,
    Workflow, WorkflowVersion,
};
use kls_store::now_ms;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;

/// Who acts: the desktop has one person, the owner.
pub const OWNER: &str = "owner";

/// A refusal the UI shows as is (the web factory's 404/409 codes).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EngineError {
    #[error("{0} not found")]
    NotFound(&'static str),
    /// `bad_state`: the action does not apply now.
    #[error("{0}")]
    BadState(String),
    /// `revision_conflict`: someone saved in between.
    #[error("the draft changed since you opened it; reload and try again")]
    RevisionConflict,
    /// `version_conflict`: the version is no longer the draft.
    #[error("version {0} is no longer the draft")]
    VersionConflict(i64),
    #[error("a workflow called \"{0}\" already exists")]
    SlugTaken(String),
    #[error("this approval was already decided")]
    AlreadyDecided,
}

pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// `Security patch train` -> `security-patch-train`.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let s = out.trim_end_matches('-').to_string();
    if s.is_empty() { "workflow".into() } else { s }
}

// ---- workflows ---------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct NewWorkflow {
    pub name: String,
    /// Defaults from the name (a number is added when it is taken).
    pub slug: Option<String>,
    pub description: String,
    pub template_id: String,
    /// `None` gives one manual trigger.
    pub graph: Option<Graph>,
}

/// A workflow with its versions and trigger rows.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowDoc {
    pub workflow: Workflow,
    pub versions: Vec<WorkflowVersion>,
    pub schedules: Vec<Schedule>,
    /// The draft version and its graph, when there is one.
    pub draft: Option<(WorkflowVersion, Graph)>,
    /// The published version and its graph.
    pub published: Option<(WorkflowVersion, Graph)>,
}

impl WorkflowDoc {
    /// What the builder edits: the draft, else the published graph.
    pub fn editable(&self) -> (i64, i64, Graph) {
        match (&self.draft, &self.published) {
            (Some((v, g)), _) => (v.version, v.revision, g.clone()),
            (None, Some((v, g))) => (v.version, 0, g.clone()),
            (None, None) => (0, 0, Graph::starter()),
        }
    }
}

pub async fn create_workflow(core: &Core, w: NewWorkflow) -> Result<Workflow> {
    let graph = w.graph.unwrap_or_else(Graph::starter);
    let issues = validate::validate(&graph, Mode::Save);
    if validate::has_errors(&issues) {
        return Err(Invalid(issues).into());
    }
    let name = if w.name.trim().is_empty() {
        "Untitled workflow".to_string()
    } else {
        w.name.trim().to_string()
    };
    let explicit = w.slug.as_ref().map(|s| slugify(s));
    let mut base = explicit.clone().unwrap_or_else(|| slugify(&name));
    // `sys-` is reserved for the defaults catalogue.
    let system = w.template_id.starts_with("default:");
    if !system && base.starts_with(SYSTEM_SLUG_PREFIX) {
        if explicit.is_some() {
            return Err(EngineError::BadState(format!(
                "\"{SYSTEM_SLUG_PREFIX}\" is kept for the factory's default workflows; pick another name"
            ))
            .into());
        }
        base = format!("wf-{base}");
    }
    let now = now_ms();
    let id = new_id();
    let est = dryrun::estimate(&graph);
    for n in 0..50 {
        let slug = if n == 0 {
            base.clone()
        } else {
            format!("{base}-{}", n + 1)
        };
        let wf = Workflow {
            id: id.clone(),
            slug: slug.clone(),
            name: name.clone(),
            description: w.description.clone(),
            template_id: w.template_id.clone(),
            status: "draft".into(),
            enabled: false,
            published_version: 0,
            draft_version: 1,
            created_at: now,
            updated_at: now,
            archived_at: None,
            ..Default::default()
        };
        let v = WorkflowVersion {
            workflow_id: id.clone(),
            version: 1,
            status: "draft".into(),
            graph_json: graph.to_json(),
            graph_sha256: graph.sha256(),
            note: String::new(),
            est_tokens_min: est.0,
            est_tokens_max: est.1,
            revision: 1,
            created_at: now,
            updated_at: now,
            published_at: None,
        };
        if core.store().insert_workflow(&wf, &v).await? {
            core.emit(
                kinds::WORKFLOW_CREATED,
                &id,
                &json!({ "slug": slug, "name": name, "templateId": w.template_id }),
            )
            .await?;
            return Ok(wf);
        }
        if explicit.is_some() {
            return Err(EngineError::SlugTaken(slug).into());
        }
    }
    Err(EngineError::SlugTaken(base).into())
}

fn graph_of(v: &WorkflowVersion) -> Graph {
    Graph::parse(&v.graph_json).unwrap_or_default()
}

pub async fn workflow_doc(core: &Core, id: &str) -> Result<Option<WorkflowDoc>> {
    let s = core.store();
    let Some(workflow) = s.workflow(id).await? else {
        return Ok(None);
    };
    let versions = s.workflow_versions(&workflow.id).await?;
    let schedules = s.schedules_for_workflow(&workflow.id).await?;
    let draft = versions
        .iter()
        .find(|v| v.status == "draft")
        .map(|v| (v.clone(), graph_of(v)));
    let published = versions
        .iter()
        .find(|v| v.status == "published")
        .map(|v| (v.clone(), graph_of(v)));
    Ok(Some(WorkflowDoc {
        workflow,
        versions,
        schedules,
        draft,
        published,
    }))
}

async fn must_workflow(core: &Core, id: &str) -> Result<Workflow> {
    core.store()
        .workflow(id)
        .await?
        .ok_or_else(|| EngineError::NotFound("workflow").into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Saved {
    pub version: i64,
    pub revision: i64,
}

/// Saves the draft. `revision` 0 means "there is no draft yet": the next
/// version is created from the graph. Shape checks only.
pub async fn save_draft(core: &Core, id: &str, revision: i64, graph: &Graph) -> Result<Saved> {
    let w = must_workflow(core, id).await?;
    if w.status == "archived" {
        return Err(EngineError::BadState("the workflow is archived".into()).into());
    }
    // A person editing a seeded default: catalogue upgrades leave it alone.
    if w.origin == "system" && !w.customised {
        core.store()
            .set_workflow_system(&w.id, &w.origin, &w.system_key, w.system_rev, true)
            .await?;
    }
    let issues = validate::validate(graph, Mode::Save);
    if validate::has_errors(&issues) {
        return Err(Invalid(issues).into());
    }
    let s = core.store();
    let est = dryrun::estimate(graph);
    let saved = if w.draft_version == 0 {
        if revision != 0 {
            return Err(EngineError::RevisionConflict.into());
        }
        let v = s
            .create_draft_version(&w.id, &graph.to_json(), &graph.sha256(), est)
            .await?
            .ok_or(EngineError::RevisionConflict)?;
        Saved {
            version: v.version,
            revision: v.revision,
        }
    } else {
        let r = s
            .save_draft(
                &w.id,
                w.draft_version,
                revision,
                &graph.to_json(),
                &graph.sha256(),
                est,
            )
            .await?
            .ok_or(EngineError::RevisionConflict)?;
        Saved {
            version: w.draft_version,
            revision: r,
        }
    };
    core.emit(
        kinds::WORKFLOW_SAVED,
        &w.id,
        &json!({ "version": saved.version, "revision": saved.revision }),
    )
    .await?;
    Ok(saved)
}

pub async fn rename_workflow(core: &Core, id: &str, name: &str, description: &str) -> Result<()> {
    let w = must_workflow(core, id).await?;
    let name = name.trim();
    if name.is_empty() {
        bail!("give the workflow a name");
    }
    core.store()
        .update_workflow_meta(&w.id, name, description.trim())
        .await?;
    core.emit(kinds::WORKFLOW_UPDATED, &w.id, &json!({ "what": "name" }))
        .await?;
    Ok(())
}

/// Slugs starting with this are the defaults catalogue's.
pub const SYSTEM_SLUG_PREFIX: &str = "sys-";

/// The zone a cron row runs in: its own, else `schedules.tz`, else this
/// machine's zone, else UTC.
pub fn schedule_zone(own: &str, setting: &str) -> String {
    for z in [own.trim(), setting.trim()] {
        if !z.is_empty() && cron::zone(z).is_some() {
            return z.to_string();
        }
    }
    jiff::tz::TimeZone::system()
        .iana_name()
        .filter(|n| cron::zone(n).is_some())
        .unwrap_or("UTC")
        .to_string()
}

/// `schedules.tz` from the settings ("" when unset or unreadable).
pub async fn settings_tz(core: &Core) -> String {
    core.store()
        .setting(kls_core::control::keys::TZ)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// The trigger rows a graph's trigger nodes need (manual triggers have none).
pub fn trigger_specs(g: &Graph, now: i64) -> Vec<ScheduleSpec> {
    trigger_specs_in(g, now, "UTC")
}

/// [`trigger_specs`] with `default_tz` for cron triggers that name no zone.
/// Such a row keeps `tz` empty, so it follows `schedules.tz` when it fires.
pub fn trigger_specs_in(g: &Graph, now: i64, default_tz: &str) -> Vec<ScheduleSpec> {
    g.triggers()
        .filter(|t| t.trigger_kind() != "manual")
        .map(|t| {
            let kind = t.trigger_kind().to_string();
            let tz = t.str("tz").trim().to_string();
            let cron_expr = if kind == "cron" {
                t.str("cron").to_string()
            } else {
                String::new()
            };
            let next = if kind == "cron" {
                cron::next_after(&cron_expr, &schedule_zone(&tz, default_tz), now)
            } else {
                None
            };
            ScheduleSpec {
                id: new_id(),
                node_id: t.id.clone(),
                kind,
                cron: cron_expr,
                tz,
                config_json: Value::Object(t.config.clone()).to_string(),
                next_run_at: next,
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Published {
    pub version: i64,
    pub schedules: Vec<Schedule>,
}

/// Publishes draft `version` after the full checks; reconciles the trigger rows.
pub async fn publish(core: &Core, id: &str, version: i64, note: &str) -> Result<Published> {
    let w = must_workflow(core, id).await?;
    if w.status == "archived" {
        return Err(EngineError::BadState("the workflow is archived".into()).into());
    }
    if w.draft_version == 0 || w.draft_version != version {
        return Err(EngineError::VersionConflict(version).into());
    }
    let s = core.store();
    let v = s
        .workflow_version(&w.id, version)
        .await?
        .ok_or(EngineError::VersionConflict(version))?;
    let g = graph_of(&v);
    let issues = validate::validate(&g, Mode::Publish);
    if validate::has_errors(&issues) {
        return Err(Invalid(issues).into());
    }
    let specs = trigger_specs_in(&g, now_ms(), &settings_tz(core).await);
    if !s
        .publish_version(&w.id, version, note.trim(), &specs)
        .await?
    {
        return Err(EngineError::VersionConflict(version).into());
    }
    let schedules = s.schedules_for_workflow(&w.id).await?;
    core.emit(
        kinds::WORKFLOW_PUBLISHED,
        &w.id,
        &json!({ "version": version, "note": note.trim(), "triggers": schedules.len() }),
    )
    .await?;
    Ok(Published { version, schedules })
}

pub async fn discard_draft(core: &Core, id: &str) -> Result<()> {
    let w = must_workflow(core, id).await?;
    if w.published_version == 0 {
        return Err(EngineError::BadState(
            "there is no published version to go back to; archive the workflow instead".into(),
        )
        .into());
    }
    if !core.store().delete_draft(&w.id).await? {
        return Err(EngineError::BadState("there is no draft".into()).into());
    }
    core.emit(
        kinds::WORKFLOW_UPDATED,
        &w.id,
        &json!({ "what": "draft_discarded" }),
    )
    .await?;
    Ok(())
}

/// Copies version `version`'s graph into the draft (creating one if needed).
pub async fn restore_version(core: &Core, id: &str, version: i64) -> Result<Saved> {
    let w = must_workflow(core, id).await?;
    let v = core
        .store()
        .workflow_version(&w.id, version)
        .await?
        .ok_or(EngineError::NotFound("version"))?;
    let g = graph_of(&v);
    let revision = if w.draft_version == 0 {
        0
    } else {
        core.store()
            .workflow_version(&w.id, w.draft_version)
            .await?
            .map(|d| d.revision)
            .unwrap_or(0)
    };
    save_draft(core, &w.id, revision, &g).await
}

pub async fn set_workflow_enabled(core: &Core, id: &str, enabled: bool) -> Result<()> {
    let w = must_workflow(core, id).await?;
    if enabled && w.published_version == 0 {
        return Err(EngineError::BadState("publish the workflow first".into()).into());
    }
    if !core.store().set_workflow_enabled(&w.id, enabled).await? {
        return Err(EngineError::BadState("the workflow is archived".into()).into());
    }
    core.emit(
        kinds::WORKFLOW_UPDATED,
        &w.id,
        &json!({ "what": "enabled", "enabled": enabled }),
    )
    .await?;
    Ok(())
}

/// Archives (history kept). Refused while runs are active unless `cancel_runs`.
pub async fn archive_workflow(core: &Core, id: &str, cancel_runs: bool) -> Result<u64> {
    let w = must_workflow(core, id).await?;
    let active = core
        .store()
        .workflow_runs_in(&w.id, &ACTIVE_RUN_STATUSES, None)
        .await?;
    if !active.is_empty() && !cancel_runs {
        return Err(EngineError::BadState(format!(
            "{} runs are still active; cancel them first",
            active.len()
        ))
        .into());
    }
    let mut cancelled = 0;
    for r in &active {
        if cancel_run(core, &r.id, "workflow archived").await.is_ok() {
            cancelled += 1;
        }
    }
    core.store().archive_workflow(&w.id).await?;
    core.emit(
        kinds::WORKFLOW_ARCHIVED,
        &w.id,
        &json!({ "cancelledRuns": cancelled }),
    )
    .await?;
    Ok(cancelled)
}

/// Dry run of the draft (or `version`).
pub async fn dry_run_workflow(
    core: &Core,
    id: &str,
    version: Option<i64>,
    sample: &Value,
) -> Result<DryRun> {
    let doc = workflow_doc(core, id)
        .await?
        .ok_or(EngineError::NotFound("workflow"))?;
    let g = match version {
        Some(v) => doc
            .versions
            .iter()
            .find(|x| x.version == v)
            .map(graph_of)
            .ok_or(EngineError::NotFound("version"))?,
        None => doc.editable().2,
    };
    Ok(dryrun::dry_run(&g, sample))
}

// ---- schedules -------------------------------------------------------------

pub async fn set_schedule_enabled(core: &Core, id: &str, enabled: bool) -> Result<Schedule> {
    let s = core.store();
    let row = s
        .schedule(id)
        .await?
        .ok_or(EngineError::NotFound("schedule"))?;
    let next = if enabled && row.kind == "cron" {
        cron::next_after(
            &row.cron,
            &schedule_zone(&row.tz, &settings_tz(core).await),
            now_ms(),
        )
    } else {
        None
    };
    s.set_schedule_enabled(id, enabled, next).await?;
    core.emit(
        kinds::SCHEDULE_UPDATED,
        id,
        &json!({ "enabled": enabled, "nextRunAt": next, "workflowId": row.workflow_id }),
    )
    .await?;
    s.schedule(id)
        .await?
        .context("schedule vanished while it was changed")
}

// ---- runs ------------------------------------------------------------------

/// What starts a run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TriggerInfo {
    /// `manual`, `cron`, `kanban`, `event`, `webhook`, `label`, `retry`.
    pub kind: String,
    pub node_id: String,
    pub schedule_id: Option<String>,
    pub label: String,
}

#[derive(Clone, Debug, Default)]
pub struct StartRun {
    pub workflow_id: String,
    pub trigger: TriggerInfo,
    pub repo: String,
    pub card_id: String,
    pub event: Value,
    pub inputs: Value,
    /// Idempotency: a second start with the same key returns the first run.
    pub dedupe_key: Option<String>,
    /// The window that holds the fire (schedule blackout, maintenance): the
    /// run is created `held` and queued when the window ends
    /// ([`release_held`]).
    pub held: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StartOutcome {
    Started(Run),
    /// The dedupe key matched an earlier run.
    Duplicate(Run),
    /// The trigger's overlap policy refused it (or nothing to do).
    Skipped(String),
}

impl StartOutcome {
    pub fn run(&self) -> Option<&Run> {
        match self {
            StartOutcome::Started(r) | StartOutcome::Duplicate(r) => Some(r),
            StartOutcome::Skipped(_) => None,
        }
    }
}

/// Starts a run of the published version. A manual start of a disabled or
/// unpublished workflow is refused.
pub async fn start_run(core: &Core, req: StartRun) -> Result<StartOutcome> {
    let s = core.store();
    let w = must_workflow(core, &req.workflow_id).await?;
    if let Some(_why) = crate::guard::halted(core).await {
        crate::guard::refuse_switch(core, "trigger", json!({ "workflowId": w.id, "trigger": req.trigger.kind, "scheduleId": req.trigger.schedule_id }),
        )
        .await?;
        return Err(EngineError::BadState("the factory is stopped".into()).into());
    }
    if w.status != "published" || w.published_version == 0 {
        return Err(EngineError::BadState("the workflow is not published".into()).into());
    }
    if !w.enabled {
        return Err(EngineError::BadState("the workflow is turned off".into()).into());
    }
    if let Some(k) = &req.dedupe_key
        && let Some(r) = s.run_by_dedupe(k).await?
    {
        return Ok(StartOutcome::Duplicate(r));
    }
    let v = s
        .workflow_version(&w.id, w.published_version)
        .await?
        .context("published version missing")?;
    let g = graph_of(&v);
    // Prerequisites: a model step whose provider no node offers blocks it.
    let missing = prerequisites(core, &g).await?;
    if !missing.is_empty() {
        let why = format!("prerequisite_missing: {}", missing.join("; "));
        crate::guard::refuse(
            core,
            "run",
            &why,
            json!({ "workflowId": w.id, "trigger": req.trigger.kind, "missing": missing }),
        )
        .await?;
        return Ok(StartOutcome::Skipped(why));
    }
    let trigger_node = crate::walker::start_node(&g, &req.trigger.node_id)
        .map(|n| n.id.clone())
        .unwrap_or_default();
    let repo = if !req.repo.is_empty() {
        req.repo.clone()
    } else {
        String::new()
    };
    if !repo.is_empty() && !g.scope.repos.is_empty() && !repo_in_scope(&g.scope.repos, &repo) {
        bail!("{repo} is not in the workflow's scope");
    }
    // Overlap policy of the trigger (manual starts queue).
    let overlap = g
        .node(&trigger_node)
        .map(|n| n.str("overlap").to_string())
        .filter(|o| !o.is_empty())
        .unwrap_or_else(|| {
            if req.trigger.kind == "manual" {
                "queue".into()
            } else {
                "skip_if_running".into()
            }
        });
    let repo_filter = (!repo.is_empty()).then_some(repo.as_str());
    let active = s
        .workflow_runs_in(&w.id, &ACTIVE_RUN_STATUSES, repo_filter)
        .await?;
    if !active.is_empty() {
        match overlap.as_str() {
            "skip_if_running" => return Ok(StartOutcome::Skipped("overlap".into())),
            "cancel_previous" => {
                for r in &active {
                    let _ = cancel_run(core, &r.id, "a newer run replaced it").await;
                }
            }
            _ => {}
        }
    }
    let now = now_ms();
    let label = if req.trigger.label.is_empty() {
        req.trigger.kind.clone()
    } else {
        req.trigger.label.clone()
    };
    let run = Run {
        id: new_id(),
        workflow_id: w.id.clone(),
        workflow_version: w.published_version,
        schedule_id: req.trigger.schedule_id.clone(),
        trigger_kind: if req.trigger.kind.is_empty() {
            "manual".into()
        } else {
            req.trigger.kind.clone()
        },
        trigger_node_id: trigger_node,
        trigger_label: label,
        dedupe_key: req.dedupe_key.clone(),
        repo,
        card_id: req.card_id.clone(),
        event_json: if req.event.is_null() {
            "{}".into()
        } else {
            req.event.to_string()
        },
        inputs_json: if req.inputs.is_null() {
            "{}".into()
        } else {
            req.inputs.to_string()
        },
        status: if req.held.is_some() {
            "held".into()
        } else {
            "queued".into()
        },
        reason: req.held.clone().unwrap_or_default(),
        dry_run: false,
        tokens: 0,
        created_at: now,
        updated_at: now,
        started_at: None,
        finished_at: None,
    };
    if !s.insert_run(&run).await? {
        // Lost a race on the dedupe key.
        if let Some(k) = &req.dedupe_key
            && let Some(r) = s.run_by_dedupe(k).await?
        {
            return Ok(StartOutcome::Duplicate(r));
        }
        bail!("the run could not be created");
    }
    if let Some(window) = &req.held {
        core.emit(
            RUN_HELD,
            &run.id,
            &json!({
                "workflowId": w.id, "trigger": run.trigger_kind, "repo": run.repo,
                "cardId": run.card_id, "reason": window, "scheduleId": run.schedule_id,
            }),
        )
        .await?;
    } else {
        core.emit(
            kinds::RUN_QUEUED,
            &run.id,
            &json!({
                "wake": true, "workflowId": w.id, "trigger": run.trigger_kind,
                "repo": run.repo, "cardId": run.card_id,
            }),
        )
        .await?;
    }
    crate::guard::audit(
        core,
        "run",
        "run.start",
        json!({ "runId": run.id, "workflowId": w.id, "version": run.workflow_version }),
        &crate::guard::Origin {
            actor: if run.trigger_kind == "manual" || run.trigger_kind == "retry" {
                kls_core::board::OWNER.into()
            } else {
                "engine".into()
            },
            kind: if run.trigger_kind == "manual" || run.trigger_kind == "retry" {
                "user"
            } else {
                "rule"
            },
            detail: json!({
                "trigger": run.trigger_kind, "label": run.trigger_label,
                "scheduleId": run.schedule_id, "nodeId": run.trigger_node_id,
                "repo": run.repo, "cardId": run.card_id,
                "eventId": req.event.get("id"), "eventKind": req.event.get("kind"),
            }),
        },
        &match &req.held {
            Some(w) => format!("held: {w}"),
            None => "queued".into(),
        },
        json!({}),
    )
    .await?;
    Ok(StartOutcome::Started(run))
}

/// A run a window holds (`held`).
pub const RUN_HELD: &str = "run.held";
/// `trigger.skipped`: a gated default an event would have started.
pub const TRIGGER_SKIPPED: &str = "trigger.skipped";

/// Queues every `held` run whose window has ended (and while the kill
/// switch allows new work). Each release is evented and audited; returns the
/// released run ids. Held runs live in the store, so they survive a restart.
pub async fn release_held(core: &Core) -> Result<Vec<String>> {
    let s = core.store();
    let mut out = Vec::new();
    if crate::guard::halted(core).await.is_some() {
        return Ok(out);
    }
    for r in s.runs_in(&["held"]).await? {
        let action = if r.trigger_kind == "cron" {
            "schedule"
        } else {
            "trigger"
        };
        if crate::guard::window(core, action).await.is_some() {
            continue;
        }
        if !s.set_run_status(&r.id, &["held"], "queued", "").await? {
            continue;
        }
        core.emit(
            kinds::RUN_UPDATED,
            &r.id,
            &json!({ "status": "queued", "action": "release", "wake": true, "heldFor": r.reason }),
        )
        .await?;
        crate::guard::audit(
            core,
            "run",
            "run.release",
            json!({ "runId": r.id, "workflowId": r.workflow_id }),
            &crate::guard::Origin::engine(
                json!({ "trigger": r.trigger_kind, "scheduleId": r.schedule_id, "heldFor": r.reason }),
            ),
            "queued",
            json!({}),
        )
        .await?;
        out.push(r.id);
    }
    Ok(out)
}

/// What a workflow needs that nothing offers: each model step's provider
/// must be advertised by a registered node (when any node says which
/// providers it holds). Empty when it may run.
pub async fn prerequisites(core: &Core, g: &Graph) -> Result<Vec<String>> {
    let nodes: Vec<kls_store::Node> = core
        .store()
        .nodes()
        .await?
        .into_iter()
        .filter(|n| n.revoked_at.is_none())
        .collect();
    let advertised: Vec<String> = nodes
        .iter()
        .flat_map(|n| n.host.capabilities.providers.iter().cloned())
        .collect();
    // No node says what it holds: nothing to judge by.
    if advertised.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for n in &g.nodes {
        if !matches!(n.kind.as_str(), "crew" | "decision" | "job" | "scan") {
            continue;
        }
        let provider = match n.str("model").split_once('/') {
            Some((p, _)) => p.to_string(),
            None => n.str("provider").to_string(),
        };
        if provider.is_empty() {
            continue;
        }
        if !advertised.iter().any(|p| p.eq_ignore_ascii_case(&provider)) {
            let msg = format!("no node offers {provider} (\"{}\")", n.title);
            if !out.contains(&msg) {
                out.push(msg);
            }
        }
    }
    Ok(out)
}

/// A workflow with `scope` is attached to `repo`: its scope is empty (any
/// repository) or names it. The Repositories screen and the workflow list
/// both count with this.
pub fn attached_to(scope: &[String], repo: &str) -> bool {
    scope.is_empty() || repo_in_scope(scope, repo)
}

/// How many of `repos` a workflow with `scope` is attached to.
pub fn attached_repo_count<'a>(
    scope: &[String],
    repos: impl IntoIterator<Item = &'a str>,
) -> usize {
    repos.into_iter().filter(|r| attached_to(scope, r)).count()
}

/// The version a workflow runs and lists: the published one, else the draft.
pub fn live_version(w: &Workflow) -> Option<i64> {
    match (w.published_version, w.draft_version) {
        (0, 0) => None,
        (0, d) => Some(d),
        (p, _) => Some(p),
    }
}

/// Runs counted in the workflow list's "7d" column.
pub const LIST_RUNS_WINDOW_MS: i64 = 7 * 86_400_000;

/// The workflow list's summary of one workflow.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListStats {
    /// First trigger in a few words, `+N` for the others ([`Graph::trigger_summary`]).
    pub triggers: String,
    /// Known repositories the workflow is attached to ([`attached_to`]).
    pub repos: usize,
    /// The scope as written (empty: any repository).
    pub scope: Vec<String>,
    /// Real runs in the last [`LIST_RUNS_WINDOW_MS`].
    pub runs_7d: i64,
}

/// One workflow's list summary from its live graph, the known repositories
/// and the per-workflow run counts ([`kls_store::Store::run_counts_since`]).
pub fn list_stats(
    workflow_id: &str,
    g: &Graph,
    repos: &[String],
    runs: &HashMap<String, i64>,
) -> ListStats {
    ListStats {
        triggers: g.trigger_summary(),
        repos: attached_repo_count(&g.scope.repos, repos.iter().map(String::as_str)),
        scope: g.scope.repos.clone(),
        runs_7d: runs.get(workflow_id).copied().unwrap_or(0),
    }
}

/// List summaries of every unarchived workflow, by id (read only).
pub async fn workflow_list_stats(core: &Core) -> Result<HashMap<String, ListStats>> {
    let s = core.store();
    let repos: Vec<String> = s
        .gh_repos(None)
        .await?
        .into_iter()
        .map(|r| r.full_name)
        .collect();
    let runs = s.run_counts_since(now_ms() - LIST_RUNS_WINDOW_MS).await?;
    let mut out = HashMap::new();
    for w in s.workflows(false).await? {
        let g = match live_version(&w) {
            Some(v) => match s.workflow_version(&w.id, v).await? {
                Some(ver) => Graph::parse(&ver.graph_json).unwrap_or_default(),
                None => Graph::default(),
            },
            None => Graph::default(),
        };
        out.insert(w.id.clone(), list_stats(&w.id, &g, &repos, &runs));
    }
    Ok(out)
}

/// `acme/api` matches `acme/api`, `acme/*` and `*` (case-insensitive).
pub fn repo_in_scope(scope: &[String], repo: &str) -> bool {
    let repo = repo.to_lowercase();
    scope.iter().any(|s| {
        let s = s.to_lowercase();
        s == "*"
            || s == repo
            || s.strip_suffix("/*")
                .is_some_and(|owner| repo.split('/').next() == Some(owner))
    })
}

async fn must_run(core: &Core, id: &str) -> Result<Run> {
    core.store()
        .run(id)
        .await?
        .ok_or_else(|| EngineError::NotFound("run").into())
}

/// Open sessions a run's open steps started: the step's session, the
/// session its dispatch started, and any open session tagged with the run.
pub async fn run_sessions(core: &Core, run_id: &str) -> Result<Vec<String>> {
    let s = core.store();
    let mut out: Vec<String> = Vec::new();
    let mut add = |sid: &str| {
        if !sid.is_empty() && !out.iter().any(|x| x == sid) {
            out.push(sid.to_string());
        }
    };
    for st in s.run_steps(run_id).await? {
        if !matches!(st.status.as_str(), "pending" | "running" | "waiting") {
            continue;
        }
        if let Some(sid) = &st.session_id {
            add(sid);
        } else if let Some(did) = st.dispatch_id.as_deref().filter(|d| !d.is_empty())
            && let Some(d) = s.dispatch(did).await?
        {
            add(&d.session_id);
        }
    }
    let q = kls_store::sessions::SessionQuery {
        run_id: Some(run_id.to_string()),
        open_only: true,
        ..Default::default()
    };
    for x in s.sessions(&q).await? {
        add(&x.id);
    }
    Ok(out)
}

pub async fn cancel_run(core: &Core, id: &str, reason: &str) -> Result<Run> {
    let s = core.store();
    let r = must_run(core, id).await?;
    let reason = if reason.trim().is_empty() {
        "cancelled by owner".to_string()
    } else {
        reason.trim().to_string()
    };
    if !s
        .set_run_status(&r.id, &ACTIVE_RUN_STATUSES, "cancelled", &reason)
        .await?
    {
        return Err(EngineError::BadState("the run has already finished".into()).into());
    }
    // Sessions its open steps started, read before the steps are closed.
    let sessions = run_sessions(core, &r.id).await?;
    s.cancel_open_steps(&r.id, "cancelled").await?;
    s.cancel_run_approvals(&r.id).await?;
    for sid in &sessions {
        if let Err(e) = core.queue_command(sid, "stop", &[]).await {
            tracing::debug!(session = %sid, "stop on cancel: {e:#}");
        }
    }
    core.emit(
        kinds::RUN_FINISHED,
        &r.id,
        &json!({ "status": "cancelled", "reason": reason, "action": "cancel", "workflowId": r.workflow_id }),
    )
    .await?;
    crate::guard::audit(
        core,
        "run",
        "run.cancel",
        json!({ "runId": r.id, "workflowId": r.workflow_id }),
        &crate::guard::Origin::person(json!({ "sessions": sessions })),
        &reason,
        json!({}),
    )
    .await?;
    must_run(core, id).await
}

/// What [`stop_all_runs`] did.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopReport {
    pub runs_cancelled: usize,
    /// Open steps (dispatches, jobs, waits) closed with their runs.
    pub steps_cancelled: usize,
    /// Sessions those steps started that were asked to stop.
    pub sessions_stopped: usize,
}

/// The engine side of the kill switch's `stop` mode: cancels every open
/// run (queued, running, waiting, paused), closes their steps and asks the
/// sessions they started to stop. Each cancel is audited.
pub async fn stop_all_runs(core: &Core, reason: &str) -> Result<StopReport> {
    let s = core.store();
    let reason = if reason.trim().is_empty() {
        "kill switch".to_string()
    } else {
        format!("kill switch: {}", reason.trim())
    };
    let mut out = StopReport::default();
    for r in s.runs_in(&ACTIVE_RUN_STATUSES).await? {
        let open = s
            .run_steps(&r.id)
            .await?
            .iter()
            .filter(|x| matches!(x.status.as_str(), "pending" | "running" | "waiting"))
            .count();
        let sessions = run_sessions(core, &r.id).await?.len();
        if cancel_run(core, &r.id, &reason).await.is_ok() {
            out.runs_cancelled += 1;
            out.steps_cancelled += open;
            out.sessions_stopped += sessions;
        }
    }
    Ok(out)
}

pub async fn pause_run(core: &Core, id: &str) -> Result<Run> {
    let s = core.store();
    let r = must_run(core, id).await?;
    let from = ["queued", "running", "waiting_approval"];
    if !from.contains(&r.status.as_str())
        || !s
            .set_run_status(
                &r.id,
                &[r.status.as_str()],
                "paused",
                &format!("paused_from:{}", r.status),
            )
            .await?
    {
        return Err(EngineError::BadState(format!("a {} run cannot be paused", r.status)).into());
    }
    core.emit(
        kinds::RUN_UPDATED,
        &r.id,
        &json!({ "status": "paused", "action": "pause" }),
    )
    .await?;
    must_run(core, id).await
}

pub async fn resume_run(core: &Core, id: &str) -> Result<Run> {
    let s = core.store();
    let r = must_run(core, id).await?;
    if r.status != "paused" {
        return Err(EngineError::BadState(format!("a {} run cannot be resumed", r.status)).into());
    }
    let back = match r.reason.strip_prefix("paused_from:") {
        Some("queued") => "queued",
        Some("waiting_approval") => "waiting_approval",
        _ => "running",
    };
    if !s.set_run_status(&r.id, &["paused"], back, "").await? {
        return Err(EngineError::BadState("the run changed; try again".into()).into());
    }
    core.emit(
        kinds::RUN_UPDATED,
        &r.id,
        &json!({ "status": back, "action": "resume", "wake": true }),
    )
    .await?;
    must_run(core, id).await
}

/// Retries a failed or cancelled run from `from_step` (default: its first
/// failed or cancelled step): a new attempt of that step, and the walker
/// re-runs what follows.
pub async fn retry_run(core: &Core, id: &str, from_step: Option<&str>) -> Result<Run> {
    let s = core.store();
    let r = must_run(core, id).await?;
    if !matches!(r.status.as_str(), "failed" | "cancelled") {
        return Err(EngineError::BadState(format!("a {} run cannot be retried", r.status)).into());
    }
    let steps = s.run_steps(&r.id).await?;
    let pick: Option<&RunStep> = match from_step {
        Some(sid) => Some(
            steps
                .iter()
                .find(|x| x.id == sid || x.node_id == sid)
                .ok_or(EngineError::NotFound("step"))?,
        ),
        None => steps
            .iter()
            .find(|x| matches!(x.status.as_str(), "failed" | "cancelled")),
    };
    if !s.reopen_run(&r.id, &["failed", "cancelled"]).await? {
        return Err(EngineError::BadState("the run changed; try again".into()).into());
    }
    if let Some(p) = pick {
        let attempt = steps
            .iter()
            .filter(|x| x.node_id == p.node_id)
            .map(|x| x.attempt)
            .max()
            .unwrap_or(0)
            + 1;
        let now = now_ms();
        s.insert_step(&RunStep {
            id: new_id(),
            run_id: r.id.clone(),
            node_id: p.node_id.clone(),
            node_type: p.node_type.clone(),
            title: p.title.clone(),
            attempt,
            status: "pending".into(),
            details_json: json!({ "retry": true }).to_string(),
            created_at: now,
            updated_at: now,
            ..Default::default()
        })
        .await?;
    }
    core.emit(
        kinds::RUN_UPDATED,
        &r.id,
        &json!({ "status": "queued", "action": "retry", "wake": true, "fromStep": pick.map(|p| p.node_id.clone()) }),
    )
    .await?;
    must_run(core, id).await
}

/// A run with everything the trace shows.
#[derive(Clone, Debug, PartialEq)]
pub struct RunDetail {
    pub run: Run,
    pub workflow: Option<Workflow>,
    pub graph: Graph,
    pub steps: Vec<RunStep>,
    pub approvals: Vec<Approval>,
}

impl RunDetail {
    pub fn can_cancel(&self) -> bool {
        ACTIVE_RUN_STATUSES.contains(&self.run.status.as_str())
    }
    pub fn can_pause(&self) -> bool {
        matches!(
            self.run.status.as_str(),
            "queued" | "running" | "waiting_approval"
        )
    }
    pub fn can_resume(&self) -> bool {
        self.run.status == "paused"
    }
    pub fn can_retry(&self) -> bool {
        matches!(self.run.status.as_str(), "failed" | "cancelled")
    }
}

pub async fn run_detail(core: &Core, id: &str) -> Result<Option<RunDetail>> {
    let s = core.store();
    let Some(run) = s.run(id).await? else {
        return Ok(None);
    };
    let workflow = s.workflow(&run.workflow_id).await?;
    let graph = s
        .workflow_version(&run.workflow_id, run.workflow_version)
        .await?
        .map(|v| graph_of(&v))
        .unwrap_or_default();
    let steps = s.run_steps(&run.id).await?;
    let approvals = s.approvals(None, Some(&run.id), 100).await?;
    Ok(Some(RunDetail {
        run,
        workflow,
        graph,
        steps,
        approvals,
    }))
}

pub async fn runs(core: &Core, f: &RunFilter) -> Result<Vec<Run>> {
    core.store().runs(f).await
}

// ---- approvals and checkpoints -----------------------------------------------

/// Approve or reject. The step resumes on approve and fails `rejected` on
/// reject (a human step then ends the run `cancelled`, `declined:<node>`).
pub async fn decide_approval(
    core: &Core,
    id: &str,
    approve: bool,
    note: &str,
    revision: Option<i64>,
) -> Result<Approval> {
    let s = core.store();
    let a = s
        .approval(id)
        .await?
        .ok_or(EngineError::NotFound("approval"))?;
    if a.state != "pending" {
        return Err(EngineError::AlreadyDecided.into());
    }
    let note: String = note.trim().chars().take(1000).collect();
    let state = if approve { "approved" } else { "rejected" };
    let Some(d) = s.decide_approval(id, state, OWNER, &note, revision).await? else {
        return Err(match s.approval(id).await? {
            Some(x) if x.state == "pending" => EngineError::RevisionConflict,
            _ => EngineError::AlreadyDecided,
        }
        .into());
    };
    let step = s.step(&a.step_id).await?;
    if let Some(st) = &step {
        let mut details: Value =
            serde_json::from_str(&st.details_json).unwrap_or_else(|_| json!({}));
        details["approval"] =
            json!({ "id": d.id, "state": state, "note": note, "decidedBy": OWNER });
        let u = if a.kind == "budget" {
            // A budget escalation: approved, the step runs again past the
            // gate once; rejected, it stops.
            if approve {
                details["budgetApproved"] = json!(true);
                StepUpdate {
                    status: Some("pending".into()),
                    error: Some(String::new()),
                    details_json: Some(details.to_string()),
                    wake_at: Some(None),
                    ..Default::default()
                }
            } else {
                StepUpdate {
                    status: Some("failed".into()),
                    error: Some("budget_rejected".into()),
                    details_json: Some(details.to_string()),
                    wake_at: Some(None),
                    finished: true,
                    ..Default::default()
                }
            }
        } else if approve {
            StepUpdate {
                status: Some("succeeded".into()),
                route: Some(match st.node_type.as_str() {
                    "gate" => "pass".into(),
                    // A decision a person confirmed takes the suggested outcome.
                    "decision" => details
                        .get("suggested")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    _ => String::new(),
                }),
                error: Some(String::new()),
                details_json: Some(details.to_string()),
                wake_at: Some(None),
                finished: true,
                ..Default::default()
            }
        } else {
            StepUpdate {
                status: Some("failed".into()),
                error: Some("rejected".into()),
                details_json: Some(details.to_string()),
                wake_at: Some(None),
                finished: true,
                ..Default::default()
            }
        };
        s.update_step(&st.id, &["waiting", "running", "pending"], &u)
            .await?;
    }
    // Leave waiting_approval if nothing else waits.
    if s.approvals(Some("pending"), Some(&a.run_id), 1)
        .await?
        .is_empty()
    {
        s.set_run_status(&a.run_id, &["waiting_approval"], "running", "")
            .await?;
    }
    core.emit(
        kinds::APPROVAL_DECIDED,
        &d.id,
        &json!({ "runId": a.run_id, "state": state, "decidedBy": OWNER, "note": note, "wake": true }),
    )
    .await?;
    Ok(d)
}

pub use kls_core::needs::{Checkpoint, CheckpointKind};

/// The engine's part of "Needs you" (approvals, gates, steps waiting for a
/// node, paused runs), most urgent kind first, newest first within a kind.
/// `Core::needs_you` adds sessions, pull requests and cards.
pub async fn checkpoints(core: &Core) -> Result<Vec<Checkpoint>> {
    core.engine_checkpoints().await
}

/// A person settles a waiting step: a gate passes or fails, a wait ends, a
/// step that waits for a node is retried now (`pass`) or failed.
pub async fn unblock_step(core: &Core, step_id: &str, pass: bool, note: &str) -> Result<()> {
    let s = core.store();
    let st = s
        .step(step_id)
        .await?
        .ok_or(EngineError::NotFound("step"))?;
    if st.status != "waiting" {
        return Err(EngineError::BadState("the step is not waiting".into()).into());
    }
    if let Some(a) = s.step_approval(&st.id).await?
        && a.state == "pending"
    {
        decide_approval(core, &a.id, pass, note, None).await?;
        return Ok(());
    }
    let mut details: Value = serde_json::from_str(&st.details_json).unwrap_or_else(|_| json!({}));
    details["unblocked"] = json!({ "by": OWNER, "pass": pass, "note": note.trim() });
    let u = if st.error == "deferred:no_host" && pass {
        // Try again now.
        StepUpdate {
            status: Some("pending".into()),
            error: Some(String::new()),
            details_json: Some(details.to_string()),
            wake_at: Some(None),
            ..Default::default()
        }
    } else if pass {
        StepUpdate {
            status: Some("succeeded".into()),
            route: Some(if st.node_type == "gate" {
                "pass".into()
            } else {
                String::new()
            }),
            error: Some(String::new()),
            details_json: Some(details.to_string()),
            wake_at: Some(None),
            finished: true,
            ..Default::default()
        }
    } else {
        StepUpdate {
            status: Some("failed".into()),
            error: Some(if st.node_type == "gate" {
                "gate_failed".into()
            } else {
                "rejected".into()
            }),
            details_json: Some(details.to_string()),
            wake_at: Some(None),
            finished: true,
            ..Default::default()
        }
    };
    if !s.update_step(&st.id, &["waiting"], &u).await? {
        return Err(EngineError::BadState("the step changed; try again".into()).into());
    }
    core.emit(
        kinds::RUN_STEP,
        &st.run_id,
        &json!({ "stepId": st.id, "nodeId": st.node_id, "action": "unblock", "pass": pass, "wake": true }),
    )
    .await?;
    Ok(())
}

/// Result of a bulk action: one line per item that failed.
pub async fn bulk_decide(core: &Core, keys: &[String], approve: bool, note: &str) -> Vec<String> {
    let mut errors = Vec::new();
    for k in keys {
        let r = if let Some(id) = k.strip_prefix("approval:") {
            decide_approval(core, id, approve, note, None)
                .await
                .map(|_| ())
        } else if let Some(id) = k.strip_prefix("step:") {
            unblock_step(core, id, approve, note).await
        } else if let Some(id) = k.strip_prefix("run:") {
            if approve {
                resume_run(core, id).await.map(|_| ())
            } else {
                cancel_run(core, id, note).await.map(|_| ())
            }
        } else {
            Err(anyhow::anyhow!("unknown checkpoint"))
        };
        if let Err(e) = r {
            errors.push(format!("{k}: {e:#}"));
        }
    }
    errors
}

// ---- schedules as edits of the published graph ------------------------------

/// Applies `edit` to the live graph and publishes it as a new version
/// (a draft in progress is refused so nobody's edits are lost).
async fn republish(
    core: &Core,
    workflow_id: &str,
    note: &str,
    edit: impl FnOnce(&mut Graph) -> Result<()>,
) -> Result<Published> {
    let w = must_workflow(core, workflow_id).await?;
    if w.draft_version != 0 {
        return Err(EngineError::BadState(
            "the workflow has an unsaved draft; publish or discard it first".into(),
        )
        .into());
    }
    if w.published_version == 0 {
        return Err(EngineError::BadState("the workflow is not published".into()).into());
    }
    let v = core
        .store()
        .workflow_version(&w.id, w.published_version)
        .await?
        .context("published version missing")?;
    let mut g = graph_of(&v);
    edit(&mut g)?;
    let saved = save_draft(core, &w.id, 0, &g).await?;
    let p = publish(core, &w.id, saved.version, note).await?;
    crate::guard::audit(
        core,
        "workflow",
        "schedule.edit",
        json!({ "workflowId": w.id, "version": p.version }),
        &crate::guard::Origin::person(json!({ "note": note })),
        "published",
        json!({}),
    )
    .await?;
    Ok(p)
}

/// Re-times a cron trigger (publishes a new version).
pub async fn retime_schedule(
    core: &Core,
    schedule_id: &str,
    cron_expr: &str,
    tz: &str,
) -> Result<Published> {
    let sch = core
        .store()
        .schedule(schedule_id)
        .await?
        .ok_or(EngineError::NotFound("schedule"))?;
    if sch.kind != "cron" {
        return Err(EngineError::BadState("only a schedule can be re-timed".into()).into());
    }
    let tz = if tz.trim().is_empty() {
        "UTC"
    } else {
        tz.trim()
    };
    if crate::cron::next_after(cron_expr.trim(), tz, now_ms()).is_none() {
        bail!("{cron_expr:?} in {tz} never fires");
    }
    let (cron_expr, tz) = (cron_expr.trim().to_string(), tz.to_string());
    let node_id = sch.node_id.clone();
    republish(
        core,
        &sch.workflow_id,
        &format!("re-timed to {cron_expr} {tz}"),
        move |g| {
            let n = g
                .node_mut(&node_id)
                .ok_or(EngineError::NotFound("trigger"))?;
            n.config.insert("cron".into(), json!(cron_expr));
            n.config.insert("tz".into(), json!(tz));
            Ok(())
        },
    )
    .await
}

/// Adds a cron trigger leading where the workflow's first trigger leads.
pub async fn add_cron_schedule(
    core: &Core,
    workflow_id: &str,
    cron_expr: &str,
    tz: &str,
) -> Result<Published> {
    let tz = if tz.trim().is_empty() {
        "UTC"
    } else {
        tz.trim()
    };
    if crate::cron::next_after(cron_expr.trim(), tz, now_ms()).is_none() {
        bail!("{cron_expr:?} in {tz} never fires");
    }
    let (cron_expr, tz) = (cron_expr.trim().to_string(), tz.to_string());
    republish(
        core,
        workflow_id,
        &format!("schedule {cron_expr} {tz} added"),
        move |g| {
            let targets: Vec<String> = g
                .triggers()
                .next()
                .map(|t| g.outgoing(&t.id).map(|e| e.to.clone()).collect())
                .unwrap_or_default();
            if targets.is_empty() {
                bail!("the workflow has no step for a schedule to start");
            }
            let id = g.next_id();
            let mut n = crate::graph::Node::new(&id, "trigger", "Schedule");
            n.config.insert("kind".into(), json!("cron"));
            n.config.insert("cron".into(), json!(cron_expr));
            n.config.insert("tz".into(), json!(tz));
            n.config.insert("overlap".into(), json!("skip_if_running"));
            n.config.insert("catchUp".into(), json!("once"));
            n.position = Some(crate::graph::Pos {
                x: 40.0,
                y: 40.0 + 120.0 * g.triggers().count() as f32,
            });
            g.nodes.push(n);
            for to in targets {
                g.edges.push(crate::graph::Edge {
                    from: id.clone(),
                    to,
                    ..Default::default()
                });
            }
            Ok(())
        },
    )
    .await
}

/// Removes a trigger (publishes a new version); the last one stays.
pub async fn delete_schedule(core: &Core, schedule_id: &str) -> Result<Published> {
    let sch = core
        .store()
        .schedule(schedule_id)
        .await?
        .ok_or(EngineError::NotFound("schedule"))?;
    let node_id = sch.node_id.clone();
    republish(
        core,
        &sch.workflow_id,
        &format!("trigger {} removed", sch.node_id),
        move |g| {
            if g.triggers().count() <= 1 {
                bail!("a workflow keeps at least one trigger");
            }
            g.remove_node(&node_id);
            Ok(())
        },
    )
    .await
}

/// Run totals for the trace header.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunTotals {
    pub tokens: i64,
    /// Share of `limits.runBudgetTokens` used, when the workflow sets one.
    pub budget_pct: Option<f64>,
    pub decisions: i64,
    pub handoffs: i64,
    pub cards: i64,
}

impl RunDetail {
    pub fn totals(&self) -> RunTotals {
        let parse =
            |s: &RunStep| serde_json::from_str::<Value>(&s.details_json).unwrap_or(Value::Null);
        RunTotals {
            tokens: self.run.tokens,
            budget_pct: self
                .graph
                .limits
                .run_budget_tokens
                .filter(|b| *b > 0)
                .map(|b| self.run.tokens as f64 * 100.0 / b as f64),
            decisions: self
                .steps
                .iter()
                .filter(|s| s.node_type == "decision" && s.status == "succeeded")
                .count() as i64,
            handoffs: self
                .steps
                .iter()
                .filter(|s| parse(s).get("handoff").is_some())
                .count() as i64,
            cards: self
                .steps
                .iter()
                .filter(|s| parse(s).get("routeCardId").is_some_and(|v| !v.is_null()))
                .count() as i64,
        }
    }
}

/// A new draft workflow whose trigger answers `ev` (Events: "Start a
/// workflow from this event"): a GitHub event becomes a `webhook` trigger
/// on its webhook name, a card move a `kanban` trigger, anything else an
/// `event` trigger on its kind. The draft has a notify step to replace.
pub async fn create_from_event(core: &Core, ev: &kls_store::Event) -> Result<Workflow> {
    let data: Value = serde_json::from_str(&ev.data_json).unwrap_or(Value::Null);
    let mut cfg = serde_json::Map::new();
    let title = if ev.kind.starts_with("github.") {
        let names = crate::forge::webhook_events(ev, &data);
        let name = names.first().cloned().unwrap_or_else(|| ev.kind.clone());
        cfg.insert("kind".into(), json!("webhook"));
        cfg.insert("event".into(), json!(name));
        name
    } else if ev.kind == "card.moved" {
        let to = data
            .pointer("/detail/to")
            .and_then(Value::as_str)
            .unwrap_or("");
        cfg.insert("kind".into(), json!("kanban"));
        cfg.insert("toList".into(), json!(to));
        format!("card moved to {to}")
    } else {
        cfg.insert("kind".into(), json!("event"));
        cfg.insert("event".into(), json!(ev.kind));
        ev.kind.clone()
    };
    cfg.insert("overlap".into(), json!("queue"));
    let mut t = crate::graph::Node::new("n1", "trigger", &title);
    t.config = cfg;
    t.position = Some(crate::graph::Pos { x: 40.0, y: 80.0 });
    let mut n = crate::graph::Node::new("n2", "notify", "Notify");
    n.config
        .insert("message".into(), json!("{workflow}: {status}"));
    n.config.insert("severity".into(), json!("info"));
    n.position = Some(crate::graph::Pos { x: 260.0, y: 80.0 });
    let mut g = Graph {
        nodes: vec![t, n],
        ..Default::default()
    };
    g.edges.push(crate::graph::Edge {
        from: "n1".into(),
        to: "n2".into(),
        ..Default::default()
    });
    if let Some(repo) = data
        .get("repo")
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
    {
        g.scope.repos = vec![repo.to_lowercase()];
    }
    create_workflow(
        core,
        NewWorkflow {
            name: format!("On {title}"),
            description: format!("Started from event {} ({})", ev.id, ev.kind),
            graph: Some(g),
            ..Default::default()
        },
    )
    .await
}

/// What a run touched (the run page's Touched panel and fact chips):
/// repositories, pull requests, cards, agents, models, nodes and sessions,
/// read from the run and its steps' details.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunFacts {
    pub repos: Vec<String>,
    pub pulls: Vec<String>,
    pub cards: Vec<String>,
    pub agents: Vec<String>,
    pub models: Vec<String>,
    pub nodes: Vec<String>,
    pub sessions: Vec<String>,
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !s.is_empty() && !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

impl RunDetail {
    pub fn facts(&self) -> RunFacts {
        let mut f = RunFacts::default();
        push_unique(&mut f.repos, &self.run.repo);
        push_unique(&mut f.cards, &self.run.card_id);
        for s in &self.steps {
            let d: Value = serde_json::from_str(&s.details_json).unwrap_or(Value::Null);
            let get = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            push_unique(&mut f.agents, &get("agent"));
            push_unique(&mut f.agents, &get("crew"));
            push_unique(&mut f.nodes, &get("nodeId"));
            push_unique(&mut f.cards, &get("routeCardId"));
            push_unique(&mut f.sessions, s.session_id.as_deref().unwrap_or(""));
            let model = get("model");
            if !model.is_empty() {
                push_unique(
                    &mut f.models,
                    format!("{}/{model}", get("provider")).trim_start_matches('/'),
                );
            }
            for k in ["url", "prUrl", "pr"] {
                let u = get(k);
                // GitHub `/pull/N`, GitLab `/-/merge_requests/N`, Gitea `/pulls/N`.
                if kls_github::forge::kind::pull_ref_of(&u).is_some() {
                    push_unique(&mut f.pulls, &u);
                }
            }
            if let Some(h) = d.get("handoff") {
                push_unique(
                    &mut f.pulls,
                    h.get("pr").and_then(Value::as_str).unwrap_or(""),
                );
            }
        }
        f
    }
}

/// The run's evidence pack: what started it (and from which version),
/// every step with its outcome and details, approvals, decisions, routing
/// cards with their notes, facts and the run's ledger entries. Written to
/// `<data>/exports/run-<short>.json` by [`write_evidence`].
pub async fn evidence(core: &Core, run_id: &str) -> Result<Value> {
    let d = run_detail(core, run_id)
        .await?
        .ok_or(EngineError::NotFound("run"))?;
    let s = core.store();
    let version = s
        .workflow_version(&d.run.workflow_id, d.run.workflow_version)
        .await?;
    let mut cards = Vec::new();
    for id in d.facts().cards {
        if let Some(c) = core.card(&id).await? {
            cards.push(json!({
                "id": c.id, "key": c.key, "title": c.title, "status": c.status, "labels": c.labels,
                "pr": c.pr_url, "history": kls_core::board::cards::history(&c),
            }));
        }
    }
    let q = kls_store::audit::LedgerQuery {
        text: Some(d.run.id.clone()),
        limit: 500,
        ..Default::default()
    };
    let ledger: Vec<Value> = core
        .audit_entries(&q)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|e| json!({ "seq": e.seq, "at": e.at, "kind": e.kind, "actor": e.actor, "msg": e.msg, "hash": e.hash }))
        .collect();
    let steps: Vec<Value> = d
        .steps
        .iter()
        .map(|st| {
            json!({
                "node": st.node_id, "type": st.node_type, "title": st.title, "attempt": st.attempt,
                "status": st.status, "route": st.route, "error": st.error, "tokens": st.tokens,
                "startedAt": st.started_at, "finishedAt": st.finished_at,
                "details": serde_json::from_str::<Value>(&st.details_json).unwrap_or(Value::Null),
            })
        })
        .collect();
    Ok(json!({
        "kind": "keleustes.evidence/v1",
        "run": d.run,
        "workflow": d.workflow.as_ref().map(|w| json!({ "id": w.id, "name": w.name, "origin": w.origin, "systemKey": w.system_key })),
        "version": version.as_ref().map(|v| json!({ "version": v.version, "graphSha256": v.graph_sha256, "note": v.note })),
        "event": serde_json::from_str::<Value>(&d.run.event_json).unwrap_or(Value::Null),
        "steps": steps,
        "approvals": d.approvals,
        "totals": d.totals(),
        "facts": d.facts(),
        "cards": cards,
        "ledger": ledger,
        "writtenAt": now_ms(),
    }))
}

/// Writes the evidence pack and returns its path; the write is audited.
pub async fn write_evidence(core: &Core, run_id: &str) -> Result<std::path::PathBuf> {
    let v = evidence(core, run_id).await?;
    let dir = core.data_dir().join("exports");
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!("{}.json", crate::exec::short_id(run_id)));
    let body = serde_json::to_string_pretty(&v)?;
    tokio::fs::write(&path, &body).await?;
    use sha2::{Digest, Sha256};
    let sha = hex::encode(Sha256::digest(body.as_bytes()));
    crate::guard::audit(
        core,
        "run",
        "evidence.export",
        json!({ "runId": run_id, "path": path.display().to_string() }),
        &crate::guard::Origin::person(json!({})),
        "written",
        json!({ "sha256": sha }),
    )
    .await?;
    Ok(path)
}

#[cfg(test)]
mod facts_tests {
    use super::*;

    #[test]
    fn run_facts_collect_pull_urls_of_every_forge() {
        let step = |d: Value| RunStep {
            details_json: d.to_string(),
            ..Default::default()
        };
        let d = RunDetail {
            run: Run::default(),
            workflow: None,
            graph: Graph::default(),
            steps: vec![
                step(json!({ "prUrl": "https://github.com/acme/api/pull/7" })),
                step(json!({ "url": "https://gitlab.example.com/grp/proj/-/merge_requests/42" })),
                step(json!({ "pr": "https://gitea.example.com/o/r/pulls/3" })),
                step(json!({ "url": "https://gitlab.example.com/grp/proj/-/issues/9" })),
            ],
            approvals: vec![],
        };
        let f = d.facts();
        assert_eq!(f.pulls.len(), 3, "{:?}", f.pulls);
        assert!(f.pulls.iter().any(|p| p.contains("merge_requests/42")));
    }
}

#[cfg(test)]
mod list_tests {
    use super::*;

    #[test]
    fn attachment_and_live_version() {
        let repos = ["acme/api", "acme/web", "other/x"];
        assert_eq!(attached_repo_count(&[], repos), 3);
        assert_eq!(attached_repo_count(&["acme/*".into()], repos), 2);
        assert_eq!(attached_repo_count(&["ACME/API".into()], repos), 1);
        assert_eq!(attached_repo_count(&["none/x".into()], repos), 0);
        assert!(attached_to(&[], "any/thing"));
        let w = |p, d| Workflow {
            published_version: p,
            draft_version: d,
            ..Default::default()
        };
        assert_eq!(live_version(&w(0, 0)), None);
        assert_eq!(live_version(&w(0, 2)), Some(2));
        assert_eq!(live_version(&w(3, 4)), Some(3));
    }

    #[test]
    fn list_stats_reads_graph_repos_and_runs() {
        let mut g: Graph = serde_json::from_value(json!({
            "nodes": [
                {"id":"a","type":"trigger","config":{"kind":"webhook","event":"pull_request"}},
                {"id":"b","type":"trigger","config":{"kind":"manual"}}
            ],
            "scope": {"repos": ["acme/*"]}
        }))
        .unwrap();
        let repos = vec!["acme/api".to_string(), "other/x".to_string()];
        let runs = HashMap::from([("w1".to_string(), 5)]);
        let s = list_stats("w1", &g, &repos, &runs);
        assert_eq!(s.triggers, "webhook pull_request +1");
        assert_eq!(s.repos, 1);
        assert_eq!(s.runs_7d, 5);
        g.scope.repos.clear();
        let s = list_stats("w2", &g, &repos, &runs);
        assert_eq!((s.repos, s.runs_7d), (2, 0));
    }
}
