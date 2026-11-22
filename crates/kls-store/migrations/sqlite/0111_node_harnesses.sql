-- Harnesses a node's host advertises (HostAdvert.harnesses, harnesses.md).
ALTER TABLE nodes ADD COLUMN harnesses_json TEXT NOT NULL DEFAULT '[]';
