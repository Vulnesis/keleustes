# belai features and their Keleustes counterparts

Audit date: 2026-11-21. Scope: every belai feature that talks to the Vulnetix
cloud (`$VULNETIX_WEB_URL`, `/api/site/v1/belai/...`), or that would gain from
a central coordinator. For each one: what Keleustes carries today
(`proto/keleustes/node/v1/node.proto`, `crates/kls-node`,
`belai/internal/factorylink`) and what is missing before Keleustes can
coordinate it and dispatch it to another `belai rc` (host A asks Keleustes,
Keleustes runs it on host B).

Ground rules this plan keeps:

- `node.proto` is a shared contract (docs/WORKSTREAMS.md). Every proto
  snippet below is a proposal for the coordinator. All of them are additive
  and must pass `buf breaking`.
- Everything Keleustes sends is remote, untrusted content on the node. A new
  payload gets the same classifier kind the website's equivalent has
  (`KindKanban`, `KindRepoMemory`, `KindPeer`, `KindTeleport`, remote prompt
  through `CleanPrompt`). No exemptions and no second path (belai AGENTS.md).
- Requests carry identifiers, not documents. A document is fetched through a
  call that the server answers only for the delivered request that names it.
  This copies the website's rule for library and teleport fetches.

Paths below are relative to `belai/` or `keleustes/`.

## Inventory

Coverage: **done** means it works end to end over the node protocol.
**partial** means the wire exists but fields, callers or surfaces are missing.
**missing** means there is no counterpart. **n/a** means it should not have one.

