-- Keleustes governance: the session fact index. One row per (session, kind,
-- value): the repositories, pull requests, cards, agents, models, nodes,
-- runs, vulnerabilities and files a session touched, read from the session
-- row, its transcript and its host audit lines.
CREATE TABLE session_facts (
    session_id TEXT NOT NULL,
    kind       TEXT NOT NULL,
    value      TEXT NOT NULL,
    first_at   BIGINT NOT NULL,
    last_at    BIGINT NOT NULL,
    hits       BIGINT NOT NULL DEFAULT 1,
    PRIMARY KEY (session_id, kind, value)
);
CREATE INDEX session_facts_value ON session_facts (kind, value);

-- How far each session's transcript and audit lines were folded.
CREATE TABLE session_fact_cursor (
    session_id TEXT PRIMARY KEY,
    entry_idx  BIGINT NOT NULL DEFAULT -1,
    line_idx   BIGINT NOT NULL DEFAULT -1
);
