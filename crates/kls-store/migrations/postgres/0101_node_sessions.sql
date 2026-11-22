-- belai counterparts (WP-A): a session's structured open ask, what remote
-- input it admits, its git facts, and per provider/model usage.
CREATE TABLE session_extras (
    session_id       TEXT PRIMARY KEY,
    pending_ask_json TEXT NOT NULL DEFAULT '',
    accepts_json     TEXT NOT NULL DEFAULT '',
    git_json         TEXT NOT NULL DEFAULT '',
    usage_json       TEXT NOT NULL DEFAULT '',
    turns            BIGINT NOT NULL DEFAULT 0,
    tool_calls       BIGINT NOT NULL DEFAULT 0,
    updated_at       BIGINT NOT NULL DEFAULT 0
);

CREATE TABLE session_usage (
    session_id         TEXT NOT NULL,
    provider           TEXT NOT NULL,
    model              TEXT NOT NULL,
    input_tokens       BIGINT NOT NULL DEFAULT 0,
    output_tokens      BIGINT NOT NULL DEFAULT 0,
    cache_read_tokens  BIGINT NOT NULL DEFAULT 0,
    cache_write_tokens BIGINT NOT NULL DEFAULT 0,
    cost_usd           DOUBLE PRECISION NOT NULL DEFAULT 0,
    updated_at         BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (session_id, provider, model)
);
