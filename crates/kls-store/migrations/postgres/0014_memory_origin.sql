-- Memory notes: the host a note came from and the A2A message it was
-- written from (Memory screen: source host, "From A2A" link).
ALTER TABLE memory_notes ADD COLUMN host TEXT NOT NULL DEFAULT '';
ALTER TABLE memory_notes ADD COLUMN a2a_ref TEXT NOT NULL DEFAULT '';
