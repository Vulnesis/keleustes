-- WP2 engine: one row per System One decision (its rationale, kept for
-- `systemone.retention` and counted against the daily cap).
CREATE TABLE engine_decisions (
    id          TEXT PRIMARY KEY,
    at          BIGINT NOT NULL,
    run_id      TEXT NOT NULL DEFAULT '',
    step_id     TEXT NOT NULL DEFAULT '',
    workflow_id TEXT NOT NULL DEFAULT '',
    node_id     TEXT NOT NULL DEFAULT '',
    question    TEXT NOT NULL DEFAULT '',
    choice      TEXT NOT NULL DEFAULT '',
    outcome     TEXT NOT NULL DEFAULT '',
    -- `route`, `human`, `fail`, `capped`.
    action      TEXT NOT NULL DEFAULT '',
    -- `chosen`, `below_threshold`, `abstained`, `daily_cap`.
    why         TEXT NOT NULL DEFAULT '',
    confidence  DOUBLE PRECISION NOT NULL DEFAULT 0,
    threshold   DOUBLE PRECISION NOT NULL DEFAULT 0,
    provider    TEXT NOT NULL DEFAULT '',
    model       TEXT NOT NULL DEFAULT '',
    -- The node that answered.
    node_ref    TEXT NOT NULL DEFAULT '',
    latency_ms  BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX engine_decisions_at ON engine_decisions (at);
CREATE INDEX engine_decisions_run ON engine_decisions (run_id);
