# Requests from the governance workstream

Governance owns the vault, budgets, settings, the kill switch, trust, audit,
memory, notifications and retention. These are the public APIs other
workstreams must call, and the changes we need in files we do not own.

## Public APIs to call (all in `kls-core`)

### Kill switch: `kls_core::control` (rule 3)

- `control::check(core, action, target, origin).await?` is **the** check.
  Call it first in every dispatch, job, trigger fire, schedule run, workflow
  step, provision action and outbound node frame. While the factory is
  paused or stopped it emits an audited `factory.refused` event and returns
  an error that `control::is_blocked(&e)` recognises. `action` is one of
  `dispatch`, `job`, `trigger`, `schedule`, `step`, `provision`,
  `node_frame`, `grant`. `target` is what it acts on (run, node or card), and
  `origin` is who or what caused it (`workflow:<id>`, `run:<id>`,
  `agent:<name>`, `owner`, `node:<id>`).
- `control::allowed(..)` returns the same answer as a bool.
- `control::check_a2a(core, from, to, origin)` is the A2A gate. It refuses
  under the A2A blackout **and** under the kill switch, and audits
  `a2a.withheld`. There is no bypass for built-ins or crews.
- `control::state(core)` returns `ControlState { mode: Running|Pause|Stop, reason, since, .. }`.
- `control::pause / stop / resume` (or `set_mode`) set the switch. A stop
  runs every registered `control::Stopper`. Register one with
  `control::register_stopper(core, Arc<dyn Stopper>)`.
- `kls_core::vault::grant_for_purpose` already calls `control::check`, so
  no key leaves while the switch is on.

### Budgets: `kls_core::vault::budget_gate` (P0 #2, builtin-agents P8)

- `budget_gate(core, &Spend, origin).await? -> BudgetGate` runs before any
  model work (dispatch, job, decision). It returns `Allow`,
  `Downgrade { provider, model }`, `Pause`, `Escalate` or `Block`, and it
  records alert crossings once per period (`budget.alert`) and refusals
  (`budget.blocked`). Both are audited.

### Key tests: `kls_core::vault::probe`

- `probe::probe_key(provider, value)`, `probe::probe_stored(core, id, by)`,
  `probe::spawn_probe`, `probe::test_all`. These are indicators only and
  never gate a save.

### Estimates: `kls_core::estimates`

These return human-equivalent cost and time for cards, runs, sessions and
PRs, built from the role rates in Settings > Estimates (`delivery.*`). See the module
docs for details.

## Requests

(Each request names the owner, the file and the exact change.)

### To the engine and board agent (`crates/kls-engine`, `kls-core/src/board.rs`, board screens)

1. **Kill switch check (rule 3)** in `kls-engine/src/exec.rs`. Call
   `kls_core::control::check(core, "dispatch"|"job"|"step"|"trigger"|"schedule", target, origin)`
   at the top of `dispatch`, `start_job`, `fire_event_triggers`, `fire_one`
   (cron) and `advance`. Replace `Engine::stopped()` with
   `!control::state(core).mode.running()`. **Pause semantics**: steps already
   `running`/`waiting` may settle (`settle_dispatch` and `settle_waits` keep
   running while the mode is `Pause`), and nothing new starts. Stop is
   handled by the registered `Stopper` (`kls-app/src/control.rs`, which
   calls `ops::cancel_run`). Add a test that a dispatch under pause is
   refused and that the refusal is audited.
