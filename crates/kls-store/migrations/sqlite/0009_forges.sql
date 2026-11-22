-- Keleustes M4: every forge, not only github.com (docs/gaps/forges.md 3.5).
-- One row per signed-in place. Holds no secret: credentials stay in each
-- forge CLI's own store (gh, glab, tea).

CREATE TABLE forge_connections (
    id           TEXT PRIMARY KEY,          -- 'github.com' or uuid v7
    kind         TEXT NOT NULL,             -- github|ghes|gitlab|gitea|forgejo
    host         TEXT NOT NULL,
    label        TEXT NOT NULL DEFAULT '',
    login        TEXT NOT NULL DEFAULT '',
    cli_name     TEXT NOT NULL DEFAULT '',  -- tea login name
    git_protocol TEXT NOT NULL DEFAULT 'https',
    route_pref   TEXT NOT NULL DEFAULT 'auto',
    enabled      BIGINT NOT NULL DEFAULT 1,
    -- Last AuthState and CliProbe as JSON, for the UI before a recheck.
    status_json  TEXT NOT NULL DEFAULT '{}',
    -- Who added it: 'migration', 'user', 'cli' (provenance).
    origin       TEXT NOT NULL DEFAULT '',
    created_at   BIGINT NOT NULL,
    updated_at   BIGINT NOT NULL
);
CREATE UNIQUE INDEX forge_connections_host ON forge_connections (kind, host);
INSERT INTO forge_connections (id, kind, host, origin, created_at, updated_at)
    VALUES ('github.com', 'github', 'github.com', 'migration', 0, 0);

-- Keys stay `owner/name` (`acme`) on github.com; elsewhere they are
-- `host/owner/name` (`codeberg.org/acme`) and these columns say where.
ALTER TABLE gh_orgs ADD COLUMN connection TEXT NOT NULL DEFAULT 'github.com';
ALTER TABLE gh_orgs ADD COLUMN forge_login TEXT NOT NULL DEFAULT '';
ALTER TABLE gh_repos ADD COLUMN connection TEXT NOT NULL DEFAULT 'github.com';
ALTER TABLE gh_repos ADD COLUMN forge_full_name TEXT NOT NULL DEFAULT '';
ALTER TABLE gh_repos ADD COLUMN forge_id BIGINT;
CREATE INDEX gh_repos_connection ON gh_repos (connection);
-- 0 when the forge sends no rate headers and calls are counted instead.
ALTER TABLE gh_quota ADD COLUMN budget_known BIGINT NOT NULL DEFAULT 1;
-- signing gains 'unsigned_push' (built locally, pushed over git).
ALTER TABLE gh_resolutions ADD COLUMN connection TEXT NOT NULL DEFAULT 'github.com';
