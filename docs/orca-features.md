# Orca (orcastrator.dev) feature inventory vs Keleustes + belai

Purpose: list every Orca feature, one capability per row, and say whether
Keleustes (the desktop Software Factory) and belai (its nodes) have it, how
well, and what to do about it. The coordinator triages rows by setting
Status and Owner.

Last updated: 2026-11-21.

## What Orca is

Orca is `orcastrator` on npm and the `orca` CLI. It is TypeScript, MIT
licensed, by happycatlabs (author ratley). The site links
`github.com/ratley/orca`, which redirects to `github.com/happycatlabs/orca`.
The project has had two products under one name:

- **Lane substrate (current, `orca/v1`).** Added 2026-07-11. It is a
  process-per-verb CLI that sends one prompt to Codex (app-server), Claude
  (`claude -p`) or Cursor (`cursor-agent`). It records durable, evidence-only
  state and returns one JSON envelope. Its own words: "a substrate for
  orchestrators, not an orchestrator". It does not plan, route or judge.
- **Graph run harness (legacy).** Feb to Jun 2026, Codex-only after
  2026-02-23. `orca "<task>"` plans a task graph and runs it with review
  cycles, validators, flows, hooks and PR commands. Master removed it on
  2026-07-13 (commit `2b0add4`, "feat!: remove legacy graph CLI"). The latest
  npm release, **0.2.25** (published 2026-07-13 21:45 UTC), was built before
  that commit and still ships both CLIs (`dist/cli/commands/{run,plan,pr,...}`
  next to `dist/cli/lane-commands.js`). Legacy rows are in area L.

The website is one page with anchor sections. It has no pricing, no paid
tier, no changelog page, no blog, no FAQ and no separate docs site:
`/docs`, `/features`, `/pricing`, `/changelog`, `/blog`, `/faq`,
`/sitemap.xml`, `/robots.txt` and `/llms.txt` all return 404. The repo has
9 GitHub releases (v0.2.17 to v0.2.25) with auto-generated notes. All of them
cover legacy work, and v0.2.25 is a bare changelog link. There are no PyPI or
crates packages and no videos. The only third-party mention found is a
pkgstats listing. The repo has 5 stars.

## Legend

| Column | Values |
|---|---|
| Have? | **yes**: Keleustes or belai does it. **partial**: some of it, or a different but overlapping mechanism. **no**: absent (grepped). **in-progress**: code exists in a workstream that is not finished (the gaps docs say so) |
| Quality vs Orca | **better** / **on par** / **weaker** / **n/a**, with a one-line reason |
| Recommendation | **skip: not relevant**, **skip: ours better**, **improve: \<what to learn\>**, **build** |
| Status | Every row starts as `unassessed`. The coordinator sets triaged / working / done / skipped |

Abbreviations in the Keleustes/belai column: `K/` = `keleustes/crates/`,
`B/` = `belai/`, `KD/` = `keleustes/docs/`.

## Sources

| Id | URL | What it is |
|---|---|---|
| SITE | https://www.orcastrator.dev/ | The one-page site. Anchors: `#get-started #mental-model #surfaces #cmd-dispatch #cmd-inspect #cmd-answer #cmd-resume #cmd-lanes #cmd-kill #cmd-agents #cmd-contract #question-flow #envelopes #exit-codes #storage #agent-skill` |
| README | https://github.com/happycatlabs/orca/blob/master/README.md | Current README (lane substrate) |
| LANES | https://github.com/happycatlabs/orca/blob/master/docs/LANES.md | Architecture/contract spec: store, locks, envelope, AX rules 1-13, adapter contract, scope fence, test tiers |
| TYPES | https://github.com/happycatlabs/orca/blob/master/src/types/lane.ts | Zod schemas (the authoritative contract) |
| SKILL | https://github.com/happycatlabs/orca/blob/master/skills/orca/SKILL.md | Operator skill shipped in the npm package |
| AXSKILL | https://github.com/happycatlabs/orca/blob/master/skills/analyze-orca-usage/SKILL.md | AX-friction analyzer skill + `scripts/analyze_orca_usage.py` |
| CODEX | https://github.com/happycatlabs/orca/blob/master/src/adapters/codex/manifest.ts | Codex manifest, caveats, declared facts (`sandbox.ts` next to it) |
| CLAUDE | https://github.com/happycatlabs/orca/blob/master/src/adapters/claude/adapter.ts | Claude adapter + manifest |
| CURSOR | https://github.com/happycatlabs/orca/blob/master/src/adapters/cursor/adapter.ts | Cursor adapter + manifest |
| CI | https://github.com/happycatlabs/orca/tree/master/.github/workflows | `codex-code-review.yml`, `claude-code-review.yml`, publish (npm OIDC) |
| NPM | https://www.npmjs.com/package/orcastrator | 0.1.0 (2026-02-19) to 0.2.25 (2026-07-13) |
| LEG | https://github.com/happycatlabs/orca/blob/445a62b/README.md | Last legacy README (graph CLI). The same code is in npm 0.2.25 |
| PLAN | https://github.com/happycatlabs/orca/blob/7b97085/docs/PLAN.md | Legacy engineering plan (OpenClaw hooks, original Claude-orchestrator design) |
| HIST | https://github.com/happycatlabs/orca/commits/master | Commits: `d44fbd0` codex-only, `2ab121d` clarification flow, `f670a51` review flows, `015e92b` lane substrate, `2b0add4` legacy removal |
| PKGSTATS | https://www.pkgstats.com/pkg:orcastrator | The only third-party listing found (describes 0.2.24 legacy) |

Keleustes and belai sources read: `KD/README.md` (repo root),
`KD/WORKSTREAMS.md`, `KD/gaps/{harnesses,harness-proto-request,belai-counterparts,builtin-agents,sf-parity,host-management}.md`,
`K/kls-app/src/{main,cli}.rs`, `K/kls-core/src/harness.rs`,
`K/kls-node/src/{lost,screen}.rs`, `K/kls-engine/src/{graph,exec,dryrun,guard}.rs`,
`proto/keleustes/node/v1/node.proto`, `B/docs/{harness-sessions,headless,finish-review,hooks,run-caps,benchmarks,session-assessment}.md`,
and `B/internal/{harnessrun,harnessdetect,harnesstail,rc/factoryharness.go}`.

Harness sessions (Claude Code, Codex, Cursor and the rest run in their own
TUI) are marked **in-progress**. `KD/gaps/harnesses.md` says "design, nothing
built". Since then the proto fields have landed (`HostAdvert.harnesses = 12`,
`Dispatch.harness = 46..49`, `service Screen`, `Board.A2APeers`), belai
`rc/factory.go:463` calls `startHarness`, and the K/ and B/ code listed in
`KD/gaps/harness-proto-request.md` exists. No acceptance test proves it end
to end yet.

## Feature table

### A. Install, distribution, docs

