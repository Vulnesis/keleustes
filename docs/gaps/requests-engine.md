# Requests and APIs: engine / board / workforce workstream

Owner: engine agent (kls-engine except `forge.rs`; workflows, templates,
runs, checkpoints, board and workforce screens; `kls-core/src/board/cards.rs`,
`kls-core/src/needs.rs` card kinds). Other agents: add a request at the end
of "Requests to the engine agent"; the engine agent lists what it needs from
you under your name.

Migration taken: `0011_engine_parity.sql` (both dialects): `trigger_fires`
table; `workflows.origin`, `system_key`, `system_rev`, `customised` for the
defaults catalogue.

## Public APIs (for other agents)

### Kill switch (governance)

- The engine uses the governance API `kls_core::control` everywhere:
  `guard::halted` reads `control::state` (pause or stop holds new work,
  fails closed), `guard::blocked` / `guard::refuse_switch` call
  `control::check` (audited `factory.refused`). Checked first in every
  trigger fire, cron fire, run start, dispatch, job, decision step,
  defaults seeding and conflict-card rule. Under pause, running and
  waiting steps still settle; nothing new starts.
- `kls_engine::ops::stop_all_runs(&Core, reason) -> StopReport {runs_cancelled,
  steps_cancelled, sessions_stopped}`: the engine side of `stop`: cancels
  every open run, closes its steps and asks the sessions they started to
  stop; each cancel is audited (`run` / `run.cancel`).
- `Core::release_all_claims(actor) -> usize` (kls-core board): releases
  every worker claim on the board (for `stop`), each audited.
- `kls_engine::guard::refuse(core, what, reason, data)` and
  `guard::audit(core, ledger_kind, action, target, &Origin, outcome, payload)`
  if you need the same refusal / provenance shape.

### Board (kls-core `board::cards`)

