-- WP1 (governance): vault rotation grace and version lineage, discovered
-- models and auto-rotation.

-- The live version number of a key (1 at save, +1 per rotation).
ALTER TABLE vault_secrets ADD COLUMN version BIGINT NOT NULL DEFAULT 1;
-- Auto-rotate after this many days (0: never). Needs a management key.
ALTER TABLE vault_secrets ADD COLUMN rotate_every_days BIGINT NOT NULL DEFAULT 0;
-- Model ids the last accepted key test listed, JSON list (never the key).
ALTER TABLE vault_secrets ADD COLUMN models_json TEXT NOT NULL DEFAULT '[]';
-- The provider's own id of the live key (a minted key's handle), if known.
ALTER TABLE vault_secrets ADD COLUMN remote_id TEXT NOT NULL DEFAULT '';

-- One row per value a key has held. The previous value stays sealed (under
-- the master key, never plaintext) until grace_until so a bad rotation can be
-- rolled back; the sweep then shreds nonce and ciphertext and keeps the
-- fingerprint for the lineage.
CREATE TABLE vault_secret_versions (
    secret_id TEXT NOT NULL,
    version BIGINT NOT NULL,
    fingerprint TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL,
    retired_at BIGINT,
    grace_until BIGINT,
    nonce BLOB,
    ciphertext BLOB,
    remote_id TEXT NOT NULL DEFAULT '',
    origin TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (secret_id, version)
);