| # | Orca feature | Orca source | Keleustes/belai equivalent | Have? | Quality vs Orca | Recommendation | Status | Owner | Notes |
|---|---|---|---|---|---|---|---|---|---|
| O-001 | Global install from the npm registry with npm, pnpm, yarn or bun | SITE#get-started, README | `just package` builds deb, pacman and AppImage (cargo-packager); belai is a Go binary | partial | on par: native packages suit a desktop app | skip: not relevant | skipped |  | No release channel or auto-update yet (README "Not built yet") |
| O-002 | npm publish with trusted OIDC provenance | CI (`publish.yml`), HIST `9e30685` | none | no | n/a | improve: sign and attest release artifacts when a release channel exists | deferred: no release channel yet |  | |
| O-003 | Single-page CLI reference site with a "copy page" button and a copyable SKILL.md | SITE | `README.md`, `B/docs/keleustes.md` | partial | weaker: no public reference page | skip: not relevant | skipped |  | |
| O-004 | Library API: exports `LaneStore`, schemas, envelope and contract helpers, adapter registry, TS types | README "Package API", TYPES | Rust crates are internal; nothing is published as an SDK | no | n/a | skip: not relevant | skipped |  | |

### B. Dispatch and lane lifecycle

| # | Orca feature | Orca source | Keleustes/belai equivalent | Have? | Quality vs Orca | Recommendation | Status | Owner | Notes |
|---|---|---|---|---|---|---|---|---|---|
| O-005 | `dispatch --agent <codex\|claude\|cursor>`: start one native-agent turn from the CLI | SITE#cmd-dispatch, README | `keleustes sessions dispatch --node --cwd --prompt [--harness <id>]` (`K/kls-app/src/cli.rs:24`) | yes | better: picks a node in the fleet as well as the agent; vault keys, provenance, audit, kill switch | skip: ours better | skipped |  | `--harness` path is in-progress |
| O-006 | `--model` per dispatch | SITE#cmd-dispatch | `--model`, `--provider` (`cli.rs`); `Capabilities.models` | yes | on par | skip: ours better | skipped |  | Harness models must match `^[A-Za-z0-9][A-Za-z0-9._:/@-]{0,127}$` (`B/docs/harness-sessions.md`) |
| O-007 | `--cwd` working directory (default: current directory) | SITE#cmd-dispatch | `--cwd` (required); `DispatchSpec.dirs`, `workspace`, `clone` | yes | better: clone and workspace options | skip: ours better | skipped |  | |
| O-008 | `--label`: human label on the lane | SITE#cmd-dispatch | none on `DispatchSpec` (`K/kls-core/src/gateway.rs:12`); sessions get a title from belai | no | weaker | improve: add an optional title/label to dispatch so CLI callers can name sessions | done | OP-1 CLI agent-usability | `--label` rides in `Trigger.subject` (no proto change); `session.label` in the CLI, Label fact on the Sessions screen. `K/kls-app/src/cli.rs`; test `K/kls-app/tests/cold_agent.rs` |
| O-009 | `--timeout <ms>`: one deadline shared by connect, session bind and the turn; exit 5 on expiry | SITE#cmd-dispatch, LANES §2.5 | `--wait N` only waits for start or refusal (`cli.rs:43`); belai run caps bound time per run (`B/docs/run-caps.md`) | partial | weaker: the CLI has no end-to-end deadline | improve: `--timeout` on dispatch/prompt that waits for the turn to end and returns a distinct exit code | done | OP-1 CLI agent-usability | `--timeout` is one deadline over queue, delivery, bind and turn; exit 5 `timeout`, the work is not cancelled. `K/kls-app/src/ax/view.rs`; test `refusals_have_their_own_exit_codes_and_are_audited` |
| O-010 | Dispatch blocks until the turn completes, fails, is killed, times out or blocks | SITE#cmd-dispatch | `sessions dispatch --wait` returns at start; `sessions transcript --follow` streams to the end | partial | weaker for scripts: two commands where Orca uses one | improve: `--wait-for done\|ask` on dispatch | done | OP-1 CLI agent-usability | `dispatch --wait-for started\|ask\|done\|ended`. `K/kls-app/src/cli.rs`; test `cold_agent_dispatch_ask_answer_resume` |
| O-011 | Handle line printed first (lane id before any agent work), so a crash mid-run leaves an addressable id | LANES Rule 2 | `sessions dispatch` prints the session id once the node starts it (`cli.rs:240`) | partial | weaker: no id is printed before the node acks | improve: print the dispatch id right after it is queued | done | OP-1 CLI agent-usability | Handle line `{kind:handle,dispatchId}` printed and flushed once queued; every session verb takes the dispatch id. Test: the handle survives a timeout |
| O-012 | Surface `lane`: internal worker that reports back to a coordinator | SITE#surfaces | `Dispatch.kind = session\|worker\|crew`; crew steps wait on `session.ended` | yes | better: workers, crews and board claiming | skip: ours better | skipped |  | |
| O-013 | Surface `task` (Codex only): durable, named native thread a person follows in Codex Desktop through a shared `CODEX_HOME` | SITE#surfaces, CODEX caveats | Harness sessions run the native TUI and mirror its native jsonl, so the session is in Codex's own store and `codex resume <uuid>` works (`B/docs/harness-sessions.md`) | in-progress | on par: same idea without the Codex app-server; no named-thread title | improve: pass a thread or session name where the harness supports it | done | OP-2 harness hardening (harness agent) | Claude, pi and Copilot get `--name kls-<sid8>` (profile `name_flag`), goose `--name kls-<sid>`; Codex has no start-time name flag (`B/internal/harness/launch.go`) |
| O-014 | `lanes`: list every lane record, newest first | SITE#cmd-lanes | `keleustes sessions list [--node] [--open] [--limit]` | yes | better: filters, plus a Sessions screen | skip: ours better | skipped |  | |
| O-015 | One write-capable lane per working directory (guidance) | SKILL "Dispatch patterns" | belai worktrees (`B/docs/worktrees.md`), `DispatchSpec.workspace`; conflict detection (`K/kls-forge/src/conflicts.rs`) | yes | better: isolation instead of advice | skip: ours better | skipped |  | |
| O-016 | Agent sandbox per lane: Codex `workspace-write` by default, `read-only` for read-only lanes, approval policy `never` | CODEX (`sandbox.ts`) | belai sandbox, permissions and modes (`B/docs/sandbox.md`, `firewall-permissions.md`); `--mode`; harness `never_flags` refuse skip-permissions/yolo | yes | better: firewall, classifier and hooks on top | skip: ours better | skipped |  | |

### C. Observation: inspect, events, liveness

