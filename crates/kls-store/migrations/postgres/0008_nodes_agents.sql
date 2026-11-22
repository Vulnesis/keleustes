-- Agent profiles and crews a node's host advertises (HostAdvert.agents,
-- HostAdvert.crews), offered as pickers in the New session dialog.
ALTER TABLE nodes ADD COLUMN agents_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE nodes ADD COLUMN crews_json TEXT NOT NULL DEFAULT '[]';
