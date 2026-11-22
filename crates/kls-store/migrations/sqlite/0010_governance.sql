-- Keleustes governance: vault key tests and integrity checks, provider test
-- endpoints, kill switch mode, budget alert crossings.

-- Last key test (indicator only; never gates a save). test_state is ''
-- (untested), 'testing', 'accepted', 'refused', 'unreachable' or 'skipped'.
ALTER TABLE vault_secrets ADD COLUMN test_state TEXT NOT NULL DEFAULT '';
ALTER TABLE vault_secrets ADD COLUMN test_status BIGINT;
ALTER TABLE vault_secrets ADD COLUMN test_ms BIGINT;
ALTER TABLE vault_secrets ADD COLUMN test_detail TEXT NOT NULL DEFAULT '';
ALTER TABLE vault_secrets ADD COLUMN tested_at BIGINT;
-- Integrity: the sealed value opened with the master key and matched its
-- fingerprint (sha256 of the value, first 16 hex chars).
ALTER TABLE vault_secrets ADD COLUMN fingerprint TEXT NOT NULL DEFAULT '';
ALTER TABLE vault_secrets ADD COLUMN verified_at BIGINT;
ALTER TABLE vault_secrets ADD COLUMN verify_error TEXT NOT NULL DEFAULT '';

-- Free endpoint that checks a key (a models list) and how the key is sent:
-- 'bearer', 'x-api-key' (Anthropic) or 'x-goog-api-key' (Google).
ALTER TABLE vault_providers ADD COLUMN probe_url TEXT NOT NULL DEFAULT '';
ALTER TABLE vault_providers ADD COLUMN probe_auth TEXT NOT NULL DEFAULT 'bearer';
UPDATE vault_providers SET probe_url = 'https://api.anthropic.com/v1/models', probe_auth = 'x-api-key' WHERE id = 'anthropic';
UPDATE vault_providers SET probe_url = 'https://api.openai.com/v1/models', probe_auth = 'bearer' WHERE id = 'openai';
UPDATE vault_providers SET probe_url = 'https://generativelanguage.googleapis.com/v1beta/models', probe_auth = 'x-goog-api-key' WHERE id = 'google';
UPDATE vault_providers SET probe_url = 'https://openrouter.ai/api/v1/auth/key', probe_auth = 'bearer' WHERE id = 'openrouter';
UPDATE vault_providers SET probe_url = 'https://api.mistral.ai/v1/models', probe_auth = 'bearer' WHERE id = 'mistral';

-- Kill switch mode while stopped: 'pause' (running steps settle) or 'stop'
-- (open runs cancelled, claims released). '' when running.
ALTER TABLE factory_control ADD COLUMN mode TEXT NOT NULL DEFAULT '';
UPDATE factory_control SET mode = 'stop' WHERE stopped <> 0;

-- One row per budget, period and threshold crossed, so an alert fires once.
CREATE TABLE budget_alerts (
    budget_id    TEXT NOT NULL,
    period_start BIGINT NOT NULL,
    pct          BIGINT NOT NULL,
    at           BIGINT NOT NULL,
    PRIMARY KEY (budget_id, period_start, pct)
);