| # | Orca feature | Orca source | Keleustes/belai equivalent | Have? | Quality vs Orca | Recommendation | Status | Owner | Notes |
|---|---|---|---|---|---|---|---|---|---|
| O-017 | `inspect <lane>`: current state plus events | SITE#cmd-inspect | `keleustes sessions show` (JSON: state, open ask, accepts, usage) and `sessions transcript` | yes | on par | skip: ours better | skipped |  | |
| O-018 | `inspect --follow` | SITE#cmd-inspect | `sessions transcript --follow` | yes | on par | skip: ours better | skipped |  | |
| O-019 | `inspect --since <seq>`: only events after a seq | SITE#cmd-inspect | Entries carry `idx`, but the CLI has no `--since` | partial | weaker | improve: `transcript --since <idx>` | done | OP-1 CLI agent-usability | `sessions transcript --since <idx>` (idx > N); `entry` lines with a schema. `K/kls-app/src/cli.rs` |
| O-020 | `inspect --wait-for blocked\|done [--timeout]` (terminal states also satisfy `blocked`, so the wait never hangs) | SITE#cmd-inspect, LANES §2.5 | none | no | weaker | build: `sessions wait <id> --for ask\|ended --timeout` for scripts and agents | done | OP-1 CLI agent-usability | `sessions wait <id> --for started\|ask\|done\|ended --timeout`; the end satisfies ask. `K/kls-app/src/ax/view.rs`; cold-agent test |
| O-021 | Append-only event stream with gap-free, strictly increasing `seq` | LANES §2.1, §2.3 | Session entries with `idx`; Link frames sequenced, acked and replayed from an outbox; event bus `core.emit` | yes | better: sequenced, replayed wire plus a hash-chained audit | skip: ours better | skipped |  | |
| O-022 | Event kinds: created, agent_started, progress, question, answered, resume_started, result, failed, killed, heartbeat | LANES §2.3 | Session entries and roles; `session.*`, `harness.*`, `screen.*` event kinds | yes | on par | skip: ours better | skipped |  | |
| O-023 | `heartbeat` on first native output, so a slow cold start can be told from a hang (Cursor cold start is 30-100 s) | LANES §2.3, CURSOR caveat `cold_start` | Session heartbeats, node presence; harness Screen stream shows live output (`K/kls-node/src/screen.rs`) | partial | on par: live screen output is a liveness signal | improve: a `first_output` marker on harness sessions | done | OP-2 harness hardening (harness agent) | `harness_first_output` transcript line with `startupMs`, audited `first_output` (`B/internal/harnessrun/hardening.go`); proto field in harness-proto-request.md §5 |
| O-024 | Stale-running warning: `running` while the recorded pid is dead. It warns and never auto-transitions | LANES §2.3 | Lost sessions: node away longer than `node.lost_grace_minutes` (10) ends them with reason `lost` (`K/kls-node/src/lost.rs`) | yes | better: settles waiting runs automatically | skip: ours better | skipped |  | Orca has no daemon; Keleustes does |
| O-025 | Empty-result warning: `completed` with empty text is flagged as possibly not done | LANES §2.3 | belai finish review checks the tree, not the text (`B/docs/finish-review.md`) | partial | on par | improve: flag sessions that end with no reply and no diff | done | OP-2 harness hardening (harness agent) | End with no reply and no work-tree change adds a `harness_empty` warning line and an `empty_result` audit fact (`hardening.go` `outcome`) |
| O-026 | Per-turn usage (`inputTokens`, `outputTokens`, `costUsd`) | LANES §2.3 | `SessionUpsert.usage`, `SessionEnd.usage/turns/tool_calls`, per-model usage rows (WP-A, `KD/gaps/belai-counterparts.md`); harness usage read from native transcripts | yes | better: rolled up for budgets | skip: ours better | skipped |  | |
| O-027 | Harness-measured timing: `wallMs`, `startupMs` split out from `apiMs`, never self-reported | LANES Rule 6 | No wall, startup or API split on the wire (grep `wall_ms\|startup` in node.proto: none) | no | weaker | improve: record dispatch→start and start→first-output latency per node and harness | done | OP-2 harness hardening (harness agent) | Runner-measured startupMs, readyMs, firstReplyMs, wallMs travel on SessionEnd.timing (proto, §5) and are stored with the session end facts; the picker shows p50/p95 computed from those stored facts per node and harness (`kls_core::harness::harness_startup`, `apply_startup`), falling back to the node advert; also feeds O-033 |
| O-028 | Native liveness snapshot through `adapter.inspect` (nativeStatus, lastActivityAt) | LANES §4 | Session `updated_at`, presence, `HostStatus` (WP-B) | yes | on par | skip: ours better | skipped |  | |

### D. Questions, answers, resume, continuity

| # | Orca feature | Orca source | Keleustes/belai equivalent | Have? | Quality vs Orca | Recommendation | Status | Owner | Notes |
|---|---|---|---|---|---|---|---|---|---|
| O-029 | Blocked questions: `status:blocked` with `blocked.questions[{id,question,options}]`; blocked counts as success (exit 0) | SITE#question-flow, LANES Rule 4 | belai sessions: structured `pending_ask`, clarify questionnaires, per-option buttons (WP-A); harness first-run dialogs become asks (`B/docs/harness-sessions.md`) | yes | better: permission asks, clarify forms and plan review, all audited | skip: ours better | skipped |  | |
| O-030 | Codex question parking through `request_user_input`, with steering in developerInstructions (best effort) | CODEX caveat `questions_best_effort` | Harness sessions only turn first-run dialogs into asks; there are no mid-task question asks for third-party harnesses | in-progress | weaker for harnesses; better for belai | improve: map Codex and Claude permission or question prompts on the screen to asks | done | OP-2 harness hardening (harness agent) | The PTY screen runs through a VT emulator (charmbracelet/x/vt v0.1.0, MIT) into a grid (`B/internal/harnessrun/grid.go`). Codex approval ("Would you like to run the following command?", y / p / esc) and Claude tool permission (1 / 2 / esc) become asks; tests replay recorded real screens (`testdata/codex-approval.raw`, `claude-permission.raw`); checked with real Codex 0.164.0 (allow_once ran the command) and Claude 2.1.330. A final reply that asks the owner a question (from the native transcript; conservative: its last paragraph) becomes a free-text "question" ask, and the answer is typed as the next prompt (origin owner-answer, kill switch, audit); tests replay recorded Claude and Codex transcripts; checked with real Claude |
| O-031 | `answer <lane> <text>` | SITE#cmd-answer | `keleustes sessions answer <id> <option> [--payload]` | yes | better: typed answers | skip: ours better | skipped |  | |
| O-032 | Answer generations: a replaced answer is never deleted by consumption of the old one | LANES §2.1 | Inbox items with ids and `InboxAck`; ask ids | yes | on par | skip: ours better | skipped |  | |
| O-033 | Live vs parked question: `next[]` says whether to wait or to resume | LANES Rule 5 | none (no follow-up hints) | no | weaker | improve: see O-047 | done | OP-2 harness hardening (harness agent) | Dialog asks carry `live:true` while the harness waits on the screen; on harness exit an open ask is closed as `expired` (`B/internal/harnessrun/dialogs.go`) |
| O-034 | `resume <lane> <prompt>`: a new turn on the same native session; completed lanes may resume | SITE#cmd-resume | `sessions prompt` for live sessions; `dispatch --harness --resume <native id>` for ended harness sessions; belai `-resume`/`-continue`; teleport | yes | better: teleport across hosts | skip: ours better | skipped |  | |
| O-035 | Continuity verification on resume: `thread-id-match`, `session-id-match`, `nonce-echo`; fail loud with `continuity_unverified` (exit 4) | LANES Rule 7, §4.1 | none: harness resume checks the native id shape only (grep `continuity`: no hits) | no | weaker | build: after a harness resume, check that the native transcript file or session id is the one recorded; refuse otherwise | done | OP-2 harness hardening (harness agent) | Resume offered only with a continuity method; pre-launch file check and session-id-match on new native records or the prompt hook; mismatch or no evidence stops with `continuity_unverified` (tests: TestResume*); resume checked with real Claude |
| O-036 | Resume refused while a live question poller is attached (no two drivers on one session) | LANES §2.5 | One rc daemon owns each session; inbox is serialized | yes | on par | skip: ours better | skipped |  | |
| O-037 | Ownerless-running reclaim: a resume may take over a lane whose resumer pid is dead | LANES §2.5 | Lost-session sweep (`lost.rs`), teleport | partial | on par | skip: ours better | skipped |  | |
| O-038 | Failed, killed and lost lanes cannot be reopened | LANES §2.5 | Ended sessions stay ended; resume starts a new harness session | yes | on par | skip: ours better | skipped |  | |

