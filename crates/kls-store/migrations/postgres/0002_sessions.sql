-- Keleustes M1: sessions, transcript entries, session inbox, dispatches,
-- jobs, and raw host audit lines (M6 chains them).

CREATE TABLE sessions (
    id                  TEXT PRIMARY KEY,
    node_id             TEXT NOT NULL,
    title               TEXT NOT NULL DEFAULT '',
    cwd                 TEXT NOT NULL DEFAULT '',
    repo                TEXT NOT NULL DEFAULT '',
    branch              TEXT NOT NULL DEFAULT '',
    provider            TEXT NOT NULL DEFAULT '',
    model               TEXT NOT NULL DEFAULT '',
    -- tui | worker | rc | acp | headless
    kind                TEXT NOT NULL DEFAULT '',
    -- live | idle | waiting | ended
    state               TEXT NOT NULL DEFAULT '',
    mode                TEXT NOT NULL DEFAULT '',
    agent               TEXT NOT NULL DEFAULT '',
    dispatch_id         TEXT NOT NULL DEFAULT '',
    card_id             TEXT NOT NULL DEFAULT '',
    run_id              TEXT NOT NULL DEFAULT '',
    started_at          BIGINT NOT NULL DEFAULT 0,
    -- The host's clock.
    updated_at          BIGINT NOT NULL DEFAULT 0,
    -- Server clock of the last upsert or append (the live window).
    seen_at             BIGINT NOT NULL DEFAULT 0,
    input_tokens        BIGINT NOT NULL DEFAULT 0,
    output_tokens       BIGINT NOT NULL DEFAULT 0,
    cost_usd            DOUBLE PRECISION NOT NULL DEFAULT 0,
    pending_question_id TEXT NOT NULL DEFAULT '',
    pending_question    TEXT NOT NULL DEFAULT '',
    entry_count         BIGINT NOT NULL DEFAULT 0,
    -- Next transcript index the server expects (high water + 1).
    next_index          BIGINT NOT NULL DEFAULT 0,
    ended_at            BIGINT,
    end_reason          TEXT NOT NULL DEFAULT '',
    created_at          BIGINT NOT NULL
);
CREATE INDEX sessions_node ON sessions (node_id);
CREATE INDEX sessions_seen ON sessions (seen_at);
CREATE INDEX sessions_card ON sessions (card_id);
CREATE INDEX sessions_run ON sessions (run_id);

CREATE TABLE session_entries (
    session_id TEXT NOT NULL,
    idx        BIGINT NOT NULL,
    role       TEXT NOT NULL DEFAULT '',
    text       TEXT NOT NULL DEFAULT '',
    at         BIGINT NOT NULL DEFAULT 0,
    meta_json  TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (session_id, idx)
);

-- Prompts, answers and commands waiting for a session. An item stays
-- undelivered (streamed again on reconnect) until the session acks it.
CREATE TABLE session_inbox (
    session_id   TEXT NOT NULL,
    seq          BIGINT NOT NULL,
    id           TEXT NOT NULL UNIQUE,
    -- prompt | answer | command
    kind         TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}',
    created_at   BIGINT NOT NULL,
    delivered_at BIGINT,
    acked_at     BIGINT,
    -- pending | delivered | done | failed | refused
    status       TEXT NOT NULL DEFAULT 'pending',
    detail       TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (session_id, seq)
);

CREATE TABLE dispatches (
    id           TEXT PRIMARY KEY,
    node_id      TEXT NOT NULL,
    kind         TEXT NOT NULL DEFAULT '',
    spec_json    TEXT NOT NULL DEFAULT '{}',
    -- queued | delivered | started | failed
    status       TEXT NOT NULL DEFAULT 'queued',
    session_id   TEXT NOT NULL DEFAULT '',
    error        TEXT NOT NULL DEFAULT '',
    -- The node_outbox seq carrying it; delivered once the node acks it.
    outbox_seq   BIGINT NOT NULL DEFAULT 0,
    card_id      TEXT NOT NULL DEFAULT '',
    run_id       TEXT NOT NULL DEFAULT '',
    created_at   BIGINT NOT NULL,
    updated_at   BIGINT NOT NULL,
    delivered_at BIGINT,
    resolved_at  BIGINT
);
CREATE INDEX dispatches_node_status ON dispatches (node_id, status);
CREATE INDEX dispatches_status ON dispatches (status);

CREATE TABLE jobs (
    id           TEXT PRIMARY KEY,
    node_id      TEXT NOT NULL,
    kind         TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}',
    -- queued | ok | failed | refused | timeout
    status       TEXT NOT NULL DEFAULT 'queued',
    output_json  TEXT NOT NULL DEFAULT '',
    error        TEXT NOT NULL DEFAULT '',
    created_at   BIGINT NOT NULL,
    updated_at   BIGINT NOT NULL,
    deadline_at  BIGINT NOT NULL DEFAULT 0,
    resolved_at  BIGINT
);
CREATE INDEX jobs_node_status ON jobs (node_id, status);

-- Host audit lines as received (Sessions.Audit); M6 hash-chains them.
CREATE TABLE audit_lines (
    node_id     TEXT NOT NULL,
    session_id  TEXT NOT NULL,
    idx         BIGINT NOT NULL,
    at          BIGINT NOT NULL DEFAULT 0,
    kind        TEXT NOT NULL DEFAULT '',
    json        TEXT NOT NULL DEFAULT '',
    received_at BIGINT NOT NULL,
    PRIMARY KEY (node_id, session_id, idx)
);
