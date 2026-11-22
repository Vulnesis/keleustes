-- belai counterparts (WP-G): each node's scheduled workers, versioned like
-- cards. Definitions sync both ways; the run record is the host's alone.
CREATE TABLE host_schedules (
    node_id       TEXT NOT NULL,
    id            TEXT NOT NULL,
    profile       TEXT NOT NULL DEFAULT '',
    crew          TEXT NOT NULL DEFAULT '',
    cron          TEXT NOT NULL DEFAULT '',
    tz            TEXT NOT NULL DEFAULT '',
    cwd           TEXT NOT NULL DEFAULT '',
    enabled       BIGINT NOT NULL DEFAULT 0,
    last_fired    BIGINT NOT NULL DEFAULT 0,
    last_status   TEXT NOT NULL DEFAULT '',
    next_run      BIGINT NOT NULL DEFAULT 0,
    version       BIGINT NOT NULL DEFAULT 1,
    deleted       BIGINT NOT NULL DEFAULT 0,
    meta_json     TEXT NOT NULL DEFAULT '',
    created_at    BIGINT NOT NULL DEFAULT 0,
    updated_at    BIGINT NOT NULL DEFAULT 0,
    -- Server clock of the last change (the pull cursor).
    changed_at    BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (node_id, id)
);
CREATE INDEX host_schedules_changed ON host_schedules (node_id, changed_at);
