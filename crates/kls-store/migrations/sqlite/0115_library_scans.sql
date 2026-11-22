-- belai counterparts (WP-K): what a node's library_scan found (names, paths
-- and hashes only), so the owner can pick items to import.
CREATE TABLE library_scans (
    dispatch_id TEXT NOT NULL,
    node_id     TEXT NOT NULL,
    idx         BIGINT NOT NULL,
    kind        TEXT NOT NULL DEFAULT '',
    name        TEXT NOT NULL DEFAULT '',
    path        TEXT NOT NULL DEFAULT '',
    sha256      TEXT NOT NULL DEFAULT '',
    harness     TEXT NOT NULL DEFAULT '',
    verdict     TEXT NOT NULL DEFAULT '',
    created_at  BIGINT NOT NULL,
    PRIMARY KEY (dispatch_id, idx)
);
CREATE INDEX library_scans_node ON library_scans (node_id, created_at);