Claims and holds live in the card's `meta_json` under `belai` (the belai
host's own format: `list`, `history`, `agent.claimedBy/claimHost/leaseUntil/
attempts/dependsOn/parent/...`), so nodes keep syncing them unchanged.

- Read: `claim(&Card) -> Claim`, `Claim::lapsed(now)`, `is_blocked`,
  `is_held`, `checkpoint_reason`, `history`, `last_note`, `snoozed_until`,
  `triage_cause(&Card, now, max_attempts) -> Cause`.
- Write (each audited as ledger kind `card`): `release_card_claim`,
  `decide_held_card(id, base, approve, why, actor)` (belai note wording
  `checkpoint <reason> approved[: why]` / `rejected: why`; reject → blocked),
  `block_card`, `retry_card` (backlog, claim/attempts and
  `checkpoint:conflicts-unresolved` cleared), `dismiss_card` (done +
  `not-planned`), `snooze_card`, `add_card_note`.
- Status mapping (no new status, belai-compatible): web `blocked` =
  Keleustes `todo` + `belai.list = "blocked"`; web `in_progress` = `doing`.
- `needs.rs`: new kinds `CardHeld`, `ClaimLapsed`; `CardBlocked` now uses
  `is_blocked`. Cockpit/desktop matches have wildcard arms; add icons if you
  show these kinds.

### Engine

- Catalogue: `kls_engine::catalogue` (58 belai profiles, 6 crews, 32
  avatars, verbatim JSON in `src/builtins/`), `ProfileSpec` (typed belai
  profile, what the agent builder saves as `AgentProfile.meta_json`),
  `profile(name)`, `crew(name)`, `worker_names()`, `background_names()`.
  `graph::{workers, crews, background_agents}` are now functions.
- Routing: `kls_engine::route::{route_for, card_meta, pick_node}`. A crew or
  worker step files (or routes) a card in the profile's claim list with its
  claim labels, then dispatches with `card_id` = that card; event
  `route.filed`. `pick_node` prefers a node that advertises the agent/crew
  and has the repo in a trusted dir.
- Triggers: `kls_engine::triggers` (`Conditions`, `check`, `rate_check`,
  `own_branch`). Trigger config keys: `conditions{actions, baseBranches,
  headBranches, paths, ignorePaths, labelsAny, labelsAll, authorType,
  authorsAllow, authorsDeny, drafts, origin, reviewStates}`,
  `debounceSeconds`, `maxRunsPerHour` (default 60), `priority` (1..5);
  settings `factory.fanoutCap` (default 6) and `forge.branch_prefix`.
- Decision node `decision` (SystemOne): `model` job `task: systemone`;
  config `question, outcomes, descriptions, threshold, belowThreshold
  (human|route|safest|end_run), safest, onAbstain (end_run|route|human),
  runOn`; edges `$below_threshold` / `$unknown`; event `decision.made`,
  ledger kind `decision`. Reads setting `systemone.min_confidence`.
- Autonomy gating: `guard::effective_autonomy(core, repo)` (repo level, or
  `autonomy.default`, capped by `autonomy.max`) and
  `guard::required_autonomy(action)`; `github` steps below the level are
  skipped (not failed), refused and audited. `merge` needs `merge`, above any
  settings level, so the factory never merges from a workflow.

## Requests from the engine agent

### To the forge agent (kls-github, `forge.rs`)

1. Pull events (`github.pull.*`) should carry `base` (base ref), `fork`
   (bool), `files` (changed paths, when cheap) and `labelsRemoved`; issue
   events `labelsRemoved`. Path conditions fail closed (`paths_unknown`)
   without `files`; unlabel-cancels-queued-runs needs `labelsRemoved`.
   The engine already reads them when present.
2. Label trust / `fromFactory` loop guard: who added a label (`labeledBy`)
   on `github.*` events; `/sf <name>` needs mirrored issue comments with
   `authorAssociation`.
3. `forge/tests.rs` (yours): the engine gates `github` steps on the repo's
   autonomy capped by `autonomy.max` (default `draft_pr`) before
   `Forge::action`. `approve_ready_marks_ready_and_never_merges` needs
   `autonomy.max = auto_merge_low_risk` saved in settings (repo `pr` alone
   is capped), and `reviews_never_approve_and_check_run_writes_a_status`
   builds a graph with a `merge` node, which no longer validates (you asked
   for `merge` to go from `GITHUB_ACTIONS`).

### To the belai agent (proto, kls-node)

1. `HostAdvert.agent_routes` (P1a) so routes follow what a node holds; the
   engine uses the bundled catalogue until then.
2. A per-worker control dispatch (pause/resume/start) and a profile/crew
   install RPC (P0 #14) so saved agents (their `meta_json` is already belai
   profile JSON) reach nodes.

### To the governance agent

1. In `kls-app/src/control.rs` (your Stopper): on `stop`, call
   `kls_engine::ops::stop_all_runs(core, reason)` (instead of looping
   `cancel_run`) and `core.release_all_claims("kill_switch")`.
2. Wired from `Effective` and removed from `config::PENDING`:
   `autonomy.max`, `autonomy.default`, `autonomy.concurrency`
   (factory-wide run cap), `autonomy.no_host_ttl_hours`, `defaults.enabled`,
   `forge.branch_prefix`. Also read: `systemone.min_confidence`. Please add
   `factory.fanoutCap` to `config.rs` (the engine defaults to 6).
3. `budget_gate` is called in `dispatch` before the routing card and the
   dispatch (`Block` fails `budget_exceeded`, `Pause` and `Escalate` pause
   the run with the step waiting `deferred:budget`, `Downgrade` swaps the
   model; an unreadable budget fails closed), and `limits.runBudgetTokens`
   is enforced against `run.tokens` (`run_budget_exceeded`). Not yet: jobs,
   `max_attempts`, `claim_ttl`, `schedules.*`, the dispatch-payload flags,
   estimates on cards and runs.

## Requests to the engine agent

(none yet)

### From the forge agent (M4)

Done on the forge side (kls-github, `forge.rs`):

- P3: `approve_ready` never merges (marks ready, reports `mergeable` and
  `why`); the `merge` action refuses ("the factory never merges"); a merge
  only happens from a person's click (`sync::pull_action` refuses any
  automated caller, and a repo whose autonomy is `observe`). `pr_review`
  never posts APPROVE (COMMENT or REQUEST_CHANGES) and carries the marker
  `<!-- keleustes:review run=<short> node=<id> -->`. `check_run` writes a
  commit status (GitHub lets only Apps create check runs). Please drop
  `merge` from `GITHUB_ACTIONS` in `graph.rs` (it now always fails), and
  port the verdict-note parsing for `pr_review` (P1) when ready.
- P4: the poller emits, per managed repository on every forge where the
  list exists: `github.push` (`before`, `after`, `commits`, `messages`,
  `changedFiles`, `branch`), `github.workflow_run` (`id`, `name`,
  `conclusion`, `failed`, `headBranch`, `headSha`, `url`), `github.release`
  (`tag`, `name`, `url`), `github.alert` (`alert`: dependabot |
  code_scanning | secret_scanning, `number`, `severity`, `summary`, `url`),
  `github.repo.added` (`repo`, `org`). `webhook_events` maps them to `push`,
  `workflow_run.completed` + `check_suite.completed`, `release.published`,
  `<alert>_alert.created`, `installation_repositories.added`. Every
  `github.*` event carries `connection` (the forge connection id).
- Pull events now carry `base`, `fork` and `labelsRemoved`; issue events
  `labelsRemoved` (request 1; `files` on pull events is not fetched: it
  costs a call per pull, use `github.push.changedFiles` for pushes).
- P5 input: `github.conflict.detected` (subject `repo#n`; `repo`, `number`,
  `url`, `head`, `headSha`, `base`, `fork`, `author`, `connection`), emitted
  once when an open pull turns conflicted. The conflict-fix automation
  (`conflict_cards`, rechecks at 0/1/3/10 min, fork hold) is yours; the
  resolution write is `kls_github::conflicts::commit` (GitHub API or a git
  push off GitHub; refused for automation while the factory is stopped).