### E. Kill and process control

| # | Orca feature | Orca source | Keleustes/belai equivalent | Have? | Quality vs Orca | Recommendation | Status | Owner | Notes |
|---|---|---|---|---|---|---|---|---|---|
| O-039 | `kill <lane>`: SIGTERM to the process group, wait 5 s, SIGKILL, verify it is gone, then record `killed` | SITE#cmd-kill, LANES Rule 12 | `sessions command <id> stop`; harness stop types the exit sequence, then SIGTERM and SIGKILL to the group (`B/docs/harness-sessions.md`) | yes | on par | improve: report whether termination was verified on the session end | done | OP-2 harness hardening (harness agent) | End line and meta record `termination verified\|unverified` (kill -0 on the process group after exit); audit `stopped` carries `state` |
| O-040 | Kill is idempotent on killed lanes; on settled lanes it is `invalid_state` | SITE#cmd-kill | Stop on an ended session | partial | n/a | skip: not relevant | skipped |  | |
| O-041 | Spawn-time process identity recorded before protocol init, so a pre-protocol hang can be killed | LANES §2.1 | Harness runs as its own session leader (`harnessrun/pty_unix.go`) | partial | on par | skip: ours better | skipped |  | |
| O-042 | Forced kill of an abandoned Codex app-server after `cleanupGraceMs`, with the outcome reported | LANES §2.5 | Harness stop escalates to SIGKILL | partial | on par | skip: not relevant | skipped |  | Orca's Codex adapter uses app-server; ours uses the TUI |
| O-043 | Output caps (4 MB stdout line, 256 KB stderr tail); going over tree-kills and fails `adapter_error` | LANES §2.5 | Screen replay 256 KiB (`screen.rs`, `harness_screen.rs`), headless caps (`B/docs/headless.md`) | yes | on par | skip: ours better | skipped |  | |
| O-044 | A post-deadline exit 0 is not laundered into success | LANES §2.5 | belai run caps end the run (`B/docs/run-caps.md`) | partial | on par | skip: not relevant | skipped |  | |
| O-045 | Factory-wide stop (no Orca equivalent; listed for contrast) | n/a | Kill switch, pause and stop, A2A blackout (`KD/WORKSTREAMS.md` rule 3) | yes | better | skip: ours better | skipped |  | Not an Orca feature; row kept so kill coverage reads complete |

### F. Adapters and capability manifests

