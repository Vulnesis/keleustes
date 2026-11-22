-- Keleustes engine parity: trigger safeguards and the defaults catalogue.

-- One row per trigger evaluation that passed its conditions: what the
-- debounce window and the runs-per-hour cap count, and the provenance of
-- every fire (or the safeguard that held it back).
CREATE TABLE trigger_fires (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    schedule_id TEXT NOT NULL,
    -- What the fire is about: `owner/repo#7`, a card id, or ''.
    subject     TEXT NOT NULL DEFAULT '',
    at          BIGINT NOT NULL,
    event_id    BIGINT,
    run_id      TEXT,
    -- `started`, `duplicate` or `skipped:<reason>`.
    outcome     TEXT NOT NULL
);
CREATE INDEX trigger_fires_schedule ON trigger_fires (schedule_id, at);

-- Defaults catalogue: a seeded workflow keeps its catalogue key and
-- revision; an edit by a person marks it customised (upgrades skip it).
ALTER TABLE workflows ADD COLUMN origin TEXT NOT NULL DEFAULT 'user';
ALTER TABLE workflows ADD COLUMN system_key TEXT NOT NULL DEFAULT '';
ALTER TABLE workflows ADD COLUMN system_rev BIGINT NOT NULL DEFAULT 0;
ALTER TABLE workflows ADD COLUMN customised BIGINT NOT NULL DEFAULT 0;
