-- Keleustes M6: the hash-chained audit ledger and teleport bundles.

-- One permanent chain: Keleustes' own significant events and the host
-- audit lines (Sessions.Audit) folded in. Each row's hash is SHA-256 over
-- the previous row's hash and the canonical JSON of the row. Append-only.
CREATE TABLE audit_ledger (
    seq          BIGINT PRIMARY KEY,
    at           BIGINT NOT NULL,
    -- setting | session_deleted | vault | approval | dispatch | a2a_blackout | ...
    kind         TEXT NOT NULL,
    actor        TEXT NOT NULL DEFAULT '',
    -- user | agent | host | system | rule
    actor_kind   TEXT NOT NULL DEFAULT '',
    msg          TEXT NOT NULL DEFAULT '',
    subject_json TEXT NOT NULL DEFAULT '{}',
    payload_json TEXT NOT NULL DEFAULT '{}',
    -- "factory" or "host:<nodeId>".
    origin       TEXT NOT NULL DEFAULT 'factory',
    node_id      TEXT NOT NULL DEFAULT '',
    session_id   TEXT NOT NULL DEFAULT '',
    -- What the entry was made from ("event:<id>", "line:<node>:<session>:<idx>"),
    -- so folding the same source twice is a no-op. NULL for direct entries.
    ref_key      TEXT,
    prev_hash    TEXT NOT NULL,
    hash         TEXT NOT NULL
);
CREATE UNIQUE INDEX audit_ledger_ref ON audit_ledger (ref_key);
CREATE INDEX audit_ledger_kind ON audit_ledger (kind, seq);
CREATE INDEX audit_ledger_session ON audit_ledger (session_id);
CREATE INDEX audit_ledger_at ON audit_ledger (at);

CREATE FUNCTION audit_ledger_append_only() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit ledger is append-only';
END
$$;

CREATE TRIGGER audit_ledger_no_change BEFORE UPDATE OR DELETE ON audit_ledger
FOR EACH ROW EXECUTE FUNCTION audit_ledger_append_only();

-- Session bundles moving between hosts. The bytes live in the
-- content-addressed blob store under <data>/artifacts/.
CREATE TABLE teleports (
    id            TEXT PRIMARY KEY,
    session_id    TEXT NOT NULL DEFAULT '',
    from_node     TEXT NOT NULL,
    to_node       TEXT NOT NULL,
    title         TEXT NOT NULL DEFAULT '',
    -- requested | ready | delivered | expired | failed
    status        TEXT NOT NULL DEFAULT 'requested',
    size          BIGINT NOT NULL DEFAULT 0,
    sha256        TEXT NOT NULL DEFAULT '',
    error         TEXT NOT NULL DEFAULT '',
    created_at    BIGINT NOT NULL,
    updated_at    BIGINT NOT NULL,
    uploaded_at   BIGINT,
    downloaded_at BIGINT,
    expires_at    BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX teleports_status ON teleports (status, expires_at);
CREATE INDEX teleports_to ON teleports (to_node, status);