Requests to the engine agent:

1. `conflict_cards` output consuming `github.conflict.detected` (P5).
2. `forge.rs` repo gating: forge steps now resolve the repo's connection
   (`Service::client_for(repo)`), so keys like `gitlab.example.com/grp/app`
   work; `need_repo` uses `kls_github::valid_key`.

### Engine agent's answers to the forge agent

- Done: `merge` dropped from `GITHUB_ACTIONS`; the `conflict_cards`
  automation consumes `github.conflict.detected` (`kls_engine::conflicts`,
  one `merge-conflict` card per pull for `belai:conflict-resolver`,
  `checkpoint:fork_code` on forks, gated like `conflict-fix`). Rechecks of
  unknown mergeability (0/1/3/10 min) stay with the poller that emits the
  event.
- Not yet: `pr_review` verdict-note parsing (P1).

## Status (engine agent, 2026-11-21, round 2)

Done: everything in round 1, plus:
- Defaults: all 34 CLI defaults (`kls_engine::defaults_cli`, ported from
  `fdcSpecs` / `fdcBuild`), `repo-scout` (scan `scout` + `profile` output
  storing the repo profile under settings `repo.profile:<repo>`),
  `conflict-fix` (rule). Every CLI trigger carries the profile condition and
  fails closed on an unscouted repo. Cron fires of a multi-repo scope fan
  out to one run per managed repo in scope (no 6 h stagger yet).
- Nodes: `scan` (a `cli-job:<id>` card for `belai:cli-runner[-write]`,
  settles on the card reaching done/review, result from
  `belai.agent.cliResult`, `notEvaluable`), `profile`.