| # | Orca feature | Orca source | Keleustes/belai equivalent | Have? | Quality vs Orca | Recommendation | Status | Owner | Notes |
|---|---|---|---|---|---|---|---|---|---|
| O-046 | `agents`: live adapter manifests (resume, kill, questions, continuityMethods, models, browserUse, worktrees, caveats, overhead) | SITE#cmd-agents | `keleustes sessions harnesses <node>`; `HostAdvert.harnesses`; `Capabilities.{dispatch_kinds,job_kinds,models,can_clone,harness_sessions}` | in-progress | on par: per node, which Orca cannot do | improve: add per-harness capability flags (resume, asks, worktrees) to `Harness` | done | OP-2 harness hardening (harness agent) | harnessdetect advertises kill, asks, named, continuity; resume only with continuity (`B/internal/harnessdetect/detect.go`); `kls_store::HarnessInfo` fields added; proto mapping requested (§5) |
| O-047 | Machine-readable caveats `{code,note}` per adapter (e.g. `default_permissions_deny_writes`, `cold_start`, `silent_resume_rebind`, `posix_only`) | CODEX, CLAUDE, CURSOR | harnessdetect states `launch\|unverified\|broken\|gui\| done | partial | weaker | improve: ship caveat codes in launch profiles and show them in the harness picker | done | OP-2 harness hardening (harness agent) | `caveats: [{code, note}]` in launch.json are advertised as `Harness.caveats` (proto, §5), stored in `HarnessInfo.caveats` and shown in the picker with their codes on hover (`K/kls-ui/src/screens/harness_picker.rs`) |
| O-048 | Measured startup overhead in the manifest (`startupMsP50/P95`, `measuredAt`) and Codex prompt-token overhead (~24k) | CODEX `CODEX_DECLARED_FACTS` | none | no | weaker | improve: measure per host (see O-027) and show it in the picker | done | OP-2 harness hardening (harness agent) | p50/p95/samples/measuredAt of the last 50 startups per node and harness from timing.jsonl, shown in the picker as "~1.6 s start (p95 4.0 s, n runs)" |
| O-049 | Codex adapter through the Codex app-server (`@happycatlabs/codex-client`), thread-id continuity | CODEX, README | Codex harness profile runs the interactive `codex` TUI in a PTY; `jsonl-codex` tail | in-progress | n/a: a deliberate choice (interactive TUI, never `exec`) | skip: not relevant | skipped |  | `KD/gaps/harnesses.md` ask 2 requires interactive use |
| O-050 | Claude adapter: one-shot `claude -p`, session-id continuity, writes denied by default | CLAUDE | Claude Code profile: `claude --session-id <uuid>` interactive, hooks through `--settings`, A2A MCP bridge | in-progress | better: interactive with hooks, A2A and resume | skip: ours better | skipped |  | |
| O-051 | Cursor adapter: `cursor-agent`, chat continuity, worktree support, known model list | CURSOR | Cursor in the harness table at conf "M" (vendor docs, not checked on a host) | in-progress | weaker until it is verified | improve: verify the Cursor launch profile and its `store.db` tail | blocked: cursor-agent not installed | OP-2 harness hardening (harness agent) | Not verifiable here: cursor-agent is not installed on this host. Profile stays conf M with caveats cold_start, silent_resume_rebind (resume not offered), screen_only, not_checked_on_host; the chat store (Orca: ~/.cursor/chats/<md5(realpath cwd)>/<id>/meta.json) is not tailed |
| O-052 | Model list per adapter (Claude `fable/opus/sonnet/haiku`; a Cursor snapshot pointing at `agent --list-models`) | CLAUDE, CURSOR | `Capabilities.models`; belai `modelfetch`/`models` | yes | on par | skip: ours better | skipped |  | |
| O-053 | `browserUse` capability ("available via config" for Codex) | CODEX | none advertised | no | n/a | skip: not relevant | skipped |  | |
| O-054 | Codex `structuredOutput` (turn/start `outputSchema`) declared | CODEX | belai headless `-json-schema FILE` with a repair turn (`B/docs/headless.md`) | yes | better: checked against the schema | skip: ours better | skipped |  | |
| O-055 | POSIX-only kill declared; `kill` reported false on Windows | CODEX `posix_only` | `Capabilities.harness_sessions` false on Windows (node.proto:288); `pty_windows.go` | yes | on par | skip: ours better | skipped |  | |
| O-056 | Adapter interface: `dispatch`/`resume`/`inspect`/`capabilities`; adapters never write the store | LANES §4, Rule 10 | Launch profiles (`B/internal/harness`, `tools/gen-harnesses/launch.json`) plus `harnessrun`; belai is the only writer to Keleustes | in-progress | better: data-driven profiles for 84 harnesses, against 3 hand-coded adapters | skip: ours better | skipped |  | |
| O-057 | Third-party harness coverage: 3 agents | README | 84-harness registry; T1/T2 launch facts for about 26 (`KD/gaps/harnesses.md` B3) | in-progress | better | skip: ours better | skipped |  | |

### G. Machine-readable contract and agent experience (AX)

| # | Orca feature | Orca source | Keleustes/belai equivalent | Have? | Quality vs Orca | Recommendation | Status | Owner | Notes |
|---|---|---|---|---|---|---|---|---|---|
| O-058 | `contract [--schema envelope\|event\|manifest]`: versioned JSON Schemas, exit-code table and verb synopses from the binary | SITE#cmd-contract | none for the `keleustes` CLI; the node protocol is protobuf (`node.proto`) | no | weaker for agent callers | build: `keleustes contract` (JSON schemas of CLI outputs) if agents will drive the CLI | done | OP-1 CLI agent-usability | `keleustes contract [--schema envelope\|handle\|entry\|session\|finding] [--skill]`, built from the clap tree. `K/kls-app/src/ax/contract.rs` |
| O-059 | Every verb prints exactly one JSON envelope as the last stdout line, schema-checked, on success and on failure | LANES Rule 1 | Keleustes CLI prints tab-separated text; only `sessions show` prints JSON (`cli.rs:333`). belai `-output-format json\|stream-json` (`B/docs/headless.md`) | partial | weaker (Keleustes); on par (belai headless) | improve: `--json` on every `keleustes sessions` and `nodes` verb | done | OP-1 CLI agent-usability | Global `--json` on every verb, schema-checked before print (`K/kls-app/src/ax/{mod,schema}.rs`); forge/host wrapped generically (`KD/gaps/requests-ax.md`) |
| O-060 | Exit codes with meaning (0 ok incl. blocked, 2 usage, 3 agent, 4 state/not found/continuity, 5 timeout); never 0 with `ok:false` | SITE#exit-codes | Keleustes CLI: anyhow errors exit 1; belai headless has its own exit-code table | partial | weaker (Keleustes) | improve: distinct exit codes for refused (kill switch), not found and timeout | done | OP-1 CLI agent-usability | 0 ok, 1 failed, 2 usage, 3 node/agent, 4 not found/state, 5 timeout, 6 kill switch, 7 policy; derived in `ax::exit_of`. Tests `exit_codes_follow_the_envelope`, `refusals_have_their_own_exit_codes_and_are_audited` |
| O-061 | Three separate status axes: `delivery` (ack), `nativeStatus` (protocol), `semanticOutcome` (validator only) | SITE#envelopes, LANES §5 | Dispatch status (queued/started/refused), session state, workflow gates and approvals; finish review | partial | on par: all three exist but are not one envelope | improve: show delivered / agent-done / verified as separate fields on sessions and runs | done | OP-1 CLI agent-usability | delivery / agent / verified in `K/kls-core/src/sessions/axes.rs` (`session_axes`, `run_axes`); CLI `session`, Sessions screen fact. Runs screen requested (`KD/gaps/requests-ax.md`) |
| O-062 | `semanticOutcome` clamped to `unknown` without an explicit validator; nothing is inferred from prose | LANES Rule 9 | belai finish review: "No model is asked" (`B/docs/finish-review.md`); test/lint detection | yes | better: validators exist and run | skip: ours better | skipped |  | |
| O-063 | `next[]`: literal, copy-paste follow-up commands in every envelope | LANES Rule 5 | belai headless plan-review hint prints commands (`B/cmd/belai/planhint.go`); not in the Keleustes CLI | partial | weaker | improve: print next commands after `dispatch`, `answer` and refusals | done | OP-1 CLI agent-usability | next[] in every session envelope: one answer per option, wait, prompt, resume. `K/kls-app/src/ax/view.rs::next_for` |
| O-064 | `error.code` from a closed enum plus `error.remediation` | LANES Rule 8 | Refusals are evented and audited with reasons (`K/kls-engine/src/guard.rs`) | partial | on par | improve: closed refusal codes in CLI output | done | OP-1 CLI agent-usability | 16 closed codes with remediation (`K/kls-app/src/ax/mod.rs::CODES`); kill switch and session refusals classified by type |
| O-065 | stderr is not part of the contract; only stdout is | LANES §2.2 | belai headless: stdout carries only result lines | yes | on par | skip: ours better | skipped |  | |
| O-066 | `--help` is human-readable and the one documented exemption | LANES §2.2 | clap `--help` | yes | on par | skip: not relevant | skipped |  | |
| O-067 | Lane ids validated (`^lane_[0-9a-f]{8}$`) before any path is built | LANES Rule 13 | uuid v7 ids, looked up in the DB; no path is built from an id | yes | better | skip: ours better | skipped |  | |

### H. Durable state and integrity

| # | Orca feature | Orca source | Keleustes/belai equivalent | Have? | Quality vs Orca | Recommendation | Status | Owner | Notes |
|---|---|---|---|---|---|---|---|---|---|
| O-068 | File store under `${ORCA_HOME:-~/.orca}/lanes/<id>/` (`lane.json`, `events.ndjson`, `answer.txt`, `artifacts/`) | SITE#storage | SQLite or Postgres (`keleustes.db`, `--database-url`), `artifacts/` | yes | better: transactional DB | skip: ours better | skipped |  | |
| O-069 | `ORCA_HOME` isolates tests and automation from personal history | SITE#storage | `--data DIR` | yes | on par | skip: ours better | skipped |  | |
| O-070 | Interprocess lane lock: O_EXCL lockfile, owner token, stale takeover with a recovery gate | LANES §2.1 | DB transactions | yes | better: no hand-rolled lock | skip: ours better | skipped |  | |
| O-071 | Crash-torn NDJSON tail repair; fsync before rename | LANES §2.1 | DB durability | yes | better | skip: ours better | skipped |  | |
| O-072 | Crash replay: status recovered from the durable log when `lane.json` lags | LANES §2.1 | Outbox replay after reconnect | yes | on par | skip: ours better | skipped |  | |
| O-073 | CAS settlement: the first terminal status cannot change (a kill cannot be overwritten by `completed`) | LANES Rule 11 | Not verified: no explicit "terminal is immutable" guard found on session end | partial | unknown | improve: assert that `session.end` is first-writer-wins, with a test | done | OP-1 CLI agent-usability | Already first-writer-wins (`end_session ... WHERE ended_at IS NULL`); asserted by `K/kls-core/src/sessions.rs::session_end_is_first_writer_wins` |
| O-074 | Lane-produced artifacts directory | README "Durable state" | `artifacts/` (teleport bundles and evidence) | yes | on par | skip: ours better | skipped |  | |
| O-075 | Provenance and hash-chained audit (no Orca equivalent; listed for contrast) | n/a | Audit ledger, provenance on every action (`KD/WORKSTREAMS.md` rules 1-2) | yes | better | skip: ours better | skipped |  | Not an Orca feature |

### I. Agent skills, AX tooling, testing

| # | Orca feature | Orca source | Keleustes/belai equivalent | Have? | Quality vs Orca | Recommendation | Status | Owner | Notes |
|---|---|---|---|---|---|---|---|---|---|
| O-076 | Operator SKILL.md shipped in the package, telling an agent how to dispatch, route, answer and resume | SKILL, SITE#agent-skill | None for the `keleustes` CLI. belai sessions use Fleet through `belai connect nodes\|dispatch\|prompt` and A2A tools | no | weaker | build: a `keleustes` operator skill (or belai built-in) for agents that drive the factory from a shell | done | OP-1 CLI agent-usability | `KD/skills/keleustes-operator/SKILL.md`, embedded: `keleustes contract --skill` |
| O-077 | Routing rules of thumb per agent (Codex for deep work, Cursor for scoped edits, Claude for advice) | SKILL | Engine routing, profiles, SystemOne decision node (`K/kls-engine/src/route.rs`, `decision.rs`) | yes | better: evidence and profile-based | skip: ours better | skipped |  | |
| O-078 | `analyze-orca-usage`: mines `~/.orca/lanes`, sorts friction into a fixed taxonomy (interface_misread, kill_before_output, ...), dedups into a JSONL fix queue, points at transcripts | AXSKILL | `B/docs/session-assessment.md` (a one-time manual study); Keleustes audit/events | partial | weaker: no repeatable tool | improve: a script over Keleustes sessions/events that reports refusals, lost sessions, stops before first output and unanswered asks | done | OP-1 CLI agent-usability | `keleustes analyze-usage [--since] [--ax-only] [--append-findings] [--session]`: 13-kind taxonomy, JSONL dedup by findingId. `K/kls-app/src/ax/analyze.rs` + test |
| O-079 | Cold-agent AX benchmark: an agent with no docs completes dispatch→blocked→answer→resume using only `contract` output; a regression is a contract bug | LANES §7 Tier 3 | belai Harbor benchmarks measure cost and pass rate (`B/docs/benchmarks.md`) | partial | n/a | improve: add a cold-agent CLI test once O-058/O-059 exist | done | OP-1 CLI agent-usability | `K/kls-app/tests/cold_agent.rs`: fake node over mTLS; every command from `contract` or next[]; lines schema-checked |
| O-080 | Test tiers: hermetic unit, CLI integration with a fake adapter, opt-in live smoke against real agents | LANES §7 | `just check`, `just e2e` (real belai and headless Keleustes), fake gateway tests | yes | better: real two-binary e2e | skip: ours better | skipped |  | |
| O-081 | Automated PR code review in CI (Codex reusable workflow with a sticky comment; Claude review workflow) | CI | `pr-reviewer` template (`K/kls-engine/src/templates.json`); belai review (`B/docs/review.md`) | yes | on par | skip: ours better | skipped |  | |

### J. Scope fence (things Orca deliberately leaves out; Keleustes has most of them)

| # | Orca feature | Orca source | Keleustes/belai equivalent | Have? | Quality vs Orca | Recommendation | Status | Owner | Notes |
|---|---|---|---|---|---|---|---|---|---|
| O-082 | No daemon or supervisor (process per verb) | LANES §6 | Keleustes server and belai rc daemon | yes | better | skip: ours better | skipped |  | Orca lists it as out of v0 |
| O-083 | No MCP server mode | LANES §6 | belai A2A MCP bridge (`belai a2a-mcp`), belai MCP | yes | better | skip: ours better | skipped |  | |
| O-084 | No auto-routing or track-record telemetry (planned once a validator exists) | LANES §6 | Routing, budgets, delivery analytics | partial | better | skip: ours better | skipped |  | |
| O-085 | No agent-to-agent messaging (everything goes through the caller) | LANES §6 | A2A relay, peer directory, owner as participant | yes | better | skip: ours better | skipped |  | |

### L. Legacy graph run harness (npm 0.2.25 still ships it; removed on master 2026-07-13)

| # | Orca feature | Orca source | Keleustes/belai equivalent | Have? | Quality vs Orca | Recommendation | Status | Owner | Notes |
|---|---|---|---|---|---|---|---|---|---|
| O-086 | `orca "<task>"`: a planner breaks the task into a dependency graph and runs it in a persistent Codex session | LEG | Workflows are hand-built or template graphs (`K/kls-engine/src/graph.rs`); belai plan mode and plans (`B/internal/plans`) | partial | on par: Keleustes does not have the model build the graph | improve: consider "plan a task into a workflow draft" (a model job that returns a graph for review) | skipped: legacy (user: current Orca only) | | |
| O-087 | `--spec <file.md>`: run from a markdown spec | LEG | belai `-prompt -` reads stdin; cards and issues as input | partial | on par | skip: not relevant | skipped: legacy (user: current Orca only) | | |
| O-088 | `orca plan`: plan only, no execution | LEG | Workflow dry run (`K/kls-engine/src/dryrun.rs`); belai plan mode | yes | better: dry run has cost estimates | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-089 | Planning gate: a low-effort turn decides whether to plan or run directly | HIST `c4e5d23`, LEG `thinkingLevel.decision` | belai plan hint and routing (`B/cmd/belai/planhint.go`, `B/internal/modes`) | partial | on par | skip: not relevant | skipped: legacy (user: current Orca only) | | |
| O-090 | Pre-execution plan review that may edit the graph (add or remove tasks, change dependencies); `onInvalid: fail\|warn_skip` | LEG "Review cycle" | Graph validation (`validate.rs`), Approval and Gate nodes; belai plan lint (`B/internal/plans/lint.go`) | partial | on par | skip: not relevant | skipped: legacy (user: current Orca only) | | |
| O-091 | Per-task review against the spec and graph, `onFindings: auto_fix\|report_only\|fail`, `maxCycles` | LEG | belai finish review sends a completion back up to twice; Approval/Gate nodes | yes | on par | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-092 | Post-execution validators auto-detected from package.json, a review-and-fix loop, `ORCA_SKIP_VALIDATORS` | LEG | belai `testdetect`, `lintdetect`, `lintloop`, `testpass`; `-no-finish-review` | yes | better: language-agnostic detection | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-093 | Multi-agent mode: dependency-ready waves run in parallel, `maxParallelTasks` (writes `multi_agent=true` to global `~/.codex/config.toml`) | LEG | Engine concurrency per graph plus `autonomy.concurrency`; crews | yes | better: no global config side effect | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-094 | Retry policy: permanent vs transient errors, `maxRetries` | LEG, `src/core/retry-policy.ts` @445a62b | Engine retries with backoff (`exec.rs:366`), no-host retry | yes | on par | skip: ours better | skipped: legacy (user: current Orca only) | | Orca classifies schema/validation errors as permanent |
| O-095 | `resume --last`: retry from the last checkpoint | LEG | Engine checkpoints, belai checkpoints (`B/internal/checkpoint`) | yes | on par | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-096 | `cancel` a run | LEG | `ops::cancel_run`; label removal and `sf:stop` cancel runs (`exec.rs:1169`) | yes | better | skip: ours better | skipped: legacy (user: current Orca only) | | No `keleustes runs` CLI verb |
| O-097 | `status [--last\|--run]` and `list`, with an interactive run picker on a TTY | LEG | Runs screen (UI); no runs CLI (`K/kls-app/src/main.rs`) | partial | weaker in a shell | improve: `keleustes runs list\|show\|cancel` | skipped: legacy (user: current Orca only) | | |
| O-098 | `waiting_for_answer` run state plus `orca answer <run>` | LEG, HIST `2ab121d` | Approvals and asks | yes | better | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-099 | `pr draft` / `pr publish` (un-draft) / `pr create` / `pr status` (CI check rollup) | LEG | Delivery, "Draft pull request" node, Repos and Delivery screens (`K/kls-forge`) | yes | better: multiple forges (GitHub, GitLab, Gitea) | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-100 | Always works on a branch; never pushes to main or master | LEG | Draft PR delivery; forge guard (`K/kls-forge/src/forge/guard.rs`) | partial | weaker in one respect: `approve_ready` squash-merges (`KD/gaps/builtin-agents.md`) | improve: confirm a default-branch push guard exists | skipped: legacy (user: current Orca only) | | |
| O-101 | `pr.requireConfirmation` before opening a PR | LEG config | Autonomy levels and approvals (`guard.rs` `required_autonomy`) | yes | better | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-102 | Config layering: `~/.orca/config.ts`, then `./orca.config.ts`, then `--config` | LEG | Settings YAML, project overrides (`project_overrides.rs`); belai settings layers | yes | better: a project cannot raise sensitive settings | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-103 | Typed config helpers `defineOrcaConfig` / `defineOrcaFlow`; unknown hook keys rejected at load | LEG | Settings validation | partial | n/a | skip: not relevant | skipped: legacy (user: current Orca only) | | |
| O-104 | Flows: named presets (baseline, planning, execution, review, overrides, summary prompts), `--flow`, `flow.default` | LEG, HIST `f670a51` | Templates (`K/kls-engine/src/templates.json`: feature-from-card, bug-repro-to-fix, pr-reviewer, ci-failure-fixer, ...) | yes | better: full graphs | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-105 | `orca flows --json`: each preset's copy-paste `usage` commands and resolved `effects` (parallel agents, skills, reviews, validators), for agents | LEG, HIST `0495f8e` | Templates screen; no JSON listing | partial | weaker for agents | improve: `keleustes templates list --json` with what each one does | skipped: legacy (user: current Orca only) | | |
| O-106 | Per-stage reasoning level (`thinkingLevel.decision/planning/review/execution`) | LEG | `DispatchSpec.effort`; per-step model and effort in workflow nodes | partial | on par | skip: not relevant | skipped: legacy (user: current Orca only) | | |
| O-107 | Hooks: function and command forms, 8 events (onMilestone, onQuestion, onTaskComplete, onTaskFail, onInvalidPlan, onFindings, onComplete, onError), event JSON on stdin, `--on-*` flags | LEG | belai hooks (`B/docs/hooks.md`); Keleustes triggers ("Factory event") and Notify node; OS notifications | yes | better | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-108 | OpenClaw notification hook adapter (auto-detects the `openclaw` binary or `OPENCLAW_GATEWAY_TOKEN`) | PLAN, `src/hooks/adapters/openclaw.ts` @7b97085 | OS notifications; A2A | no | n/a | skip: not relevant | skipped: legacy (user: current Orca only) | | |
| O-109 | Skill auto-load precedence (config, `./.orca/skills`, `~/.orca/skills`, bundled `code-simplifier`); `orca skills` lists them | LEG | belai skills (`B/docs/skills.md`); Library and library search | yes | better | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-110 | Extra skill roots per cwd for the Codex app-server (`perCwdExtraUserRoots`, app-server `skills/list`) | LEG | Library distribution to nodes (WP-E) | partial | n/a | skip: not relevant | skipped: legacy (user: current Orca only) | | |
| O-111 | Injects `AGENTS.md` into planning context | LEG | belai instruction files (`B/docs/instruction-files.md`) | yes | better | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-112 | Picks the newest installed Codex binary when `ORCA_CODEX_PATH` is unset | LEG | harnessdetect: daemon PATH, then login-shell PATH, `--version` probe | partial | on par | skip: not relevant | skipped: legacy (user: current Orca only) | | |
| O-113 | Summarizes Codex MCP servers that need login once instead of streaming auth noise | LEG | none specific | no | n/a | skip: not relevant | skipped: legacy (user: current Orca only) | | |
| O-114 | `orca setup`: interactive wizard (OpenAI key, global or project config, TS or JS) | LEG | `/connect` token join, vault key entry, `keleustes token new` | yes | better | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-115 | Session logs directory (`sessionLogs`) | LEG | belai session store; mirrored transcripts | yes | better | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-116 | Summary stage writing a PR-style handoff (changes, checks, risks, follow-ups) | LEG flow `summary` | belai handoff (`B/docs/handoff.md`, `B/internal/plans/handoff.go`) | yes | on par | skip: ours better | skipped: legacy (user: current Orca only) | | |
| O-117 | Run ids `<slug>-<unix-ms>-<hex4>`; `status.json` per run under `runsDir` / `ORCA_RUNS_DIR` | LEG | uuid v7 in the DB | yes | n/a | skip: not relevant | skipped: legacy (user: current Orca only) | | |
| O-118 | Original design: Claude as orchestrator, Codex as a read-only "consultation oracle" (Claude paths dropped 2026-02-23) | PLAN, HIST `d44fbd0` | SystemOne decision node; crews | n/a | n/a | skip: not relevant | skipped: legacy (user: current Orca only) | | Historical only |

### M. Pass 2 additions (current master only)

| # | Orca feature | Orca source | Keleustes/belai equivalent | Have? | Quality vs Orca | Recommendation | Status | Owner | Notes |
|---|---|---|---|---|---|---|---|---|---|
| O-119 | Answer several parked questions in one `orca answer`: plain text for one question, or a JSON object mapping question ids to answers (optionally `{"answers":{...}}`), validated with a readable error | https://github.com/happycatlabs/orca/blob/master/src/adapters/codex/answers.ts | belai sessions: `keleustes sessions answer --payload` for clarify questionnaires (WP-A). Harness sessions: only first-run dialog options | partial | on par for belai; weaker for harnesses | improve: when harness question asks land (O-030), accept a multi-question payload | done | OP-2 harness hardening (harness agent) | Dialog answers accept `{"decision":..}`, a plain option, or `{"answers":{"<ask id>":..}}`; unknown ids and options are refused with the valid list (`dialogDecision`) |
| O-120 | Finds native session stores the way the agent does: honours `CLAUDE_CONFIG_DIR` (default `~/.claude`) and `CODEX_HOME`, and derives Claude's project key and Cursor's `md5(workspace)` chat dir from the realpath of the cwd (pinned by live probes) | https://github.com/happycatlabs/orca/blob/master/src/adapters/claude/transcript.ts, https://github.com/happycatlabs/orca/blob/master/src/adapters/cursor/chats.ts | `B/internal/harnesstail/tail.go` resolves symlinks (`EvalSymlinks`). No hit for `CLAUDE_CONFIG_DIR` or `CODEX_HOME` in `B/internal/harness*` or `B/internal/agentstore` | partial | weaker: a host with a custom config dir would tail the wrong place | improve: honour each harness's config-dir env var in its launch profile and tailer | done | OP-2 harness hardening (harness agent) | `harness.ExpandHarness` follows CLAUDE_CONFIG_DIR and CODEX_HOME in agentstore, harnesstail and login detection; project keys use the real path of the cwd. Cursor store path not built (O-051) |
| O-121 | Native protocol output is parsed against pinned schemas. Exit 0 with no final result line, a result without `session_id`, or a malformed last result fails as `adapter_error`, never as success. Claude's `is_error` beats `subtype` | https://github.com/happycatlabs/orca/blob/master/src/adapters/claude/cli-output.ts, `src/adapters/claude/__tests__/adapter.test.ts`, `src/adapters/cursor/__tests__/cursor-adapter.test.ts` | Harness tail parses native jsonl/SQLite into entries (`B/internal/harnesstail`, `B/internal/agentstore`); belai headless validates its own result (`B/docs/headless.md`) | partial | on par: different job (Keleustes mirrors a TUI, it does not settle a one-shot) | improve: when a harness session ends, flag "ended with no final assistant turn" from the native transcript instead of assuming completion | done | OP-2 harness hardening (harness agent) | A typed prompt with no final assistant turn ends the session as `adapter_error: no final assistant turn after the last prompt` (TestNoFinalTurnIsAnAdapterErrorAndEmpty) |

## Unclear about Orca (questions for the user)

1. **Which Orca?** At least five unrelated projects are called Orca:
   stablyai/orca (onorca.dev, a parallel-agents app), orca-cli/orca,
   weill-labs/orca, samirank/orca and others. This file covers only
   orcastrator.dev (happycatlabs/orca), as asked. If the user meant
   onorca.dev, which has runs, coordinator inboxes, tasks, dispatches and
   decision gates, that is a separate inventory.
2. **Legacy or current?** npm `latest` (0.2.25) still ships the legacy graph
   CLI (`orca "<task>"`, flows, reviews, PR commands). Master and the
   website describe only the lane substrate. Should area L count as "Orca
   features" for planning, or as history? It is the area closest to
   Keleustes' workflow engine.
3. **Not released.** No npm version after 0.2.25. The removal commit
   (2026-07-13) and the last push (2026-07-14) have no tag, and nothing has
   happened in the repo for about 4 months. It may be dormant.
4. **No paid tier or hosted product** was found. Nothing was hidden behind a
   login, so every row is based on public sources.
5. **Unbuilt pieces.** `semanticOutcome: validated_pass|validated_fail`
   exists in the schema, but v0 ships no validator, and the docs never say
   what a validator would look like. Track-record telemetry and auto-routing
   are only planned (LANES §6 triggers).
6. **Codex task surface visibility.** Orca's own caveat says Codex Desktop
   indexes both lane and task threads (`threadSource` is a hint the host
   normalizes). The practical difference between the surfaces is only the
   name, so O-013's value is unclear.
7. **Verifications still owed on our side** (not Orca, but needed to close
   rows): O-073 (first terminal session state wins), O-100 (default-branch
   push guard), and whether harness sessions (O-013, O-030, O-046, O-049 to
   O-051, O-056, O-057) count as landed or in-progress. `KD/gaps/harnesses.md`
   says "nothing built", but the code is now wired.

## Discovery passes

- **Pass 1 (2026-11-21):** 118 rows (O-001 to O-118). Sources: the site,
  README, LANES.md, lane.ts, both skills, the three adapters, CI workflows,
  npm 0.2.25, and the legacy README and PLAN.md.
- **Pass 2 (2026-11-21, adversarial):** **3 new rows (O-119 to O-121)**, all
  from current master. Per the user's decision, legacy-only capabilities
  were not added. These legacy-only candidates were found and left out:
  - reviewer JSON schema with a repair prompt
  - `--plan <file>` execution
  - Claude executor options (`maxTurnsPerTask`, `--claude-only`)
  - `CLAUDE.md` injection with symlink dedupe
  - skills sent as explicit `skill` input items
  - `pr-finalize`
  - the stdout hook adapter
  - release-workflow safety checks
  - setup `--check` API-key lookup through OpenClaw and Claude env files

  Sources exhausted:
  - npm: `npm view orcastrator versions` (26 versions). Tarballs unpacked
    and READMEs and file trees diffed for 0.1.0, 0.2.0, 0.2.5, 0.2.10,
    0.2.15, 0.2.20, 0.2.24 and 0.2.25.
  - CLI help from the source: every `.option`, `.command` and `addHelpText`
    in `src/cli/lane-commands.ts`. The only unlisted item is `--detach`,
    which v0 rejects with a remediation, so it is not a capability.
  - `src/lane/contract.ts` (verbs, examples, notes) and `src/types/lane.ts`
    (every enum and field).
  - Every module doc comment under `src/adapters/**`, and the test names in
    `src/cli`, `src/lane/__tests__` and `src/adapters/**/__tests__`.
  - Skill files, `.github/workflows/*`, all 17 PRs (16 closed, 1 open, all
    legacy era), all 9 GitHub releases and their notes, and every branch.
  - The site HTML (unchanged since pass 1) and all 7 `_next` JS chunks. No
    hidden sections; the bundle holds only the install-tab strings and the
    SKILL.md text.
  - No issues exist (only PRs). `docs/codex-app-server.md` and
    `docs/codex-cli-reference.md` are reference notes about Codex, not Orca
    capabilities.

  New sources have stopped producing current-Orca capabilities.
