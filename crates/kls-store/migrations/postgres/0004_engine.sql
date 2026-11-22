-- M3 engine: workflows, versions, schedules (trigger rows), runs, steps,
-- approvals, template dismissals. Timestamps are Unix ms; booleans 0/1.

CREATE TABLE workflows (
    id                TEXT PRIMARY KEY,
    slug              TEXT NOT NULL UNIQUE,
    name              TEXT NOT NULL,
    description       TEXT NOT NULL DEFAULT '',
    template_id       TEXT NOT NULL DEFAULT '',
    -- draft (never published), published, archived
    status            TEXT NOT NULL DEFAULT 'draft',
    enabled           BIGINT NOT NULL DEFAULT 0,
    -- 0 = none
    published_version BIGINT NOT NULL DEFAULT 0,
    draft_version     BIGINT NOT NULL DEFAULT 0,
    created_at        BIGINT NOT NULL,
    updated_at        BIGINT NOT NULL,
    archived_at       BIGINT
);
CREATE INDEX workflows_updated ON workflows (updated_at);

CREATE TABLE workflow_versions (
    workflow_id    TEXT NOT NULL REFERENCES workflows (id) ON DELETE CASCADE,
    version        BIGINT NOT NULL,
    -- draft, published (the live one), superseded (an older published one)
    status         TEXT NOT NULL DEFAULT 'draft',
    graph_json     TEXT NOT NULL DEFAULT '{}',
    graph_sha256   TEXT NOT NULL DEFAULT '',
    note           TEXT NOT NULL DEFAULT '',
    est_tokens_min BIGINT NOT NULL DEFAULT 0,
    est_tokens_max BIGINT NOT NULL DEFAULT 0,
    -- optimistic concurrency for draft saves
    revision       BIGINT NOT NULL DEFAULT 1,
    created_at     BIGINT NOT NULL,
    updated_at     BIGINT NOT NULL,
    published_at   BIGINT,
    PRIMARY KEY (workflow_id, version)
);

-- One row per trigger node of a workflow's published version (no row for
-- manual triggers). Cron rows have a timer; kanban and event rows fire from
-- the event bus.
CREATE TABLE schedules (
    id            TEXT PRIMARY KEY,
    workflow_id   TEXT NOT NULL REFERENCES workflows (id) ON DELETE CASCADE,
    node_id       TEXT NOT NULL,
    kind          TEXT NOT NULL,
    cron          TEXT NOT NULL DEFAULT '',
    tz            TEXT NOT NULL DEFAULT 'UTC',
    config_json   TEXT NOT NULL DEFAULT '{}',
    enabled       BIGINT NOT NULL DEFAULT 1,
    next_run_at   BIGINT,
    last_fired_at BIGINT,
    last_run_id   TEXT,
    created_at    BIGINT NOT NULL,
    updated_at    BIGINT NOT NULL,
    UNIQUE (workflow_id, node_id)
);
CREATE INDEX schedules_next ON schedules (enabled, next_run_at);

CREATE TABLE runs (
    id               TEXT PRIMARY KEY,
    workflow_id      TEXT NOT NULL REFERENCES workflows (id) ON DELETE CASCADE,
    workflow_version BIGINT NOT NULL,
    schedule_id      TEXT,
    trigger_kind     TEXT NOT NULL DEFAULT 'manual',
    trigger_node_id  TEXT NOT NULL DEFAULT '',
    trigger_label    TEXT NOT NULL DEFAULT '',
    -- idempotency: one run per (trigger, scheduled minute) or request key
    dedupe_key       TEXT UNIQUE,
    repo             TEXT NOT NULL DEFAULT '',
    card_id          TEXT NOT NULL DEFAULT '',
    event_json       TEXT NOT NULL DEFAULT '{}',
    inputs_json      TEXT NOT NULL DEFAULT '{}',
    -- queued running waiting_approval paused succeeded failed cancelled
    status           TEXT NOT NULL DEFAULT 'queued',
    reason           TEXT NOT NULL DEFAULT '',
    dry_run          BIGINT NOT NULL DEFAULT 0,
    tokens           BIGINT NOT NULL DEFAULT 0,
    created_at       BIGINT NOT NULL,
    updated_at       BIGINT NOT NULL,
    started_at       BIGINT,
    finished_at      BIGINT
);
CREATE INDEX runs_status ON runs (status);
CREATE INDEX runs_workflow ON runs (workflow_id, created_at);
CREATE INDEX runs_created ON runs (created_at);

CREATE TABLE run_steps (
    id           TEXT PRIMARY KEY,
    run_id       TEXT NOT NULL REFERENCES runs (id) ON DELETE CASCADE,
    -- creation order within the run (the walker's "created after")
    seq          BIGINT NOT NULL,
    node_id      TEXT NOT NULL,
    node_type    TEXT NOT NULL DEFAULT '',
    title        TEXT NOT NULL DEFAULT '',
    attempt      BIGINT NOT NULL DEFAULT 1,
    -- pending running waiting succeeded failed skipped cancelled
    status       TEXT NOT NULL DEFAULT 'pending',
    -- the outcome a condition or gate took
    route        TEXT NOT NULL DEFAULT '',
    -- short machine reason (gate_failed, rejected, deferred:no_host, ...)
    error        TEXT NOT NULL DEFAULT '',
    details_json TEXT NOT NULL DEFAULT '{}',
    dispatch_id  TEXT,
    session_id   TEXT,
    node_ref     TEXT,
    -- pending: start no earlier; waiting: wake (timeout, SLA, wait end) then
    wake_at      BIGINT,
    tokens       BIGINT NOT NULL DEFAULT 0,
    created_at   BIGINT NOT NULL,
    updated_at   BIGINT NOT NULL,
    started_at   BIGINT,
    finished_at  BIGINT,
    UNIQUE (run_id, node_id, attempt)
);
CREATE INDEX run_steps_run ON run_steps (run_id, seq);
CREATE INDEX run_steps_status ON run_steps (status, wake_at);
CREATE INDEX run_steps_dispatch ON run_steps (dispatch_id);

CREATE TABLE approvals (
    id             TEXT PRIMARY KEY,
    run_id         TEXT NOT NULL REFERENCES runs (id) ON DELETE CASCADE,
    step_id        TEXT NOT NULL,
    node_id        TEXT NOT NULL,
    -- human, gate, escalation
    kind           TEXT NOT NULL DEFAULT 'human',
    -- pending approved rejected expired cancelled
    state          TEXT NOT NULL DEFAULT 'pending',
    title          TEXT NOT NULL DEFAULT '',
    approvers_json TEXT NOT NULL DEFAULT '[]',
    context_json   TEXT NOT NULL DEFAULT '{}',
    note           TEXT NOT NULL DEFAULT '',
    decided_by     TEXT NOT NULL DEFAULT '',
    revision       BIGINT NOT NULL DEFAULT 1,
    sla_at         BIGINT,
    created_at     BIGINT NOT NULL,
    updated_at     BIGINT NOT NULL,
    decided_at     BIGINT
);
CREATE INDEX approvals_state ON approvals (state, created_at);
CREATE INDEX approvals_run ON approvals (run_id);

CREATE TABLE template_dismissals (
    template_id  TEXT PRIMARY KEY,
    dismissed_at BIGINT NOT NULL
);