2. **Budget enforcement (P0 #2, builtin P8)**. In `exec.rs::dispatch` and
   `start_job`, before the gateway call, run
   `kls_core::vault::budget_gate(core, &Spend { provider, model, agent, repo, tokens: planned, usd: 0.0 }, &format!("run:{}", run.id))`.
   Map the result like this:
   - `Block` fails the step with `budget_exceeded`.
   - `Pause` calls `ops::pause_run`.
   - `Escalate` opens an approval.
   - `Downgrade { provider, model }` swaps the spec's provider and model,
     using `Capabilities.providers` when it is present.
   Also enforce `limits.runBudgetTokens` against `run.tokens`.
3. **Settings readers (P0 #3)**. Load `kls_core::audit::config::Effective`
   at start and again on `settings.saved`. Then:
   - Set `no_host_ttl_ms` from `autonomy.no_host_ttl_hours` (it is hard-coded
     to 1 h today).
   - `concurrency` caps runs factory-wide.
   - `max_attempts` is the default `maxAttempts`.
   - `claim_ttl_ms` fails a step with `card_unclaimed`.
   - `schedules_tz` is the default schedule zone.
   - `schedules_missed` is the default catch-up.
   - `defaults_enabled` gates the defaults catalogue.
   - Gate outputs with `autonomy_max`, `autonomy_default`,
     `sensitive_paths`, `branch_prefix`, `pr_ready` and `protect_branches`.
   - Pass `agent_mode`, `ask_permission`, `plan_approval` and `audit_calls`
     in the dispatch payload.
   Remove each key from `config::PENDING` as you wire it (the Settings
   screen shows an hourglass on pending keys).
4. **Card claims on stop**. Add a `release_all_claims()` to
   `kls-core/src/board.rs` once claims exist (P0 #9). Register it as a
   `control::Stopper`, or tell us and we will call it from
   `kls-app/src/control.rs`.
5. **Estimates on cards and runs**. Render `kls_core::estimates::for_card`
   and `for_run` (by-hand hours and value, model cost, return) in the board
   drawer and the run detail. Render `estimates::performance(core, since, GroupBy::Agent|Node|Model)`
   on Workforce (throughput, success rate, cost per item, median cycle,
   value).

### To the belai agent (`kls-node`, `kls-core/src/gateway.rs`, proto)

1. Call `control::check(core, "node_frame", node_id, origin)` before every
   outbound Dispatch and Job frame in `kls-node/src/gateway.rs`. Also call
   it on outbox replay in `server.rs:168`, where `grant_for_purpose` already
   refuses under the switch.
2. A2A relay paths call `control::check_a2a(core, from, to, origin)`.
3. Lost sessions are done in `kls_core::sweeps` (on `node.offline`, plus a
   10-minute grace, open sessions end `lost` through `Core::session_end`).
   No kls-node change is needed. If kls-node already sweeps, drop one of
   the two.
4. P8a: when `Dispatch.budget` and `Dispatch.tier` land, tell us. The
   engine fills them from `budget_gate`.

### To the forge agent (`kls-github`, repos, conflicts, delivery)

1. `signing.email`: read `Effective::load(core).signing_email` in the
   conflict commit.
2. Delivery screen: show `kls_core::estimates::for_range(core, since)` (the
   web estimate model, priced with the `delivery.*` rates now in
   Settings > Estimates) and the Outcomes page "by hand" figures through
   `estimates::estimate`.
3. Call `control::check(core, "provision", repo, origin)` before GitHub
   writes made by automations (draft PRs, labels, merges).

### To the library-search agent

- Imports and installs done by an automation call
  `control::check(core, "provision", item, origin)` first, and record their
  origin (URL plus content hash) in the emitted event so the audit fold
  keeps it.

## API notes (update 2)

- **`control::check` semantics.** These are unchanged for the kill switch.
  `Blocked` has one new field, `window: bool`. While the switch is off,
  `check` also holds actions inside the windows configured in Settings >
  Hosts:
  - `hosts.maintenance` holds dispatch, job, step, trigger, schedule,
    provision and node_frame.
  - `schedules.blackout` holds trigger and schedule.

  A hold emits `factory.refused` with `outcome: "held"`. Treat
  `is_blocked(&e)` as "wait and retry later", not as a failure. Nothing is
  held when neither setting is set, which is the default.
  `hosts::tests::plan_runs_steps_and_stops_on_kill_switch` passes on its own.
  It failed once in a full parallel run, so it looks timing-sensitive (its
  5 s deadline). Please make it robust; it does not depend on any
  governance change.
- **Budgets.** `budget_gate(core, &Spend, "run:<id>")` now also applies
  workflow budgets, working out the workflow from the run id. Use
  `budget_gate_scoped(core, &Spend, &SpendScope { workflow, node }, origin)`
  so node budgets apply too. The engine agent should switch to it in
  `exec.rs`, where the node is known.
- **Vault, per node.** Use `vault::grant_for_node(core, providers, ttl, purpose, Some(node_id))`.
  It leaves out keys that have expired or are scoped to other nodes (and
  audits the refusal). The belai agent should call it in place of
  `grant_for_purpose` in `kls-node/src/gateway.rs` and on `server.rs`
  replay. Env-var secrets are filed under provider `env`: add `"env"` to a
  dispatch's providers to hand them over.
- **New settings and readers.** Every key has a typed field in
  `audit::config::Effective`. They are marked pending (hourglass in
  Settings) until their consumer reads them:
  - Engine (dispatch payload): Crews (`belai.guardrails`, `belai.tests.*`,
    `belai.show_internal_work`, `belai.auto_commit_per_task`, `a2a.show`,
    `crews.lease`) and per-card budgets (`agents.*`, needs proto
    `Dispatch.budget`).
  - Engine and forge decision code: System One (`systemone.*`).
  - Engine `pick_node`: `hosts.run_on` and `hosts.run_on_fallback`.
  - Forge agent (`kls-github/src/budget.rs`): GitHub quota
    (`forge.quota.on_exhausted`, `forge.calls_per_hour`,
    `forge.quota.proactive_writes_per_hour`). Also please expose
    per-workflow GitHub calls (callers `factory:<workflowId>`) through a
    public fn so Budgets can show the breakdown.
- **Project overrides**: done (`kls_core::audit::overrides`, Settings > Project overrides, `project_prefs` dispatch, ack on `host.inventory`). The old request below is closed.
  These need a proto RPC and a kls-node handler (belai agent). Settings will
  add the section once the RPC exists.

### Forge agent's answers (M4, 2026-11-21)

1. Done: conflict commits pushed over git off GitHub are authored with
   `Effective::signing_email` when set (`PushPlan.author_forced`); GitHub's
   API commits are left as GitHub signs them.
2. Done: Delivery has Estimate (`estimates::for_range`, roles table, inputs
   and settings revision shown), Performance (`estimates::performance` by
   agent, node, model, repo, workflow) and Lifecycle tabs; Outcomes shows a
   "By hand" KPI and column priced per outcome through `estimates::estimate`
   (agent time as `sse`, decisions as `merge`). All export CSV.
3. Done: every automated forge write (and each poll pass, conflict commit)
   calls `control::check(core, "provision", target, origin)` first
   (`kls_github::forge::guard::check_kill`); a refusal also emits
   `forge.refused` and writes a `forge` ledger entry.

### From belai counterparts (WP-A/B/J)

- **Workforce live workers** (owner: `kls-core/src/vault.rs` `workers_summary`): read
  `core.node_statuses()` (`kls_core::belai::status`, latest `HostStatus` per node:
  workers, crew runs, spend) instead of guessing from `session.*` events. Worker spend is
  already charged as `usage_rows` id `worker:<node>:<worker>` (raised only, so repeats
  and reconnects charge once).
- **Needs you** (owner: `kls-core/src/needs.rs`): add open host attention requests
  (`core.store().attention("open", n)`, kinds `gh_login|provider_credentials|approve|action`)
  and structured asks (`session_extras.pending_ask`). Decide with
  `core.attention_decide(node, id, approve)` / `core.queue_ask_answer(..)`. Both are on the
  Fleet and Sessions screens today.
- **SQLite busy**: under load, writes that read then write in one transaction
  (`outbox_push`, the ledger append) fail with `database is locked (517)`. kls-node retries
  where it must (attention decisions, refused-dispatch audit); a store-level retry or
  `BEGIN IMMEDIATE` would fix it for everyone.

## Requests (update 2)

### To the engine and board agent

- **Library parity in Workforce (UI).** The core is done in
  `kls-core/src/vault.rs`:
  - `agent_to_library(core, agent_id, note)` backs a profile up to the
    library as a versioned `agent` item.
  - `install_agent_from_library(core, item_id, version)` installs or
    updates the profile, records `meta.installedFrom`, and emits an
    audited `library.installed`.
  - `library_agents(core)` lists library agents with `installed` and
    `installed_version`.

  Please add to `kls-ui/src/screens/workforce.rs`: "Back up to library" on
  an agent card, the "In the library" pane with Install / Update, the
  "Not in library" filter, and "Save to library" in the builder. The
  Library screen does not list `agent` items (`vault::AGENT_KIND` is kept
  out of `LIBRARY_KINDS`).
- **Cockpit "New workflow".** Accept `focus("new")` in
  `screens/workflows.rs` to open a new draft. The cockpit then links to it.
- **Run-on default and maintenance.** In `exec.rs::pick_node`, read
  `Effective.run_on` and `run_on_fallback`. Maintenance and blackout
  windows already hold work inside `control::check`; treat a `Blocked`
  with `window == true` as a wait.

### To the board / M2 owner (`kls-store/src/board.rs` MemoryNote)

- **Memory source host and A2A link (P2).** Add `host` and `a2a_ref` to
  `MemoryNote` from node sync. The Memory screen will then show them; it
  already groups scopes and pages 50 at a time.

## Update 3

- **Session fact index** (requested by the engine agent). This is the
  session-level counterpart of `RunDetail::facts`.
  - Fold: `kls_core::audit::facts`. It runs incrementally on every
    `session.*` event and backfills once at start. It reads the session row,
    the transcript (text and tool metadata) and the host audit lines, and
    keeps a cursor per session. Tables: `session_facts` and
    `session_fact_cursor` (migration 0015).
  - Fact kinds: repo, pr, card, agent, model, node, run, vuln, file.
  - Reader API:
    - `facts::session_facts(core, id)`
    - `facts::sessions_with(core, &[(kind, value)], limit)`
    - `facts::fact_values(core, kind, limit)`
    - `facts::parse_filters("repo:acme/api pr:acme/api#4")`
  - The Audit screen's Sessions tab filters by it (`repo:`, `pr:`, `vuln:`
    and so on, plus repository chips). **Request to the belai agent
    (Sessions screen):** show `session_facts` as chips in the session
    header, and accept the same `kind:value` filter words in the session
    list.
- **R4:** `keleustes://audit?node=<id>` opens the Audit screen filtered by
  that node (focus `node:<id>`, with a chip that clears it).
- **R6:** the daily retention run prunes `host_action_lines` older than 30
  days, and the `retention.pruned` event reports the count as
  `actionLines`.
- `audit::overrides::refused_under_the_kill_switch_and_without_the_host_gate`
  now counts refusals by exact kind and outcome from the event log. It had
  been failing because of an `events_query` prefix bug (since fixed) that
  dropped kinds containing `_`.

## Requests (WP3, 2026-11-22)

### To the owner of `kls-core/src/sweeps.rs` (timer)

1. **Done (integration, 2026-11-22):** `sweeps::delivery_tick` every `DELIVERY_SWEEP_EVERY` (5 min) from `sweeps::start`; test `sweeps::tests::delivery_tick_runs_both_sweeps_and_is_refused_under_the_kill_switch`. In `sweeps::start`, add a five-minute tick that calls
   `kls_core::github::lifecycle_sweep(&core, "sweep")` and
   `kls_core::github::rollup(&core, "sweep")` (log errors at warn). Both
   call `control::check(core, "schedule", ..)` first and return a
   `SweepReport` with `refused` set (nothing written) under the kill switch
   or a schedule blackout; every write is audited (`lifecycle` entries are
   deduplicated by ref, `delivery` entries carry the origin). Until then the
   Delivery > Lifecycle tab has Record and Link + roll up buttons (origin
   `owner`).

### To the owner of `kls-core/src/estimates.rs`

1. **Done (integration):** `classify_role` returns `classify_weighted(..).role`; `estimate_golden` stays green; test `estimates::tests::roles_from_agent_and_labels`. `classify_role` is a keyword cascade. The weighted rule table is now
   `kls_core::github::classify_weighted(agent, labels, title) -> RoleVerdict`
   (`ROLE_RULES`, `ROLE_MIN_SCORE`, `ROLE_MARGIN`, `ROLE_DEDICATED`, tie
   order), shown in Delivery > Estimate. Please make `classify_role` return
   `classify_weighted(..).role` (the golden `kls-core/testdata/estimate.golden.json`
   pins `estimate()` and is unaffected).

### To the governance agent (`kls-core/src/audit/config.rs`) — WP3 forge outputs

1. Remove from `config::PENDING`; each now has a reader in
   `kls-engine/src/forge.rs` or `kls-github/src/forge/guard.rs`:
   `forge.pr_state` (`draft_pr` default state), `forge.protected`
   (`guard::protection_on` / `protected_branch`: `draft_pr` refuses a
   protected head), `approve.sensitive_paths` (`approve_ready`), and
   `forge.branch_prefix` (now also names the factory branch in `draft_pr`).

## Requests (WP5, 2026-11-22)

1. **kls-app desktop notices (owner of `kls-app/src/desktop.rs`):** **Done (integration, 2026-11-22):** `event_note` notifies on `session.finished` with its headline and skips `info` outcomes (`session.ended` stays quiet); test `desktop::tests::event_notes`. `session.finished`
   now carries `{reason, outcome, severity, headline}` (`kls-core/src/sessions.rs`
   `session_end`), and `session.ended` carries `outcome`. A notice could use the
   headline ("Session timed out") and skip `info` outcomes such as `user_stop`.
2. **Teleport kill switch (owner of `kls-core/src/audit.rs`):** `Core::teleport_request`
   does not call `control::check`. The Sessions git strip goes through
   `Core::session_git`, which checks first and audits, but the Audit screen's
   Teleport button calls `teleport_request` directly.
3. **Engine catalogue (owner of `kls-engine/src/catalogue.rs`):** core now has
   `kls_core::vault::crew_problems` (enforced in `save_crew`), reading builtins from
   `kls-engine/src/builtins/profiles.json` with `include_str!`. The engine's
   `catalogue::crew_problems` stays for the UI's live hints; a shared copy could
   replace one of them.
4. **`hosts::library_dispatch` (owner of `kls-core/src/hosts.rs`):** WP5's publish path
   reuses it with origin `fleet/workforce`. No change needed, but a typed
   `Origin::workforce()` would make the surface explicit.

## Requests (WP2, 2026-11-22)

### To the governance agent (`kls-core/src/audit/config.rs`)

1. Wired, please drop from `config::PENDING`: `autonomy.max_attempts`
   (stop-the-line default, exec.rs), `schedules.tz` (cron zone default),
   `schedules.missed` (catch-up default), `systemone.retention` (decision
   rows pruned), `agents.*` (sent as `Dispatch.budget`), and the already
   wired `autonomy.claim_ttl_minutes`, `systemone.model`, `.fallback`,
   `.below_threshold`, `.on_abstain`.
2. **Done** (the `defaults.<key>.enabled` part: see WP1 item 2 below). Add settings (the engine reads them raw today): `systemone.daily_cap`
   (Int, default 500, "Decisions per day"), `forge.identity` (Str,
   comma-separated logins the factory acts as; "Factory identities"), and
   one `defaults.<key>.enabled` (Bool, default true) per default
   (`kls_engine::defaults::entries()`), or a list editor that writes them.

### To WP3 (forge, kls-github)

1. **Done (integration, 2026-11-22):** `kls_github::sync::{note_authored, factory_authored, Authored}`; the engine notes what `draft_pr`, `file_issues`, `repo_profile` and `issue_comment` open; pull and issue events carry `fromFactory` (labels added: whether the factory added them; else authorship) and `factoryAuthored`, comments `fromFactory`; tests `tests::pull_events_say_from_factory_*`, `tests::factory_authored_*`. Own-identity guard: set `fromFactory: true` on `github.pull.*`,
   `github.issue.*` and `github.issue.comment` events for pulls, issues and
   comments the factory itself authored (e.g. the PR `draft_pr` opened); the
   engine also skips bodies with `<!-- keleustes:` and authors in
   `forge.identity` / `keleustes[bot]` / `belai[bot]`.
2. **Done:** `authorAssociation` was already on the event; pinned by `tests::issue_opened_carries_author_association_and_factory_facts`. Put `authorAssociation` on `github.issue.opened` (the `issue-to-pr`
   default's team/outsider split; without it every issue waits for a person).
3. **Done:** `headSha` on every `github.pull.*` event (`store_pull`). `headSha` on pull events would let the run key skip a mirror read (it
   falls back to the mirrored pull's head).

## Requests (WP1, 2026-11-22)

Done for the requests above that name WP1: dropped from `config::PENDING`
every key WP2 and WP3 wired (`autonomy.max_attempts`, `schedules.tz`,
`schedules.missed`, `systemone.retention`, `agents.*`, `forge.pr_state`,
`forge.protected`, `approve.sensitive_paths` and the keys wired earlier);
added `systemone.daily_cap` and `forge.identity` to the settings document;
`Core::teleport_request` now calls `control::check` (refused and audited
under the kill switch).

1. **Anyone wiring a setting:** `config::PENDING` is now enforced by the
   test `pending_keys_have_no_reader_outside_config`
   (`kls-core/src/audit/config.rs`). It walks every crate's sources and fails
   when a listed key's literal or its `Effective` field is read outside
   `config.rs`. Still listed: `belai.mode`, `belai.ask_permission`,
   `autonomy.plan_approval`, `audit.calls`, `systemone.override` and the new
   `kanban.provider` / `.board` / `.sync` / `.lists`. When you wire one, ask
   WP1 (or the coordinator) to drop it in the same change.
2. **WP2 (`defaults.<key>.enabled`):** **Done (integration, 2026-11-22):** `config::register_extra(id, title, desc, fields)` (appends to a section id that exists; keys known are skipped; `config::sections()` replaces direct `SECTIONS` reads in core and the Settings screen); `kls_engine::defaults::register_settings()`, called by `start_with`, adds one Bool `defaults.<key>.enabled` (default on) per `entries()` to the Defaults section, saved and audited like every key; `pending_keys_have_no_reader_outside_config` stays green; tests `config::tests::registered_sections_join_the_document_once`, `defaults::tests::per_default_switches_are_settings_saved_and_audited`. Was: not added. `kls-core` cannot see
   `kls_engine::defaults::entries()`. Either register the default keys with
   core at start-up (a `config::register_extra(&[Field])` hook WP1 can
   add), or edit them from the Workflows screen.
3. **Owner of `kls-core/src/needs.rs`:** **Done (integration):** `CheckpointKind::QuotaEscalated` (`needs::quota_escalations`: newest per route while its reset window is open), opens Budgets > Usage (`cockpit::target`, a Usage button in Checkpoints); tests `needs::tests::quota_escalations_show_until_the_window_resets`, `cockpit::tests::quota_escalations_open_budget_usage`. `forge.quota.on_exhausted =
   escalate` emits `github.quota.escalated` (`kls_github::budget::kinds`,
   once per route and reset window; it already counts as an escalation for
   desktop and chat notices). It could appear in Needs you as a checkpoint
   that links to Budgets > Usage.
4. **Owner of `kls-app/src/desktop.rs`:** **Done (integration):** notices for `github.quota.escalated` (keleustes://budgets/usage), `trust.egress_reviewed` (Trust) and `vault.rotate_failed` (Vault), shown as `notify.events` allows; test `desktop::tests::event_notes`. route a `github.quota.escalated`
   notice to `Screen::Budgets` with focus `usage`, and `trust.egress_reviewed`
   / `vault.rotate_failed` to Trust / Vault.
5. **Coordinator (`kls-app/src/main.rs`):** no change is needed.
   `kls_core::audit::start` now also starts the chat-channel notifier
   (`audit::notify::start`) and, every 10 minutes, `audit::governance_tick`
   (vault grace sweep and auto-rotation, then last week's report).
6. **Migration:** `0211_wp1_vault_lineage.sql` (sqlite and postgres) adds
   `vault_secrets.version`, `rotate_every_days`, `models_json`, `remote_id`
   and the `vault_secret_versions` table.

## Requests (P1, 2026-11-22)

- **Call `Core::harness_lines` from `Core::record_audit`** (crates/kls-core/src/sessions.rs:756,
  owner of that file). Harness C8 rule 2: `harness.prompt.queued/typed/failed/refused` and
  `harness.stopped` events are built from belai's host audit lines by
  `kls_core::harness::line_event` (crates/kls-core/src/harness.rs:606) and emitted by
  `Core::harness_lines(node_id, session_id, &lines)` (:649). Pass only the lines the store took
  as new (for example, filter `lines` to `idx` above the stream's previous high-water mark before
  `append_audit_lines`), so a re-sent batch emits no duplicate events. The events add no ledger
  row; the folded host line stays the ledger entry. Test: harness.rs:1126.

## Requests (P4, 2026-11-22)

- **Owner of `crates/kls-node/src/library.rs` and `crates/kls-core/src/belai/library.rs` (library):** answer `Library.Fetch` for a `teleport_id` (no `dispatch_id`): allow it when the teleport is addressed to the calling node, is `ready` or `delivered` and not expired, and its stored needs name the ref. The needs are kept under the setting `teleport.needs.<teleport id>` as a JSON `Vec<LibraryRef>`; read them with `kls_node::teleport::stored_needs(core, id)` (or the same key from kls-core). Audit the fetch with `subject.teleport`. Until then the target installs through a `library_install` dispatch that P4 sends after the download (`crates/kls-node/src/teleport.rs` `install_needs`), so nothing is blocked.
- **Same owner, FYI:** `crates/kls-node/tests/belai.rs` `crew_round_trip_and_hook_files_confirmation` failed once in a full `cargo test -p kls-node` run and passed alone (2026-11-22); looks timing-dependent.

## Requests (P5, 2026-11-22)

From the library distribution and inventory package (P5).

1. **Host screen and Fleet Library row: the hook files step.** **Done (P6, 2026-11-22):** both use `kls-ui/src/screens/library.rs` `FilesConfirm` (host.rs, fleet.rs). `Library.Fetch`
   now answers a hook that carries bundle files only after
   `Core::confirm_bundle_files(node, item_id, version, origin)` recorded the
   owner's second confirmation of exactly those files for that node
   (`crates/kls-core/src/belai/library.rs`). `crates/kls-ui/src/screens/host.rs`
   (~:1990) and `crates/kls-ui/src/fleet.rs` (~:950) still send a hook after one
   "runs code" click, so a hook with files sent from there is refused on fetch
   (audited). Please add the files window there (list path, size and hash, then
   "Confirm files" calls `confirm_bundle_files` before `hosts::library_dispatch`),
   as `crates/kls-ui/src/screens/library.rs` `install_ui` does.
2. **Done (P6, 2026-11-22):** `DispatchSpec.budget_push` (kls-core gateway.rs), mapped in kls-node gateway.rs; producer `kls_core::belai::budget`. **Coordinator: `DispatchSpec.budget_push`.** `node.proto` has
   `Dispatch.budget_push` (53, `BudgetPush`) for kind `budget_update` (#33).
   `kls_core::gateway::DispatchSpec` has no field for it, so
   `crates/kls-node/src/gateway.rs` `dispatch_pb` sends `budget_push: None`
   (the one-line change made there). Adding the field to `DispatchSpec`
   (serde default) and mapping it lets the budget gates push limits, a
   downgrade or a pause to running work.
3. **Done (P6, 2026-11-22):** read in kls-node status.rs (HostStatus, called from server.rs) and sessions.rs (upsert); `kls_core::belai::plan`. **Coordinator: plan limits (row 48).** `HostStatus.plan_limits` (5) and
   `SessionUpsert.plan_limits` (30) exist on the wire. `kls-node/src/server.rs`
   (HostStatus) and `sessions.rs` (upsert) do not read them yet.
4. **Done (P6, 2026-11-22):** belai rc factorybudget.go, factoryknowledge.go, factoryinitdraft.go; advertised. **Belai, not P5's files:** the `budget_update` and `knowledge_reindex`
   dispatch kinds and the `knowledge_search` / `init_draft` job kinds need
   handlers in `belai/internal/rc/factory.go` (dispatch switch,
   `factoryDispatchKinds`, the advertised job kinds). The messages are in
   `node.proto` and the Go stubs are regenerated.

Changes P5 made outside its own files, for review: `crates/kls-node/src/gateway.rs`
(one field, `budget_push: None`); `crates/kls-node/tests/belai.rs` (a skill
fixture now carries a description, since saves are validated, and one new
test); `crates/kls-core/src/vault/workforce/tests.rs` (the reconcile test saves
real hook documents); `crates/kls-ui/Cargo.toml` (dev-dependency `tempfile`);
`belai/internal/rc/factory_test.go` (fake fetch returns bundle files);
`belai/internal/libinstall/crewdoc.go` (new: `Installer.SaveCrew`).
