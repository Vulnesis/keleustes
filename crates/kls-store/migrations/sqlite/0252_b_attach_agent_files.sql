-- belai-counterparts #53 / sf-parity "Builder extras: files" (B, 2026-11-22):
-- the files an agent profile carries (text only, bounded). The bytes live in
-- the content-addressed blob store (<data>/artifacts) under sha256; a
-- publish copies them into the library version (library_files / blobs).
CREATE TABLE agent_files (
    agent_id    TEXT NOT NULL,
    path        TEXT NOT NULL,
    sha256      TEXT NOT NULL,
    size_bytes  INTEGER NOT NULL,
    added_by    TEXT NOT NULL DEFAULT '',
    added_at    INTEGER NOT NULL,
    PRIMARY KEY (agent_id, path)
);
