-- Keleustes M5: vault, budgets, providers, routing, usage, agents, crews, library.

-- Provider keys sealed with XChaCha20-Poly1305 under the master key kept in
-- the OS keyring (kls-vault). Plaintext never reaches this table.
CREATE TABLE vault_secrets (
    id           TEXT PRIMARY KEY,
    provider     TEXT NOT NULL,
    env          TEXT NOT NULL,
    label        TEXT NOT NULL DEFAULT '',
    nonce        BLOB NOT NULL,
    ciphertext   BLOB NOT NULL,
    created_at   BIGINT NOT NULL,
    rotated_at   BIGINT,
    last_used_at BIGINT
);
CREATE UNIQUE INDEX vault_secrets_provider_env ON vault_secrets (provider, env);

-- Every grant handed to a node: which providers and secrets, never values.
CREATE TABLE vault_grants (
    id              TEXT PRIMARY KEY,
    providers_json  TEXT NOT NULL DEFAULT '[]',
    secret_ids_json TEXT NOT NULL DEFAULT '[]',
    missing_json    TEXT NOT NULL DEFAULT '[]',
    purpose         TEXT NOT NULL DEFAULT '',
    created_at      BIGINT NOT NULL,
    expires_at      BIGINT NOT NULL,
    revoked_at      BIGINT
);
CREATE INDEX vault_grants_created ON vault_grants (created_at);

-- Model providers Keleustes knows; `factory` = 1 lets factory work use it.
CREATE TABLE vault_providers (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    env           TEXT NOT NULL DEFAULT '',
    default_model TEXT NOT NULL DEFAULT '',
    fast_model    TEXT NOT NULL DEFAULT '',
    models_json   TEXT NOT NULL DEFAULT '[]',
    factory       BIGINT NOT NULL DEFAULT 1,
    created_at    BIGINT NOT NULL,
    updated_at    BIGINT NOT NULL
);
INSERT INTO vault_providers (id, name, env, default_model, fast_model, models_json, factory, created_at, updated_at) VALUES
    ('anthropic', 'Anthropic', 'ANTHROPIC_API_KEY', 'claude-sonnet-4-5', 'claude-haiku-4-5', '["claude-opus-4-1","claude-sonnet-4-5","claude-haiku-4-5"]', 1, 0, 0),
    ('openai', 'OpenAI', 'OPENAI_API_KEY', 'gpt-5', 'gpt-5-mini', '["gpt-5","gpt-5-mini"]', 1, 0, 0),
    ('google', 'Google Gemini', 'GEMINI_API_KEY', 'gemini-2.5-pro', 'gemini-2.5-flash', '["gemini-2.5-pro","gemini-2.5-flash"]', 1, 0, 0),
    ('openrouter', 'OpenRouter', 'OPENROUTER_API_KEY', '', '', '[]', 1, 0, 0),
    ('mistral', 'Mistral', 'MISTRAL_API_KEY', 'mistral-large-latest', 'mistral-small-latest', '["mistral-large-latest","mistral-small-latest"]', 1, 0, 0);

-- Budgets: what they limit (`applies_to` all/provider/model/agent/repo plus
-- `subject`), an optional provider filter, a period and limits.
CREATE TABLE budgets (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    applies_to      TEXT NOT NULL DEFAULT 'all',
    subject         TEXT NOT NULL DEFAULT '',
    provider        TEXT NOT NULL DEFAULT 'all',
    period          TEXT NOT NULL DEFAULT 'month',
    limit_tokens    BIGINT,
    limit_usd       DOUBLE PRECISION,
    alert_at_json   TEXT NOT NULL DEFAULT '[80,100]',
    action_at_limit TEXT NOT NULL DEFAULT 'alert_only',
    revision        BIGINT NOT NULL DEFAULT 1,
    created_at      BIGINT NOT NULL,
    updated_at      BIGINT NOT NULL
);

-- First enabled rule by priority whose non-empty matchers all match wins.
CREATE TABLE routing_rules (
    id          TEXT PRIMARY KEY,
    priority    BIGINT NOT NULL DEFAULT 100,
    match_repo  TEXT NOT NULL DEFAULT '',
    match_agent TEXT NOT NULL DEFAULT '',
    match_label TEXT NOT NULL DEFAULT '',
    match_kind  TEXT NOT NULL DEFAULT '',
    provider    TEXT NOT NULL,
    model       TEXT NOT NULL DEFAULT '',
    enabled     BIGINT NOT NULL DEFAULT 1,
    created_at  BIGINT NOT NULL,
    updated_at  BIGINT NOT NULL
);

-- Model spend. Session rows (id "session:<id>") carry the cumulative totals
-- the node reported; job rows are one per job.
CREATE TABLE usage_rows (
    id            TEXT PRIMARY KEY,
    at            BIGINT NOT NULL,
    source        TEXT NOT NULL DEFAULT 'session',
    session_id    TEXT NOT NULL DEFAULT '',
    node_id       TEXT NOT NULL DEFAULT '',
    provider      TEXT NOT NULL DEFAULT '',
    model         TEXT NOT NULL DEFAULT '',
    agent         TEXT NOT NULL DEFAULT '',
    repo          TEXT NOT NULL DEFAULT '',
    input_tokens  BIGINT NOT NULL DEFAULT 0,
    output_tokens BIGINT NOT NULL DEFAULT 0,
    cost_usd      DOUBLE PRECISION NOT NULL DEFAULT 0
);
CREATE INDEX usage_rows_at ON usage_rows (at);

CREATE TABLE agent_profiles (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    display_name TEXT NOT NULL DEFAULT '',
    description  TEXT NOT NULL DEFAULT '',
    prompt       TEXT NOT NULL DEFAULT '',
    provider     TEXT NOT NULL DEFAULT '',
    model        TEXT NOT NULL DEFAULT '',
    tools_json   TEXT NOT NULL DEFAULT '[]',
    avatar_svg   TEXT NOT NULL DEFAULT '',
    meta_json    TEXT NOT NULL DEFAULT '{}',
    version      BIGINT NOT NULL DEFAULT 1,
    created_at   BIGINT NOT NULL,
    updated_at   BIGINT NOT NULL
);
CREATE UNIQUE INDEX agent_profiles_name ON agent_profiles (name);

CREATE TABLE crews (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    description  TEXT NOT NULL DEFAULT '',
    process      TEXT NOT NULL DEFAULT 'sequential',
    members_json TEXT NOT NULL DEFAULT '[]',
    version      BIGINT NOT NULL DEFAULT 1,
    created_at   BIGINT NOT NULL,
    updated_at   BIGINT NOT NULL
);
CREATE UNIQUE INDEX crews_name ON crews (name);

-- Prompts, skills, commands and processes; each save is a new version.
CREATE TABLE library_items (
    id             TEXT PRIMARY KEY,
    kind           TEXT NOT NULL,
    name           TEXT NOT NULL,
    description    TEXT NOT NULL DEFAULT '',
    latest_version BIGINT NOT NULL DEFAULT 0,
    created_at     BIGINT NOT NULL,
    updated_at     BIGINT NOT NULL
);
CREATE UNIQUE INDEX library_items_kind_name ON library_items (kind, name);

CREATE TABLE library_versions (
    item_id    TEXT NOT NULL,
    version    BIGINT NOT NULL,
    body       TEXT NOT NULL,
    note       TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL,
    PRIMARY KEY (item_id, version)
);
