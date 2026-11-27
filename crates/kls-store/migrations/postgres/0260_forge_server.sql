-- Keleustes M4: the server behind a connection (docs/gaps/forges.md 2.3, 2.4, 6.2).
-- Product and version from the server probe, as JSON (ServerInfo): the
-- Connect chip, Forgejo told apart from Gitea, and whether glab's device
-- flow is offered (GitLab 17.9 or later).
ALTER TABLE forge_connections ADD COLUMN server_json TEXT NOT NULL DEFAULT '{}';
