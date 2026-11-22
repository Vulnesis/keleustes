-- belai-counterparts #45, #52 (P3): a session's file checkpoints (newest
-- first, at most 50) and the SHA-256 of the plan file a plan-mode session
-- wrote.
ALTER TABLE session_extras ADD COLUMN checkpoints_json TEXT NOT NULL DEFAULT '';
ALTER TABLE session_extras ADD COLUMN plan_sha256 TEXT NOT NULL DEFAULT '';
