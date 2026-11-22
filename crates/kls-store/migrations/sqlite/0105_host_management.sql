-- Host management (docs/gaps/host-management.md §6.5): the last inventory
-- each belai host reported, host actions and their lines, provision plans.
CREATE TABLE host_inventory (
  node_id TEXT PRIMARY KEY,
  at BIGINT NOT NULL,
  json TEXT NOT NULL,
  sha256 TEXT NOT NULL DEFAULT ''
);

CREATE TABLE host_actions (
  id TEXT PRIMARY KEY,
  node_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  class TEXT NOT NULL,
  params_json TEXT NOT NULL,
  dry_run BIGINT NOT NULL DEFAULT 0,
  preview_sha256 TEXT NOT NULL DEFAULT '',
  preview_json TEXT NOT NULL DEFAULT '',
  if_match TEXT NOT NULL DEFAULT '',
  origin_json TEXT NOT NULL,
  plan_run_id TEXT NOT NULL DEFAULT '',
  step BIGINT NOT NULL DEFAULT 0,
  state TEXT NOT NULL,
  requested_at BIGINT NOT NULL,
  delivered_at BIGINT,
  finished_at BIGINT,
  error TEXT NOT NULL DEFAULT '',
  result_json TEXT NOT NULL DEFAULT '',
  handoff_json TEXT NOT NULL DEFAULT '',
  last_seq BIGINT NOT NULL DEFAULT 0,
  step_label TEXT NOT NULL DEFAULT '',
  pct BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX host_actions_node ON host_actions(node_id, requested_at);
CREATE INDEX host_actions_state ON host_actions(state);
CREATE INDEX host_actions_run ON host_actions(plan_run_id);

CREATE TABLE host_action_lines (
  action_id TEXT NOT NULL,
  seq BIGINT NOT NULL,
  at BIGINT NOT NULL,
  line TEXT NOT NULL,
  PRIMARY KEY (action_id, seq)
);
CREATE INDEX host_action_lines_at ON host_action_lines(at);

CREATE TABLE host_plans (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  steps_json TEXT NOT NULL,
  version BIGINT NOT NULL,
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL
);

CREATE TABLE host_plan_runs (
  id TEXT PRIMARY KEY,
  plan_id TEXT NOT NULL,
  plan_version BIGINT NOT NULL,
  node_id TEXT NOT NULL,
  origin_json TEXT NOT NULL,
  state TEXT NOT NULL,
  step BIGINT NOT NULL DEFAULT 0,
  started_at BIGINT NOT NULL,
  finished_at BIGINT,
  error TEXT NOT NULL DEFAULT ''
);
CREATE INDEX host_plan_runs_node ON host_plan_runs(node_id, started_at);
