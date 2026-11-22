-- belai counterparts (WP-H): requests a node's worker filed because it could
-- not publish its branch itself, and the coordinator's answer.
CREATE TABLE forge_requests (
    node_id       TEXT NOT NULL,
    request_id    TEXT NOT NULL,
    item_id       TEXT NOT NULL DEFAULT '',
    worker_id     TEXT NOT NULL DEFAULT '',
    kind          TEXT NOT NULL DEFAULT '',
    repo_owner    TEXT NOT NULL DEFAULT '',
    repo_name     TEXT NOT NULL DEFAULT '',
    branch        TEXT NOT NULL DEFAULT '',
    base_sha      TEXT NOT NULL DEFAULT '',
    head_sha      TEXT NOT NULL DEFAULT '',
    bundle_sha256 TEXT NOT NULL DEFAULT '',
    bundle_bytes  BIGINT NOT NULL DEFAULT 0,
    failure       TEXT NOT NULL DEFAULT '',
    title         TEXT NOT NULL DEFAULT '',
    -- filed | steered
    state         TEXT NOT NULL DEFAULT 'filed',
    steer_event   TEXT NOT NULL DEFAULT '',
    steer_pr_url  TEXT NOT NULL DEFAULT '',
    dispatch_id   TEXT NOT NULL DEFAULT '',
    created_at    BIGINT NOT NULL,
    updated_at    BIGINT NOT NULL,
    PRIMARY KEY (node_id, request_id)
);
CREATE INDEX forge_requests_state ON forge_requests (state, created_at);
