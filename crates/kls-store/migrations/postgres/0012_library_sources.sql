-- Library search (docs/gaps/library-search.md): provenance of imported
-- items, update tracking, hook bundle files and their blobs, and the
-- search sources a person added or toggled.

-- One row per imported version: where it came from, exactly.
CREATE TABLE library_provenance (
    -- Library kind, or 'agent'.
    item_kind        TEXT NOT NULL,
    item_id          TEXT NOT NULL,
    version          BIGINT NOT NULL,
    source           TEXT NOT NULL,
    ecosystem        TEXT NOT NULL,
    -- JSON Locator.
    locator          TEXT NOT NULL,
    upstream_commit  TEXT NOT NULL DEFAULT '',
    upstream_version TEXT NOT NULL DEFAULT '',
    -- Every URL fetched for this version, exact (with the SHA or version).
    source_urls_json TEXT NOT NULL,
    -- sha256 over the source files (sorted path NUL bytes).
    upstream_sha256  TEXT NOT NULL,
    -- Canonical doc or bundle hash (equal to belai's).
    doc_sha256       TEXT NOT NULL,
    -- [{path, sha256, url}] per source file.
    files_json       TEXT NOT NULL DEFAULT '[]',
    licence          TEXT NOT NULL DEFAULT '',
    notes_json       TEXT NOT NULL DEFAULT '[]',
    flags_json       TEXT NOT NULL DEFAULT '[]',
    ack_json         TEXT NOT NULL DEFAULT '{}',
    -- Imports are person-only.
    actor            TEXT NOT NULL,
    -- 'kls' or 'node:<id>'.
    converted_by     TEXT NOT NULL DEFAULT 'kls',
    -- The ledger entry that recorded the import.
    audit_seq        BIGINT NOT NULL,
    reviewed_at      BIGINT NOT NULL,
    PRIMARY KEY (item_kind, item_id, version)
);

-- The last update check per imported item.
CREATE TABLE library_updates (
    item_kind      TEXT NOT NULL,
    item_id        TEXT NOT NULL,
    latest_commit  TEXT NOT NULL DEFAULT '',
    latest_version TEXT NOT NULL DEFAULT '',
    latest_sha256  TEXT NOT NULL DEFAULT '',
    checked_at     BIGINT NOT NULL,
    -- current | behind | gone | error
    state          TEXT NOT NULL,
    PRIMARY KEY (item_kind, item_id)
);

-- Hook bundles: the manifest per version (mirrors BelaiLibraryItemFile).
CREATE TABLE library_files (
    item_id    TEXT NOT NULL,
    version    BIGINT NOT NULL,
    path       TEXT NOT NULL,
    sha256     TEXT NOT NULL,
    size_bytes BIGINT NOT NULL,
    PRIMARY KEY (item_id, version, path)
);

-- Content-addressed bundle file bytes.
CREATE TABLE library_blobs (
    sha256     TEXT PRIMARY KEY,
    size_bytes BIGINT NOT NULL,
    data       BYTEA NOT NULL
);

-- Search sources the person added (taps, well-known sites) or toggled.
-- Built-in sources have a row only once toggled.
CREATE TABLE library_sources (
    id         TEXT PRIMARY KEY,
    -- builtin | tap | well_known | setting
    kind       TEXT NOT NULL,
    -- Tap: {spec, globs, type, eco}; well-known: {url}.
    spec_json  TEXT NOT NULL DEFAULT '{}',
    enabled    BIGINT NOT NULL DEFAULT 1,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
