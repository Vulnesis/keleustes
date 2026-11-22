//! Domain: the workflow engine (M3). The engine itself is the `kls-engine`
//! crate; persistence is `kls_store::engine`. This module names the events
//! it publishes so the UI can reload by prefix.

pub use kls_store::engine::{
    Approval, Run, RunFilter, RunStep, Schedule, Workflow, WorkflowVersion,
};

/// Event kinds of this area. Subjects: the workflow id for `workflow.*`,
/// the run id for `run.*`, the approval id for `approval.*`, the schedule
/// id for `schedule.*`, the template id for `template.*`.
pub mod kinds {
    /// Data: `{slug, name, templateId}`.
    pub const WORKFLOW_CREATED: &str = "workflow.created";
    /// A draft save. Data: `{version, revision}`.
    pub const WORKFLOW_SAVED: &str = "workflow.saved";
    /// Data: `{version, note, triggers}`.
    pub const WORKFLOW_PUBLISHED: &str = "workflow.published";
    /// Name, description, enabled flag or draft discarded. Data: `{what}`.
    pub const WORKFLOW_UPDATED: &str = "workflow.updated";
    pub const WORKFLOW_ARCHIVED: &str = "workflow.archived";

    /// A run was created. Data: `{workflowId, trigger, repo, cardId}`.
    pub const RUN_QUEUED: &str = "run.queued";
    /// A status change. Data: `{status, reason, action}`.
    pub const RUN_UPDATED: &str = "run.updated";
    /// A step was created or changed. Data: `{stepId, nodeId, status, attempt, error}`.
    pub const RUN_STEP: &str = "run.step";
    /// Terminal status. Data: `{status, reason}`.
    pub const RUN_FINISHED: &str = "run.finished";
    /// A notify step. Data: `{message, severity, nodeId}`.
    pub const RUN_NOTIFY: &str = "run.notify";

    /// Data: `{runId, stepId, nodeId, title}`.
    pub const APPROVAL_REQUESTED: &str = "approval.requested";
    /// Data: `{runId, state, decidedBy, note}`.
    pub const APPROVAL_DECIDED: &str = "approval.decided";
    /// The SLA passed and the approver was reminded.
    pub const APPROVAL_RENOTIFIED: &str = "approval.renotified";

    /// Enabled flag or timer changed. Data: `{enabled, nextRunAt}`.
    pub const SCHEDULE_UPDATED: &str = "schedule.updated";
    /// Data: `{runIds, scheduledFor, catchUp}`.
    pub const SCHEDULE_FIRED: &str = "schedule.fired";
    /// A fire or event that started nothing. Data: `{reason}`.
    pub const SCHEDULE_SKIPPED: &str = "schedule.skipped";

    /// Data: `{dismissed}`.
    pub const TEMPLATE_DISMISSED: &str = "template.dismissed";

    /// Work the kill switch or the A2A blackout held back. Data:
    /// `{what, reason, workflowId?, runId?, scheduleId?}`.
    pub const ENGINE_REFUSED: &str = "engine.refused";
    /// A SystemOne decision step settled. Data: `{runId, stepId, nodeId,
    /// question, outcome, confidence, threshold, action, provider, model}`.
    pub const DECISION_MADE: &str = "decision.made";
    /// A routing card was filed for a worker. Data: `{runId, stepId, cardId, agent}`.
    pub const ROUTE_FILED: &str = "route.filed";

    /// Prefixes the UI reloads on.
    pub const PREFIXES: [&str; 8] = [
        "workflow.",
        "run.",
        "approval.",
        "schedule.",
        "template.",
        "engine.",
        "decision.",
        "route.",
    ];
}
