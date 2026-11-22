-- belai counterparts (WP-J): a host asks the owner for something.
CREATE TABLE attention_requests (
    node_id    TEXT NOT NULL,
    id         TEXT NOT NULL,
    kind       TEXT NOT NULL,
    reason     TEXT NOT NULL,
    subject    TEXT NOT NULL DEFAULT '',
    -- open | approved | declined | lapsed
    state      TEXT NOT NULL DEFAULT 'open',
    created_at BIGINT NOT NULL,
    decided_at BIGINT,
    PRIMARY KEY (node_id, id)
);
CREATE INDEX attention_state ON attention_requests (state);
