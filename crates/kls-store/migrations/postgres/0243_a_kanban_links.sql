-- Agent A: the external kanban binding (Settings "Kanban binding"). One row
-- per board card mirrored to an item on the bound outside board (GitHub
-- Projects, GitLab issue board, Linear, Jira). `base_json` is the last value
-- both sides agreed on per field ({title, body, column}): the merge base of
-- the three-way sync. `conflict_json` holds the fields both sides changed to
-- different values ('' when none); they are never overwritten until the owner
-- picks a side. `state`: linked, gone (the item or the card was removed; the
-- other side is left alone).
CREATE TABLE kanban_links (
    provider TEXT NOT NULL,
    board TEXT NOT NULL,
    card_id TEXT NOT NULL,
    remote_id TEXT NOT NULL,
    remote_url TEXT NOT NULL DEFAULT '',
    base_json TEXT NOT NULL DEFAULT '{}',
    conflict_json TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL DEFAULT 'linked',
    -- SHA-256 of the last item content taken in (provenance).
    source_sha256 TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (provider, board, card_id)
);
CREATE UNIQUE INDEX kanban_links_remote ON kanban_links (provider, board, remote_id);

-- The last sync pass per binding, for the Settings status.
CREATE TABLE kanban_runs (
    provider TEXT NOT NULL,
    board TEXT NOT NULL,
    at BIGINT NOT NULL,
    outcome TEXT NOT NULL DEFAULT '',
    report_json TEXT NOT NULL DEFAULT '{}',
    last_ok_at BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (provider, board)
);
