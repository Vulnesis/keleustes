-- Keleustes governance: vault entry expiry, per-node scoping and kinds.

-- Past expires_at a key is refused at grant time (value kept so it can be
-- extended). NULL never expires.
ALTER TABLE vault_secrets ADD COLUMN expires_at BIGINT;
-- Node ids that may receive this key, JSON list; [] means every node.
ALTER TABLE vault_secrets ADD COLUMN nodes_json TEXT NOT NULL DEFAULT '[]';
-- 'provider' (a model key) or 'env' (a generic environment variable, such
-- as an MCP token).
ALTER TABLE vault_secrets ADD COLUMN kind TEXT NOT NULL DEFAULT 'provider';