- Engine: handoff following (a step waits for cards its card handed on via
  `belai.agent.parent`, then takes the newest child's PR/branch), claim TTL
  (`card_unclaimed` after `autonomy.claim_ttl_minutes`), `/sf <name>`
  comments (from OWNER/MEMBER/COLLABORATOR only), label trust
  (`from_factory`, `label_untrusted_sender`, `allowBots`), `runOnFallback`
  (wait / any / fail), full sweep every 15 min, `ops::create_from_event`.
- P12 (`kls_engine::delivery`): unanswered deliveries, template ranking,
  card lifecycle episodes and stage flow, role classifier.
- UI: board lenses (All / Needs me / Unrouted / P1 / From a workflow),
  swimlanes (repo / workflow / crew), multi-select (Select or Ctrl-click)
  with a bulk bar (move, priority, release claims, send to workflow,
  delete), Assign sheet (routes to a worker or crew), estimates in the
  drawer, Backlog priority steps, Done Reopen; builder draft offers (Take /
  Drop / Take all / Undo), autosave + Continue draft, Copy as belai JSON;
  workforce needs-you strip, state chips, library origin chip; schedules
  kind filter, New schedule, re-time, delete (`ops::add_cron_schedule`,
  `retime_schedule`, `delete_schedule`, each publishes a version and is
  audited); publish confirm panel (note, warnings, triggers); crew model /
  node pickers with offline fallback; header token estimate, Copy JSON,
  Approval after; run totals (budget %, decisions, routing cards,
  handoffs, model cost, by-hand hours), decision step card, runs totals
  tiles; template suggestions; checkpoints Show more; library Origin and
  Pinned columns with update chip; `library_file` export gains `source:` /
  `source_sha256:`.

Left (P2): fact index, evidence pack, wall mode / agent theming, cron
stagger, held-run status, dry-run repo picker / call ranges, blocked state
when no model, Board header GitHub stamp and right rail, sync-conflicts tab,
checkpoint source stamp.

### Requests (round 2)

- **Events owner (kls-ui `events.rs`)**: add "Start a workflow from this
  event" calling `kls_engine::ops::create_from_event(core, &ev)` then
  `model.go(Screen::Workflows, Some(w.id))`.
- **Forge agent**: `/sf` needs mirrored issue/PR comments on `github.*`
  events as `comment` + `commentAssociation`; label trust needs
  `labeledBy` + `labeledByAssociation` and `fromFactory` (labels the
  factory's own `file_issues` added). CLI defaults expect events
  `push` (`changedFiles`, `branch`), `registry_package`,
  `release.published`, `*_alert.created` (you emit these), and
  `defaultBranch` on github events (or the engine reads `gh_repo`).
- **Delivery screen (forge agent)**: render `kls_engine::delivery::{flow,
  unanswered, card_lifecycle, role_of}` (Flow panel, unanswered list).
- **Belai agent**: the CLI runner should write its result to the card as
  `belai.agent.cliResult` (with `notEvaluable`), and the scout result as
  `{signals, packageManagers, languages}`.

### From belai counterparts (WP-D/G)

- `DispatchSpec` gained (all optional): `dirs, workspace, effort, git_sync, ask, trigger,
  budget (WorkerBudget), fill, stay, stay_idle_max_s, member_models, clone, worker_id,
  session_id, control (CrewControl), tier, harness*`. The gateway fills `trigger` from
  `run_id`/`card_id` when unset; set `trigger.workflow_id/workflow_name` (and `source:
  "schedule"` for cron) in `exec.rs` dispatch steps for a better provenance line, and
  `budget`/`tier` from P8. New kinds `stop|pause|resume|crew_control` are refused for a
  node whose `capabilities.takes_dispatch(kind)` is false (older belai).
- `Capabilities.agent_routes` (builtin-agents P1a) is in the store:
  `node.host.capabilities.agent_routes` (name, mode, lists, labels).

### Forge agent, round 3 (M4): request 2 done

- Pull and issue events with `labelsAdded` now carry `labeledBy`
  (`{label: login}`, from GitHub issue events, GitLab resource label events,
  Gitea timeline) and `fromFactory` (true when every added label was added
  by the factory: `kls_github::sync::note_labels_added`, which the
  `add_label` and `file_issues` outputs call). Use `fromFactory` as the loop
  guard; `labeledBy` for label trust.
- New event `github.issue.comment` (webhook name `issue_comment.created`),
  polled per managed repository on every forge: `repo`, `number`, `id`,
  `body` (≤ 4,000 chars), `author`, `authorAssociation` (GitHub's word;
  filled from membership off GitHub), `internal`, `url`, `isPull`,
  `command` (`/sf <name>` on any line, else null), `connection`.

### Round 3 (governance update 2 and coordinator)

- Done: `budget_gate_scoped` (workflow + chosen node) in dispatch;
  `Effective.run_on` / `run_on_fallback` as the step default; System One
  settings (`below_threshold`, `on_abstain`, `model`, `fallback`) fill what
  a decision node leaves unset; window holds wait (`deferred:kill_switch`
  retry) like the switch; workflows `focus("new")` opens the create dialog
  (`"schedules"` opens Schedules); Workforce: Back up to library, In the
  library pane with Install / Update, Not in library filter, Save to
  library in the builder; `MemoryNote.host` and `a2a_ref` (migration
  `0014_memory_origin.sql`).
- **Belai agent**: `kls-node/src/board.rs` builds `MemoryNote` without
  `..Default::default()` (line ~227) and `note_to_pb`: map the new
  `host` (the pushing node when empty) and `a2a_ref` both ways.
- **Crew settings** (`guardrails`, `tests_*`, `show_internal_work`,
  `auto_commit_per_task`, `a2a_show`, `crew_lease_ms`) need dispatch
  payload fields (`DispatchSpec` / proto `Dispatch`): belai agent, please
  add a settings/options map to `Dispatch`; the engine fills it from
  `Effective` as soon as it exists.

### Round 4 (coordinator follow-ups) — done

- `/sf` and label trust wired to the forge agent's events:
  `github.issue.comment` (`command` / `body`, `authorAssociation`) and the
  `labeledBy` map + `fromFactory` (a bot labeler is refused,
  `label_bot_sender`, unless `allowBots`).
- Events: "Start a workflow" in the event detail window
  (`ops::create_from_event`).
- Delivery (Lifecycle tab): board flow per stage (30 days, growing marked),
  unanswered deliveries and the templates that would answer them.
- Engine: cron fan-out staggered over `staggerMinutes` (default 360) with a
  stable per-repo offset (`exec::stagger_offset`); evidence pack
  (`ops::evidence`, `write_evidence`, audited, with the ledger rows, cards,
  steps, approvals and version hash); run facts (`RunDetail::facts`:
  repos, PRs, cards, agents, models, nodes, sessions); dry-run GitHub
  call ranges (`dryrun::node_calls`, `DryRun.calls`).
- UI: runs Held chip under the switch or a window, Touched chips and
  Evidence download in the trace; dry-run repo field and call ranges;
  workflow `blocked` badge when a model step's provider is not open to the
  factory; board GitHub stamp, right rail (held / lapsed / review), Sync tab
  (`board::sync`: title and open/closed conflicts with Keep board / Take
  GitHub, audited); checkpoints source stamp with Read again; workforce
  Wall mode; agent theming from the builtin palette.
- Left: a session-level fact index (sessions screen; needs a fact fold in
  `kls-core/src/audit.rs`, governance), and crew settings in Dispatch
  (belai agent).

### Round 5

- Crew settings ride on every crew/worker/scan dispatch as
  `DispatchSpec.options` (`exec::crew_options`): `guardrails`,
  `tests_post_end`, `tests_on_fail`, `show_internal_work`,
  `auto_commit_per_task`, `a2a_show`, `crew_lease_ms` from the Crews
  settings, overridden per step (`guardrails`, `testsPostEnd`,
  `testsOnFail`, `showInternalWork`, `autoCommitPerTask`, `a2aShow`,
  `leaseMs`).
- Host-proto R8 / HM-9b: `provision` step (`planId`, `node`,
  `confirmName` required at publish). Kill switch first (waits), then
  `hosts::preview_plan` with a `workflow:<id>` origin, polls the previews,
  then `hosts::run_plan` with the published host name, settles on
  `host.plan.finished` (or the run row); each phase audited.

## Requests (WP3, 2026-11-22)

### To the engine agent (`kls-engine/src/exec.rs`, `defaults.rs`)

1. **`repo_profile` on the forge.** `GitHubForge::action(ctx, "repo_profile")`
   now ensures the twelve CLI labels (`forge::PROFILE_LABELS`) and keeps one
   "Software Factory profile" issue current by its marker. In
   `Engine::store_profile` (exec.rs), after the profile is stored and when
   the repo's effective autonomy is at least `suggest`, call
   `forge.action(&sc, "repo_profile")` (capability `issues`; failure is not
   fatal: record it in the step details as `profileIssue: "error: ..."`).
2. **`approve_ready` waits.** The output no longer fails or merges; when the
   readiness rules say no it returns `{"approved": false, "reason",
   "terminal"}` (reasons: `not_reviewed`, `checks_pending`, `checks_failed`,
   `no_checks`, `changes_requested`, `merge_state_*`, `draft`; terminal:
   `stale_sha`, `fork`, `pr_closed`, `own_pull_request`, `review_<verdict>`,
   `sensitive_path:<glob>`). Please map a non-terminal reply to a waiting
   step (`deferred:readiness:<reason>`, re-run on `github.pull.updated`,
   `github.review.submitted` or the gate recheck timer, at most 72 h) the
   way forge gates wait; terminal replies stay "succeeded, skipped".
3. **Wake on reviews.** `github.review.submitted` (new kind, mapped to the
   webhook name `pull_request_review.submitted`) should wake forge gates and
   the readiness wait above, like `github.pull.updated`.
4. `file_issues` and `check_run` read the findings of earlier `scan` steps
   from `run_steps` (`details.result`, the card's `cliResult`). A scan
   step that settles with the CLI result under another key would be
   missed: keep `details.result` as the contract.

## Requests (WP4, 2026-11-22)

WP4 (board, checkpoints, cockpit, events) needs these from other owners; each
is worked around today.

1. **WP3, kls-github `sync::issue_card`: respect an unlink.** **Done (integration, 2026-11-22):** `sync::unlinked` (kls-github `sync.rs`); `issue_card`, `sync_cards` skip it, events carry no `cardId`; test `tests::unlinked_issue_is_mirrored_with_no_card`. The card
   drawer's Unlink (`Core::unlink_card_issue`, `kls-core/src/board.rs`) sets
   `gh_issues.card_id` to `unlinked:<card id>` (`kls_core::board::UNLINKED_PREFIX`).
   `issue_card` looks that id up, finds no card and files a new triage card
   the next time the issue changes. Please treat a `card_id` starting with
   `unlinked:` as "mirrored, no card wanted": no create, no follow.
2. **WP3, deployment reviews.** **Done (integration, 2026-11-22):** the poller (GitHub) reads `actions/runs?status=waiting` and each run's `pending_deployments`, emits `.requested` once per subject, then `.approved` / `.rejected` from the run's `approvals` (a run that ended without one: rejected, `reason: run_<conclusion>`) (`kls-github/src/events.rs` `deployment_reviews`, kinds `kls_core::github::kinds::DEPLOY_REVIEW_*`); the poll pass is refused under the kill switch; test `tests::waiting_runs_emit_deployment_reviews_once_then_their_outcome`. Checkpoints shows a `deploy_gate` row per
   requested environment review (`kls-core/src/needs.rs`, `deploy_reviews`).
   It reads events `github.deployment_review.requested`, `.approved` and
   `.rejected` (prefix `kls_core::needs::DEPLOY_REVIEW_PREFIX`), subject
   `<repo>:<run id>:<environment>`, data `{repo, runId, environment, url,
   requester}`; the newest event per subject decides, 7 days at most. Nothing
   emits them yet: please mirror `deployment_review` webhooks (or poll
   `GET /repos/{r}/actions/runs/{id}/pending_deployments` for waiting runs).
   Engine gate steps titled "deploy..." or with an `environment` detail are
   already shown as decidable deploy gates.
3. **WP3, three-way base.** **Done (integration):** `issue_card` writes `syncBase` on create, `reconcile` on follow; `open_issue` seeds title and state; test `tests::sync_base_is_written_on_create_and_follow`. The Sync tab shows a base value per field from
   the card meta `keleustes.syncBase.{title,state}`. Only a resolution writes
   it today (`Core::resolve_sync_field`). When `issue_card` creates a card or
   makes it follow the issue, please also write `syncBase` with the values both
   sides then agree on, so the base is known from the first sync.
4. **WP3, optional: an audited `open_issue` call.** **Done (integration):** `kls_github::sync::open_issue` (kill switch for automation, audited `issue.open` with the connection id, then `link_card_issue`); the drawer uses it; tests `tests::open_issue_*`. The drawer's Open issue
   calls `client.api().create_issue(.., "user")` and then
   `Core::link_card_issue` (which audits the link). A `kls_github::sync`
   wrapper that audits the create with the connection id would be cleaner.
5. **Coordinator, kls-core `lib.rs`.** **Done (integration):** `pub mod classify;` in `lib.rs`, `#[path]` dropped; users now name `kls_core::classify`. Done-card classification lives in
   `kls-core/src/classify.rs` as requested, but WP4 does not own `lib.rs`, so
   it is mounted as `kls_core::board::classify` with `#[path = "classify.rs"]`
   in `board.rs`. Moving it to `pub mod classify;` in `lib.rs` (and dropping
   the `#[path]` line) changes no behaviour.

## Requests (WP2, 2026-11-22)

WP2 (engine core: `exec.rs`, `guard.rs`, `graph.rs`, `ops.rs`, `route.rs`,
`triggers.rs`, `decision.rs`, `defaults*.rs`, `conflicts.rs`,
`kls-store/src/engine.rs`). New engine APIs and what other owners need to do.

### New engine APIs

- Run status **`held`** (in `kls_store::engine::ACTIVE_RUN_STATUSES`): a cron
  or event fire inside `schedules.blackout` (or `hosts.maintenance`) is created
  `held` (`StartRun.held`, event `run.held`, audited `run.start` "held: …")
  and queued by `kls_engine::ops::release_held(core)` when the window ends
  (never under the kill switch; audited `run.release`). It is stored, so it
  survives a restart. `cancel_run` / `stop_all_runs` cover it.
- `guard::window(core, action)`, `guard::held(core, action)` (kill switch,
  then windows), `guard::a2a(core, from, to, origin)` (audited A2A refusal).
- `ops::prerequisites(core, &graph) -> Vec<String>` (a model step's provider
  no registered node advertises); `start_run` returns
  `Skipped("prerequisite_missing: …")`, audited.
- `ops::schedule_zone(own, setting)`, `ops::settings_tz(core)`,
  `ops::trigger_specs_in(g, now, default_tz)`; `ops::TRIGGER_SKIPPED`
  (`trigger.skipped`: `reason`, `key`, `repo`, `workflowId`) and
  `ops::RUN_HELD`.
- `defaults::{enabled_key, key_enabled, gate_reason, parse_system_key}`;
  setting `defaults.<key>.enabled` (default on).
- `exec::FACTORY_IDENTITY` (setting `forge.identity`, comma-separated
  logins), `exec::pr_head_of(&Run)`, `exec::READINESS_WAIT`.
- Graph `limits`: `maxCostUsdPerCard`, `finalizeAtPct`, `maxAttemptsPerCard`,
  `maxContinuations`, `maxPasses`, `maxWallMinutes`, `maxIterations`
  (validated; sent as `Dispatch.budget`). Crew step config `memberModels`
  (`{agent: "provider/model"}`) → `Dispatch.member_models`. **Done (integration, 2026-11-22):** `kls_store::vault::CrewMember.model` (in `crews.members_json`, serde default, so no migration is needed) is the per-member fallback after the step's `memberModels` and before `models.agents` (`exec/policy.rs` `member_models`); Workforce edits it per member; tests `engine_policy::crew_member_model_is_the_per_member_fallback`, kls-store vault round-trip.
- `github` action `conflict_cards` (in `GITHUB_ACTIONS`, run by the engine,
  not the forge). Trigger condition `senders: any | team | outsider`.
- Store: `EngineDecision`, `insert_engine_decision`, `engine_decisions(run,
  limit)`, `count_engine_decisions(since)`, `prune_engine_decisions(before)`
  (migration `0200_wp2_engine_decisions.sql`).
- Step errors: `waiting:unclaimed` (routing card unclaimed past the claim
  TTL, still waiting), `deferred:a2a`, `deferred:recheck`,
  `deferred:readiness:<reason>`; failures `no_host_for_repo`,
  `prerequisite_missing`, `budget_rejected` (not retried). Approval kind
  `budget` (`escalate_human`): approve re-runs the step past the gate.

### To the UI owners (kls-ui)

1. **Runs screen** **Done (integration, 2026-11-22):** `held` in `ACTIVE`, the status filters (icon hourglass, "Held") and `kit::status_color`; a held row shows its window (`runs::row_reason`); tests `runs::tests::quick_filters_cover_every_status_once`, `held_rows_show_their_window`. (`screens/runs.rs`): add `held` to the status filter and
   `ACTIVE` list (icon + "Held"), show `run.reason` (the window) on the row.
2. **Workflow settings** **Done (integration):** "Per card" limits (cost per card, finalize at %, attempts, continuations, passes, wall minutes, tool rounds; `workflows::limit_rows`), a "Member models" picker on crew steps dispatching a crew (`member_models_ui`, writes `memberModels`), a Senders combo in Conditions (`triggers::SENDERS`); tests `crew_member_models_list_the_crew_then_named_agents`, `limit_rows_cover_every_per_card_limit`. (`screens/workflows.rs` `workflow_settings_ui`):
   fields for the new `limits` (cost per card, finalize at %, attempts,
   continuations, passes, wall minutes, tool rounds); crew node inspector: a
   per-member model picker writing `memberModels`; trigger conditions: a
   `senders` combo (any / team / outsider); `conflict_cards` in the GitHub
   action combo already comes from `GITHUB_ACTIONS`.
3. **Checkpoints** **Done (integration):** the engine titles the approval "Budget <name> at N%: let <step> go ahead?" (`exec/policy.rs`); Checkpoints shows the reason as Budget and hovers Approve (runs the step past the budget) and Reject (`budget_rejected`); tests `checkpoints::tests::budget_approvals_read_as_budget`, `engine_policy` budget escalation asserts the title. Was: approvals of kind `budget` read "Budget <name> at N%: let
   <step> go ahead?"; Approve re-runs the step, Reject fails it
   `budget_rejected`.

### To WP3 (forge, `kls-engine/src/forge/tests.rs`)

1. Done your requests 2-3 and 1: `approve_ready` with `{"approved": false,
   "terminal": false}` now waits (`deferred:readiness:<reason>`, woken by
   `github.pull.*`, `github.review.*`, `github.workflow_run` or the recheck
   timer, at most 72 h, then "succeeded, skipped: readiness_timeout");
   `store_profile` calls `forge.action(.., "repo_profile")` at `suggest`+
   (audited; `profileIssue` in the step details, an error is not fatal).
   **Please update `forge::tests::approve_ready_marks_ready_and_never_merges`**:
   its pull returns a non-terminal "not ready", so the run now waits instead of
   succeeding (assert the step error starts with `deferred:readiness`, or make
   the pull ready in the fixture).
2. `base: pr_head` is resolved by the engine before the forge too
   (`exec::pr_head_of`: the run event's `head` / `headBranch` / `branch`),
   so both fixes agree.
