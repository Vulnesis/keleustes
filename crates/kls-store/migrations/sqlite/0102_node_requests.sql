-- belai counterparts (WP-I): work one node asked Keleustes to run on
-- another, with its origin, policy decision and outcome.
CREATE TABLE node_requests (
    id             TEXT PRIMARY KEY,
    origin_node    TEXT NOT NULL,
    origin_session TEXT NOT NULL DEFAULT '',
    -- user | agent
    initiated_by   TEXT NOT NULL DEFAULT 'agent',
    target_node    TEXT NOT NULL DEFAULT '',
    -- session | worker | crew | prompt
    kind           TEXT NOT NULL DEFAULT '',
    cwd            TEXT NOT NULL DEFAULT '',
    spec_json      TEXT NOT NULL DEFAULT '',
    -- awaiting_approval | queued | refused
    state          TEXT NOT NULL DEFAULT '',
    reason         TEXT NOT NULL DEFAULT '',
    dispatch_id    TEXT NOT NULL DEFAULT '',
    created_at     BIGINT NOT NULL,
    decided_at     BIGINT,
    decided_by     TEXT NOT NULL DEFAULT ''
);
CREATE INDEX node_requests_origin ON node_requests (origin_node, created_at);
CREATE INDEX node_requests_state ON node_requests (state);
