-- Keleustes M0: settings, join tokens, nodes, events.
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE TABLE join_tokens (
    id            TEXT PRIMARY KEY,
    secret_sha256 TEXT NOT NULL,
    label         TEXT NOT NULL DEFAULT '',
    created_at    BIGINT NOT NULL,
    expires_at    BIGINT NOT NULL,
    used_at       BIGINT,
    used_by_node  TEXT
);

CREATE TABLE nodes (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    hostname          TEXT NOT NULL DEFAULT '',
    os                TEXT NOT NULL DEFAULT '',
    arch              TEXT NOT NULL DEFAULT '',
    belai_version     TEXT NOT NULL DEFAULT '',
    host_id           TEXT NOT NULL DEFAULT '',
    capabilities_json TEXT NOT NULL DEFAULT '{}',
    trusted_dirs_json TEXT NOT NULL DEFAULT '[]',
    cert_serial       TEXT NOT NULL,
    joined_via_token  TEXT NOT NULL,
    created_at        BIGINT NOT NULL,
    last_seen_at      BIGINT,
    revoked_at        BIGINT,
    revoked_reason    TEXT,
    -- Highest NodeFrame seq applied (replay cursor for node -> server).
    node_seq          BIGINT NOT NULL DEFAULT 0,
    -- Highest ServerFrame seq the node acked.
    server_acked      BIGINT NOT NULL DEFAULT 0
);

-- Server -> node frames kept until the node acks them.
CREATE TABLE node_outbox (
    node_id    TEXT NOT NULL,
    seq        BIGINT NOT NULL,
    frame      BYTEA NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (node_id, seq)
);

CREATE TABLE events (
    id        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at        BIGINT NOT NULL,
    kind      TEXT NOT NULL,
    subject   TEXT NOT NULL DEFAULT '',
    data_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX events_kind_at ON events (kind, at);
