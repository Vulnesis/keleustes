-- Keleustes M2 coordination: kanban, card activity, memory, A2A, factory control.

-- Board cards. The server is the source of truth: every write bumps
-- `version` (edits carry the version they were based on) and moves
-- `changed_at`, a strictly increasing server cursor nodes pull after.
CREATE TABLE kanban_items (
    id          TEXT PRIMARY KEY,
    key         TEXT NOT NULL UNIQUE,
    title       TEXT NOT NULL,
    body        TEXT NOT NULL DEFAULT '',
    -- triage, backlog, todo, doing, review, done
    status      TEXT NOT NULL,
    priority    BIGINT NOT NULL DEFAULT 0,
    labels_json TEXT NOT NULL DEFAULT '[]',
    assignee    TEXT NOT NULL DEFAULT '',
    repo        TEXT NOT NULL DEFAULT '',
    branch      TEXT NOT NULL DEFAULT '',
    pr_url      TEXT NOT NULL DEFAULT '',
    meta_json   TEXT NOT NULL DEFAULT '{}',
    created_at  BIGINT NOT NULL,
    updated_at  BIGINT NOT NULL,
    version     BIGINT NOT NULL DEFAULT 1,
    -- Soft delete (0/1); kept so nodes pull the tombstone.
    deleted     BIGINT NOT NULL DEFAULT 0,
    changed_at  BIGINT NOT NULL
);
CREATE INDEX kanban_items_changed ON kanban_items (changed_at);
CREATE INDEX kanban_items_status ON kanban_items (status, updated_at);

CREATE TABLE kanban_activity (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    card_id     TEXT NOT NULL,
    at          BIGINT NOT NULL,
    actor       TEXT NOT NULL DEFAULT '',
    -- created, edited, moved, assigned, labeled, deleted, dispatched...
    kind        TEXT NOT NULL,
    detail_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX kanban_activity_card ON kanban_activity (card_id, at);

-- Shared repository memory. `updated_at` is the writer's clock (newest
-- wins); `changed_at` is the server's pull cursor.
CREATE TABLE memory_notes (
    id          TEXT PRIMARY KEY,
    scope       TEXT NOT NULL,
    text        TEXT NOT NULL,
    tags_json   TEXT NOT NULL DEFAULT '[]',
    author      TEXT NOT NULL DEFAULT '',
    created_at  BIGINT NOT NULL,
    updated_at  BIGINT NOT NULL,
    deleted     BIGINT NOT NULL DEFAULT 0,
    changed_at  BIGINT NOT NULL
);
CREATE INDEX memory_notes_scope ON memory_notes (scope, updated_at);
CREATE INDEX memory_notes_changed ON memory_notes (changed_at);

-- Agent-to-agent envelopes relayed between nodes.
CREATE TABLE a2a_messages (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    id          TEXT NOT NULL UNIQUE,
    from_agent  TEXT NOT NULL,
    to_agent    TEXT NOT NULL DEFAULT '',
    thread_id   TEXT NOT NULL,
    text        TEXT NOT NULL DEFAULT '',
    meta_json   TEXT NOT NULL DEFAULT '{}',
    at          BIGINT NOT NULL
);
CREATE INDEX a2a_messages_thread ON a2a_messages (thread_id, seq);

-- Stop/resume and the A2A blackout: one row (id = 1).
CREATE TABLE factory_control (
    id              BIGINT PRIMARY KEY,
    stopped         BIGINT NOT NULL DEFAULT 0,
    stopped_since   BIGINT,
    stopped_reason  TEXT NOT NULL DEFAULT '',
    a2a_blackout    BIGINT NOT NULL DEFAULT 0,
    a2a_since       BIGINT,
    a2a_reason      TEXT NOT NULL DEFAULT ''
);
INSERT INTO factory_control (id) VALUES (1);
