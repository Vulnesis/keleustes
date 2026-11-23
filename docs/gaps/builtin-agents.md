# Gap: built-in agents and automatic behaviour

Audit date: 2026-11-21. Read-only audit of the web Software Factory (vdb-site API, website, Prisma
models) against Keleustes. Every path below is relative to `/home/chris/GitHub/Vulnesis/`.

Abbreviations: `H/` = `vdb-site/api/internal/handler/`, `D/` = `vdb-site/docs/`, `W/` =
`website/.repo/`, `B/` = `belai/internal/agentprofile/`, `K/` = `keleustes/crates/`.

## How it runs in Keleustes

Keleustes never calls a model. Everything below runs in one of three ways:

| Route | What it is | Exists today |
|---|---|---|
| **Rule** | Pure Rust in `kls-engine` / `kls-core` / `kls-github`, no model | yes |
| **Worker card** | A board card routed to a belai worker profile (the profile's claim list, labels and assignee), plus a `Dispatch{kind:"worker"\|"crew", agent}` so the node starts the worker. The worker claims the card through the synced board (belai `internal/factorylink/kanban.go`). This is how the web factory runs **every** built-in profile (`H/factory_engine_route.go:54-166`, `H/factory_engine_node_crew.go:69-175`) | dispatch yes, **routing card no** |
| **Model job** | `Job{kind:"model", payload:{"task": ...}}` on a node. belai runs `draft`, `avatar`, `classify` (the *security admission* classifier, not a card classifier) and `systemone` (`belai/internal/rc/factoryjobs.go:31-34,435-460`; handlers in `belai/cmd/belai/factoryjobs.go:148-420`) | `draft` only (`K/kls-core/src/vault.rs:1089-1150`) |

Two facts drive most of the plan:

1. **belai ignores `Dispatch.card_id` for workers.** `belai/internal/rc/factory.go:209-219` starts
   workers for a profile or crew, and they claim any card that matches their `kanban.lists`, `labels`
   and assignee. Keleustes' crew step (`K/kls-engine/src/exec.rs:1526-1580`) sends a dispatch but files
   no card. A builtin worker it starts therefore has nothing to claim, unless the run began from a card
   that happens to carry the right labels.
2. **`HostAdvert.agents` carries names only** (`keleustes/proto/keleustes/node/v1/node.proto:163-165`,
   belai `internal/factorylink/host.go:26-61`). The web factory reads each profile's lists and labels
   from what the host advertised (`BelaiHost.rcProfiles`, `H/factory_engine_route.go:1-50`). Keleustes
   needs the same data, or a copy of belai's table.

Findings outside parity that matter:

- **Keleustes merges, and the web factory never does.** `approve_ready` squash-merges unless the node
  says `merge:false` (`K/kls-engine/src/forge.rs:284-310`), and there is a separate `merge` action
  (`forge.rs:321-335`, listed in `GITHUB_ACTIONS` at `K/kls-engine/src/graph.rs:584-593`). The web
  rule is "The factory never merges" (`D/factory.md` section 13, `H/factory_engine_approve.go:1-20`).
- `pr_review` posts a fixed message with an event the node sets, and `APPROVE` is allowed
  (`forge.rs:352-371`). The web factory parses the `belai:pr-reviewer` verdict note and never approves
  from this step (`H/factory_engine_review.go:73-396`).
- The `nightly-regression` template's `job` step uses the job kind `tests.run`
  (`K/kls-engine/src/templates.json`). belai refuses that kind: it runs only `github`, `model` and
  `teleport` jobs (`belai/internal/rc/factoryjobs.go:39-45,133`).
- `BACKGROUND_AGENTS` (`K/kls-engine/src/graph.rs:599-610`) lists `deps-npm`, which does not exist
  (belai's profile is `deps-javascript`). It also leaves out `deps-apple`, `deps-ci`,
  `deps-containers`, `deps-dotnet`, `deps-javascript`, `deps-jvm`, `deps-other`, `deps-php` and
  `deps-ruby`. `WORKERS` (`graph.rs:613-626`) has 12 of belai's 40 workers. `CREWS`
  (`graph.rs:627-633`) has no `belai:maintenance`.

## 1. Built-in agent profiles (belai, 58) and crews (6)

The profiles live in belai and run there, so Keleustes does not copy their prompts to run them. It
needs their names and routing data (lists, labels, the on-success move, read-only). The web factory
keeps its list of dispatchable workers in `H/factory_templates.go:60-80` (`factoryWorkerProfiles`, 40
names), and `H/factory_palette_workers_test.go` checks that list against belai.

Each profile's system prompt is a single JSON line, so the line numbers below are exact (a JSON string
cannot span lines). Where a profile has an `identity` line, it comes just before the prompt. The builtin
avatars are `B/builtin/avatars/*.svg`, listed under `avatars` in `B/builtins-index.json:359` onward.

| Profile | Mode | Claims: list / labels → on success | RO | System prompt (file:line, under `B/`) | Web use (default / template / label) | Keleustes |
|---|---|---|---|---|---|---|
| `belai:ai-capability-reviewer` | worker | backlog / sf:ai-review → review +ticket | yes | `builtin/ai-capability-reviewer.json` L5 (identity L4) | tpl ai-capability-review (firewall source) | missing: not in WORKERS, no routing card |
| `belai:alert-triager` | worker | backlog / sf:alert-triage → review +ticket | yes | `builtin/alert-triager.json` L5 (identity L4) | tpl dependabot-alert-triage, code-scanning-triage | missing: not in WORKERS, no routing card |
| `belai:triage-vulns` | single | - |  | `builtin/belai-triage-vulns.json` L12 | - | n/a (background; never dispatched) |
| `belai:builder` | worker | backlog / build → review +needs-review |  | `builtin/builder.json` L13 (identity L12) | crews bug/feature/delivery/ui; default issue-to-pr; sf:write-tests, sf:backport, sf:docs | partial: in WORKERS, no routing card |
| `belai:ci-explainer` | worker | backlog / sf:explain-failure → done | yes | `builtin/ci-explainer.json` L5 (identity L4) | tpl failure-explainer, deploy-failed-card | missing: not in WORKERS, no routing card |
| `belai:cli-runner` | worker | backlog / cli-job:<id> → done |  | `builtin/cli-runner.json` L49 (identity L4) | default repo-scout + every CLI scan stage (31 read-only jobs); sf:scout | missing: not in WORKERS, no routing card |
| `belai:cli-runner-write` | worker | backlog / cli-job:<id> → review +needs-review |  | `builtin/cli-runner-write.json` L43 (identity L4) | default cli-sca-fix and every mutating CLI job | missing: not in WORKERS, no routing card |
| `belai:conflict-resolver` | worker | backlog / merge-conflict → review +needs-review |  | `builtin/conflict-resolver.json` L5 (identity L4) | default conflict-fix; POST /checkpoints/conflicts; sf:rebase | missing: not in WORKERS, no routing card |
| `belai:container-fixer` | worker | backlog / sf:fix-container → review +needs-review |  | `builtin/container-fixer.json` L5 (identity L4) | default cli-containers-fix | missing: not in WORKERS, no routing card |
| `belai:debug` | single | - |  | `builtin/debug.json` L12 | - | n/a (background; never dispatched) |
| `belai:defend-writer` | worker | backlog / sf:no-fix → review +needs-review |  | `builtin/defend-writer.json` L5 (identity L4) | default cli-defend (sf:no-fix) | missing: not in WORKERS, no routing card |
| `belai:deps-apple` | single | - |  | `builtin/deps-apple.json` L4 | - | n/a (background; never dispatched) |
| `belai:deps-ci` | single | - |  | `builtin/deps-ci.json` L4 | - | n/a (background; never dispatched) |
| `belai:deps-containers` | single | - |  | `builtin/deps-containers.json` L4 | - | n/a (background; never dispatched) |
| `belai:dep-scope` | worker | backlog / sf:dep-scope → review +needs-review |  | `builtin/dep-scope.json` L5 (identity L4) | tpl dependency-scope-fix | missing: not in WORKERS, no routing card |
| `belai:deps-dotnet` | single | - |  | `builtin/deps-dotnet.json` L4 | - | n/a (background; never dispatched) |
| `belai:deps-go` | single | - |  | `builtin/deps-go.json` L4 | - | n/a (background; never dispatched) |
| `belai:deps-javascript` | single | - |  | `builtin/deps-javascript.json` L4 | - | n/a (background; never dispatched) |
| `belai:deps-jvm` | single | - |  | `builtin/deps-jvm.json` L4 | - | n/a (background; never dispatched) |
| `belai:deps-other` | single | - |  | `builtin/deps-other.json` L4 | - | n/a (background; never dispatched) |
| `belai:deps-php` | single | - |  | `builtin/deps-php.json` L4 | - | n/a (background; never dispatched) |
| `belai:deps-python` | single | - |  | `builtin/deps-python.json` L4 | - | n/a (background; never dispatched) |
| `belai:deps-ruby` | single | - |  | `builtin/deps-ruby.json` L4 | - | n/a (background; never dispatched) |
| `belai:deps-rust` | single | - |  | `builtin/deps-rust.json` L4 | - | n/a (background; never dispatched) |
| `belai:dep-strategist` | worker | backlog / sf:dep-check → review +needs-review |  | `builtin/dep-strategist.json` L5 (identity L4) | tpl dependabot-strategy-check; sf:dep-check | missing: not in WORKERS, no routing card |
| `belai:designer` | worker | backlog / ui-design → done | yes | `builtin/designer.json` L13 (identity L12) | crew ui; tpl ui-polish-pass | partial: in WORKERS, no routing card |
| `belai:egress-reviewer` | worker | backlog / sf:egress-review → review +ticket | yes | `builtin/egress-reviewer.json` L5 (identity L4) | tpl egress-decision (sandbox source) | missing: not in WORKERS, no routing card |
| `belai:fanout` | single | - |  | `builtin/fanout.json` L12 | - | n/a (background; never dispatched) |
| `belai:flaky-hunter` | worker | backlog / sf:flaky → review +ticket | yes | `builtin/flaky-hunter.json` L5 (identity L4) | tpl flaky-test-quarantine; sf:quarantine | missing: not in WORKERS, no routing card |
| `belai:github-investigator` | worker | backlog / sf:github-investigate → review +ticket | yes | `builtin/github-investigator.json` L5 (identity L4) | tpl github-risk-investigation | missing: not in WORKERS, no routing card |
| `belai:guardian-aws-investigator` | worker | backlog / sf:guardian-aws → review +ticket | yes | `builtin/guardian-aws-investigator.json` L5 (identity L4) | Guardian tpls (siem source) | missing: not in WORKERS, no routing card |
| `belai:guardian-cloudflare-investigator` | worker | backlog / sf:guardian-cloudflare → review +ticket | yes | `builtin/guardian-cloudflare-investigator.json` L5 (identity L4) | Guardian tpls (siem source) | missing: not in WORKERS, no routing card |
| `belai:guardian-remediator` | worker | backlog / sf:guardian-remediate → review +needs-review |  | `builtin/guardian-remediator.json` L5 (identity L4) | Guardian tpls (siem source) | missing: not in WORKERS, no routing card |
| `belai:iac-fixer` | worker | backlog / sf:fix-iac → review +needs-review |  | `builtin/iac-fixer.json` L5 (identity L4) | default cli-iac-fix | missing: not in WORKERS, no routing card |
| `belai:license-fixer` | worker | backlog / license → review +needs-review |  | `builtin/license-fixer.json` L5 (identity L4) | default cli-license | missing: not in WORKERS, no routing card |
| `belai:lint-fixer` | worker | backlog / lint-drift → review +needs-review |  | `builtin/lint-fixer.json` L5 (identity L4) | crew maintenance; default lint-sweep | partial: in WORKERS, no routing card |
| `belai:package-reviewer` | worker | backlog / sf:package-review → review +ticket | yes | `builtin/package-reviewer.json` L5 (identity L4) | tpl new-package-review (firewall source) | missing: not in WORKERS, no routing card |
| `belai:patcher` | worker | backlog / vuln → review +needs-verify |  | `builtin/patcher.json` L13 (identity L12) | crew security; default cli-sast-fix | partial: in WORKERS, no routing card |
| `belai:plan-handoff` | single | - |  | `builtin/plan-handoff.json` L12 | - | n/a (background; never dispatched) |
| `belai:planner` | worker | backlog / plan → done | yes | `builtin/planner.json` L13 (identity L12) | crew feature; default issue-to-pr; sf:plan, sf:split, sf:split-pr | partial: in WORKERS, no routing card |
| `belai:pqc-fixer` | worker | backlog / sf:pqc → review +needs-review |  | `builtin/pqc-fixer.json` L5 (identity L4) | default cli-cbom-pqc-fix | missing: not in WORKERS, no routing card |
| `belai:pr-reviewer` | worker | backlog / pr-review → done +reviewed | yes | `builtin/pr-reviewer.json` L5 (identity L4) | default pr-review; tpl pr-reviewer | partial: in WORKERS, no routing card |
| `belai:pr-steward` | worker | backlog / sf:pr-quality → done | yes | `builtin/pr-steward.json` L5 (identity L4) | tpl low-quality-pr-comment; sf:describe | missing: not in WORKERS, no routing card |
| `belai:release-steward` | worker | backlog / sf:release → review +ticket | yes | `builtin/release-steward.json` L5 (identity L4) | tpl release-steward | missing: not in WORKERS, no routing card |
| `belai:repo-onboarder` | worker | backlog / sf:onboard → review +needs-review |  | `builtin/repo-onboarder.json` L5 (identity L4) | tpl repo-onboarding | missing: not in WORKERS, no routing card |
| `belai:repro` | worker | backlog / repro → done | yes | `builtin/repro.json` L13 (identity L12) | crew bug (default ci-fix); tpl label-repro, bug-repro-to-fix; sf:repro | partial: in WORKERS, no routing card |
| `belai:reviewer` | worker | review / needs-review → done |  | `builtin/reviewer.json` L13 (identity L12) | crews bug/feature/delivery/maintenance/ui; default issue-to-pr; tpl label-review; sf:review, sf:explain | partial: in WORKERS, no routing card |
| `belai:scout` | worker | backlog / scout → done | yes | `builtin/scout.json` L13 (identity L12) | crew delivery (default test-sweep); tpl kanban-triage-autopilot | partial: in WORKERS, no routing card |
| `belai:secret-triager` | worker | backlog / sf:secret-triage → review +ticket | yes | `builtin/secret-triager.json` L5 (identity L4) | default cli-secrets; tpl secret-alert-triage; sf:secret, sf:rotate-secret | missing: not in WORKERS, no routing card |
| `belai:tea-publisher` | worker | backlog / sf:tea-publish → done | yes | `builtin/tea-publisher.json` L5 (identity L4) | tpl build-artifacts-to-tea, sbom-to-tea, container-published-to-tea; sf:sbom | missing: not in WORKERS, no routing card |
| `belai:test-fixer` | worker | backlog / tests-failing → review +needs-review |  | `builtin/test-fixer.json` L5 (identity L4) | crew maintenance; default test-sweep; tpl label-fix-tests; sf:fix-tests | partial: in WORKERS, no routing card |
| `belai:ticket-writer` | worker | backlog / sf:ticket → done | yes | `builtin/ticket-writer.json` L5 (identity L4) | tpl issue-to-ticket, settings-fix-ticket; sf:triage, sf:ticket | missing: not in WORKERS, no routing card |
| `belai:triage` | single | - |  | `builtin/triage.json` L4 | - | n/a (background; never dispatched) |
| `belai:verifier` | worker | review / needs-verify → done |  | `builtin/verifier.json` L13 (identity L12) | crew security | partial: in WORKERS, no routing card |
| `belai:vulnetix-scanner` | single | - |  | `builtin/vulnetix-scanner.json` L4 | - | n/a (background; never dispatched) |
| `belai:vuln-scout` | worker | backlog / vuln-scan → done |  | `builtin/vuln-scout.json` L13 (identity L12) | crew security; tpl security-patch-train | partial: in WORKERS, no routing card |
| `belai:workflow-fixer` | worker | backlog / sf:fix-workflow → review +needs-review |  | `builtin/workflow-fixer.json` L5 (identity L4) | claims sf:fix-workflow (same label cli-gha-posture-fix routes to workflow-hardener) | missing: not in WORKERS, no routing card |
| `belai:workflow-hardener` | worker | backlog / sf:workflow-review → done | yes | `builtin/workflow-hardener.json` L5 (identity L4) | default cli-gha-posture-fix; tpl actions-workflow-hardening | missing: not in WORKERS, no routing card |

Crews (`B/crews/*.json`):

| Crew | Members | In Keleustes `CREWS` |
|---|---|---|
| `belai:bug` | repro, builder, reviewer | yes |
| `belai:delivery` | scout, builder, reviewer | yes |
| `belai:feature` | planner, builder, reviewer | yes |
| `belai:maintenance` | test-fixer, lint-fixer, reviewer | **no** |
| `belai:security` | vuln-scout, patcher, verifier | yes |
| `belai:ui` | designer, builder, reviewer | yes |

The web factory's copy of this table is `factoryBuiltinCrews` (`H/factory_templates.go:23-35`).

Prompts for the model jobs. These run on the node, so Keleustes passes data, not prompt text:

| Prompt | Web source (verbatim) | Node source used by Keleustes (verbatim) |
|---|---|---|
| Agent draft | `vdb-site/api/internal/agentspec/draft.go:70-120` (`SystemPrompt`); run loop `H/belai_ai_draft.go:190-241` | `belai/internal/agentdraft/agentdraft.go:250-298` |
| Avatar | `H/belai_ai_avatar.go:154-173` (`belaiAIAvatarSystem`); brief `H/belai_avatar_brief.go:1-375` (`belaiAvatarBrief` at :348-375); template `H/belai_pix.svg` | `belai/internal/avatar/avatar.go:139-160` |
| SystemOne decision | `vdb-site/api/internal/systemone/clef.go`, `tev1.go` (wire bodies); the questions come from node config | `belai/cmd/belai/factoryjobs.go:355-358` (system), :359 (user) |
| Done-card classification | `H/belai_kanban_classify.go:37-55` (categories, descriptions, floor 0.3); question text at :151-157 | none: run it as a `systemone` job with the same question |
| CLI decision question | `H/factory_defaults_cli.go:663-669` (`fdcQuestion`) | node config |
| Template decision questions | `H/factory_templates.go:377-650` (`fwBDecision` calls; builder at :273); `H/factory_templates_guardian.go:94` | node config |

## 2. Catalogue

Status values:

- **done**: equivalent behaviour exists.
- **partial**: pieces exist, but the behaviour differs or does not run on its own.
- **missing**: nothing in Keleustes.
- **n/a**: the trigger source is SaaS-only and a single-user desktop does not have it. Port only if
  the source is added.

### 2.1 Model-driven, unprompted

| Feature | Trigger | Web source | Keleustes | Keleustes location | How it should run |
|---|---|---|---|---|---|
| SystemOne decision node (Jev / Clef / Clef-flash / Tev1 / `vault:<slug>`): thresholds, `below_threshold`, `on_abstain`, `$unknown` / `$below_threshold` edges, fallback adapter, decision row + audit + event | a `decision` step runs | `H/factory_engine_node_decision.go:57-350`, `vdb-site/api/internal/systemone/*.go`, `D/factory-systemone.md:1-643`, `D/factory.md:3137-3334` | missing (rule-based `condition` only) | `K/kls-engine/src/graph.rs:557-568` (no `decision` type), `exec.rs:1245-1260` | Model job `task:"systemone"` (`{state, questions}` → `{answers{probabilities}, provider, model, calibrated:false}`). Threshold, abstain and routing are Rust in the engine |
| Done-card classification (Jev): tag a card that reaches done with a PR as `sast`/`sca`/`secrets`/`iac`/`container`/`malware`/`cloud`/`defect`, once (`agent.class` marker) | a card write lands in done with a PR; also on demand | `H/belai_kanban_classify.go:91-312`, `D/belai-kanban-classify.md:1-76` | missing | none | Model job `systemone` with one `choice` question over the 8 categories, using the web descriptions; floor 0.3; the marker stops a repeat; Rust applies the label |
| Agent profile draft from a premise, validated and retried (3 per error, 8 calls) | a person posts a premise (automatic from then on) | `H/belai_ai_draft.go`, `vdb-site/api/internal/agentspec/`, `D/belai-workers-ai.md:40-72` | done (on node) | `K/kls-core/src/vault.rs:1089-1150` | Model job `draft` (belai validates against its own spec) |
| Avatar drawing: 5 per hour per profile, svgguard, candidates | a person asks (agent builder) | `H/belai_ai_avatar.go`, `H/belai_avatar_brief.go`, `H/belai_avatar.go` (host fallback), `D/belai-workers-ai.md:73-87` | missing. The draft job asks for `avatar_svg` in `want`, but belai's draft never returns one, and the UI draws initials | `K/kls-core/src/vault.rs:1124-1130`, `K/kls-ui/src/screens/workforce.rs:1120-1160` | Model job `avatar` (`{displayName, palette[4], reportStyle, focus, vocabulary}` → `{svg}`); rate limit and `sanitize_svg` in Rust; ship the builtin avatars |
| Session decision events: each role-manager System One call in a transcript becomes a `decision` event, capped per upload | transcript upload | `H/factory_session_decision_events.go:1-97` | partial (a decisions report exists but is not fed from session entries) | `K/kls-core/src/github.rs:485-600` | Rule: fold `Entry` rows whose meta marks a role-manager decision into `decision` events in `kls-node/src/sessions.rs` |

### 2.2 Default workflows (42, seeded per managed org, run unprompted)

All 42 are **missing** as defaults in Keleustes. Nothing is seeded, and
`K/kls-engine/src/templates.json` holds 8 templates that a person must install.

Web sources:

- Data: `H/factory_defaults.go:157-328` and `H/factory_defaults_cli.go:197-645`, built by `fdcBuild`
  (`:687-800`).
- Seed and upgrade: `H/factory_defaults_seed.go:142-529`.
- Gate: `H/factory_defaults_gate.go:126-347`.
- Docs: `D/factory.md:4741-5031`, `W/software-factory.md:190-225`.

| Key | Trigger | Agent / work | Output | Min autonomy | Web line | Run in Keleustes |
|---|---|---|---|---|---|---|
| `pr-review` | PR opened / synchronize / ready / reopened, not a draft; a fork PR waits for an admin | `belai:pr-reviewer` | `pr_review` (COMMENT / REQUEST_CHANGES, inline) | suggest | `factory_defaults.go:157` | worker card + rule output |
| `ci-fix` | `workflow_run` failure on the default branch; `workflow_run` / `check_suite` failure on a PR branch | `belai:bug` crew | `draft_pr` (base `pr_head` for a PR) | draft_pr | `:178` | crew card + rule output |
| `test-sweep` | push to default (15 min window, 2/h); weekly cron | `belai:delivery`, then `belai:test-fixer` | `draft_pr`, `ifNoBranch: skip` | draft_pr | `:206` | crew card |
| `lint-sweep` | same push; weekly cron | `belai:lint-fixer` | `draft_pr` | draft_pr | `:228` | worker card |
| `issue-to-pr` | issue opened; an outsider's issue waits for an admin | planner → plan approval → builder → reviewer | `draft_pr` | draft_pr | `:249` | crew `belai:feature` card + `human` |
| `approve-ready` | PR / review / green check suite | rule | `approve_ready` (APPROVE only when ready; never merges) | auto_merge_low_risk | `:272` | rule |
| `conflict-fix` | push to default (120 s, 12/h); PR opened / synchronize / reopened (60 s, 60/h) | rule files a `merge-conflict` card → `belai:conflict-resolver` | `conflict_cards` | draft_pr | `:292` | rule + worker card |
| `repo-scout` | repo added; push touching manifests / Dockerfile / TF / workflows / OpenAPI / `.vulnetix/config*`; `sf:scout`; weekly | scan `cli` job `scout` → `belai:cli-runner` | `repo_profile` | observe | `:306` | worker card (`cli-job:scout`) + rule |
| `cli-scan-nightly` … `cli-analyze` (34) | cron / push filters / label / alert webhooks, each with a profile condition | scans via `belai:cli-runner` (`cli-job:<id>`); fixes via `cli-runner-write`, `patcher`, `secret-triager`, `iac-fixer`, `container-fixer`, `pqc-fixer`, `license-fixer`, `defend-writer`, `workflow-hardener` | `file_issues`, `check_run`, `issue_comment`, `draft_pr`, `tea_publish`, `vex`, `notify`, `human` | observe … draft_pr | `factory_defaults_cli.go:197-645`, one spec each (keys at :197, :211, :225, :232, :246, :253, :268, :282, :289, :303, :310, :323, :337, :344, :358, :372, :388, :404, :420, :436, :452, :465, :476, :492, :506, :513, :527, :543, :561, :577, :589, :602, :615, :632) | worker cards + decision job + rule outputs |

Shared machinery for the defaults (all **missing** unless noted):

| Feature | Web source | Keleustes | Keleustes location | How |
|---|---|---|---|---|
| Seed; upgrade that follows the catalogue (`systemRev`, graph hash); `customised` freeze; `sys-` slug reservation | `H/factory_defaults_seed.go:142-363`, `D/factory.md:4780-4844` | missing | – | Rule in `kls-engine`, at startup and when a repo becomes managed |
| Gate: `defaults.enabled`, `defaults.<key>.enabled`, repo `defaultsOff`, archived, autonomy below required; `trigger.skipped` reasons (hourly per repo) | `H/factory_defaults_gate.go:126-191` | partial: autonomy is stored per repo, but the engine never reads it | `K/kls-store/src/github.rs:56-72,804` | Rule |
| Debounce window, hourly run cap, run key `sys:<key>:<repo>:<subject>[:<sha>]` | `D/factory.md:4768-4778,4833-4844` | missing (no debounce or cap; event runs already have dedupe) | `K/kls-engine/src/ops.rs` (dedupe) | Rule |
| Profile condition on every trigger (`feTriggerProfileGate`, `fdProfileFilter`, fail closed) | `H/factory_profile_condition.go`, `H/factory_defaults_gate.go:309-347`, `D/factory.md:5000-5014` | missing | – | Rule |
| Repository profile: file-name signals from mirrored manifests and workflows, refreshed when a repo is added and on a manifest push | `H/factory_repo_profile.go:59-460`, `D/factory.md:1437-1461` | missing | – | Rule over the GitHub mirror (tree listing via a `github` job) |
| Cron stagger: a fixed offset per repo inside the spread | `H/factory_cron_stagger.go:25-50` | missing | `K/kls-engine/src/cron.rs` | Rule in the cron tick |
| Proactive write cap (`forge.quota.proactive_writes_per_hour`) | `D/factory.md:4849` | partial: the factory share and reserve exist, but there is no separate proactive cap | `K/kls-github/src/budget.rs` | Rule |
| Defaults section in settings (`GET /defaults`) | `H/factory_defaults_routes.go` | missing | – | UI in the settings and repos screens |

### 2.3 Engine outputs and node behaviour (rule-driven)

| Feature | Trigger | Web source | Keleustes | Keleustes location | How |
|---|---|---|---|---|---|
| Crew routing card: a card in the profile's claim list, with every claim label and the advertised assignee, and the brief in its body | crew step; `scan` step with `source: cli` | `H/factory_engine_route.go:54-166`, `H/factory_engine_node_crew.go:132-175` | **missing** | `K/kls-engine/src/exec.rs:1526-1580` | Rule: file the card via `kls-core::board`, then `Dispatch{worker}` |
| `pr_review` from the verdict note (`verdict:` line, at most 20 `finding: FILE:LINE:` lines); inline where the line is on the diff; marker in the body; never approves | after `belai:pr-reviewer` | `H/factory_engine_review.go:73-396` | partial (static body, any event) | `K/kls-engine/src/forge.rs:352-371` | Rule |
| `approve_ready`: waits for open, not a draft, same repo, head unchanged, the factory's own review marked approve, checks green, no sensitive path, reviews not stale; then an APPROVE review; never merges | PR / review / check events | `H/factory_engine_approve.go:68-394` | partial and **unsafe** (marks the PR ready and squash-merges) | `forge.rs:284-310`; `merge` at `:321-335` | Rule; drop the merge |
| `conflict_cards`: find conflicting PRs (the run's PR, or up to 20 open PRs on the default branch after a push); recheck unknown mergeability at 0/1/3/10 min with one-shot wakes; one open card per PR URL; `checkpoint:fork_code` on forks | conflict-fix default | `H/factory_engine_conflicts.go:62-219`, card in `H/factory_checkpoints_conflicts.go:94-347`, `D/factory.md:4878-4902` | missing (a manual three-way editor and a desktop notice exist) | `K/kls-github/src/conflicts.rs`, `merge3.rs`, `K/kls-app/src/desktop.rs:299-316` | Rule + worker card for `belai:conflict-resolver` (card body is a fenced block: `H/factory_checkpoints_conflicts.go:120-160`) |
| Conflict ledger: parse the `conflict attempt N/5 ...` notes; Retry clears `checkpoint:conflicts-unresolved` | card notes | `W/software-factory.md:226-250`, `website/src/shared/conflict-ledger.ts` | missing | – | Rule (parser) in `kls-core::board`; UI in the conflicts screen |
| `file_issues`: severity floor; grouping by run or id; a cap on issues; a body marker so a rescan updates the issue; the hand-off label is never re-applied; `from: profile` | CLI defaults | `H/factory_engine_node_issues.go:135-541` | partial (one issue per call, no marker, no findings) | `forge.rs:420-445` | Rule |
| `check_run`: conclusion from findings and exit code (failure on 1, neutral on 3); skipped without the checks permission | CLI defaults | `H/factory_engine_node_issues.go:542-634` | partial (fixed conclusion) | `forge.rs:388-418` | Rule |
| `repo_profile` output: store the signals, ensure the 12 CLI labels, keep one "Software Factory profile" issue | repo-scout | `H/factory_engine_node_profile.go:62-265` | missing | – | Rule |
| `draft_pr`: base is the default branch (from the mirror, else one forge read, else main); stacked on `pr_head`; `ifNoBranch: skip`; checks whether the PR already exists | crew done | `H/factory_engine_node_output.go`, `H/factory_engine_prbase.go:1-112` | partial | `forge.rs:198-283` | Rule |
| Scan node: `source: cli` → `belai:cli-runner` card `cli-job:<id>`; rescan on the crew's branch; `notEvaluable: report`; harness finding `<job>:exit-1` | scan step | `H/factory_engine_node_scan.go`, `D/factory-engine.md:178-251` | missing (no `scan` node) | `graph.rs:557-568` | Worker card; parse the result note into findings |
| Handoff following: a step waits for the cards its worker handed on, and takes the PR or branch of the newest finished one | a worker hands off (repro → builder) | `H/factory_engine_handoff.go:22-91` | missing | – | Rule over the card parent links |
| Capacity wait: a refusal for the worker cap, or a card left unclaimed under a crew run, waits instead of spending an attempt | dispatch refused | `H/factory_engine_capacity.go:31-54` | missing (a refusal fails the step) | `exec.rs:1580-1605` | Rule |
| No-host reasons in words, and their TTL | no node can run the step | `H/factory_engine_nohost.go`, `D/factory-engine.md:357-492` | partial: only "offline" and "none online"; it picks the first online node and ignores the repo and the agent | `exec.rs:1488-1524` | Rule: pick the node by advertised agent + trusted dir |
| Human step: SLA re-notify / end_run | an approval waits | `H/factory_engine_node_human.go` | done | `exec.rs` (`ask_person`) | – |
| Gate: checks from the mirror, return_to_crew, stop-the-line | gate step | `H/factory_engine_node_gate.go` | done (forge + manual) | `forge.rs:448+`, `graph.rs:15,580-581` | – |
| Lease recovery on start | process start | `D/factory.md:3568-3588` | done | `exec.rs:481-492` | – |
| Run notifications (started, succeeded, failed, stopped_line, cancelled, paused, held), with the failure cause in words | run events | `H/factory_notify.go:57-325`, `H/factory_notify_failure.go` | partial: desktop notices cover only approvals, questions and conflicts; the `notify` node emits `run.notify` | `K/kls-app/src/desktop.rs:236-316` | Rule in `desktop.rs` |
| Prerequisites: block a workflow whose model or adapter has no provider | before a run | `H/factory_prereqs.go:117-494` | missing | – | Rule: no node advertises the provider |
| Attribution: the bot signs; a person is co-author only when a person started the run | forge writes | `H/factory_engine_attribution.go` | partial (trailers are parsed for delivery only) | `K/kls-github/src/sync.rs:696` | Rule |
| Kill switch, A2A blackout | a person | `D/factory.md:203-252` | done | `K/kls-core/src/board.rs`, `K/kls-node/src/board.rs` | – |
| Quota guard (classes, share, reserve, secondary limit) | every GitHub call | `H/factory_engine_quota.go` | done | `K/kls-github/src/budget.rs` | – |
| Webhook coverage: push, `workflow_run`, `check_suite` failure, `installation_repositories.added`, `release.published`, `registry_package`, Dependabot / code scanning / secret scanning alerts | GitHub webhooks | `D/factory.md:3634-3691,4667-4694` | partial: the poller emits only PR open / close / merge / synchronize / ready / draft / checks completed, issues and labels | `forge.rs:496-560`, `K/kls-github/src/poller.rs:45-140` | Rule: poll default-branch commits, workflow runs, releases and alerts with ETags, and emit the matching names |

### 2.4 Labels, board and checkpoints

| Feature | Trigger | Web source | Keleustes | Keleustes location | How |
|---|---|---|---|---|---|
| `sf:` label set (34) | a person syncs | `H/factory_labels.go:39-72` | done | `K/kls-github/src/labels.rs` | – |
| Label dispatch: `/sf <name>` comment from a team member; removing a label cancels unstarted runs; `sf:stop` cancels every run on the subject; priority and fan-out cap; `fromFactory` loop guard | label / comment | `H/factory_label_dispatch.go:33-136`, `D/factory.md:4695-4740,4927-4934` | partial (an added label starts its trigger, nothing more) | `forge.rs:543-560` | Rule |
| Checkpoint labels: `checkpoint:*` cards held in Needs you; approve / reject / retry notes; `fork_code`, and the plan and design holds from planner, designer and repro | a worker writes the label | `W/belai-checkpoints.md:21-79`, `W/factory-checkpoints.md` | missing (Needs you shows review and `blocked` cards only) | `K/kls-core/src/needs.rs` | Rule + UI |
| Ticket standard: title under 70 characters, six `##` sections; show the gaps | card write | `H/factory_ticket.go:55-142`, `D/factory.md:1504-1525`, golden `H/testdata/ticket-standard.golden.json` | missing | – | Rule |
| Issue → card mirror | poll | `H/belai_github_engine.go`, `D/belai-github-sync.md` | done (inbound) | `K/kls-github/src/sync.rs:451-612` | – |
| Board → GitHub outbound sync, retry, hourly escalation of open conflicts, closing the issue when its card is done | board change / timers | `H/belai_github_scheduler.go:1-120` | partial (inbound only) | `sync.rs` | Rule |
| Claim lease merge: a renewal moves only `leaseUntil` | node push | `H/belai_kanban_agent.go` | done in belai sync | `K/kls-node/src/board.rs` | – |
| Memory replica: at most 500 notes per scope, oldest tombstoned, tombstones sticky | node push | `H/belai_memory.go:256-306` | partial (CRUD, no cap) | `K/kls-core/src/board.rs`, `K/kls-store/src/board.rs` | Rule |
| Assignment mail | a card is assigned to a person | `H/belai_assignees.go`, `D/belai-assignment-mail.md` | n/a (one user) | – | – |

### 2.5 Budgets, routing, sessions

| Feature | Trigger | Web source | Keleustes | Keleustes location | How |
|---|---|---|---|---|---|
| Budget check before every model call and crew dispatch (the tightest budget wins); ledger with dedupe; alert thresholds; actions at 100 % (hard stop, pause, `downgrade_to_fast`) | dispatch | `H/factory_engine_budget.go:224-668`, `D/factory.md:2027-2148`, `D/factory-engine.md:729-775` | partial: `check_budget` exists, but the engine never calls it, and `check_budget_now` is called only from a test | `K/kls-core/src/vault.rs:594-690`, `exec.rs:1526` | Rule before `dispatch` and `start_job` |
| Fast tier after a downgrade (pick a provider the node advertised) | budget action | `H/factory_engine_tier.go:32-70` | missing | – | Rule using `Capabilities.providers` |
| Per-item agent budgets (`agents.*` settings > workflow limits > step), sent with the dispatch | dispatch | `H/factory_agent_budget.go:59-202` | missing (`Dispatch` has no field for them) | proto `Dispatch` | Proto change (P8a) |
| Provider routing for a step that names no model | dispatch | `D/factory.md:2114-2126` | partial (`route_for` is used by the draft only) | `vault.rs:431` | Rule in `exec.rs` |
| Session finished: outcome (succeeded, failed, timeout, budget_exceeded, rate_limited, stopped, ...), derived from the transcript for older hosts; `session.finished` event; notification | session end | `H/belai_session_finished.go:101-330` | partial (the end is recorded, but there is no outcome or notice) | `K/kls-core/src/sessions.rs` | Rule |
| Lost-session sweep (a host stopped sending heartbeats) | timer | `H/belai_session_finished.go:334-400` | partial (`node.offline` is emitted, but sessions are not marked lost) | `K/kls-node/src/server.rs:586` | Rule on `node.offline` + a timer |
| Host attention: a host asks for a login or a step approval | node | `H/belai_host_attention.go`, `D/belai-host-attention.md` | missing (questions only) | `K/kls-core/src/needs.rs` | Needs a proto frame if wanted (P10a) |
| Host scheduled agents (belai `rc` schedules, listed and edited) | a person; node cron | `H/belai_schedules.go`, `W/belai-console.md:930-937` | missing | – | Out of scope unless the node offers schedules on the link |
| Session facts index (fact kinds per session) | entries arrive | `H/belai_facts.go`, `vdb-site/api/internal/facts/`, `D/belai-session-facts.md` | missing (Trust reads audit lines only) | `K/kls-core/src/audit/trust.rs` | Rule (port the extractors) |

### 2.6 Delivery analytics and lifecycle

| Feature | Trigger | Web source | Keleustes | Keleustes location | How |
|---|---|---|---|---|---|
| Capture hooks (outcomes, human actions, commits, people) | PR / issue / session changes | `H/factory_delivery_capture.go`, `D/factory-delivery-capture.md:20-49` | done | `K/kls-github/src/sync.rs:718-990`, `K/kls-store/src/github.rs` | – |
| Rollup sweep (missed hooks, decision days, people) | timer | `H/factory_delivery_sweep.go:1-1730` | partial (the reads compute on demand) | `K/kls-core/src/github.rs:276-1190` | Rule |
| Outcome linking (session ↔ card ↔ PR ↔ issue) | sweep | `H/factory_delivery_link.go` | partial | `K/kls-store/src/github.rs` | Rule |
| Role classifier (scout / builder / verifier / reviewer ..., a table of weighted rules) | sweep | `H/factory_delivery_roles.go:352-552`, `D/factory-delivery-capture.md:244-398` | missing | – | Rule |
| "By hand" estimate (hours and dollars, golden case) | read | `H/factory_delivery_estimate.go:79-221`, golden `website/src/shared/factory-delivery.golden.json` | missing | – | Rule (pure, golden-tested) |
| Lifecycle ledger (stages, 5 min sweep, coverage) | timer | `H/factory_lifecycle_sweep.go:44-281`, `D/factory-lifecycle.md` | missing | – | Rule |
| Unanswered deliveries (events that started no run) | read | `H/factory_events_unanswered.go` | missing | – | Rule (events anti-join runs) |
| Template ranking signals | read | `H/factory_templates_signals.go` | missing | `K/kls-engine/src/templates.rs` | Rule |

### 2.7 Built-in templates (a person installs them, then they run on their own)

The web factory ships 14 templates in `H/factory_templates.go:367-660`, 25 event templates in
`H/factory_templates_events.go:53-466`, and the Guardian templates in
`H/factory_templates_guardian.go:85-265`.

Keleustes ships 8 (`K/kls-engine/src/templates.json`):

- `feature-from-card`, `bug-repro-to-fix`, `dependency-hygiene`, `session-follow-up`,
  `kanban-triage-autopilot`, `delivery-sweep`
- `nightly-regression`: its job kind is broken (`tests.run`, see above)
- `pr-reviewer`: posts with `issue_comment` instead of `pr_review`

Web templates Keleustes lacks:

- From `factory_templates.go`: `security-patch-train`, `nightly-blackbox`, `feature-from-issue`,
  `malware-response`, `secret-leak-response`, `release-provenance`, `ui-polish-pass`,
  `ci-failure-fixer`, `dependabot-triage`.
- All 25 event templates: `failure-explainer`, `flaky-test-quarantine`, `default-branch-broke`,
  `build-artifacts-to-tea`, `sbom-to-tea`, `low-quality-pr-comment`, `dependabot-strategy-check`,
  `dependency-scope-fix`, `actions-workflow-hardening`, `issue-to-ticket`, `secret-alert-triage`,
  `dependabot-alert-triage`, `code-scanning-triage`, `release-steward`, `repo-onboarding`,
  `label-fix-tests`, `label-repro`, `label-review`, `new-package-review`, `ai-capability-review`,
  `egress-decision`, `github-risk-investigation`, `settings-fix-ticket`, `deploy-failed-card`,
  `container-published-to-tea`.

Eleven web templates use a SystemOne decision node (the `fwBDecision` calls listed in section 1), so
they wait on package P2.

### 2.8 SaaS trigger sources (n/a unless a source is added)

| Feature | Web source | Keleustes | Note |
|---|---|---|---|
| Package firewall, AI firewall, egress and Guardian (`siem`) sources; sightings, batch keys, covered state | `H/factory_sources.go`, `H/factory_sources_fire.go`, `H/factory_sources_siem.go`, `D/factory.md:5032-5225` | n/a | The producers are SaaS services |
| Sighting research (deps.dev, npm, pypistats, MCP registry, DoH, RDAP, crt.sh) | `H/factory_sighting_facts.go` | n/a | Could become a model-free research job on a node |
| A rule decision closes the review ticket | `D/factory.md:5123-5134` | n/a | – |
| `github-risk` and `github-posture` sources (risky GitHub events from webhooks and the org audit log) | `D/github-risk-rules.md`, `D/github-posture-rules.md` | missing | Portable: rules over the poller and `gh api /orgs/{org}/audit-log` |
| Sandbox Clef bridge, Strix call memory and review | `W/software-factory.md:286-293,358-502`, `H/pix_*` | n/a | Sandboxes are out of scope |
| Messaging centre (Slack, Teams, Chat, webhooks), web push | `H/factory_notify.go`, `H/belai_session_finished.go` | n/a; desktop notifications take their place | – |

## 3. Porting plan

Packages are independent unless a dependency is named. Proto changes belong to the coordinator
(`keleustes/docs/WORKSTREAMS.md`), so each one is listed here to be agreed first.

### P1. Builtin catalogue and routing cards (foundation; unblocks P3, P5, P6)

- `K/kls-engine/src/graph.rs`: replace `WORKERS`, `CREWS` and `BACKGROUND_AGENTS` with a table
  generated from `B/builtin/*.json` and `B/crews/*.json` (name, mode, lists, labels, cli jobs,
  on_success, read_only, description). Add `belai:maintenance` and fix the `deps-*` names.
- New `K/kls-engine/src/route.rs`: port `feClaimRoute`, `feCrewEntry`, `feCardLabels` and
  `feAssigneeFor` (`H/factory_engine_route.go:54-166`). Build the card body from `feCardBody`
  (`H/factory_engine_node_crew.go:132-175`).
- `K/kls-engine/src/exec.rs` `dispatch`:
  - File the routing card, in `backlog` or `review` as the profile claims, and store `cardId` on the
    step; then dispatch.
  - Pick a node that advertises both the agent and the repository (`HostAdvert.agents`,
    `trusted_dirs`).
- Proto (P1a): add `HostAdvert.agent_routes` (`repeated AgentRoute{name, mode, lists, labels,
  read_only}`) so the route follows what the node holds. Until then, use the bundled table.
- Bundle the builtin avatars (`B/builtins-index.json` `avatars`) into `kls-ui`.
- Tests:
  - The route for every builtin matches belai's claim rule (golden from the JSON files).
  - A crew step files one card and sends one dispatch.
  - `deps-javascript` is a background agent.

### P2. SystemOne decision node

- `graph.rs`: add the node type `decision` (`question`, `options`/outcomes, `adapter` = `node:<id>`
  or any, `threshold`, `belowThreshold`, `onAbstain`, `fallback`), with validation in `validate.rs`
  and dry run support.
- `exec.rs`:
  - Build the state from prior step outputs, ids and counts only (`decisionState`,
    `H/factory_engine_node_decision.go:247-268`).
  - Run `Job{model, task:"systemone"}`.
  - Apply the threshold and the abstain action (`applyDecision`, `:269-350`), and route the
    `$unknown` / `$below_threshold` edges.
  - Record a decision event and an audit entry; these feed the decisions report at
    `K/kls-core/src/github.rs:485`.
- Settings: `systemone.min_confidence`, `systemone.below_threshold`, `systemone.on_abstain` (the
  first already exists at `K/kls-core/src/audit/config.rs:238`).
- No proto change. belai answers `calibrated:false`, so treat every threshold as uncalibrated.
- Tests: a fake gateway answers; each abstain action; a malformed answer fails closed.

### P3. Forge outputs without merging (needs P1 for `pr_review`)

- `K/kls-engine/src/forge.rs`:
  - Remove the merge path from `approve_ready`, and drop the `merge` action from `GITHUB_ACTIONS`
    (or limit it to an explicit person action outside workflows).
  - Port the readiness rules (`H/factory_engine_approve.go:110-237`) and the sensitive paths.
- `pr_review`: parse the reviewer's card note (`feParseReviewNotes`,
  `H/factory_engine_review.go:110-151`); post inline comments only on diff lines (`fePatchLines`);
  add the marker (`feReviewMarker`); never APPROVE.
- `file_issues` / `check_run`: port `H/factory_engine_node_issues.go:135-634` (marker, grouping,
  severity floor, conclusion).
- `draft_pr`: base branch resolution (`H/factory_engine_prbase.go`), `pr_head` stacking,
  `ifNoBranch`.
- Tests: golden verdict notes; `approve_ready` refuses on a stale head, a sensitive path or a failing
  check; a rescan updates the same issue.

### P4. Event coverage from the poller

- `K/kls-github/src/poller.rs`, `sync.rs`: poll default-branch commits (push), workflow runs and
  check suites (failures), releases, Dependabot / code scanning / secret scanning alerts, and new
  repositories of managed orgs; emit `github.*` kinds for each.
- `forge.rs` `webhook_events`: map them to `push`, `workflow_run.completed` (with its conclusion),
  `check_suite.completed`, `release.published`, `installation_repositories.added`,
  `dependabot_alert.created`, `code_scanning_alert.created` and `secret_scanning_alert.created`.
  Carry `changedFiles` for path filters.
- Tests: fake GitHub sequences produce each name once, and an ETag 304 produces none.

### P5. Conflict-fix automation (needs P1, P4)

- New `K/kls-engine/src/conflicts.rs`, the `conflict_cards` output (`H/factory_engine_conflicts.go:62-219`):
  - Recheck at 0/1/3/10 min with `wait` wakes.
  - At most 20 candidates, and one open card per PR URL.
  - `checkpoint:fork_code` on forks.
  - Card shape from `H/factory_checkpoints_conflicts.go:94-347`.
- UI: a "Resolve with belai:conflict-resolver" action beside the existing editor in
  `K/kls-ui/src/screens/conflicts.rs`.
- Port the ledger parser (`website/src/shared/conflict-ledger.ts`) into `kls-core::board`. Retry
  clears `checkpoint:conflicts-unresolved`.
- Tests: dedupe; fork hold; golden ledger lines.

### P6. Defaults catalogue (needs P1, P2, P3, P4)

- New `K/kls-engine/src/defaults/`:
  - `catalogue.rs`: the 8 core graphs.
  - `cli.rs`: the 34 specs as data, plus a port of `fdcBuild`.
  - `seed.rs` and `gate.rs`.
- Store: migration `0008_defaults.sql` in both dialects, adding `workflows.origin`, `system_key`,
  `system_rev` and `customised`; repo `defaults_off`; a repo profile table (signals, `scouted_at`).
- Engine: debounce window, hourly cap, run key, overlap, cron stagger; the profile condition at the
  trigger and at schedule fan-out; autonomy enforcement (read `K/kls-store/src/github.rs:56`).
- The `repo_profile` output and the profile issue.
- A `scan` node with `source:cli` that files a `cli-job:<id>` card for `belai:cli-runner` (or
  `-write`) and parses the result note into findings.
- UI: a Defaults section in settings, and per-repo state in repos.
- Tests: seeding is idempotent; an upgrade skips customised defaults and open drafts; every gate
  reason; each of the 42 graphs passes `validate.rs`.

### P7. Label dispatch

- `K/kls-engine/src/ops.rs` / `exec.rs`, porting `H/factory_label_dispatch.go:33-136`:
  - `/sf <name>` comments from members.
  - Removing a label cancels unstarted runs; `sf:stop` cancels every run on the subject.
  - `feTriggerPriority` and `feFanoutCap`.
  - The `fromFactory` loop guard.
- Tests: a comment from an outsider is ignored; a label removal cancels only unstarted runs.

### P8. Budgets and per-item limits

- `exec.rs`: call `check_budget_now` before `dispatch` and `start_job`; record usage; raise alerts;
  apply the actions (stop, pause, and `downgrade_to_fast` using `Capabilities.providers`).
- Proto (P8a): add `Dispatch.budget` (`max_cost_usd_per_item`, `max_passes_per_item`,
  `max_wall_per_item_ms`, `finalize_at`) and `Dispatch.tier`. belai layers the budget over the
  profile's own (`H/factory_agent_budget.go:94-160`).
- Prerequisites (`H/factory_prereqs.go`): block a workflow whose provider no node offers.
- Tests: the tightest budget wins; a hard stop fails the step; a downgrade picks an advertised
  provider.

### P9. Model jobs: avatar and done-card classification

- `K/kls-core/src/vault.rs`: add `draw_avatar` (model job `avatar`, 5 per hour per profile,
  candidates kept, `sanitize_svg`), and stop asking the `draft` job for `avatar_svg`.
- New `K/kls-core/src/classify.rs`:
  - On `card.moved` to done, for a card with a PR and no class marker, run `systemone` with the
    8-category choice question (`H/belai_kanban_classify.go:37-157`), floor 0.3.
  - Apply the label within the 8-label cap, then write the marker.
- Tests: a fake gateway; an unsure answer leaves the card alone; a second move does not ask again.

### P10. Self-healing and notices

- `exec.rs`: capacity wait (`H/factory_engine_capacity.go`), handoff following
  (`H/factory_engine_handoff.go`), and no-host reasons in words.
- `K/kls-core/src/sessions.rs`: the session outcome (port `notify.Derive`), and lost sessions on
  `node.offline` plus a timer.
- `K/kls-app/src/desktop.rs`: notices for run started / failed / stopped_line / held, worded as
  `fwFailureCause` does (`H/factory_notify_failure.go`).
- Tests: a refused dispatch waits without spending an attempt; a repro → builder handoff opens the
  builder's PR.

### P11. Board rules

- `K/kls-core/src/needs.rs`: `checkpoint:*` cards with approve / reject / retry
  (`W/belai-checkpoints.md:21-79`).
- `K/kls-core/src/board.rs`: ticket standard gaps (golden `H/testdata/ticket-standard.golden.json`),
  and the 500-note cap per memory scope with tombstones.
- `K/kls-github/src/sync.rs`: outbound board → issue sync, and closing the issue when its card is
  done.
- Tests: the golden ticket cases; the cap tombstones the oldest note.

### P12. Delivery and lifecycle

- `K/kls-core/src/github.rs`: the role classifier (`H/factory_delivery_roles.go:352-552`), the
  estimate (golden `website/src/shared/factory-delivery.golden.json`), unanswered deliveries, and the
  template ranking signals.
- New `K/kls-core/src/lifecycle.rs` and a migration: the ledger and its five-minute sweep
  (`H/factory_lifecycle_sweep.go`).
- Tests: the two goldens; the sweep is idempotent.

### Proto changes, all

| Id | Change | Why |
|---|---|---|
| P1a | `HostAdvert.agent_routes` | route cards to what the node holds |
| P8a | `Dispatch.budget`, `Dispatch.tier` | per-item budgets and the fast tier |
| P10a (optional) | `HostAttention` frame (login / approve) | host attention requests |

No new job kinds are needed. `model` (`draft`, `avatar`, `systemone`) and `github` cover every model
and GitHub call above. The `nightly-regression` template must stop using `tests.run` and run its
tests through a worker card (`belai:test-fixer` or `belai:delivery`) instead.


## Status re-check (2026-11-22)

Every row above whose Keleustes column was not `done` or `n/a` was re-verified against the code on 2026-11-22. So were the intro findings, the crew table, the 2.7 template lists, the 2.8 `github-risk` row and the proto table. The 40 worker profiles share one mechanism (bundled catalogue plus routing card) and are grouped into one row; all 58 belai names are in `kls-engine/src/builtins/profiles.json`. The status is `done` only with code evidence.

Totals (84 rows): 32 done, 42 partial, 9 missing, 1 n-a.

Key findings:

- **Foundation done.** The catalogue (58 profiles, 6 crews including `belai:maintenance`, 32 avatars), routing cards, the decision node, label dispatch, checkpoint labels, the conflict ledger, handoff following, lost sessions, host attention, session facts and the proto fields P1a and P10a are all in place.
- **Most remaining partials are in forge outputs** (`kls-engine/src/forge.rs`):
  - `file_issues` ignores findings, severity, grouping and the marker, so the 34 CLI defaults never surface their findings.
  - `check_run` takes its conclusion from config only.
  - `pr_review` has no verdict parsing.
  - `approve_ready` posts no APPROVE review and has no readiness rules.
  - `draft_pr` resolves `pr_head` literally.
- **Budget gaps.** P8a is only half done: the engine never fills `DispatchSpec.budget` / `tier`. The budget gate runs before dispatch but not before `start_job` or the decision job.
- **Missing:**
  - done-card classification
  - the "Resolve with belai:conflict-resolver" button
  - the prerequisites check
  - 4 named templates (`nightly-blackbox`, `feature-from-issue`, `malware-response`, `secret-leak-response`)
  - all 25 event templates
  - the `github-risk` / `github-posture` sources
- **`cli-image` never fires.** It triggers on `registry_package`, which the poller does not emit.

| item | section | porting package | status | evidence (file:line, under `crates/` or `proto/`) | what's left |
|---|---|---|---|---|---|
| Intro: Keleustes merges (approve_ready squash-merge, `merge` action) | intro | P3 | done | kls-engine/src/forge.rs:296-314 (approve_ready marks ready, `"merged": false`); forge.rs:350-354 (`merge` bails "the factory never merges"); kls-engine/src/graph.rs:655-664 (no `merge` in GITHUB_ACTIONS) | - |
| Intro: `pr_review` allows APPROVE | intro | P3 | done | kls-engine/src/forge.rs:363-373 (only REQUEST_CHANGES or COMMENT; marker at :329-335) | - (verdict parsing is a separate row, 2.3) |
| Intro: `nightly-regression` uses job kind `tests.run` | intro | P6/- | done | kls-engine/src/templates.json:272, :316-321 (step n2 is now `crew`, `dispatch: worker`, `agent: belai:test-fixer`); no `tests.run` in templates.json | - |
| Intro: BACKGROUND_AGENTS has `deps-npm`, lacks 9 deps-*; WORKERS 12/40 | intro | P1 | done | kls-engine/src/graph.rs:671-683 (background_agents/workers/crews now read the catalogue); kls-engine/src/catalogue.rs:360-380, tests :472-478 (58 profiles, 40 workers, 18 background, `deps-npm` none, `deps-javascript` background) | - |
| 40 builtin worker profiles (ai-capability-reviewer, alert-triager, builder, ci-explainer, cli-runner, cli-runner-write, conflict-resolver, container-fixer, defend-writer, dep-scope, dep-strategist, designer, egress-reviewer, flaky-hunter, github-investigator, guardian-aws-investigator, guardian-cloudflare-investigator, guardian-remediator, iac-fixer, license-fixer, lint-fixer, package-reviewer, patcher, planner, pqc-fixer, pr-reviewer, pr-steward, release-steward, repo-onboarder, repro, reviewer, scout, secret-triager, tea-publisher, test-fixer, ticket-writer, verifier, vuln-scout, workflow-fixer, workflow-hardener): in catalogue + routing card | 1 | P1 | done | All 40 names present with `mode: worker` and a `kanban` block in kls-engine/src/builtins/profiles.json (40 `"kanban"` keys); loaded by kls-engine/src/catalogue.rs:21,321-324; route kls-engine/src/route.rs:43-88; test route.rs:211-244 asserts every builtin worker routes to its first claim list and labels; card filed by kls-engine/src/exec.rs:2406-2502 before dispatch (exec.rs:2626-2633; `card_id` = route card in the DispatchSpec at exec.rs:2632-2666) | Route takes only the first claim list (route.rs:45); routes come from bundled/saved profile, not from the node's advertised `agent_routes` (those only choose the node) |
| 18 background/single profiles (triage-vulns, debug, deps-apple, deps-ci, deps-containers, deps-dotnet, deps-go, deps-javascript, deps-jvm, deps-other, deps-php, deps-python, deps-ruby, deps-rust, fanout, plan-handoff, triage, vulnetix-scanner) | 1 | P1 | n-a | All 18 in builtins/profiles.json with `mode: single`; kls-engine/src/catalogue.rs:221-224 (`is_background`), route.rs:228 (deps-go never routes) | - (correctly never dispatched) |
| Missing profiles vs. doc table | 1 | P1 | done | builtins/profiles.json has exactly the 58 names in the doc table (grep of `"name"` = 58, none missing, none extra) | - |
| Builtin avatars bundled (32) | 1 | P1 | done | kls-engine/src/builtins/avatars.json (32 SVGs = 32 `avatar_id`s in profiles.json); catalogue.rs:332-358; shown in kls-ui/src/screens/workforce.rs:115-118 | - |
| Crew `belai:maintenance` | 1 (crews) | P1 | done | kls-engine/src/builtins/crews.json:60; catalogue.rs:476 test; route.rs:235-236 (routes to belai:test-fixer) | - |
| SystemOne decision node | 2.1 | P2 | done | kls-engine/src/graph.rs:624-639 (`decision` in NODE_TYPES); kls-engine/src/decision.rs:17-18 (`$below_threshold`, `$unknown`), :67-75 payload `task: systemone`; exec.rs:2775-2936 (job, threshold, settings fallbacks, `decision.made` event, audit, human on abstain); validate.rs:596-641; UI kls-ui/src/screens/workflows.rs:2196 | - |
| Done-card classification (Jev) | 2.1 | P9 | done | `kls-core/src/classify.rs` (`kls_core::classify`, `pub mod classify;` in `lib.rs`): eligible = done, PR, no class marker or category label; `classify_card` `:307` checks the kill switch (`job`) and A2A blackout, runs a `model` job `{"task":"systemone"}` with the 8-category choice question, floor 0.3 `:78` (card left alone), applies the label and the `keleustes.class` marker (provider, model, node, origin), emits `card.classified`, audits every outcome; runs in the background on card writes (`after_write` `:253`, setting `board.classify`), on demand from the drawer (`kls-ui/src/screens/board.rs:1630`); tests `classifies_once…`, `below_the_floor…`, `refused_under_the_kill_switch`, `refused_under_the_a2a_blackout` | - |
| Avatar drawing (model job `avatar`) | 2.1 | P9 | done | WP5: `draw_avatar` `kls-core/src/vault/workforce.rs:467` (model job `avatar` {displayName, palette, reportStyle, focus, vocabulary} -> {svg}, 5 per hour per profile, `sanitize_svg`, last 5 candidates kept, kill switch, audited); draft no longer asks for `avatar_svg` (`kls-core/src/vault.rs` draft_agent want list); builder Draw button `kls-ui/src/screens/workforce.rs:1207`. Test: avatar_job_is_sanitised_rate_limited_and_stops_with_the_switch | - (candidate picker UI shows the latest only) |
| Session decision events (role-manager decisions in transcripts) | 2.1 | - | done | WP5: `Core::session_append` folds newly stored role-manager entries (not hidden, not harness) into `decision.session` events, 10 per upload, the last carrying `more` (`kls-core/src/sessions.rs:349`, rule `decision_of` `kls-core/src/sessions/outcome.rs:296`); done in core so kls-node needs no change. Test: end_records_the_outcome_and_decisions_are_folded | - |
| Default `pr-review` | 2.2 | P6 | done | kls-engine/src/defaults.rs:31-37, :164-198; output parses the verdict note with inline findings kls-engine/src/forge.rs:622, kls-engine/src/forge/outputs.rs:430 (golden kls-engine/testdata/verdict-notes.golden.json) | - |
| Default `ci-fix` | 2.2 | P6 | done | defaults.rs:220-272 (PR `check_suite.completed` plus `workflow_run.completed` on `$default`, condition reads `checks` / `failed` / `conclusion`, rev 2); `base: pr_head` resolves in forge.rs:291-322 and, before the forge, in kls-engine/src/exec.rs:2370-2385 (`pr_head_of` exec.rs:388, the run's head or `headBranch`); tests exec/tests/engine_policy.rs `draft_pr_on_pr_head_gets_the_branch_not_the_word`, defaults.rs `defaults_carry_push_workflow_run_and_outsider_triggers` | - |
| Default `test-sweep` | 2.2 | P6 | done | defaults.rs `push_default` :161 (push on `$default`, 900 s debounce, 2/h), wired as n5 in test-sweep; cron follows `schedules.tz`/`schedules.missed` (defaults.rs `cron`, rev 2) | - |
| Default `lint-sweep` | 2.2 | P6 | done | defaults.rs `push_default` :161 wired as n4 in lint-sweep (rev 2) | - |
| Default `issue-to-pr` | 2.2 | P6 | done | defaults.rs:335-401 (n1 `senders: team`; n7 `senders: outsider` → n8 human "Approve an outsider's issue" → planner); condition `senders` kls-engine/src/triggers.rs:34, :445 (unknown association = outsider, fails closed) | Forge should put `authorAssociation` on issue events (requested); without it every issue waits for a person |
| Default `approve-ready` | 2.2 | P6/P3 | done | defaults.rs:337-359; output kls-engine/src/forge.rs:499 (readiness rules kls-engine/src/forge/outputs.rs:676, APPROVE review via `ForgeApi::approve`) | - (a waiting readiness is reported, not re-polled: request to engine) |
| Default `conflict-fix` | 2.2 | P5 | done | kls-engine/src/conflicts.rs:50-95 (rule, now also `defaults.conflict-fix.enabled`), `file_card` :114; rechecks and the 20-PR scan through the `conflict_cards` output (row below) | - |
| Default `repo-scout` | 2.2 | P6 | partial | defaults.rs:360-392; forge side `repo_profile` action kls-engine/src/forge.rs:909 (12 labels, profile issue) | exec.rs `store_profile` does not call it yet (request to engine, requests-engine.md WP3 #1) |
| 34 CLI defaults (`cli-scan-nightly` … `cli-analyze`) | 2.2 | P6 | partial | file_issues files the scans' findings with floor/group/cap/marker kls-engine/src/forge.rs:766; check_run conclusion from findings/exit kls-engine/src/forge.rs:695; `registry_package` mapped kls-engine/src/forge.rs:1167 and polled (kls-github poller) | `vex` and malware `card` stages are still `notify` nodes (defaults_cli.rs, engine) |
| Seed / upgrade / customised freeze / `sys-` slug | 2.2 (shared) | P6 | partial | defaults.rs seed (upgrade by `system_rev`); `sys-` reserved: kls-engine/src/ops.rs:114 (`SYSTEM_SLUG_PREFIX` :290; a user name becomes `wf-sys-…`, an explicit `sys-` slug is refused), test `sys_slugs_are_reserved_for_defaults` | Upgrade follows rev only (no graph hash); no skip for open drafts |
| Gate (`defaults.enabled`, per-key enabled, `defaultsOff`, archived, autonomy; `trigger.skipped` reasons) | 2.2 (shared) | P6 | done | defaults.rs:488-520 (`enabled_key`, `key_enabled`, gate reason `default_disabled`), `gate_reason` :643; `trigger.skipped` (reason, key, repo, at most hourly per workflow, audited) kls-engine/src/exec.rs:1054 `note_gated_defaults`; test `a_gated_default_notes_trigger_skipped_hourly` | Governance: list `defaults.<key>.enabled` in config.rs (requested) |
| Debounce, hourly cap, run key | 2.2 (shared) | P6 | done | triggers.rs `run_key` :489 (`sys:<key>:<repo>:<subject>[:<sha>]`, sha from the event or the mirrored pull), used as the dedupe key for seeded workflows exec.rs:1504 | - |
| Profile condition on every trigger (fail closed) | 2.2 (shared) | P6 | done | triggers.rs:79-82 (`profile_unknown` / `profile`), triggers.rs:237-260 ProfileMatch; cron fan-out exec.rs:800-810; event facts exec.rs:1030 | - |
| Repository profile (file-name signals) | 2.2 (shared) | P6 | partial | Profile comes from the CLI scout scan result: exec.rs:1601-1636 `store_profile`, triggers.rs:211-233 RepoProfile stored in settings `repo.profile:<repo>` | Not computed by Keleustes from the mirrored tree; depends on belai's scout result shape |
| Cron stagger | 2.2 (shared) | P6 | done | exec.rs:364-373 (`stagger_offset`), :816-827 (`notBefore` per repo) | - |
| Proactive write cap | 2.2 (shared) | P6 | partial | setting only: kls-core/src/audit/config.rs:411, :593, :689 | Not enforced anywhere (no reader outside config.rs) |
| Defaults section in settings / repos | 2.2 (shared) | P6 | partial | settings section kls-core/src/audit/config.rs:202-214 (rendered by kls-ui/src/screens/settings.rs:1210); system/customised badge kls-ui/src/screens/workflows.rs:475-479 | No per-default list or per-key switch; repos screen has no `defaults_off` editing (kls-ui/src/screens/repos.rs:709-800 shows autonomy, board, merge, cap only) |
| Crew routing card | 2.3 | P1 | done | exec.rs:2406-2502 (`file_route_card`: list status, labels, belai meta, body, `route.filed` event, audit); route.rs:91-133 | - |
| `pr_review` from verdict note | 2.3 | P3 | done | kls-engine/src/forge.rs:622 (verdict + `finding: FILE:LINE:`, inline comments on diff lines via `ForgeApi::review_inline`, once per run/node/head, never APPROVE); outputs.rs:430, golden test outputs.rs:853; test kls-engine/src/forge/tests.rs:855 | - |
| `approve_ready` readiness + APPROVE | 2.3 | P3 | done | kls-engine/src/forge.rs:499; rules kls-engine/src/forge/outputs.rs:676 (open, same repo, run head = PR head, factory approve review on the head, no change requests, checks green, mergeable, not own PR) and sensitive paths :596-625 + `approve.sensitive_paths`; refusals audited (guard::refuse); test kls-engine/src/forge/tests.rs:954 | - |
| `conflict_cards` | 2.3 | P5 | done | `conflict_cards` in GITHUB_ACTIONS (graph.rs:681), run by the engine exec/policy.rs:289 (the run's pull or up to 20 open pulls, unknown mergeability rechecked at 0/1/3/10 min, `deferred:recheck`, autonomy `draft_pr`, kill switch first); conflicts.rs:99 `RECHECK_MINUTES`, :101 `MAX_PULLS`; test `conflict_cards_output_files_cards_and_rechecks_unknown` | - |
| Conflict ledger + Retry clears `checkpoint:conflicts-unresolved` | 2.3 | P5 | done | kls-core/src/board/cards.rs:29, :300-343 (`conflict_ledger`), :588-610 (`retry_card` drops CONFLICTS_UNRESOLVED at :601); UI kls-ui/src/screens/board/claims.rs:188-223 | - |
| "Resolve with belai:conflict-resolver" action in conflicts screen | 2.3 | P5 | missing | no `resolver`/`belai` reference in kls-ui/src/screens/conflicts.rs | Button beside the three-way editor |
| `file_issues` | 2.3 | P3 | done | kls-engine/src/forge.rs:766 (floor, group run/id, cap, kinds/skipKinds, marker so a rescan updates/reopens, findings table, `from: profile`, own-author check); outputs.rs:168; tests kls-engine/src/forge/tests.rs:620,721 | - |
| `check_run` | 2.3 | P3 | done | kls-engine/src/forge.rs:695 (failure on exit 1 or a finding at the floor, neutral on exit 3, `when: on_findings`); outputs.rs:314; test kls-engine/src/forge/tests.rs:776 | Commit status on every forge (no check-run API for user tokens) |
| `repo_profile` output | 2.3 | P6 | partial | exec.rs:1601-1636 stores signals; kls-engine/src/forge.rs:909 ensures the 12 labels and keeps one marked profile issue | Engine must call the forge action (request) |
| `draft_pr` base / stacking | 2.3 | P3 | done | kls-engine/src/forge.rs:291 (`pr_head`, default, named), :324 (branch from cfg/card/inputs else `forge.branch_prefix` + card key, protected-head refusal, `forge.pr_state`), :459 reviewers (codeowners / logins / org/team, unsupported reported) | - |
| Scan node (`source: cli`) | 2.3 | P6 | partial | exec.rs:2095-2115 (cli-job card for cli-runner / -write); settle on card exec.rs:1509-1531 (`cliResult`, `notEvaluable`); graph.rs:478-482 palette | Rescan on the crew's branch; harness finding `<job>:exit-1`; parse result note into findings |
| Handoff following | 2.3 | P10 | done | exec.rs:1427-1437, :1462-1486 (`open_children`/`handoffs_open`), :1534-1597 (waits, takes newest child's PR and branch) | - |
| Capacity wait | 2.3 | P10 | done | exec.rs capacity refusal → `deferred:capacity`; an unclaimed routing card now waits (`waiting:unclaimed`, re-looked each claim TTL, `route.unclaimed` evented + audited) until the step timeout, then `card_unclaimed` (exec.rs:3600-3650); test `unclaimed_card_waits_then_fails_at_the_timeout` | - |
| No-host reasons in words + TTL | 2.3 | P1/P10 | done | route.rs:138-199 (pick by advertised agent/crew + trusted dir, "no online node offers X"); exec.rs:2331-2400 (no_host TTL from settings `autonomy.no_host_ttl_hours`) | - |
| Run notifications | 2.3 | P10 | partial | kls-app/src/desktop.rs:336-354 (`failure_cause` words), :359-374 (`run.notify`, `run.finished` failed) | started, succeeded, cancelled, paused, held notices |
| Prerequisites (no provider for the model/adapter) | 2.3 | P8 | done | kls-engine/src/ops.rs:817 `prerequisites` (a model step's provider must be advertised by a registered node when any node lists providers); `start_run` refuses `prerequisite_missing` (audited) ops.rs:608; test `prerequisites_block_a_provider_no_node_offers` | Adapter/harness prerequisites not checked |
| Attribution (bot signs, person co-author) | 2.3 | - | done | kls-github/src/forge/attribution.rs (web rules, replays the web's attribution-cases.json, testdata/attribution-cases.json); appended to PR, issue, comment and review bodies kls-engine/src/forge.rs:200-226 | - |
| Webhook coverage | 2.3 | P4 | done | events.rs poll_repo plus `registry_package` (events.rs:306, GitHub; mapped kls-engine/src/forge.rs:1167) and review submitted (sync.rs:341) | Packages and risk not polled on Gitea/GitLab (no API in the forge layer) |
| Label dispatch (`/sf`, removal cancels, `sf:stop`, priority, fan-out cap, `fromFactory`) | 2.4 | P7 | done | triggers.rs:263-303 (sf_command, command_of, trusted_association), :484 priority; exec.rs:1085-1113 (outsider comment, from_factory, bots), :1137-1147 fanout_cap, :1203-1255 label_controls (removed label cancels queued, `sf:stop` cancels all) | - |
| Checkpoint labels (`checkpoint:*` held, approve/reject/retry) | 2.4 | P11 | done | kls-core/src/board/cards.rs:11-14, :144-155, :494-540; kls-core/src/needs.rs:54-55, :360; UI kls-ui/src/screens/checkpoints.rs:713 | - |
| Ticket standard | 2.4 | P11 | done | rule + golden `kls-core/src/board/rules.rs`; every card write runs it (`create_card`, `update_card` when title/body change, node pushes) and puts the gap codes on the event as `ticket` (`kls-core/src/board.rs:338`, `gap_codes` `rules.rs:286`); gaps shown live in the drawer and the new-card dialog (`kls-ui/src/screens/board/tabs.rs:167`); test `card_writes_carry_golden_ticket_gaps` `rules.rs:343` | - |
| Board → GitHub outbound sync | 2.4 | P11 | done | kls-core/src/board/sync.rs:283 (three-way), kls-github/src/sync.rs:792-1070 (push, retry, close on done, conflict escalation `github.sync.escalated`) | - |
| Memory replica 500-note cap | 2.4 | P11 | done | kls-core/src/board/rules.rs:20, :281 (`enforce_memory_cap`); called kls-core/src/board.rs:587, :673 | - |
| Budget check before every model call and dispatch | 2.5 | P8 | done | exec/policy.rs:174 `budget_check` (run cap, `budget_gate_scoped`, block / downgrade / pause / escalate / fail closed) called from dispatch (exec.rs:2819), `start_job` (exec.rs:3409) and `run_decision` (exec.rs:3039); tests `start_job_over_budget_is_refused`, `decision_over_budget_is_refused` | - |
| Fast tier after downgrade | 2.5 | P8 | done | exec/policy.rs:134 `fast_on_node` (the gate's fast model when the node offers it, else the fast model of a provider the node advertises); `Dispatch.tier = "fast"`; tests `downgrade_sets_the_fast_tier`, `the_fast_tier_comes_from_a_provider_the_node_offers` | - |
| Per-item agent budgets | 2.5 | P8 | done | exec/policy.rs:19 `worker_budget` (`agents.*` settings, overridden by workflow `limits` graph.rs:80-105, step tokens as `max_tokens`) on every dispatch (exec.rs:2868); test `dispatch_carries_budget_tier_and_member_models` | - |
| Provider routing for a step with no model | 2.5 | P8 | done | exec/policy.rs:64 `step_model` → `kls_core::vault::route_for` (rules, `models.agents`, profile, default) from dispatch; source on the dispatch audit and step (`modelFrom`); test `a_step_with_no_model_takes_the_per_agent_model_map` | - |
| Session finished outcome | 2.5 | P10 | done | WP5: taxonomy `OUTCOMES` and `derive` (host reason, turn error classes, stop asked, interrupted turn, goal state) `kls-core/src/sessions/outcome.rs:40,227`; `session_end` emits `session.finished` with outcome/severity/headline and adds `outcome` to `session.ended` (`kls-core/src/sessions.rs:421`); `Core::session_outcome` :379; Sessions header Outcome chip | Desktop notice still keys on `session.ended` (kls-app owner may switch to the headline) |
| Lost-session sweep | 2.5 | P10 | done | kls-node/src/lost.rs:1-30 (grace sweep every 60 s, revoked nodes); kls-core/src/sweeps.rs:5-8; notice desktop.rs:388-392 | - |
| Host attention | 2.5 | P10 | done | proto node.proto:73, :176, :193, :916-940; kls-node/src/attention.rs:1-20; UI kls-ui/src/fleet.rs:548-575 (approve/deny); notice desktop.rs:383-387 | - |
| Host scheduled agents | 2.5 | - | done | kls-node/src/schedules.rs:1-20; UI kls-ui/src/fleet.rs:743-795, kls-ui/src/screens/host.rs:1584-1590 | - |
| Session facts index | 2.5 | - | done | kls-core/src/audit/facts.rs:1-25, :230, :244; folded by kls-core/src/sweeps.rs:14-15; UI kls-ui/src/screens/audit.rs:104 | - |
| Rollup sweep | 2.6 | P12 | partial | `kls-core/src/github.rs:1859` (`rollup`: links, fills agent time / model cost / start from card sessions, counts missed merged-pull captures, audited, `delivery.rollup` event, kill-switch gated); Delivery > Lifecycle button | No timer yet (requested from the `sweeps.rs` owner); missed captures are counted, not re-captured (that needs the forge client). |
| Outcome linking | 2.6 | P12 | done | `kls-core/src/github.rs:1819` (`link_outcomes`: outcome <-> card by PR, PR ref from the card; each link audited `outcome.link`), `outcome()` links sessions, runs and issues `:1601`; tests `:2414`, `:2528` | - |
| Role classifier | 2.6 | P12 | partial | Weighted rule table `kls-core/src/github.rs:2186` (`ROLE_RULES`), `classify_weighted` `:2281` (heaviest rule per evidence kind, min score, margin, dedicated workers, tie order), test `:2399`; shown in Delivery > Estimate `kls-ui/src/screens/delivery.rs:2286` | `estimates::classify_role` still uses the keyword cascade (requested from its owner). |
| "By hand" estimate | 2.6 | P12 | partial | kls-core/src/estimates.rs:1-40 (port of website estimate, roles, rates, conservative case) | Golden test from factory-delivery.golden.json not ported |
| Lifecycle ledger | 2.6 | P12 | partial | kls-engine/src/delivery.rs:137-240 (episodes, card_lifecycle, flow, computed on read); UI kls-ui/src/screens/delivery.rs:97 | Stored ledger, 5-min sweep, coverage |
| Unanswered deliveries | 2.6 | P12 | done | kls-engine/src/delivery.rs:36-80; UI kls-ui/src/screens/delivery.rs:98 | - |
| Template ranking signals | 2.6 | P12 | done | kls-engine/src/delivery.rs:92-135 (`rank_templates`); UI kls-ui/src/screens/templates.rs:46-47 | - |
| Template `pr-reviewer` output | 2.7 | P3 | done | kls-engine/src/templates.json:756, step uses `"action": "pr_review"` at :819 | - |
| Template `security-patch-train` | 2.7 | P6 | done | templates.json:1124 | - |
| Template `ui-polish-pass` | 2.7 | P6 | done | templates.json:1020 | - |
| Template `ci-failure-fixer` | 2.7 | P6 | done | templates.json:840 | - |
| Template `dependabot-triage` | 2.7 | P6 | done | templates.json:931 | - |
| Template `release-provenance` | 2.7 | P6 | done | templates.json:1228 | - |
| Template `nightly-blackbox` | 2.7 | P6 | done | `kls-engine/src/templates.json:1306` (cron, repro, verifier, decision, issue) | - |
| Template `feature-from-issue` | 2.7 | P6 | done | `kls-engine/src/templates.json:1471` (label trigger, planner, approval, builder, reviewer, gate, draft PR) | - |
| Template `malware-response` | 2.7 | P6 | done | `kls-engine/src/templates.json:1652` (Dependabot alert trigger, malware check, patcher, decision, notify, issue) | The web trigger is a SaaS finding; here a Dependabot alert whose summary names malware |
| Template `secret-leak-response` | 2.7 | P6 | done | `kls-engine/src/templates.json:1841` (secret_scanning_alert.created, decision, notify, owner approval) | - |
| 25 event templates (failure-explainer, flaky-test-quarantine, default-branch-broke, build-artifacts-to-tea, sbom-to-tea, low-quality-pr-comment, dependabot-strategy-check, dependency-scope-fix, actions-workflow-hardening, issue-to-ticket, secret-alert-triage, dependabot-alert-triage, code-scanning-triage, release-steward, repo-onboarding, label-fix-tests, label-repro, label-review, new-package-review, ai-capability-review, egress-decision, github-risk-investigation, settings-fix-ticket, deploy-failed-card, container-published-to-tea) | 2.7 | P6 | partial | 18 ported, `kls-engine/src/templates.json:1957-3708`; tests `kls-engine/src/templates.rs` (`every_trigger_event_is_one_the_poller_emits`, `every_template_dry_runs`, `the_web_catalogue_is_ported`) | n/a (no source): new-package-review, ai-capability-review, egress-decision, github-risk-investigation, settings-fix-ticket (firewall/egress/siem), deploy-failed-card (no deployment_status event), container-published-to-tea (no registry_package event). TEA publish has no output node, so the three TEA templates end in a notice |
| `github-risk` / `github-posture` sources | 2.8 | P4 | done | posture rules kls-github/src/events.rs:426-468 (public, secret scanning / push protection off, protection removed); audit-log rules :526, `poll_org_risk` :554 (hourly, poller.rs:211); RISK mapped to `github-risk`/`github-posture` kls-engine/src/forge.rs:1168; test tests.rs:1522 | GitHub only |
| P1a `HostAdvert.agent_routes` | proto | P1 | done | proto/keleustes/node/v1/node.proto:258, :275, :320 (`AgentRoute`); used for node choice kls-engine/src/route.rs:154-166, test :313-329 | Routing card itself still uses bundled/saved profile, not the advert |
| P8a `Dispatch.budget`, `Dispatch.tier` | proto | P8 | done | proto fields node.proto:369-370, :381-383; filled by the engine exec.rs:2868-2870 (`budget`, `tier`, `member_models`) | - |
| P10a `HostAttention` frame | proto | P10 | done | proto/keleustes/node/v1/node.proto:73 (rpc Attention), :176, :193, :916-940 | - |
