-- Harness sessions (harnesses.md): which third-party harness ran a session,
-- its version, its native session id (for resume) and the transcript source.
ALTER TABLE session_extras ADD COLUMN harness_json TEXT NOT NULL DEFAULT '';