| # | belai feature | belai location | Cloud endpoint today (`/api/site/v1/belai` +) | Keleustes coverage | Missing pieces: proto / kls-node / kls-core / kls-ui / factorylink |
|---|---|---|---|---|---|
| 1 | Join, link, presence, revocation | `internal/factorylink/{join,link,node,token}.go`, `cmd/belai/connectcmd.go`, `internal/tui/connect.go` | none (website equivalent: `PUT /hosts/{id}`) | **done** | none |
| 2 | Factory control (stop, A2A blackout) | `factorylink/control.go`, `rc/factory.go` | `POST /hosts/{id}/rc/heartbeat` → `a2aBlackout` | **done** | none |
| 3 | Session mirror (TUI, `rc` sessions) | `sessionsync/syncer.go`, `factorylink/sessions.go`, `tui/session_sync.go`, `cmd/belai/rccmd.go` | `PUT /sessions/{id}`, `POST /sessions/{id}/entries`, `/heartbeat`, `/end` | **partial** | factorylink: `upsertFor` never sets `state` (always `live`), `input_tokens`, `output_tokens`, `cost_usd`, `card_id`, `run_id`, `pending_question*`. `EndWith` drops the `sessionmeasure` rollup. proto: no end-of-session usage rollup. kls-core: no fallback that reads usage from entries. |
| 4 | Remote prompts | `sessionsync/prompt.go`, `factorylink/sessions.go` (Inbox → `RemotePrompt`) | `GET /hosts/{id}/inbox`, `POST /prompts/{id}/ack` | **done** | none |
| 5 | Approvals: permission asks, clarify questionnaires, plan review | `internal/webask`, `tui/web_asks.go`, `internal/clarify` | ask and `ask_answer` transcript entries, `POST /answers/{id}/ack` | **partial** | factorylink: never publishes `pending_question_id`/`pending_question`, so the Sessions screen never shows a question. Answers are expected as webask JSON (`{"kind":"permission","payload":{"decision":...}}`), but kls-ui `answer_ui` sends free text. belai refuses it. proto: no structured ask (kind, options, questionnaire). kls-core/kls-ui: no per-option buttons and no clarify form. |
| 6 | Session controls (`stop`, `interrupt`, mode/model/effort, `checkout`, `shell`, `command <name>`) | `internal/sessionctl`, `fleet/controls.go`, `rc/controls.go` | `PUT /sessions/{id}` → `Controls`, `POST /commands/{id}/ack` | **partial** | `Command` frames work. factorylink: `PutSessionControls`/`HeartbeatControls` return empty `Controls`, so Keleustes cannot see or switch the ask gate, remote-answers gate or control state. proto: no "what this session accepts" (web-controls, web-shell, remote prompts/answers). kls-ui: sends `queue_command(name, [])` only, with no args, so model/mode/checkout cannot be picked. |
| 7 | `rc` dispatch: start a session | `rc/factory.go:factoryStart`, `rc/session.go`, `rc/workspace.go` | `GET /hosts/{id}/dispatch`, `POST /dispatches/{id}/ack` | **partial** | proto `Dispatch` lacks the website's `dirs`, `workspace`, `effort`, `gitSync`, `ask`, `trigger`. belai: `card_id`/`run_id` are dropped and not reported on the session. |
| 8 | `rc` dispatch: worker / crew start | `rc/workers.go`, `rc/crews.go`, `rc/adopt.go`, `rc/clone.go` | same queue, kinds `worker`, `crew` | **partial** | proto lacks `fill`, `stay`, `stayIdleMax`, `memberModels`, `budget` (`WorkerBudget`), `clone` (+ `can_clone` capability), `trigger`. belai drops the vault grant on worker/crew dispatches. |
| 9 | Stop one `rc` session | `rc/daemon.go` (`stop`) | dispatch kind `stop` | **partial** | Works only as an inbox `stop` command, and only with `--web-controls`. No host-level stop dispatch that is independent of the session's gates. |
| 10 | Worker pause/resume, crew run control | `rc/crewcontrol.go`, `fleet/supervisor.go` | dispatch kinds `pause`, `resume`, `crew_control` | **missing** | proto: dispatch kinds and a `CrewControl` message. belai `factoryStart`: accepts only `session/worker/crew`. kls-core/kls-ui: workforce controls. |
| 11 | rc heartbeat: running sessions, workers, crew runs, worker spend | `rc/daemon.go:heartbeat`, `sessionsync.RCWorker`, `RCCrewRun` | `POST /hosts/{id}/rc/heartbeat`, `/rc/offline` | **missing** | proto: no host status frame. kls-core `workers_summary` guesses workers from `session.*` events, and workers never send any (row 12). kls-store: node status table. kls-ui: workforce/fleet show no live workers. |
| 12 | Fleet worker mirror (transcripts, web prompts, controls, lessons, audit) | `fleet/worker.go` (`Sync *sessionsync.Client`), `cmd/belai/agentcmd.go:466` | sessions, inbox, memory, audit endpoints | **missing** | belai only: `agentcmd.go` passes `headless.SyncClient` (website). It needs `factorylink.ActiveBackend(home,"worker")`, `ActiveMemory`, and an audit sink. The proto already has kind `worker`. |
| 13 | Audit log upload | `sessionsync/audit.go`, `internal/audit` | `PUT/POST /hosts/{id}/audit/{stream}` | **partial** | kls-node `Sessions.Audit` and kls-core `record_audit` exist, but belai never calls them (docs/keleustes.md: "the audit log is not uploaded"). proto: `AuditBatch` is per session, while belai's audit is a host stream. |
| 14 | Kanban sync | `internal/kanban/sync.go`, `factorylink/kanban.go` | `GET /kanban/items`, `POST /kanban/items/batch` | **done** | none (classified `KindKanban`) |
| 15 | Repository memory | `internal/repomemory`, `sessionsync/memory.go`, `factorylink/memory.go` | `GET /memory`, `POST /hosts/{id}/memory` | **partial** | Done for the TUI. belai: the `rc` daemon in factory mode (`rc/daemon.go:Run` skips it), `rc` sessions and fleet workers replicate only to the website. Worker lessons never reach Keleustes. |
| 16 | A2A relay | `internal/a2a`, `internal/a2alink`, `factorylink/a2a.go` | `POST /hosts/{id}/a2a`, inbox `a2a` | **done** | none (classified `KindPeer`). Addressing needs a node directory (row 42). |
| 17 | Teleport | `internal/teleport`, `factorylink/teleport.go`, `rc/factoryteleport.go`, `cmd/belai/teleportcmd.go` | `POST/GET /hosts/{id}/teleports...`, dispatch `teleport_code`, `teleport_backup` | **partial** | Bundle upload/download works. proto/bundle: no agent profile or crew of the session (the website's `TeleportProfile`/`TeleportCrew`), so a target that lacks the profile resumes without it. There is also no `teleport_backup` equivalent. |
| 18 | Jobs: `github` (gh argv) | `rc/factoryjobs.go`, `cmd/belai/factoryjobs.go` | none (website runs gh itself) | **done** | none |
| 19 | Jobs: `model` (`draft`, `avatar`, `classify`, `systemone`), agent drafts | `rc/factoryjobs.go`, `internal/agentdraft`, `rc/avatar.go` | `/agent-drafts/{id}/result`, `/hosts/{id}/agent-creators/...` | **done** | none (kls-core `draft_agent` uses the job) |
| 20 | Agent profile library: backup, install, automatic sync | `rc/profiles.go`, `rc/autosync.go`, `internal/libinstall`, `internal/agentprofile` | `/library/profiles/...`, `/library/backups`, `/library/sync`, dispatch `profile_backup`/`profile_install` | **missing** | Keleustes stores agents (`kls-core/vault.rs:save_agent`) but cannot put one on a node. `HostAdvert.agents` holds names only, with no id/version/hash. proto: library service + dispatch kinds. kls-ui: "install on node", diverged state. |
| 21 | Crew library | `rc/crews.go`, `libinstall` | `/library/crews/...`, `/library/crew-backups`, dispatch `crew_backup`/`crew_install` | **missing** | as row 20, for `kls-core` crews |
| 22 | Library items: skill, prompt, command, process, repo, budget, provider, rewrite, mcp, hook | `internal/libitem`, `internal/libstore`, `rc/items.go`, `internal/commandlib`, `internal/skills` | `/library/items/...`, `/library/item-backups`, `/library/sync/items`, dispatch `item_backup`/`item_install` | **missing** | kls-core `LIBRARY_KINDS` is `prompt, skill, command, process` only. There is no wire to nodes. Hooks need the strictest path (see risks). |
| 23 | Library scan / import from other harnesses | `rc/scan.go`, `internal/libscan`, `internal/agentimport` | `/library/scans`, `/library/imports`, dispatch `library_scan`/`library_import` | **missing** | proto: dispatch kinds + upload RPC. kls-ui: library import view. |
| 24 | Host inventory: profile/crew/item hashes, knowledge catalogue | `rc/inventory.go`, `rc/knowledge.go`, `rc/libhash.go` | `PUT /hosts/{id}` (inventory) | **missing** | proto `HostAdvert`: names only. |
| 25 | Provider keys | `factorylink/grant.go`, `rc/provider_keys.go`, `internal/credentials` | `/library/provider-keys`, dispatch `provider_keys_install`/`_remove` | **partial** | Per-dispatch/per-job `VaultGrant` is done. Persistent install is missing, deliberately (keep the grant model). Gap: grants for worker/crew dispatches are dropped, so workers need host keys. |
| 26 | MCP server secrets | `rc/mcp_secrets.go`, `internal/mcp` | `/library/mcp-secrets`, dispatch `mcp_secrets_install`/`_remove` | **missing** | proto: no MCP secret in `VaultGrant`. kls-core vault: secret → (server, key) binding. |
| 27 | Pix Sandbox vault env and scrub report | `sessionsync/vaultenv.go`, `internal/vaultenv` | `GET /hosts/{id}/vault/env`, `POST /hosts/{id}/vault/scrub` | **n/a** | Keleustes runs no sandboxes. The grant pipe covers the desktop case. |
| 28 | Project preferences from the console | `rc/prefs.go` (`--web-project-settings`) | dispatch `project_prefs` | **missing** | proto: dispatch kind + prefs payload. kls-ui: per-node directory settings. |
| 29 | Scheduled agents | `internal/schedule`, `rc/scheduler.go`, `schedule/sync.go` | `GET/PUT /hosts/{id}/schedules` | **missing** | belai: in factory mode the daemon does not even run the local scheduler or crew adoption (`rc/daemon.go:Run` returns into `runFactory` first). proto: schedule mirror. kls-engine has its own cron triggers that can dispatch, but nothing ties them to host schedules. |
| 30 | Forge relay (worker push/PR/issue/commit requests) and coordinator `steer` | `rc/forgerelay.go`, `fleet/escalate.go`, `fleet/coord.go` | `POST /hosts/{id}/forge-requests`, dispatch `steer` | **missing** | belai: factory mode never runs `forgeRelay`, so spooled requests pile up. proto: forge request upload + `steer` dispatch. kls-github: a coordinator that turns a request into `github` jobs or a node push. |
| 31 | Git facts for the console | `internal/gitsync`, `tui/git_sync.go` | `SessionMeta.Git` on `PUT /sessions/{id}` | **partial** | Only `branch` and `repo` are mapped. Commit, dirty, ahead/behind and detached are dropped. |
| 32 | Usage reporting | `internal/budget` (ledger), `internal/sessionmeasure`, `sessionsync/rollup.go` | end rollup on `POST /sessions/{id}/end`, worker spend in rc heartbeat | **partial** | proto fields exist (`SessionUpsert.input_tokens/...`). kls-core `usage_from_session_event` reads them. factorylink never fills them, so Budgets shows zero. |
| 33 | Budget enforcement on nodes | `sessionsync/workerbudget.go`, `fleet/limits.go`, `internal/budget` | `budget` on worker/crew dispatch | **missing** | proto: `WorkerBudget` on `Dispatch`, plus a budget state push for live sessions (kls-core `check_budget` actions `downgrade_to_fast` etc. never reach a node). |
| 34 | Host attention requests (a headless host asks a person) | `sessionsync/hostrequest.go`, `rc/attention.go` | `POST /hosts/{id}/attention`, decisions in heartbeat | **missing** | proto: request + decision frames. kls-core `needs_you`: host requests. Kinds change: no Vulnetix login. Instead `gh_login`, `provider_credentials`, `approve`. |
| 35 | Notifications | `internal/notify` | none (local desktop) | **missing** | kls-app: desktop notifications for a node's pending ask, attention request, run notify (`run.notify` exists). Depends on rows 5 and 34. |
| 36 | ACP session mirror | `cmd/belai/acpsync.go`, `internal/acp` | sessions endpoints (upload only) | **missing** | belai only: use `factorylink.ActiveBackend(home,"acp")` when a profile is active. The proto already has kind `acp`. |
| 37 | Headless `belai -prompt` | `internal/headless` | none for transcript; kanban/A2A as rows 14/16 | **partial** | Board and A2A go to Keleustes. The transcript is not mirrored anywhere. Optional mirror with kind `headless` (already in proto). |
| 38 | Hooks | `internal/hooks` | via library item kind `hook` (row 22) | **missing** | Distribution only, through row 22 with a confirmation gate. No remote hook execution. |
| 39 | Plugins | `internal/plugins` | none | **n/a** | Installed by the user after a full listing, by design. Keleustes should at most show the installed list (inventory, row 24). |
| 40 | Custom slash commands | `internal/commandlib` | `command <name>` control, library kind `command` | **partial** | Running a host's command through `Command` works. Distributing it is row 22. |
| 41 | Cross-host dispatch (host A asks Keleustes to run work on host B) | `/agent start`, `belai agent start`, `rc` | website console only (no host-originated dispatch) | **missing** | proto: node-originated `Fleet.Request`/`Watch`. kls-node: service + policy. kls-core: approval queue, origin tracking. kls-ui: policy settings, approval cards. factorylink: client. belai: `--on <node>` on `/agent start`, `belai rc dispatch`. |
| 42 | Node directory (pick a target node, A2A peers, teleport target) | `teleportcmd.go` (`-teleport-to <node-id>` typed by hand), `a2alink` | website host list | **missing** | proto: `Fleet.Nodes`. belai: completion for `-teleport-to`, `/fleet nodes`. |
| 43 | Session trigger provenance | `sessionsync/trigger.go`, `rc/trigger.go` | `trigger` on dispatch | **missing** | proto: `Trigger` on `Dispatch` (workflow run, card, origin node). This is the provenance sentence of row 41. |
| 44 | Vulnetix login, decisions/systemone/model APIs | `internal/vulnetixcli`, `internal/decisions` | `/api/site/v1/cli/device*`, `/api/alpha/decisions`, `/api/v1/systemone` | **n/a** | These are provider or identity endpoints, not coordination. Keleustes already reaches systemone through the `model` job. |

**Counts:** 44 rows. 7 done, 14 partial, 20 missing, 3 n/a.

The pattern: transport for sessions, board, memory, A2A, teleport and jobs is
in place. What is missing is (a) the fields belai already has but does not
send, (b) every website dispatch kind beyond `session/worker/crew` (22 more kinds,
see `rc/daemon.go:handle`), (c) all host-originated coordination (there is no
RPC a node can use to ask for work elsewhere), and (d) several daemon loops
that `runFactory` skips (scheduler, forge relay, crew adoption, memory
replication).

## Work packages

Each package stands alone unless it says it depends on another. Proto
changes are additive. They use field numbers above those used today:
`SessionUpsert` ≥22, `SessionEnd` ≥4, `Answer` ≥3, `Dispatch` ≥16,
`Capabilities` ≥6, `HostAdvert` ≥10, `NodeFrame.body` ≥13,
`ServerFrame.body` ≥19, `VaultGrant` ≥4, `AuditBatch` ≥3,
`TeleportHeader` ≥7.

A shared rule for version skew: an old belai ignores new fields and refuses
unknown dispatch kinds ("update Belai on the host"). Keleustes must therefore
check `Capabilities.dispatch_kinds` (WP-D) before it offers a feature for a
node.

---

### WP-A Session fidelity: state, usage, structured asks, controls (rows 3, 5, 6, 31, 32)

The largest gain for the smallest change. Most fields already exist on the
wire.

**Proto**

```proto
message SessionUpsert {
  // ... existing 1..21
  // Structured open ask. pending_question_id/pending_question stay filled
  // (id and title) for servers that predate it.
  PendingAsk pending_ask = 22;
  // What remote input this session admits. The console offers only these.
  SessionAccepts accepts = 23;
  GitFacts git = 24;
  // Per provider/model usage so far (cumulative, like input_tokens).
  repeated ModelUsage usage = 25;
}

message PendingAsk {
  string id = 1;
  // "permission", "clarify", "plan", "checkpoint".
  string kind = 2;
  // Harness-composed one-line summary (tool name and target), capped.
  string title = 3;
  string tool = 4;
  // Decisions the host will accept for this ask, e.g. "allow_once", "deny".
  repeated AskOption options = 5;
  // clarify: the questionnaire in webask's JSON shape.
  string questions_json = 6;
  int64 asked_at_ms = 7;
}
message AskOption { string id = 1; string label = 2; }

message SessionAccepts {
  bool prompts = 1;   // sync.remote_prompts
  bool answers = 2;   // sync.remote_answers
  bool controls = 3;  // --web-controls
  bool shell = 4;     // --web-shell
  repeated string controls_allowed = 5; // sessionctl names
  repeated string commands = 6;         // custom command names (no templates)
}

message GitFacts {
  string commit = 1;
  bool dirty = 2;
  int32 ahead = 3;
  int32 behind = 4;
  bool detached = 5;
}

message ModelUsage {
  string provider = 1;
  string model = 2;
  int64 input_tokens = 3;
  int64 output_tokens = 4;
  int64 cache_read_tokens = 5;
  int64 cache_write_tokens = 6;
  double cost_usd = 7;
}

message SessionEnd {
  // ... existing 1..3
  repeated ModelUsage usage = 4;
  uint32 turns = 5;
  uint32 tool_calls = 6;
}

message Answer {
  string question_id = 1;
  string text = 2;
  // Structured answer: one of the PendingAsk option ids, or for "clarify"
  // the webask answer JSON in payload_json. When set, text is a note.
  string decision = 3;
  string payload_json = 4;
}
```

**belai (factorylink, sessionsync)**

- `upsertFor`: fill `state` (live/idle/waiting from the syncer's turn
  state), `card_id`/`run_id` (from the dispatch, see WP-D), the usage
  fields, `git` from `SessionMeta.Git`, and `accepts` from the syncer's
  `RemotePrompts`/`RemoteAnswers`/`RemoteCommands` options.
- Publish the open ask: when the syncer uploads an `ask` entry, upsert
  `pending_ask` from the webask record. Clear it on the `ask_answer` entry.
  The title is composed by the harness from the tool name and the classified
  target, never from model text.
- `EndWith`: send the `sessionmeasure` rollup as `usage`.
- Map `Answer.decision`/`payload_json` onto the existing webask answer shape,
  so `webask` validation against the open ask is unchanged.
- `PutSessionControls` returns the controls Keleustes holds (see kls-core).

**Keleustes**

- kls-store: migration (sqlite + postgres) for `pending_ask_json`,
  `accepts_json`, `git_json`, `usage_json` on sessions, and a `session_usage`
  table keyed (session, provider, model).
- kls-core: `queue_answer(session, ask_id, decision, payload)` refuses a
  decision that is not in the stored `options`. `vault::sync_usage` reads per
  model usage.
- kls-ui sessions: one button per `AskOption`, a clarify form built from
  `questions_json` (fields shown as untrusted text), a command menu limited to
  `accepts.controls_allowed`/`commands` with argument pickers (model list from
  the node's providers).

**Tests**

- factorylink: `sessions_test.go`. Upsert carries usage/state/ask, an answer
  with `decision` becomes the webask JSON, a decision outside the open ask is
  refused by the existing validator.
- kls-node `tests.rs`: round trip upsert → `pending_ask` stored → UI answer →
  inbox item with decision.
- kls-store test on SQLite for the new columns. `buf breaking`.

**Risk.** A Keleustes permission answer approves a tool call on another
machine. Keep it behind `sync.remote_answers` (off by default). Never offer
`allow_always` or rule-writing decisions remotely unless the host lists them
in `options`. Show the ask's `title` and `tool` in Keleustes as untrusted text:
it may quote repository paths.

---

### WP-B Host status: live sessions, workers, crew runs, spend (rows 11, 32)

**Proto**

```proto
message NodeFrame {
  oneof body {
    // ... hello 10, heartbeat 11, host 12
    HostStatus status = 13;
  }
}

// Sent on change and at most every heartbeat. Live-only (seq 0).
message HostStatus {
  int64 at_ms = 1;
  uint32 running_sessions = 2;
  repeated WorkerStatus workers = 3;
  repeated CrewRunStatus crew_runs = 4;
}

message WorkerStatus {
  string id = 1;
  string profile = 2;
  string crew = 3;
  string run = 4;
  // "starting", "working", "idle", "paused", "stopped", "failed".
  string state = 5;
  string card_id = 6;
  string cwd = 7;
  string session_id = 8;
  int64 started_at_ms = 9;
  int64 beat_at_ms = 10;
  string reason = 11;
  uint32 done = 12;
  uint32 failed = 13;
  string branch = 14;
  // Monotonic while the worker lives. The server charges the difference.
  int64 tokens = 15;
  int64 cost_micro_usd = 16;
  int64 cost_live_micro_usd = 17;
}

message CrewRunStatus {
  string id = 1;
  string crew = 2;
  string state = 3;
  string reason = 4;
  repeated string scope_dirs = 5;
  // member profile -> replicas wanted / running, as JSON (RCMember shape).
  string members_json = 6;
  uint32 max_workers = 7;
  bool stay = 8;
}
```

**belai.** `rc/factory.go`: send `HostStatus` from the same fleet registry
snapshot `RCHeartbeatState` builds (`sessionsync.RCWorker`/`RCCrewRun`). Add
`Node.SendStatus` to `factorylink/link.go`. Only the link owner sends it.

**Keleustes.**

- kls-node `server.rs:apply`: store the status.
- kls-store: `node_status` (latest JSON per node) and `worker_spend` (worker
  id → last seen cost).
- kls-core: `workers_summary` reads `node_status` instead of guessing from
  session events. Spend deltas feed `record_usage`. Emit `node.status`.
- kls-ui workforce and fleet: live workers table, crew run state.

**Tests.** factorylink fake link receives status. kls-node test that a status
frame lands in the store and a spend delta is charged once across repeats
and reconnects.

---

### WP-C Every surface mirrors to Keleustes, and factory mode runs the daemon's own loops (rows 12, 13, 15, 29 local part, 30 local part, 36, 37)

Mostly belai. The proto already has kinds `worker`, `acp`, `headless`.

**Proto (audit only)**

```proto
message AuditBatch {
  string session_id = 1;  // empty for a host stream
  repeated AuditLine lines = 2;
  // Host audit stream id (belai's audit stream); set when session_id is empty.
  string stream_id = 3;
}
```

**belai**

- `cmd/belai/agentcmd.go`: when `factorylink.ActiveBackend(home,"worker")`
  succeeds, give `fleet.Worker` that backend instead of
  `headless.SyncClient`. This needs `fleet.Worker.Sync` to accept a
  `sessionsync.Backend`, which the TUI already does. Memory through
  `factorylink.ActiveMemory`.
- `cmd/belai/acpsync.go`: same with kind `acp`. Opt-in mirror for
  `belai -prompt` with kind `headless`.
- Audit: an `AuditSink` over `Sessions.Audit` (stream id = belai's), used by
  the TUI, workers and the daemon in place of `sessionsync.StartAudit` when a
  profile is active.
- `rc/daemon.go:runFactory`: also start the memory replicator (Keleustes
  backend), `scheduler` (local schedules keep firing), `adoptCrews`, and
  `forgeRelay` once WP-H gives it a destination.

**Keleustes.** kls-core `record_audit` accepts host streams into the
hash-chained ledger. The trust/audit screens filter by node.

**Tests.** A fleet worker run against the factorylink fake server mirrors
entries with kind `worker`. In factory mode the daemon runs the scheduler (a
due local schedule fires). kls-node audit test for a host stream.

**Risk.** Workers can take remote prompts (`sync.remote_prompts`). Keleustes
prompts reach them through the same `CleanPrompt` + classifier path. Do not
add a worker-only shortcut.

---

### WP-D Dispatch fidelity and lifecycle control (rows 7, 8, 9, 10, 43; capability negotiation)

**Proto**

```proto
message Dispatch {
  // ... existing 1..15
  // Extra workspace directories (each must be one the host offers).
  repeated string dirs = 16;
  string workspace = 17;
  string effort = 18;
  // Unset leaves the host default.
  optional bool git_sync = 19;
  optional bool ask = 20;
  Trigger trigger = 21;
  WorkerBudget budget = 22;
  bool fill = 23;
  bool stay = 24;
  uint32 stay_idle_max_s = 25;
  // member profile -> "provider/model".
  map<string, string> member_models = 26;
  CloneSpec clone = 27;
  // kind "pause"/"resume": the worker. kind "stop": the session.
  string worker_id = 28;
  string session_id = 29;
  CrewControl control = 30;
}
// New kinds: "stop", "pause", "resume", "crew_control".

message Trigger {
  // "console", "workflow", "schedule", "card", "node".
  string source = 1;
  string run_id = 2;
  string workflow_id = 3;
  string workflow_name = 4;
  string card_id = 5;
  string subject = 6;
  // For source "node": who asked (WP-I).
  string origin_node = 7;
  string origin_session = 8;
}

message WorkerBudget {
  double max_cost_usd = 1;
  double finalize_at = 2;
  uint32 max_continuations = 3;
  uint32 max_attempts = 4;
  string max_wall = 5;
  uint32 max_passes = 6;
  uint32 max_tokens = 7;
  uint32 max_iterations = 8;
  double price_input_per_mtok = 9;
  double price_output_per_mtok = 10;
}

message CloneSpec { string owner = 1; string name = 2; string branch = 3; }

message CrewControl {
  string run = 1;
  // "worker_pause", "worker_resume", "worker_weight", "scope_add",
  // "scope_remove", "replicas", "stop".
  string op = 2;
  string worker = 3;
  string dir = 4;
  string profile = 5;
  int32 weight = 6;
  bool paused = 7;
  uint32 replicas = 8;
}

message Capabilities {
  // ... existing 1..5
  // Dispatch kinds this belai understands. Old hosts send none, which
  // means {"session","worker","crew"}.
  repeated string dispatch_kinds = 6;
  repeated string job_kinds = 7;
  bool can_clone = 8;
  // provider -> models offered, so the console's pickers are real.
  repeated ProviderModels models = 9;
}
message ProviderModels { string provider = 1; repeated string models = 2; }
```

**belai.** `rc/factory.go:factoryStart` maps every new field onto the
existing `sessionsync.Dispatch` and runs the existing checks
(`start`, `startWorkers`, `PauseWorker`, `CrewControl`, `stop`).
`Trigger.Clean` produces the provenance sentence, which reaches the model
only inside the classified provenance attachment, as today. `factoryAdvert`
fills `dispatch_kinds`, `job_kinds`, `can_clone` and models. Pass `card_id`
and `run_id` on to the session (WP-A).

**Keleustes.** kls-core `DispatchSpec` gains the fields. The `NodeGateway`
gains `control(node, kind, ...)`. kls-engine `dispatch` steps set `trigger`
(run/workflow) and `budget` (from the step + budgets). kls-ui: workforce
pause/resume/stop, crew run controls. Hide each option the node does not list
in `dispatch_kinds`.

**Tests.** factorylink/rc: `factory_test.go` cases for each kind and each
refused shape. kls-node gateway test that a `crew_control` dispatch is queued
and replayed after a reconnect. kls-core test that an unsupported kind for a
node is refused before queueing.

---

### WP-E Library distribution: profiles, crews, items, teleport profiles (rows 17 profile part, 20, 21, 22, 24, 38, 40)

Dispatches carry identifiers. The node fetches documents with an RPC that
Keleustes answers only for the delivered dispatch (or teleport) naming them.
That is the website's rule.

**Proto**

```proto
service Library {
  // A document for a delivered dispatch (or teleport) that names it.
  rpc Fetch(LibraryFetch) returns (LibraryDoc);
  // A host's copy, for a backup dispatch or the automatic sync.
  rpc Backup(LibraryDoc) returns (LibraryReceipt);
  // Automatic sync: the host's hashes in, one verdict per item out.
  rpc Reconcile(LibraryInventory) returns (LibraryVerdicts);
}

message LibraryRef {
  // "profile", "crew", or a libitem kind ("skill", "prompt", "command",
  // "process", "repo", "budget", "provider", "rewrite", "mcp", "hook").
  string kind = 1;
  string id = 2;
  string name = 3;
  int64 version = 4;
  string sha256 = 5;
}

message LibraryFetch {
  // The dispatch or teleport this fetch serves.
  string dispatch_id = 1;
  string teleport_id = 2;
  LibraryRef ref = 3;
}

message LibraryDoc {
  string dispatch_id = 1;
  LibraryRef ref = 2;
  // Canonical document bytes (markdown / JSON) as libitem defines them.
  bytes doc = 3;
  // Hook bundles: script files, each hashed into ref.sha256 (HashBundle).
  repeated LibraryFile files = 4;
}
message LibraryFile { string path = 1; bytes data = 2; string sha256 = 3; }
message LibraryReceipt { LibraryRef stored = 1; }

message LibraryInventory { repeated LibraryRef items = 1; }
message LibraryVerdicts { repeated LibraryVerdict verdicts = 1; }
message LibraryVerdict {
  LibraryRef ref = 1;
  // "push", "current", "diverged", "install".
  string verdict = 2;
}

message Dispatch {
  // New kinds: "library_install", "library_backup".
  repeated LibraryRef library = 31;
  bool overwrite = 32;
}

message HostAdvert {
  // ... existing 1..9
  repeated LibraryRef inventory = 10;
}

message TeleportHeader {
  // ... existing 1..6
  // Profile and crews the session ran under. The target fetches any it
  // lacks with Library.Fetch(teleport_id).
  repeated LibraryRef needs = 7;
}
```

**belai.** factorylink `library.go` client. `rc/factory.go` maps
`library_install`/`library_backup` onto the existing `installProfile`,
`installCrew`, item install and backup code (`internal/libinstall`,
`internal/libstore`), and `autosync.go` gains a Keleustes backend over
`Reconcile`. `internal/teleport` fetches `needs` through `Library.Fetch`.
`factoryAdvert` sends `inventory` (`rc/inventory.go` hashes).

**Keleustes.** kls-core: `LIBRARY_KINDS` extended to every belai kind.
Agents and crews get stable ids and versions in the shape belai needs
(markdown profile export). kls-node `library.rs` (new owned file):
authorize `Fetch` against the dispatch row (same node, delivered, not
expired). kls-ui library/workforce: install on a node, diverged badge, and
the host inventory per node.

**Tests.** kls-node: a fetch for a dispatch addressed to another node, or not
delivered, is `PermissionDenied`. factorylink fake: install round trip.
belai: an invalid profile from Keleustes is refused by `agentprofile`
validation, and a hook bundle with a mismatched hash is refused.

**Risks.**

- Hook items run code. Keep belai's rule: a hook reaches a host only through
  an explicit install request, never through `Reconcile`, closed by
  `sync.hooks`, checked with `hooks.ParseDefinition` and
  `hooks.allowed_programs`. Keleustes should also require a per-install
  confirmation in its UI. `Reconcile` must never return `install` for kind
  `hook` or `mcp`.
- Profile, skill and command text is model-facing. It passes
  `libstore.UntrustedText` and the profile validator on the node. Skill text
  stays `KindSkill`, and command expansion is admitted like typed text, as
  today.
- Plugins stay out of scope (row 39).

---

### WP-F Secrets for workers and MCP servers (rows 25, 26)

**Proto**

```proto
message VaultGrant {
  // ... existing 1..3
  repeated McpSecret mcp = 4;
}
message McpSecret {
  // An MCP server installed on the host, and one of the keys its entry binds.
  string server = 1;
  string key = 2;
  string value = 3;
}
```

**belai.** `factorylink/grant.go`: MCP secrets are held like provider
secrets (memory only, concealed by `vaultenv`, dropped at expiry), and feed
only the named server's bound key. Worker/crew dispatches stop dropping the
grant: pass it to `belai agent start` on the same inherited-pipe path as
`rc-session` (no Windows, as today).

**Keleustes.** kls-core vault: a binding from a secret to (provider) or
(MCP server, key). `grant_for_purpose` includes MCP bindings for the
dispatch's agent.

**Tests.** The grant formats without values. An MCP secret for a key the
server's entry does not bind is ignored. A worker dispatch with a grant
reaches the worker process's resolver and not its tools' environment.

**Risk.** A worker is long-lived (`stay`, up to 24 h). Cap the grant's TTL at
the dispatch's `stay_idle_max_s` and revoke on `stop`.

---

### WP-G Central schedules (row 29)

Two halves. Keleustes becomes the scheduler for factory work, and host-local
schedules become visible and editable.

**Proto**

```proto
service Board {
  // ... existing
  rpc SchedulePull(SchedulePullRequest) returns (SchedulePage);
  rpc SchedulePush(ScheduleBatch) returns (SchedulePushResult);
}
message ScheduleItem {
  string id = 1;
  string profile = 2;   // worker profile
  string crew = 3;
  string cron = 4;      // five fields
  string tz = 5;
  string cwd = 6;       // a directory the host offers
  bool enabled = 7;
  int64 last_fired_ms = 8;
  string last_status = 9;
  int64 version = 10;
  bool deleted = 11;
  string meta_json = 12;
}
message SchedulePullRequest { int64 since_ms = 1; }
message SchedulePage { repeated ScheduleItem items = 1; int64 next_since_ms = 2; }
message ScheduleBatch { repeated ScheduleItem items = 1; }
message SchedulePushResult {
  repeated ScheduleItem accepted = 1;
  repeated ScheduleItem conflicts = 2;
}
```

**belai.** `schedule/sync.go` gains a Keleustes `Remote` (same shape as
`KanbanRemote`), and the daemon runs it in factory mode (WP-C).

**Keleustes.** kls-store `host_schedules` per node, versioned like cards.
kls-engine: a schedule trigger whose target is "node X" or "any node with
profile P and directory D" (gateway `pick_job_node` extended with
`agents`/`trusted_dirs`), firing a `worker` dispatch with
`trigger.source = "schedule"`. kls-ui: schedules screen per node plus factory
schedules.

**Tests.** kls-engine cron fires a dispatch to the least busy eligible node.
Conflict on a stale schedule version. belai schedule sync against the fake
server.

---

### WP-H Forge relay and coordinator steer (row 30)

**Proto**

```proto
service Forge {
  // A worker's spooled request: push, pull_request, issue_sync, commit.
  // The first part carries the request; later parts carry the git bundle.
  rpc File(stream ForgePart) returns (ForgeFiled);
}
message ForgePart {
  ForgeRequest request = 1;
  bytes chunk = 2;
  bool last = 3;
}
message ForgeRequest {
  string request_id = 1;   // 16 hex
  string item_id = 2;
  string worker_id = 3;
  string kind = 4;
  string repo_owner = 5;
  string repo_name = 6;
  string branch = 7;       // item attempt branch only
  string base_sha = 8;
  string head_sha = 9;
  string bundle_sha256 = 10;
  int64 bundle_bytes = 11;
  string failure = 12;     // closed list (auth, rate_limited, ...)
}
message ForgeFiled { string request_id = 1; bool duplicate = 2; }

message Dispatch {
  // kind "steer": the coordinator's answer to a request a worker filed.
  string steer_request = 33;
  string steer_event = 34;
  string steer_pr_url = 35;
  int64 steer_until_ms = 36;
  string steer_reason = 37;
}
```

**belai.** `rc/forgerelay.go` files to Keleustes when a profile is active,
after `fleet.ValidForgeRequest`. `factoryStart` maps `steer` onto
`fleet.CoordSpec` (already validated twice).

**Keleustes.** kls-github: a coordinator that, for a request from a node
without push credentials, asks a gh-authenticated node to open the PR or sync
the issue (`github` job), and answers with `steer`. kls-ui delivery screen
shows the requests.

**Risk.** A bundle is another host's git data. Keleustes must not unpack it or
run git on it. If a second node has to push it, that node must verify the
bundle hash and push only to the `belai/<item>` attempt branch shape
(`forgeBranch`), with hooks disabled. This needs its own review before
shipping. The simpler first step is to route only PR/issue actions (no push)
through `github` jobs.

---

### WP-I Fleet directory and cross-host dispatch: host A → Keleustes → host B (rows 41, 42; enables teleport target picking and A2A addressing)

This is what the user asked for: one belai asks Keleustes to run something on
another belai rc.

**Proto**

```proto
// Node-originated coordination. Every call is checked against the
// factory's node policy; nothing is dispatched on a node's word alone
// unless the owner allowed it.
service Fleet {
  rpc Nodes(NodesRequest) returns (NodeList);
  // Ask for work on another node (or "any" eligible node).
  rpc Request(DispatchRequest) returns (DispatchTicket);
  // Status of requests this node made, then live changes.
  rpc Watch(WatchRequest) returns (stream DispatchStatus);
  // Prompt a session on another node (still needs the target's
  // sync.remote_prompts and goes through its inbox).
  rpc Prompt(PeerPrompt) returns (DispatchTicket);
}

message NodesRequest {}
message NodeList { repeated NodeSummary nodes = 1; }
message NodeSummary {
  string node_id = 1;
  string name = 2;
  bool online = 3;
  bool remote_control = 4;
  repeated string trusted_dirs = 5;   // only if policy shares them
  repeated string agents = 6;
  repeated string crews = 7;
  repeated string providers = 8;
  uint32 running_sessions = 9;
  uint32 max_sessions = 10;
  // Live sessions as A2A agent ids "<nodeId>:<sessionId>".
  repeated string agent_ids = 11;
}

message DispatchRequest {
  // Empty: Keleustes picks an eligible node.
  string target_node = 1;
  // Same shape the console sends. Kinds limited by policy.
  Dispatch spec = 2;
  // The session on this node that asked, for provenance.
  string origin_session = 3;
  // "user" (typed by a person: slash command or CLI) or "agent"
  // (a model's tool call).
  string initiated_by = 4;
}
message DispatchTicket {
  string request_id = 1;
  // "queued", "awaiting_approval", "refused".
  string state = 2;
  string reason = 3;
  string dispatch_id = 4;
  string target_node = 5;
}
message WatchRequest { repeated string request_ids = 1; }
message DispatchStatus {
  string request_id = 1;
  string dispatch_id = 2;
  // "awaiting_approval", "queued", "delivered", "started", "refused",
  // "failed", "finished".
  string state = 3;
  string session_id = 4;
  string reason = 5;
  string target_node = 6;
}
message PeerPrompt {
  string target_session = 1;   // "<nodeId>:<sessionId>"
  string text = 2;
  string origin_session = 3;
  string initiated_by = 4;
}
```

**belai**

- factorylink `fleet.go` client.
- TUI: `/fleet nodes`, and `/agent start <profile> --on <node|any>` and
  `/crew start ... --on`, which file a `Request` and show `Watch` updates as
  system lines. CLI: `belai agent start -on <node>`,
  `belai rc dispatch -on <node> -cwd ... -prompt ...`.
- `-teleport-to` completes node names from `Nodes`. A2A peers can be listed
  from `agent_ids`.
- No model tool in the first version. If one is added later, it goes through
  the permission ask (mutating, no `allow_always`), sends
  `initiated_by = "agent"`, and its prompt text is the model's, so the target
  treats it as remote.

**Keleustes**

- kls-node `fleet.rs` (new owned file). `Nodes` filters by policy. `Request`
  validates the spec as the console does, records origin
  (`trigger.source = "node"`, `origin_node`, `origin_session`), then either
  queues through `NodeGateway::dispatch` or parks it as an approval.
- kls-core: a node policy (off by default). Per (origin node, target node,
  kind, directory) it decides auto-approve, ask, or deny. Approvals join
  `needs_you`. Factory stop and budgets apply to node requests like any
  dispatch. A rate limit per origin.
- kls-ui: settings for node policy, approval cards ("build-box asks laptop to
  start worker `patcher` in ~/src/api"), and origin shown on sessions.

**Tests.** kls-node: policy off refuses. Policy "ask" parks the request
and the approval queues it. A request naming a directory the target does not
offer is refused before queueing. `Watch` streams through to `started`.
Factory stop refuses. factorylink fake: `--on` flow. A `Prompt` to a session
whose `accepts.prompts` is false is refused (WP-A).

**Risks.**

- Prompt injection fan-out: a model on host A that is steered by repository
  text could spread work across the fleet. Mitigations: policy off by
  default, `initiated_by` recorded and "agent" requests always need approval,
  per-origin rate limits, factory stop.
- The target sees the request as remote. Prompt text goes through
  `CleanPrompt` and the classifier. The provenance sentence ("asked by node
  build-box, session …") is cleaned trigger data in the classified
  attachment, never a system block.
- The directory gate stays on the target: Keleustes can only name directories
  the target advertises, and the target re-checks trust.
- `Nodes` reveals other hosts' directories and agent ids. Share
  `trusted_dirs` only when policy allows.

---

### WP-J Attention requests and notifications (rows 34, 35)

**Proto**

```proto
message NodeFrame {
  oneof body {
    AttentionRequest attention = 14;
  }
}
message AttentionRequest {
  string id = 1;
  // Closed list: "gh_login", "provider_credentials", "approve", "action".
  string kind = 2;
  // Closed list: "credential_expiring", "reauth_needed",
  // "approval_needed", "action_needed".
  string reason = 3;
  // An identifier only (provider slug, profile name, card id).
  string subject = 4;
}

message ServerFrame {
  oneof body {
    AttentionDecided attention_decided = 19;
  }
}
message AttentionDecided {
  string id = 1;
  // "approved", "declined", "lapsed".
  string state = 2;
}
```

Note: `NodeFrame` replay today is "not replayable (all node frames)". An
attention request should use `seq > 0` so the server acks it, which is the
first replayable node frame. Alternatively, send it as a unary
`Board.Attention` RPC.

**belai.** `rc/attention.go` files to Keleustes when the factory is active,
using the existing kinds and reasons (no free text). A `login` request has no
meaning without Vulnetix, so the daemon maps a missing gh login to
`gh_login`.

**Keleustes.** kls-core `needs_you` includes host requests. kls-app tray and
desktop notifications fire for a new `pending_ask` (WP-A), an attention
request, and `run.notify`, using fixed templates with identifiers only
(belai's `notify` rule). Notification text never carries model or tool
output.

**Tests.** kls-node: an attention request with an unknown kind is refused.
A decision is replayed after a reconnect. belai: a decided request settles
once.

---

### WP-K Host and project settings, scan and import (rows 23, 28)

Lower priority.

**Proto.** New `Dispatch` kinds `project_prefs`, `library_scan`,
`library_import`:

```proto
message Dispatch {
  // project_prefs: flat keys checked by config.ProjectPrefs on the host.
  map<string, string> prefs_set = 38;
  repeated string prefs_unset = 39;
  // library_scan / library_import.
  string item_kind = 40;
  string scan_dispatch = 41;
  string path = 42;
  string sha256 = 43;
  string target = 44;
}
service Library {
  // ... WP-E
  rpc ScanReport(LibraryScanReport) returns (Ok);
}
message LibraryScanReport {
  string dispatch_id = 1;
  // Names, paths, hashes only (rc/scan.go report items).
  repeated LibraryRef found = 2;
  repeated string paths = 3;
}
```

**belai.** Map the kinds onto `rc/prefs.go` (still gated by
`--web-project-settings`) and `rc/scan.go`. An import uploads with
`Library.Backup` and must reproduce the scan's hash.

**Keleustes.** kls-ui: per-node project settings and the library import
view.

**Tests.** A prefs key outside `config.ProjectPrefs` is refused. An import
whose hash differs from the scan is refused.

## Suggested order

1. **WP-A** and **WP-C**: mostly factorylink and belai wiring. They fix Budgets,
   Sessions "waiting" and workforce visibility with little proto change.
2. **WP-B** and **WP-D**: live workforce and full control of what is already
   dispatched.
3. **WP-I**: cross-host dispatch. It depends on WP-D (trigger, capability
   kinds) and is most useful with WP-B (load) and WP-A (`accepts`).
4. **WP-E**, **WP-F**: library and secrets, so a target host can be given what
   a dispatch needs.
5. **WP-G**, **WP-J**, **WP-H**, **WP-K**.

Ownership note (docs/WORKSTREAMS.md): every package touches `node.proto` and
most touch `kls-node/src/server.rs` (`apply`, frame handling), which are
coordinator-owned. New services should go in new kls-node files (`fleet.rs`,
`library.rs`, `forge.rs`) so that only the router wiring touches `server.rs`.

## Cross-cutting risks

- **One classifier path.** Every new inbound payload maps onto an existing
  belai type and gate: answers to webask validation, prompts to
  `CleanPrompt` + classifier, library text to `libstore.UntrustedText` + the
  kind's validator, provenance to cleaned `Trigger`, and a peer prompt to a
  remote prompt. Reviewers should refuse any factorylink code that hands
  Keleustes text to a model directly.
- **Remote approval is remote code execution by consent.** WP-A answers and
  WP-I requests both let one machine cause actions on another. Keep the
  host-side switches (`sync.remote_answers`, `--web-controls`,
  `--web-shell`, `--web-project-settings`) as the final gate. Keleustes
  policy adds to them and never replaces them.
- **Secrets.** New grant contents (MCP, workers) keep the memory-only,
  concealed, scoped, dropped-at-expiry rules. Nothing in `HostStatus`,
  `NodeSummary` or an audit line may carry a value.
- **Version skew.** Gate every new kind on `Capabilities.dispatch_kinds`, so
  an old host refuses cleanly instead of half-applying a request.
- **Replay.** New server frames (`attention_decided`, new dispatch kinds) are
  replayable and must stay idempotent on the node (ack after apply; dispatch
  ids de-duplicated as today).

## Status (implementation log)

- **WP-A** landed: `SessionUpsert.pending_ask/accepts/git/usage`, `SessionEnd.usage/turns/tool_calls`,
  structured `Answer.decision/payload_json` (kls-core `queue_ask_answer`, audited, kill switch refuses approvals),
  per-model usage rows, Sessions screen ask buttons / clarify form / controls menu.
- **WP-D** landed: `Dispatch` 16-30 + `tier` 45 + `options` 51, `Capabilities.dispatch_kinds/job_kinds/can_clone/models`,
  kinds `stop|pause|resume|crew_control|project_prefs`, gateway refuses kinds a node does not list, kill switch via
  `control::check` (stop/pause pass), default trigger from run/card.
- **WP-I** landed: `Fleet` service (Nodes, Request, Watch, Prompt), policy off by default, agent requests always wait,
  rate limit, audit, `belai connect nodes|dispatch|prompt`, Fleet screen approvals.
- **WP-B** landed: `NodeFrame.status` (HostStatus), `node_status`/`worker_spend`, spend charged once.
- **WP-J** landed: `Board.Attention` + `NodeFrame.attention`, replayable `AttentionDecided`, desktop notice.
- **WP-C** landed: worker/ACP mirrors, host audit streams (`AuditBatch.stream_id`, `Sessions.AuditHead`), factory-mode
  memory replication and scheduler.
- **WP-G** landed: `Board.SchedulePull/SchedulePush` (`ScheduleItem`), `host_schedules`, owner edits (audited),
  `Core::dispatch_scheduled` (schedule trigger, least busy eligible node), belai `factorylink.ScheduleRemote`, factory
  mode schedule sync, no firing while stopped, Fleet Schedules card.
- **WP-E** landed: `Library` service (Fetch/Backup/Reconcile, dispatch-scoped, audited), `Dispatch.library/overwrite`,
  kinds `library_install|library_backup`, `HostAdvert.inventory`, belai `factorylibrary.go` (profile via agentprofile
  validation, items via libstore behind `sync.<kind>`, Keleustes agent JSON with `meta.belaiMarkdown` for lossless
  host-to-host profiles), Fleet "Library" install row (hook/mcp need a second click), `keleustes sessions library`.
  Not yet: `TeleportHeader.needs` fetch (the field exists; teleports do not record needs), crews, hook bundle files,
  belai autosync over `Reconcile`.
- **WP-F** landed: `VaultGrant.mcp` (`McpSecret`), `DispatchSpec.mcp` bindings (vault env secret -> server key),
  per-node grants (`vault::grant_for_node`) for dispatches and jobs, grant TTL capped at `stay_idle_max_s`, MCP values
  cleared from the outbox; belai worker grants over inherited pipes (rc -> `agent start` -> each worker), MCP secrets
  via `credentials.UseMCPGrant` (only bound `cred:` keys answer), crews take no grant.
- **WP-H** landed (first step: no bundles): `Forge.File` (request only, a bundle part is refused), `forge_requests`,
  `Core::forge_file` (shape checks, dedupe, audit, `forge.request` event), `Core::forge_steer` -> `steer` dispatch
  (`Dispatch.steer_*` 33-37, `steer_pr` 52; kill switch applies, audited); belai relay files with Keleustes in factory
  mode, factory-mode workers spool again, `steer` mapped onto `fleet.CoordSpec`. The coordinator decision (which node
  opens the PR, glab/tea via forge jobs) is the forge agent's: request in requests-governance/engine not needed,
  see `docs/gaps/forges-proto-request.md`.
- **WP-K** landed: `project_prefs` (`Dispatch.prefs_set/prefs_unset`), `library_scan` / `library_import`
  (`Dispatch.item_kind/scan_dispatch/path/sha256/target`, `Library.ScanReport`, `library_scans`,
  `Core::library_import`), imports re-read and hash-checked on the host and stored only for that import and hash;
  Fleet Library row: Scan, scanned items with Import.
- **WP-I TUI** landed: `/fleet nodes`, `/fleet start <profile> --on <node|any> [--cwd DIR]`, `/fleet crew <crew> --on ...`
  (requests are the user's; progress as system lines).
- **Scout** stack facts landed: `cliResult.packageManagers` / `languages` from a closed manifest table
  (`clijobs.DetectStack`), plus `notEvaluable`.
- **OP-2 harness hardening (harness request §5)** landed: advert kill/asks/named/continuity/caveats/startup
  percentiles; SessionEnd termination_verified/empty_result/adapter_error/timing, stored and shown on the ended session.

## Status re-check (2026-11-22)

Checked against the current code in both repos (rows marked done on
2026-11-21 are not repeated). Paths are relative to each repo root; belai
paths start with `belai/`.

| item | priority | status | evidence (file:line) | what's left |
|---|---|---|---|---|
| #3 Session mirror | P0 | done | belai/internal/factorylink/asks.go:145 (state/usage/ask), belai/internal/factorylink/sessions.go:168 (end rollup); crates/kls-node/src/sessions.rs:191 (card/run from dispatch) | none |
| #5 Approvals / structured asks | P0 | done | belai/internal/factorylink/asks.go:83; crates/kls-core/src/belai.rs:289 (audited, stop refuses), crates/kls-ui/src/screens/sessions.rs:1198 | none (test: kls-core belai.rs `structured_answer_checks_and_audit`) |
| #6 Session controls | P1 | done | belai/internal/factorylink/asks.go:184 (accepts); crates/kls-ui/src/screens/sessions.rs:1395, :1441 (args) | none |
| #7 rc dispatch: session start fields | P1 | done (P3, 2026-11-22) | crates/kls-core/src/sessions/start.rs:44 (`StartFields`), :99 (step config), :178 (checked per kind), :332 (`dispatch`: kill switch, console trigger, audit with every field by name); crates/kls-engine/src/exec.rs:284, :2931, :2984 (step config → spec, refused step `invalid_start_fields`); crates/kls-ui/src/screens/sessions.rs:232, :2422 (New session › Options: extra dirs, workspace, effort, git sync, ask), :2291; tests: kls-core sessions/start/tests.rs, kls-engine exec/tests.rs:333, kls-ui sessions.rs:3002, crates/kls-node/tests/dispatch_fidelity.rs:184 (frame over TLS), belai/e2e/keleustes_dispatch_test.go (real binaries: `rc-session -add-dir DIR -effort high`); P6: crates/kls-ui/src/screens/workflows.rs:1938 (`start_fields_ui`, Start options on a crew/worker/session step: extra dirs, workspace, effort, git sync, ask, fill, stay + idle wait, clone, MCP bindings; live check by `StartFields::validate`), :2439; test workflows.rs:3437 | none |
| #8 rc dispatch: worker / crew fields | P1 | done (P3, 2026-11-22) | crates/kls-core/src/sessions/start.rs:178 (`fill` crew only, `stay`/`stay_idle_max_s` ≤ 24 h, `clone` owner/name@branch); crates/kls-engine/src/exec.rs:2931; crates/kls-ui/src/screens/sessions.rs:2422 (Worker toggle, Stay + idle minutes, Fill, Clone); belai/e2e/keleustes_dispatch_test.go (`agent run ... -stay -stay-idle-max 10m0s`) | Crews take no vault grant by design (factory.go:500). |
| #9 Stop one rc session | P1 | done | belai/internal/rc/factory.go:422; crates/kls-core/src/vault/workforce.rs:622 | none |
| #10 Worker pause/resume, crew control | P1 | done | belai/internal/rc/factory.go:430, :471; crates/kls-core/src/vault/workforce.rs:586; crates/kls-ui/src/screens/workforce/controls.rs:130; P3 2026-11-22: crates/kls-core/src/vault/workforce.rs:597 (`CrewEdit`: `scope_weight`, `scope_pause`, `scope_add`, `scope_remove`, `replicas`, bounds 1–10 and 0–8), :793; crates/kls-ui/src/screens/workforce/controls.rs:247, :284 (Scope editor per crew run); test kls-core vault/workforce/tests.rs:292 (audited with origin, kill switch and A2A refuse widening edits, narrowing ones pass) | Node status reports scope paths but not weights, so the weight slider starts at 1. `pool`, `switch_after`, `final_retry`, `max_workers` and worker target/effort have no `CrewControl` field on the wire (see the design note below). |
| #11 Host status | P1 | done | belai/internal/factorylink/rc.go:147; crates/kls-core/src/belai/status.rs:66; crates/kls-ui/src/screens/workforce.rs:185 | `vault::workers_summary` still guesses from events (vault.rs:2070); could be dropped for status. |
| #12 Fleet worker mirror | P1 | done | belai/cmd/belai/workerfactory.go:22; belai/cmd/belai/agentcmd.go:472 | none |
| #13 Audit log upload | P1 | done (P2, 2026-11-22) | belai/internal/rc/factory.go:81, belai/cmd/belai/workerfactory.go:31; belai/internal/tui/session_sync.go:184 (`keleustesAudit` → `sessionsync.StartAuditOn`, flushed and closed in `factoryStop`); crates/kls-node/src/sessions.rs:372; test belai/internal/tui/session_sync_audit_test.go (uploaded stream passes `audit.Verify`) | none |
| #15 Repository memory | P1 | done | belai/internal/rc/factory.go:87; belai/cmd/belai/workerfactory.go:27; belai/internal/tui/session_sync.go:170 | none |
| #17 Teleport | P1 | done | crates/kls-node/src/teleport.rs:84 (needs cleaned at upload), :131 (stored, `library_backup` to the origin), :168,:385 (resolved into the download header), :194 (`library_install` on the target); belai/internal/factorylink/teleport.go:92,179,281; belai/internal/rc/factoryteleport.go:146; crates/kls-node/tests/teleport_needs.rs | Library.Fetch(teleport_id) requested (requests-governance.md P4); install runs as a `library_install` dispatch until then. |
| #20 Agent profile library | P2 | partial | crates/kls-core/src/belai/library.rs:82, :224; belai/internal/rc/factorylibrary.go:100; crates/kls-ui/src/fleet.rs:981 | Install/backup by dispatch works. No automatic sync: `LibraryReconcile` (belai/internal/factorylink/library.go:33) has no caller and `librarySync` runs only in website mode (belai/internal/rc/daemon.go:602). |
| #21 Crew library | P2 | done | belai/internal/rc/factorylibrary.go:136 (install via libinstall.SaveCrew, belai/internal/libinstall/crewdoc.go:17), :179 (backup as crew JSON with `meta.belaiCrew`, :382), :94 (profiles before crews); crates/kls-core/src/belai/library.rs:354 (crew backup checked), :454 (reconcile by canonical hash); tests: belai/e2e/keleustes_library_test.go (real binaries A → Keleustes → B), belai/internal/rc/factorylibrary_test.go:83, crates/kls-node/tests/belai.rs:1429; P6: crates/kls-core/src/belai/library.rs:399, :417 (`crew_from_backup`: also saved to the Crews table through `vault::save_crew`, idempotent by the canonical hash, an owner-made crew kept, audited with origin `host:<node>`); tests library.rs:672, crates/kls-node/tests/belai.rs:1484 | none |
| #22 Library items | P2 | done | crates/kls-core/src/vault.rs:107 (`repo`, `budget`, `provider`, `rewrite`), :2394 (shape check, canonical JSON); crates/kls-node/src/library.rs:68 (bundle files sent); crates/kls-core/src/belai/library.rs:126, :237 (files only after the second confirmation, audited, kill switch); belai/internal/rc/factorylibrary.go:162 (`libstore.InstallBundle`, each file hash checked); tests: crates/kls-core/src/vault.rs:3844, crates/kls-node/tests/belai.rs:1429, crates/kls-ui/src/screens/library.rs:863; P6: crates/kls-ui/src/screens/library.rs:95 (`FilesConfirm`: path, size, hash; "Confirm files" calls `confirm_bundle_files` before `hosts::library_dispatch`), used by crates/kls-ui/src/screens/host.rs:2341 and crates/kls-ui/src/fleet.rs:1062; test library.rs:997 | none |
| #23 Library scan / import | P2 | done | belai/internal/rc/factorylibrary.go:209; crates/kls-core/src/belai/library.rs:267, :320; crates/kls-ui/src/fleet.rs:934 | none |
| #24 Host inventory | P2 | done | belai/internal/rc/factorylibrary.go:251 (crews with hash), :264 (knowledge catalogue), :270 (plugins, :298); belai/internal/rc/inventory.go:139; crates/kls-core/src/belai/library.rs:54 (catalogue kinds answer "listed"); proto/keleustes/node/v1/node.proto LibraryRef comment; tests: belai/internal/rc/factorylibrary_test.go:83, :177 | Knowledge and plugins ride in `inventory` as kinds `knowledge` / `plugin`; no Keleustes UI lists them separately yet (Host Library tab shows them as "listed"). |
| #25 Provider keys | P1 | done | crates/kls-core/src/vault.rs:575; belai/internal/rc/factory.go:492 | Crews run on host keys (deliberate, WP-F). |
| #26 MCP server secrets | P2 | done (P3, 2026-11-22) | crates/kls-core/src/sessions/start.rs:452 (`mcp_bind`: env secret must exist, names only in settings `mcp.bindings`, provenance by/surface/at, kill switch refuses, audited), :493 (`mcp_unbind`, allowed under the switch); crates/kls-ui/src/screens/vault.rs:195, :1684 (Vault › MCP tab); crates/kls-ui/src/screens/sessions.rs:2422 (pick bindings per dispatch); engine step key `mcp`; test crates/kls-node/tests/dispatch_fidelity.rs:184 (binding → `VaultGrant.mcp` value on the frame; value absent from outbox, dispatch rows, ledger and events) | none |
| #27 Pix Sandbox vault env | - | n/a | - | none |
| #28 Project preferences | P2 | done | belai/internal/rc/factory.go:467; crates/kls-ui/src/screens/project_overrides.rs:190 | none |
| #29 Scheduled agents | P1 | done | belai/internal/rc/factory.go:96; crates/kls-core/src/belai/schedules.rs:212 | none |
| #30 Forge relay and steer | P1 | partial (P2, 2026-11-22) | belai/internal/rc/factory.go:103; crates/kls-core/src/belai/forge.rs:120 (`forge_pending`), :157 (`forge_steer_by`: `control::check` first, refusal audited `factory.refused`, caller as actor, origin `console` for a person); crates/kls-ui/src/screens/delivery.rs:2422 ("Forge requests" panel: Took over / Wait 1h / Failed); crates/kls-node/src/forge.rs:29; test forge.rs:261 | No automatic coordinator turning requests into `github`/`forge` jobs; bundle parts refused. |
| #31 Git facts | P2 | done | belai/internal/factorylink/asks.go:206; crates/kls-ui/src/screens/sessions.rs:826 | none |
| #32 Usage reporting | P1 | done | belai/internal/factorylink/asks.go:170, :221 | none |
| #33 Budget enforcement on nodes | P1 | done (P6, 2026-11-22) | crates/kls-core/src/gateway.rs:125 (`DispatchSpec.budget_push`), crates/kls-node/src/gateway.rs:560 (mapped onto `Dispatch.budget_push`), :445 (shape checked, refusal audited); crates/kls-core/src/belai/budget.rs:97 (`push`: `control::check` first, a run narrows to its open sessions, nodes that take `budget_update` and have live work, trigger `budget:<id>` and `origin_json`, audited `budget_push`), :249 (`on_event`: `budget.saved` sends the caps left (`caps_left` :226) and a resume after a pause; the engine gate's `budget.limit` downgrade / `budget.blocked` pause go to the run's work, once per 10 min), crates/kls-core/src/sweeps.rs:232 (runs in the node-serving process); belai/internal/rc/factory.go:467, belai/internal/rc/factorybudget.go:114 (pause/resume via the pause marker, downgrade via the control target, limits tighten only), belai/internal/fleet/control.go:172 (worker applies the caps from the next item, acked); tests: kls-core belai/budget.rs (4), crates/kls-node/tests/p6_wiring.rs `a_budget_push_reaches_the_node_and_the_kill_switch_holds_it`, belai/internal/rc/factoryp6_test.go, belai/e2e/keleustes_p6_test.go `TestKeleustesBudgetPush` (real binaries: save → frame → worker control file; kill switch refuses, audited) | rc sessions have no live channel: a push names them as "noted" and they take budgets at their next dispatch. |
| #34 Host attention requests | P1 | done | belai/internal/rc/factoryattention.go:28; crates/kls-core/src/belai/attention.rs:41; crates/kls-ui/src/fleet.rs:548 | none |
| #35 Notifications | P2 | done | crates/kls-app/src/desktop.rs:257, :383 | none |
| #36 ACP session mirror | P2 | done | belai/cmd/belai/acpsync.go:49 | none |
| #37 Headless `belai -prompt` | P2 | done (P2, 2026-11-22) | belai/internal/headless/mirror.go:44 (`StartMirror` over `factorylink.ActiveBackend(dir, "headless")`, upload only, off with `sync.enabled` false or `-no-transcript`); belai/cmd/belai/main.go:778; test belai/internal/headless/mirror_test.go | The mirror takes no remote prompt, answer or command, so the kill switch has nothing to stop; it keeps uploading while stopped (a record of what ran). |
| #38 Hooks | P2 | done | see #22: crates/kls-core/src/belai/library.rs:126 (files with hashes after the second confirmation), crates/kls-ui/src/screens/library.rs:488 (Install → runs code → Confirm files) | as #22 |
| #39 Plugins | - | n/a | - | none (inventory listing is #24) |
| #40 Custom slash commands | P2 | done | crates/kls-core/src/vault.rs:104 (`command`); crates/kls-ui/src/screens/sessions.rs:1441 | none |
| #41 Cross-host dispatch | P0 | done (P1, 2026-11-22) | crates/kls-core/src/belai/fleet.rs:455 (`screen`: `check_a2a` for an agent's request, audited `a2a.withheld`), :379 (`release`: a parked agent request is refused on approval while the blackout holds); test crates/kls-node/tests/belai.rs:621 `cross_host_dispatch_a2a_blackout` | none |
| #42 Node directory | P2 | done | belai/cmd/belai/teleportcmd.go:233,272,297; belai/cmd/belai/main.go:271; belai/cmd/belai/teleportcmd_nodes_test.go | `-teleport-to` takes an id, a name or a unique prefix from `Fleet.Nodes`; `-teleport-to ?` lists the nodes. No shell completion script (belai has none). |
| #43 Session trigger provenance | P1 | done | belai/internal/rc/factory.go:333; crates/kls-node/src/gateway.rs:172 | none |
| #44 Vulnetix login, decisions | - | n/a | - | none |
| WP-E `TeleportHeader.needs` fetch | P1 | partial | crates/kls-node/src/teleport.rs:131,168,194,385 | Needs recorded, backed up and sent; the target gets a `library_install` dispatch. `Library.Fetch(teleport_id)` itself waits on the library owner (requests-governance.md "Requests (P4)"). |
| WP-E crews | P2 | missing | belai/internal/rc/factorylibrary.go:100 | Crew install/backup on the host. |
| WP-E hook bundle files | P2 | missing | crates/kls-node/src/library.rs:59 | Send and verify `LibraryFile` parts; keep the second-click gate. |
| WP-E belai autosync over `Reconcile` | P2 | done (P2, 2026-11-22) | belai/internal/factorylink/reconcile.go (`RC.LibraryReconcile`); belai/internal/rc/autosync.go:553 (`factoryReconcile`, every 5 min, hashes only, skipped while stopped, audited `host.dispatch` `library_reconcile` with counts); belai/internal/rc/factory.go:107; test belai/internal/rc/factoryreconcile_test.go | Keleustes takes `Backup`/`Fetch` only for a dispatch that named the item, so a push or install verdict files an `action` attention (subject `library`, at most hourly) instead of moving the document. |
| WP-I A2A blackout on `Fleet.Request` | P0 | done (P1, 2026-11-22) | crates/kls-core/src/belai/fleet.rs:379, :455; crates/kls-node/tests/belai.rs:621 | none (see #41) |
| WP-H forge bundles and coordinator | P1 | partial (P2, 2026-11-22) | crates/kls-core/src/belai/forge.rs:157; crates/kls-ui/src/screens/delivery.rs:2422; crates/kls-node/src/forge.rs:29 | A person steers from the Delivery screen; bundle upload and an automatic coordinator are left. |

Counts (37 rows not done on 2026-11-21, after the P2 package 2026-11-22): 21 done, 13 partial, 0 missing, 3 n/a. All 44 rows: 28 done, 13 partial, 0 missing, 3 n/a. WP sub-items: 2 done, 2 partial, 2 missing.

P3 (2026-11-22): #7, #8 and #26 moved from partial to done; #10 gained the scope and replicas UI. The counts above predate that and the other 2026-11-22 updates.

### New inventory rows from the belai sweep (2026-11-22)

The sweep covered `internal/tui/commands.go`, `cmd/belai/*.go` and `docs/*.md`. Each row below is a user-facing belai feature that coordinates across hosts or could be dispatched centrally. None has a Keleustes counterpart, and none was in rows 1–44. Every dispatch kind in `rc/factory.go:276` (`factoryDispatchKinds`) is already covered by rows 1–44.

These are covered elsewhere, so they get no row here:
- the AI firewall (`host-management.md` §1.3);
- hazard rails and egress refusals (Trust screen, `sf-parity.md`);
- guardian, the merge-conflict resolver and CLI jobs (`builtin-agents.md`);
- worktrees, trusted dirs and session retention (`host-management.md` §2);
- profile knowledge files (row 20);
- session measures (rows 3 and 32);
- `/inbox` (row 34);
- the fleet directory (rows 41 and 42).

These are local only and get no row: bot attribution (Pix only), notifications (row 35), plugins (row 39), LSP, diff, code-mode internals, context offload and prompt cache.

| # | belai feature | belai location | Cloud endpoint today | Keleustes coverage | Missing pieces: proto / kls-node / kls-core / kls-ui / factorylink |
|---|---|---|---|---|---|
| 45 | Built-in session actions run from the console (`/compact`, `/relay` reviewed handoff, `/undo`, `/rewind`, `/commit`, `/btw`, `/rename`) | `tui/relay.go`, `tui/rewind_view.go`, `internal/checkpoint`, `tui/commit_flow.go`, `tui/side.go`; allowlist `factorylink/asks.go:184-203` | none | **missing** | proto: `Command` names, plus `SessionAccepts.controls_allowed` entries. `rewind` needs a checkpoint list on the upsert, and `relay` needs a `PendingAsk` kind `relay_review`. belai: accept these through sessionctl behind `--web-controls`, with a person confirming. kls-core: queue the command with args, plus audit. kls-ui: action menu and rewind picker. Extends row 6. |
| 46 | Resume or fork a saved belai session on its host (`-resume`, `/resume`, `/tree` fork) | `internal/session/{branch,browse}.go`, `tui/resume.go`, `tui/tree_view.go` | none | **missing** | proto: `resume_session` / `fork_entry` on a `session` Dispatch. `harness_resume` (`node.proto:390`) is for third-party harnesses only. belai rc: a resume path in `factoryStart`. kls-core: resume from a stored session row. kls-ui: Resume / Fork buttons on ended sessions. |
| 47 | Rubric code review as a node job (`/review`, `belai review`) | `internal/review`, `cmd/belai/reviewcmd.go`, `docs/review.md` | none | **partial** | proto: job kind `review` (worktree, branch or PR; base) and `ReviewResult` (path, line, severity, criterion, verdict). factorylink: run `belai review`. kls-core: store the findings and feed `pr_review` from them (today it comes from a model's verdict note, `kls-engine/src/defaults.rs:204-210`). kls-ui: findings panel. |
| 48 | Provider plan limits / session intelligence (`/intel`: 5h/7d windows, rpm/tpm, pace, runway) | `internal/budget/{planlimit,intel,intelstate}.go`, `tui/intel_sync.go` | `intel_state` entry on `POST /sessions/{id}/entries` | **missing** | proto: `PlanLimit` (provider, window, used fraction, `reset_at`) on HostStatus or the session upsert. factorylink: map `intel_state`. kls-core: limits per node and provider; routing skips a node whose window is nearly used. kls-ui: limit bars in Fleet / Budgets. |
| 49 | Knowledge index: status, reindex, search across hosts | `internal/knowledge`, `cmd/belai/knowledgecmd.go`, `/knowledge` | catalogue only (row 24) | **missing** | proto: dispatch kind `knowledge_reindex` and a `Knowledge.Search` RPC (addresses and snippets cleaned by the classifier). kls-node: the service. kls-core: fan-out search. kls-ui: knowledge tab on Host / Library. |
| 50 | OpenTelemetry export, set per node | `internal/otel`, `cmd/belai/telemetry.go`, `docs/telemetry.md` | none | **missing** | host-management `config_set` has no `telemetry.*` keys. Headers go through a vault grant. Optional factory-wide OTLP endpoint. kls-ui: Telemetry in the host Config tab. kls-core: audit each change. |
| 51 | Harness gates on each dispatch: finish review, lint loop, checkpoints, code mode, `commit_mode`; finish-review findings | `cmd/belai/{finishreview,lintloop,checkpoints,runcaps}.go`, `internal/{finishreview,lintloop,codemode}` | none | **partial** | proto: new keys in `Dispatch.options` (tighten-only). Finish-review findings as a session entry kind. belai: honour the keys. kls-ui: switches in the dispatch dialog. |
| 52 | Unattended dispatch: answers up front, plan file returned (`-answers`, `planhint`) | `cmd/belai/answers.go`, `cmd/belai/planhint.go` | none | **partial** | proto: `answers_json` on Dispatch. A plan-file upload on completion for `mode=plan`. kls-core: plan review, then an "execute" dispatch. kls-ui: "Answers" and "Plan first" in the dispatch dialog. |
| 53 | Attachments (images, `@` files) on remote prompts and dispatches | `tui/attach*.go`, `internal/imageguard`, `docs/image-attachments.md` | none | **done** (B, 2026-11-22) | proto: attachments (name, mime, sha256, chunked bytes) on `Prompt` (`node.proto:679`, text only) and `Dispatch`, with caps. belai: imageguard and the classifier check received bytes. kls-core: store, plus a hash in audit. kls-ui: attach control. |
| 54 | Draft AGENTS.md (`/init`) as a job across repos | `tui/init_cmd.go`, `docs/instruction-files.md` | none | **missing** | proto/factorylink: model job kind `init_draft`. kls-core: a person accepts, then a PR via the forge (row 30). kls-ui: "Draft AGENTS.md" under Repos. |
| 55 | Ad hoc worker with no stored profile (`-prompt` plus `-tools`, `-claim`, `-to`, `-publish`, `-read-only`) | `cmd/belai/adhoc.go` | none | **missing** | proto: `AdhocWorker` spec on a `worker` Dispatch (today only an `agent` name, `node.proto:350-351`). belai rc: build the profile from the spec. kls-core: provenance records the inline spec. kls-ui: "One-off worker" form. |

| # | priority | status | evidence | what's left |
|---|---|---|---|---|
| 45 | P1 | done (P3b, 2026-11-22) | proto node.proto `Command` comment, `SessionUpsert.checkpoints`=31 + `Checkpoint`, `PendingAsk.relay_json`=8, `Capabilities.dispatch_features`=12; belai/internal/rc/actions.go:31 (`ActionNames`), :160 (`runAction`: rename, compact, relay → `relay_review` ask, undo, rewind, commit, btw; text admitted, audited `host.dispatch` `session_action`), :339 (`answerRelay`: only approve or an admitted edit replaces the conversation), :392 (checkpoint list); belai/internal/rc/session.go:311, :462 (idle / running); belai/cmd/belai/rcactions.go:196 (host implementations), :84 (checkpoints in rc sessions); belai/internal/factorylink/sessions.go:247 (stopped: only undo/rewind), :561 (`ToCommand`); belai/internal/factorylink/asks.go:124 (checkpoints), :258 (controls_allowed), :309 (relay answer); crates/kls-core/src/sessions/actions.rs:21, :79 (`queue_action`: controls_allowed, kill switch except undo/rewind, A2A for non-owner origins, audited); crates/kls-core/src/belai.rs:169, :435 (relay review); crates/kls-ui/src/screens/sessions.rs:1607 (action menu, rewind picker), :1789 (relay review); tests: belai/internal/rc/actions_test.go, belai/internal/factorylink/actions_test.go, board_test.go `TestLinkRecordsFactoryControl` (stopped), kls-core sessions::resume::tests `console_actions_are_checked_queued_and_audited`, `relay_review_answers`; e2e belai/e2e/keleustes_resume_test.go (rename round trip). **Conversation rewind (agent D, 2026-11-22):** belai/internal/session/branch.go:245 (`RewindPoint`, shared by the TUI, belai/internal/tui/checkpoints.go:212, :245, and the console); belai/internal/rc/actions.go:225, :323 (`restoreTo`: a rewind puts the files back, then a branch marker returns the conversation to just before the checkpoint's prompt and history is rebuilt from the chain via `headless.TurnsFromEntries`, as the TUI's /rewind "both"; a safety copy, which has no prompt, puts back files only; undo stays files only like the TUI's /undo; an open relay review is closed; a `system` provenance line `meta.kind=rewind` with command id, checkpoint, prompt/from/to/branch entry ids, entries and prompts rewound, file counts, safety checkpoint), :102 (`auditFacts`: `host.dispatch` `session_action` with `itemId` = the Keleustes inbox item, which kls-core audits with its origin, and `checkpoint`, `conversation`, `restored`/`removed`/`left`, `safety`, `from`/`to`, `rewound`, `prompts`; keys added at belai/internal/audit/audit.go:196 and docs/audit.md); kill switch: rewind stays wind-down on both sides (no model runs); harness sessions still list no actions (belai/internal/factorylink/asks.go:242); crates/kls-core/src/sessions/actions.rs:186 (`rewound`: entries off the chain from the newest line, read from `meta_json` id/parentId, nothing marked without a `branch` line or across an unsynced gap), :222 (`rewind_prompt`); crates/kls-ui/src/screens/sessions.rs:2050, :2124 (rewound entries muted with a "rewound" chip), :1672 (the rewound prompt goes back into an empty composer); tests: belai/internal/rc/actions_test.go `TestConsoleRewindRestoresFilesAndConversation` (files, branch marker, next turn sees 2 turns, provenance line, audit event, safety copy files-only, unknown id refused), belai/internal/session/branch_test.go `TestRewindPoint`, factorylink actions_test.go (harness lists no undo/rewind), board_test.go (rewind arrives while stopped, compact refused), kls-core sessions::actions::tests (2); e2e `TestKeleustesResumeForkAndActions` (real binaries: marker written, rewind from Keleustes removes it, branch marker parents to before the prompt, provenance line reaches the Keleustes transcript) | The console rewind always restores both (no code-only / conversation-only choice: `rewind` takes one argument). |
| 46 | P1 | done (P3b, 2026-11-22) | proto `Dispatch.resume_session`=54, `fork_entry`=55; belai/internal/rc/factorysession.go:114 (`checkResume`: id shape, same project, entry is a user msg or final reply), belai/internal/rc/daemon.go:1049, :1137 (audited `target`), :1287 (`-resume`/`-fork` argv); belai/cmd/belai/rcactions.go:45 (`rcResume`: `ForkPath` into a new session, history replayed via `headless.TurnsFromEntries`); crates/kls-core/src/sessions/resume.rs:57 (kill switch, A2A, feature check, trigger `resume:`/`fork:`, audited); crates/kls-node/src/gateway.rs:401, :664; crates/kls-ui/src/screens/sessions.rs:1715 (Resume / Fork); CLI `keleustes sessions resume <sid> [--fork E]`; tests: belai/internal/rc/factorysession_test.go `TestCheckResume`, belai/cmd/belai/rcactions_test.go, kls-core `resume_and_fork_carry_provenance_and_audit`, kls-node tests/dispatch_fidelity.rs `p3_fields_cross_the_wire_behind_their_features`; e2e belai/e2e/keleustes_resume_test.go (real binaries: resume and fork) | Resume always starts a new session id (the old row stays ended). |
| 47 | P1 | partial | `crates/kls-engine/src/defaults.rs:204-210` | job kind, `ReviewResult`, findings UI |
| 48 | P1 | done (P6, 2026-11-22) | crates/kls-node/src/status.rs:47, :53 (HostStatus `plan_limits`), crates/kls-node/src/sessions.rs:190, :213 (SessionUpsert `plan_limits`); crates/kls-core/src/belai/plan.rs:166 (`node_plan_limits`, provenance node/session/source/at), :195, :219 (over the threshold `routing.plan_limit_skip` (0.9): `belai.plan_limit.near` + audit `plan_limit`, once per reset), :150 (`route_filter`), crates/kls-node/src/gateway.rs:127 (jobs), crates/kls-engine/src/exec.rs:2702 (steps); crates/kls-ui/src/fleet.rs:660, :729, crates/kls-ui/src/screens/host.rs:1026 (bars); belai/internal/rc/factoryplan.go:68 (HostStatus from the usage ledger: 5h, 7d, rpm, tpm); tests: kls-core belai/plan.rs (3), crates/kls-node/tests/p6_wiring.rs `plan_limits_from_status_and_sessions_land_per_node`, kls-ui fleet.rs `plan_rows_show_provider_window_and_tone`, belai factoryp6_test.go | belai does not fill `SessionUpsert.plan_limits` yet (its upsert is built in factorylink/asks.go, owned elsewhere this round); the host status carries the windows. When every node is over the threshold the list is kept (work waits on the provider). |
| 49 | P2 | done (P6, 2026-11-22) | belai/internal/rc/factory.go:469, belai/internal/rc/factoryknowledge.go:126 (`knowledge_reindex`: profile key or offered project root/key, runs `belai agent knowledge -index`, which classifies; re-advertises), :266, :323 (`knowledge_search` job: index read only, no model, snippet one cleaned line ≤ 320 B); crates/kls-core/src/belai/knowledge.rs (`search`/`search_on` fan-out over nodes that list the job, hits name their node, snippets cleaned, audited with the query hash only; `reindex` audited; both refused under the kill switch); crates/kls-ui/src/screens/knowledge.rs (Memory › Knowledge: search, per-index Reindex); tests: kls-core belai/knowledge.rs (3), crates/kls-node/tests/p6_wiring.rs `knowledge_search_and_init_draft_go_out_as_jobs`, belai factoryp6_test.go, belai/e2e/keleustes_p6_test.go `TestKeleustesKnowledgeAcrossHosts` (real binaries: reindex → search hit from the host; kill switch refuses) | none |
| 50 | P2 | missing | no `otel` / `otlp` in `crates/` or `proto/` | config keys, vault grant for headers, UI |
| 51 | P2 | done (P3b, 2026-11-22) | proto `Dispatch.options` comment (gate keys); belai/internal/rc/factorygates.go:62 (`ApplyGateOptions`, tighten-only), belai/internal/rc/factorysession.go:241 (`gateIgnored`: audited `option_ignored`), belai/cmd/belai/rccmd.go:890; crates/kls-core/src/sessions/start.rs:101, :171; crates/kls-ui/src/screens/sessions.rs:2675 (switches); tests `TestGateOptionsOnlyTighten`, kls-core `answers_gates_and_one_off_workers` | Finish-review findings as a `finish_review` entry: shape documented in node.proto, belai does not emit it yet. Workflow editor (C, 2026-11-22): Gates form kls-ui/src/screens/workflows.rs:2207, Scripted answers :2074, One-off worker :2264, checked by the engine validator kls-engine/src/validate.rs:445 `start_problems` (also run by dry run and publish, :589) and shown under the form :1996; tests `validate::tests::start_fields_answers_gates_adhoc`, kls-ui `answers_gates_and_adhoc_forms_write_what_the_engine_reads`, `start_options_render_for_each_kind` |
| 52 | P2 | done (P3b, 2026-11-22) | proto `Dispatch.answers_json`=56, `SessionEnd.plan_sha256`=14; belai/internal/rc/factorysession.go:166 (`checkAnswers`: every note admitted by `Options.AdmitText`, "the answers were refused"), belai/cmd/belai/rcactions.go:112, :124 (`plan_file` entry), :297 (`rcAdmitText`); belai/internal/rc/session.go:795, :848; belai/internal/factorylink/sessions.go:170; crates/kls-core/src/sessions/start.rs:142, resume.rs:180 (`execute_plan`, trigger `plan:<sha>`); crates/kls-node/src/sessions.rs:287; UI Answers / Plan first / Execute plan; tests `TestCheckAnswersAdmitsEveryNote`, `TestRCPlanFileAndAnswers`, kls-core `execute_plan_needs_a_plan_and_names_it`; workflow step form kls-ui/src/screens/workflows.rs:2074 (C) | The plan rides in the transcript (no `Jobs.Upload` RPC exists). |
| 53 | P2 | done (B, 2026-11-22) | proto node.proto:786 (`Prompt.attachments`=2), :475 (`Dispatch.attachments`=58), :795 (`Attachment` name/mime/sha256/size, caps in its comment), :805 (`AttachmentRequest`), :60 (`Sessions.FetchAttachment`, 64 KiB parts), :313 (feature `attachments`); caps 8 per prompt/dispatch, PNG/JPEG ≤ 8 MiB, UTF-8 text ≤ 64 KiB, 16 MiB total; crates/kls-core/src/attachments.rs:222 (`store_bytes`: sniffed, capped, content-addressed in `<data>/artifacts`), :314 (`queue_prompt`: kill switch, A2A blackout for non-owner origins, caps, blob held, host feature, audited `attachment` with hashes; inbox holds references only), :400 (`authorize`: only the node whose dispatch or inbox item lists the hash, refused paused/stopped, each read audited), :296 (`check_spec`); crates/kls-core/src/sessions/start.rs:69, :142 (StartFields/feature, audit_json carries hashes); crates/kls-node/src/sessions.rs:391 (`fetch_attachment` stream), :23; crates/kls-node/src/gateway.rs:475, :681; crates/kls-ui/src/screens/attach.rs:35 (Attach control), screens/sessions.rs:2004 (composer), :2439 (New session); CLI `sessions prompt|dispatch --attach FILE` (crates/kls-app/src/cli.rs:345); belai/internal/factorylink/attachments.go:44 (`CheckAttachments`), :82 (`FetchAttachment`: size + SHA-256 checked), belai/internal/factorylink/sessions.go:235 (inbox prompt), belai/internal/rc/attachments.go:39 (`AdmitAttachments`: imageguard for images, classifier for text, one refusal refuses all), belai/internal/rc/factory.go:540, daemon.go:1076, :1331 (admitted by the daemon, 0600 `-attachments` file), session.go:238, :375 (first turn / later prompts, transcript marker only), factorysession.go:36, :87, belai/cmd/belai/rccmd.go:1094, :1105; TUI and fleet workers refuse such prompts (tui/session_sync.go:324, fleet/webprompts.go:66); docs belai/docs/keleustes.md "Attachments", image-attachments.md; tests: kls-store attach.rs, kls-core attachments::tests (8), kls-node tests/sessions.rs `prompt_attachments_are_fetched_by_their_node_only`, belai rc/attachments_test.go (`TestAdmitAttachments`, `TestFactorySessionAttachments`, `TestRemotePromptAttachments`, `TestCheckAttachmentRefs`), factorylink/attachments_test.go `TestInboxPromptAttachments`; e2e belai/e2e/keleustes_attach_test.go `TestKeleustesAttachments` | Workflow steps cannot attach files yet (no step config key); a terminal (TUI) session and fleet workers refuse attachments instead of taking them; peer prompts (Fleet.Prompt) carry none. |
| 54 | P2 | done (P6, 2026-11-22) | belai/internal/rc/factoryinitdraft.go:93, belai/cmd/belai/initdraftjob.go:43 (`init_draft` job: offered dir or matching repo, one model call over the classified repo-map facts, nothing written, sha256 and base sha); crates/kls-core/src/belai/initdraft.rs (`request` checks the hash and keeps the draft pending with provenance; `decide`; audited `init_draft`; request and accept refused under the kill switch); crates/kls-github/src/forge/propose.rs (`propose_file`: branch, one commit, pull request on GitHub/GitLab/Gitea through `Client::request` (kill switch, write cap, forge audit); `accept_init_draft`); crates/kls-ui/src/screens/init_draft.rs, crates/kls-ui/src/screens/repos.rs (Repositories › Draft AGENTS.md: Draft, Review/edit, Accept → PR, Reject); tests: kls-core belai/initdraft.rs (3), kls-github forge/propose.rs (2), crates/kls-node/tests/p6_wiring.rs, belai/e2e/keleustes_p6_test.go `TestKeleustesAgentsDraft` | none |
| 55 | P2 | done (P3b, 2026-11-22) | proto `Dispatch.adhoc`=57 + `AdhocWorker`; belai/internal/rc/factorysession.go:281 (`checkAdhoc`: shapes, prompt admitted), :334 (`-prompt=` argv), belai/internal/rc/workers.go:83 (audited prompt hash + tools); crates/kls-core/src/sessions/start.rs:182; crates/kls-ui/src/screens/workforce/controls.rs:551, :599 (One-off worker); CLI `keleustes sessions worker --prompt`; tests `TestAdhocChecksAndArgv`, kls-core `answers_gates_and_one_off_workers`; workflow step form kls-ui/src/screens/workflows.rs:2264 (C), a step with `adhoc` needs no agent and names none (kls-engine/src/validate.rs:445) | The prompt is visible in the host process list (argv), as with `belai agent start -prompt`. |

New rows: 11 (8 missing, 3 partial).

### Design note: proto-gated rows 45, 46, 51, 52, 55 (P3, 2026-11-22)

P3 changed no `.proto` file. These are the additive wire changes the next
step needs, with the belai handlers and the Keleustes producers. Every field
is optional, so an older host ignores it and an older Keleustes never sends
it; `buf breaking` stays clean. Field numbers are the next free ones in
`proto/keleustes/node/v1/node.proto` on 2026-11-22 (`Dispatch` ends at 53,
`SessionUpsert` at 30, `PendingAsk` at 7, `SessionAccepts` at 6); re-check
them before the edit, because the library work also changes this file.

Every new path keeps the five rules: the console queues it through an
audited `kls-core` function that calls `control::check` first (resume and
fork start work, so pause and stop refuse them; console actions that only
wind down, such as `undo` before a stop, follow `sessions::WIND_DOWN`), the
origin rides in `Dispatch.trigger` / `origin_json`, and the host classifies
any text it takes.

**Row 45, console session actions.** No new message. `Command.name` takes
`compact`, `relay`, `undo`, `rewind`, `commit`, `btw` and `rename`; args stay
`repeated string`.

```proto
message SessionUpsert {
  // ...
  repeated Checkpoint checkpoints = 31;   // newest first, at most 50
}
message Checkpoint {
  string id = 1;        // belai internal/checkpoint id
  int64 at_ms = 2;
  string label = 3;     // cleaned one line, at most 120 chars
  uint64 entry_index = 4;
}
// PendingAsk.kind gains "relay_review"; options "approve", "edit", "drop".
message PendingAsk {
  // ...
  string relay_json = 8;  // the handoff draft for review (untrusted)
}
```

belai: add the seven names to the sessionctl allowlist that
`factorylink/asks.go:184-203` advertises in `controls_allowed`, behind
`--web-controls`. Map them onto `tui/relay.go`, `tui/rewind_view.go`
(`rewind <checkpoint id>`), `tui/commit_flow.go`, `tui/side.go` (`btw`) and
`internal/checkpoint`. Fill `checkpoints` in the upsert from
`internal/checkpoint`. `relay` raises a `relay_review` ask, and the relay
goes out only when a person answers `approve`. Keleustes: `Core::queue_command`
(kls-core/src/sessions.rs) checks the name against `controls_allowed`; the
UI adds an action menu and a rewind picker beside the controls at
`kls-ui/src/screens/sessions.rs` `controls_ui`.

**Row 46, resume or fork a belai session.**

```proto
message Dispatch {
  // ...
  string resume_session = 54;  // a belai session id on this host
  string fork_entry = 55;      // with resume_session: fork at this entry id
}
```

belai rc: in `factoryRequest` (`rc/factory.go:283`) copy both fields. In
`startGrant` (`rc/daemon.go:980`), when `resume_session` is set, check its
shape (no `session.ValidID` exists yet; add one). It must be a session of this host whose cwd is still
offered (`Allowed(d.dirs(), …)`). Then start `rc-session` with new `-resume ID` and
`-fork ENTRY` flags (the TUI's `-resume` at `cmd/belai/main.go:208` and `internal/session/branch.go` do the work) when `fork_entry` is set. Refuse
an unknown id with "this host has no such session". Keleustes: the
`DispatchSpec` fields; `start::dispatch` refuses them on kinds other than
`session`; Resume and Fork buttons on ended sessions in the session detail
(fork takes the picked entry's id from `Entry.meta_json`).

**Row 51, harness gates per dispatch.** No new field: the keys go in
`Dispatch.options` (51). Each one can only tighten a gate:
`finish_review=on`, `lint_loop=on`, `checkpoints=on`, `code_mode=off` and
`commit_mode=none|per_task|end`. One proto addition carries the findings:

```proto
// Entry.meta_json of a role "system" entry with meta.kind "finish_review":
// {"findings":[{"path","line","severity","criterion","verdict"}]} (no
// message change; documented here so both sides agree on the shape).
```

belai: `rc-session` reads `Options` (`rc/daemon.go:304`) and applies each key
through the existing flag helpers. Those are
`applyFinishReviewFlag` (`cmd/belai/finishreview.go:16`),
`applyLintLoopFlag` (`lintloop.go:12`) and
`applyNoCheckpointsFlag` (`checkpoints.go:9`), plus the code-mode and commit
settings in `runcaps.go`. A key that would loosen a gate the host config set
is ignored and audited `host.dispatch` `option_ignored`. Keleustes: a step
config key `gates` merged into `crew_options` (`kls-engine/src/exec.rs:282`),
and switches under Options in the New session dialog.

**Row 52, unattended dispatch.**

```proto
message Dispatch {
  // ...
  string answers_json = 56;  // cmd/belai/answers.go shape, at most 64 KiB
}
message SessionEnd {
  // ...
  string plan_sha256 = 14;   // mode "plan": the plan file it wrote
}
```

The plan file comes back as an artifact over the existing `Jobs.Upload`
(no new RPC), with its sha256 in `SessionEnd`. belai: `factoryRequest` copies
`answers_json`. `rc-session` writes it to a 0600 file under the session dir
and passes `-answers` (`cmd/belai/answers.go`). The answers go through the
classifier like a prompt, so a refusal ends the start with "the answers were
refused". At the end of a `mode=plan` session, `planhint.go` names the file,
and the host uploads it and sets `plan_sha256`. Keleustes: an "Answers" box
and a "Plan first" switch in the dialog. A plan review screen then sends an
"execute" dispatch with `trigger.subject = "plan:<sha>"` (provenance) after a
person approves.

**Row 55, ad hoc worker.**

```proto
message AdhocWorker {
  string prompt = 1;           // the profile prompt, classified on the host
  repeated string tools = 2;   // tool names, checked against the catalogue
  repeated string claim_lists = 3;
  repeated string claim_labels = 4;
  string to = 5;               // hand-off list
  string publish = 6;          // "none", "branch", "pr"
  bool read_only = 7;
}
message Dispatch {
  // ...
  AdhocWorker adhoc = 57;      // kind "worker" with agent empty
}
```

belai rc: in `startWorkersGrant` (`rc/workers.go:55`), build the profile with
the code behind `adhocFlags.profile` (`cmd/belai/adhoc.go:35`, moved into a
package both can call). Validate it as a stored profile is validated, classify the
prompt, then start it as an ephemeral profile (`agentprofile/ephemeral.go`).
Refuse `adhoc` together with `agent`. Keleustes: `DispatchSpec.adhoc` and a
"One-off worker" form on the Workforce Start row. The inline spec (prompt
hash, tools, lists) goes into the dispatch audit line as provenance, and the
prompt text is never logged.
