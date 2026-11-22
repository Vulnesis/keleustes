-- belai counterparts (WP-B): each node's latest live status (workers, crew
-- runs) and the spend last charged per worker.
CREATE TABLE node_status (
    node_id     TEXT PRIMARY KEY,
    at          BIGINT NOT NULL DEFAULT 0,
    status_json TEXT NOT NULL DEFAULT '',
    received_at BIGINT NOT NULL DEFAULT 0
);

CREATE TABLE worker_spend (
    node_id        TEXT NOT NULL,
    worker_id      TEXT NOT NULL,
    tokens         BIGINT NOT NULL DEFAULT 0,
    cost_micro_usd BIGINT NOT NULL DEFAULT 0,
    updated_at     BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (node_id, worker_id)
);
