# SF parity audit: web Software Factory vs Keleustes

Date: 2026-11-21.

- **Web** is `website/`: Vue pages `src/pages/resolve/factory-*.vue` and `vault.vue`, components `src/components/factory/*` and `src/components/vault/*`, docs in `.repo/*`. It is backed by `vdb-site/` (Go handlers and `docs/`).
- **Keleustes** is `keleustes/crates/*`.
- **Status** is done, partial or missing. n/a means the feature does not apply on a single-user desktop. "Keleustes-specific" marks something only Keleustes has.

Out of scope, not reported: sandboxes and Pix, vulnes.is sign-in, multi-tenant/org/plan gating, and email/web push. Keleustes uses OS notifications, so those were checked instead.

Covered by other auditors and only cross-referenced here: built-in agent profiles and automatic agent features, Library search, belai-side features, and non-GitHub forges.

Web line numbers are approximate where a page was read from extracted template blocks. Each section's own `### Gaps` list carries the per-area hints. The consolidated, deduplicated list is at the end. Every "missing" was grepped in the Rust workspace first.

## Summary counts (table rows, approximate)

| Area | done | partial | missing |
|---|---|---|---|
| Cockpit, Events, Board, Checkpoints | 42 | 34 | 89 |
| Workforce, Agent builder, Session, Runs, Run | 33 | 36 | 91 |
| Workflows, Templates, Repos, Budgets, Trust | 94 | 43 | 83 |
| Audit, Settings, Memory, Conflicts, Delivery pages | 69 | 52 | 52 |
| Provider Vault | 8 | 10 | 23 |
| Background behaviours (server) | 30 | 13 | 59 |
| **Total** | **276** | **188** | **397** |

Cross-cutting findings, each confirmed in code:

- **Budgets never stop anything.** `kls_core::vault::check_budget_now` (`crates/kls-core/src/vault.rs:669`) is only called from tests.
- **About 20 settings in `crates/kls-core/src/audit/config.rs` save but are never read.** For example, `autonomy.no_host_ttl_hours` defaults to 6 h, but `crates/kls-engine/src/exec.rs:298` hard-codes 1 h.
- **The vault cannot test a key.** The workspace has no HTTP client (GitHub goes through `gh`). The add-key dialog also closes before the save returns, so a validation error arrives as a toast after the typed value has been discarded (`crates/kls-ui/src/screens/budgets.rs:1199-1216`).

## /resolve/factory (Cockpit)

Web: `website/src/pages/resolve/factory.vue` (FV), `components/factory/FactoryCockpitHud.vue` (HUD), `FactoryCockpitFleet.vue`, `FactoryCockpitHealth.vue`, `FactoryFlow.vue`, `FactoryTouching.vue`, `FactoryRunsOn.vue`, `FactoryVersionSkew.vue`, `FactoryFriction.vue`, `delivery/FactoryCockpitDelivery.vue`, rules `src/shared/factory-cockpit.ts` (FC). Keleustes: `crates/kls-ui/src/screens/cockpit.rs` (CK), `crates/kls-ui/src/fleet.rs`, `crates/kls-core/src/needs.rs`, `crates/kls-core/src/board.rs`.
`control/*` (ControlPage, ControlSwitch, ControlMeter, GitHubTrafficPanel...) is the shared frame of Repos/Budgets/Trust/Settings, not the cockpit; it is covered with those pages.

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Live-stream status line ("live via event stream" / "stream down, listing" / "connecting") + "cached · refreshing" | FV:406-410 | missing | none | Keleustes is bus-driven; no visible indicator of event-tail health. |
| Scope select (managed / all / org:x) | FV:413-433, FC:77 | missing | none | Single-user, but "all managed orgs vs one org" still applies (multi GitHub orgs). |
| Time range select 24h / 7d / 30d drives KPIs + panels | FV:434-445, FC:29-37 | missing | none | Cockpit has no range at all. |
| Pause all automations + inline confirm with reason (max 500) | FV:455-538 | done | cockpit.rs:502-575 | Confirm is a window; char_limit REASON_MAX. |
| Resume automations | FV:466-472 | done | cockpit.rs:518-523 | |
| Paused banner: since, by whom, "fires held and handled on resume" | FV:594-604 | partial | cockpit.rs:142-170 | Chip shows since+reason; no "by" (single user ok) and no held-fires count. Verify engine actually queues fires while paused (kls-engine/cron.rs). |
| Stopped vs paused distinction | FV:596 | missing | none | Only `stopped` bool. |
| A2A blackout on/off + reason + banner | FV:475-492, 545-580, 606-614 | done | cockpit.rs:504-517, 152-166 | Banner lacks "since". |
| 403/409 error notices for switch actions | FV:325-354 | done | model.act error path | n/a single user. |
| "New workflow" link in bar | FV:494, 392 | missing | none | One-click to Workflows new draft. |
| "Factory API not available" notice | FV:617-623 | n/a | — | Local core always present. |
| KPI tile: Tokens today (raw count, split pix-fast/smart/vault/runs, or "N of M" budget bar + projection pill) | FC:465-466, 549; doc software-factory.md:144 | missing | none | Token data exists in kls-store (sessions/engine) but no tile. |
| KPI tile: Est. spend today (hidden when not metered) | FC:467, 551 | missing | none | |
| KPI tile: System One calls vs daily cap, split by model | FC:468, 556 | missing | none | System One belongs to belai/agents auditors; tile itself missing. |
| KPI tile: Working | FC:469 | partial | cockpit.rs:230-245 | "Live sessions" tile approximates; runs in progress not counted. |
| KPI tile: Done today | FC:471 | missing | none | |
| KPI tile: Free worker slots (links Fleet) | FC:471, 573 | missing | none | Nodes online tile only. |
| KPI tile: Draft PRs opened | FC:472 | missing | none | gh_pulls mirror has the data. |
| KPI tile: Gates holding | FC:473 | partial | cockpit.rs:259-266 | "Approvals" tile counts pending approvals only, not gates. |
| KPI tile: Firewall blocks | FC:474 | n/a | — | Cloud firewall (out of scope). |
| KPI tile: GitHub quota with source stamp | FC:475, 581; FV:690-695 | missing | none (quota shown in repos.rs) | kls-github QuotaView exists; not on cockpit. |
| KPI tile: Cache hit rate | FC:476, 589 | missing | none | |
| KPI tile: merge conflicts (open PRs conflicting with base) | FV:698-706 | missing | none | gh_pulls.mergeable=="conflict" is stored. |
| Extra Keleustes tiles: Nodes online, Needs you, In review, Open cards | — | done (extra) | cockpit.rs:229-285 | |
| KPI tiles link to their page/section | FV:653-660 | done | cockpit.rs:286-291 | |
| KPI wall balancing (even rows ≤8) | FC kpiColumns | done | kit::Grid | |
| KPI read failure + Try again | FV:640-650 | partial | cockpit.rs:124-128 | Error shown, no retry. |
| Needs you list, oldest first, label/text/detail/age, primary + secondary action | FV:717-795, FC:654-690 | partial | cockpit.rs:300-340, 581-627 | One action button; no secondary link, no detail/note line. |
| Needs you: counts by kind chips (link into Checkpoints filtered) | FV:738-753 | missing | none | |
| Needs you: segmented filter (?need=all/conflicts/signing/other) | FV:756-764 | missing | none | |
| Needs you: "Events that waited on a person" link | FV:731-735 | missing | none | Events has no `needs` filter. |
| Needs you: "View all N in Checkpoints" | FV:808-814 | done | cockpit.rs:306 ("Open checkpoints") | No count in label. |
| Needs you includes signing-key-dropped rows | FV:357-360 | missing | none | Signing is covered by vault/settings auditor; note cross-ref. |
| Needs you row opens where it lives (run/session/card/checkpoint row) | FC:654-678 | done | cockpit.rs:36-60 | PR/card rows go to Checkpoints row: verify; target() sends card kinds to Board. |
| Section nav HUD/Fleet/Health/Delivery + "Token use and budgets" link | FV:820-840 | missing | none | Single scroll page in Keleustes without anchors. |
| Lazy section load when scrolled near | FV:101-118 | n/a | — | Native app; acceptable. |
| HUD: control plane (inputs: managed orgs, webhook events, schedules, finding triggers; outputs: draft PRs, evidence; providers; persistence; ledger) | HUD:82-247 | missing | none | Sandbox / cloud firewall parts out of scope; the inputs/outputs summary (orgs, schedules, draft PRs, providers) still applies locally. |
| HUD: hosts seen in last 24h with online/kind/line | HUD:179-195 | partial | cockpit.rs:388-440 (Fleet card) | Keleustes lists nodes; no 24h window. |
| HUD: Running now — workflow lanes (run, repo, host kind, per-node state glyphs, tokens, %), live sessions (title, repo, profile, host, started) , +N sessions | HUD:250-365 | partial | cockpit.rs:345-385, 629-667 | Only live sessions; no workflow-run lanes with node states/progress/tokens. |
| HUD: links Runs tab / Sessions tab | HUD:255-262 | partial | cockpit.rs:350 | "All sessions" only. |
| HUD: System One decisions table (time, node, model, outcome, conf, action, link to run/session), live-pushed | HUD:367-460; FV:205-220 | missing | none | |
| HUD: Token economy 14-day bars (pix-fast/smart/vault), allowance %, cache hit, most expensive workflow, link Budgets | HUD:462-545 | missing | none | budgets.rs has usage; not on cockpit. |
| HUD: "When the factory works" heatmap runs by hour × day, held-for-maintenance cells, tz | HUD:547-593 | missing | none | |
| HUD: Gates and provenance (SLSA L3 count, TEA releases today, quality gate holds, awaiting human, hold rows with Inspect/CTA: merge readiness, plan approval, release jail) | HUD:595-670, FC:1030-1075 | missing | none | trust.rs has provenance; no cockpit panel. |
| HUD: Event stream tail (10, decisions excluded) + link | HUD:672-700 | done | cockpit.rs:443-495 | Check decision exclusion. |
| HUD: Touching (top repos, vulns, domains, files written by distinct sessions) | FactoryTouching.vue:36 | missing | none | Needs a fact index over sessions. |
| Fleet: project rail with hosts and free slots | FactoryCockpitFleet.vue:1-20, 164-200 | missing | none | fleet.rs is a node admin table. |
| Fleet: "who can run this repo" eligibility per host with reasons | FactoryCockpitFleet.vue:12-14 | missing | none (backend `eligible()` in kls-node/src/gateway.rs:72) | Logic exists, not surfaced. |
| Fleet graph (workflow / hosts modes, search, show filter, node inspector, pause/resume, start crew) | FactoryCockpitFleet.vue:296-460 | missing | none | |
| Fleet: Workers table (worker, profile, state, host, card, run) | FactoryCockpitFleet.vue:466 | missing | none | |
| Fleet: Runs on matrix (model/mode/effort/profile × host, ?by=) | FactoryRunsOn.vue:62 | missing | none | |
| Fleet: node list status/name/host/last seen/capabilities, add/rename/revoke | — | done (extra) | fleet.rs:52-390 | Separate screen. |
| Health: Hosts with online / late heartbeat / offline, heartbeat age, free slots, link to host + host events | FactoryCockpitHealth.vue:44 | partial | fleet.rs:115-140 | Only online/offline; no "late" state, no per-host events link. |
| Health: Flow (queue stages: open, growing glyph, arrivals vs completions, sparkline, oldest item link; range today..mtd) | FactoryFlow.vue:44; shared/factory-flow.ts:15-19 | missing | none | |
| Health: Version skew (belai version, OS per host) | FactoryVersionSkew.vue:47 | missing | none | node.host has os; version not compared. |
| Health: Friction (tool errors, failed/interrupted turns, denied permissions by tool/model/host) | FactoryFriction.vue:58 | missing | none | |
| Delivery section in cockpit with own range (drange), hidden when off | FV:150-160, 960-990; FactoryCockpitDelivery.vue | partial | screens/delivery.rs | Separate Delivery screen covers reports; no cockpit summary block. Detail by delivery auditor. |
| Live updates: summary/facts/flow/fleet reloads coalesced from event stream | FV:224-247 | done | crate::loader::Loader (event-keyed) | |
| 30s local clock for ages | FV:262 | done | cockpit.rs (now_ms each frame) | |
| Old ?tab= links → anchors | FV:254-266 | n/a | — | |

## /resolve/factory-events (Event stream)

Web: `website/src/pages/resolve/factory-events.vue` (EV), `src/shared/factory-events.ts` (FE), `components/factory/FactoryFactFilter.vue`, `FactoryFactChips.vue`, `FactoryEventLink.vue`. Keleustes: `crates/kls-ui/src/events.rs` (KE), `crates/kls-ui/src/model.rs:45` (EVENT_WINDOW=500), `crates/kls-store/src/lib.rs:490-540`.

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Live stream, newest first | EV:1-10 | done | events.rs:135-345 | |
| Stream state indicator | EV:317-330, 455 | partial | events.rs:150-158 | Shows Live/Paused only. |
| Follow / Pause; paused buffers new events | EV:471 | done | events.rs:139-147, 236-252 | |
| "Show N new events" button | EV:761-768 | partial | events.rs:236-252 | Count shown as a card, not a click-to-apply button (Resume does it). |
| Saved views (name, select, delete; localStorage) | EV:478, 505-585 | missing | none | Store in settings table (kls-store). |
| Export JSONL of current view | EV:486 | missing | none | |
| Copy link to this view | EV:493 | missing | none | Could be "copy filter" or deep-link `keleustes://`. |
| "Opened from" origin chips (context from linking page) | EV:540-553 | missing | none | |
| Structured filter keys kind/src/run/card/agent/crew/repo/host/wf/session/budget/node/step/needs/fact (any-of within key, all-of across) | factory-links.ts FACTORY_EVENT_FILTER_KEYS | missing | none | Keleustes: one free-text box + kind-prefix chip. |
| Filter chips with remove ×, filter expression line | EV:595-612, 297 | missing | none | |
| Deep links from other pages pre-filtered (FactoryEventLink: run, card, repo, board events...) | FactoryEventLink.vue:1-4 | missing | none | `Events::focus` absent; Screen::Events gets no filter arg. |
| Fact filter (kind:value chips, facts only, live off while on, truncated note) | FactoryFactFilter.vue, EV:11-15, 333 | missing | none | |
| Time range 1h / 24h / 7d / Custom from–to | EV:621-660, FE:34-39 | missing | none | Only last 500 events in memory. |
| Back to live (from custom/around) | EV:617 | missing | none | |
| Around an entry (?around, oldest first around it) | EV:94, 761 | missing | none | |
| Facets with counts: Kind, Source, Workflow, Run, Repository, Agent, Card, Host, Session (server stats) | EV:692-730, FE:203-212 | partial | events.rs:16-27, 185-197 | Only 10 hard-coded kind prefixes, counts over loaded window. |
| Matched / total counts, rate per minute | EV:298-300 | partial | events.rs:225-230 | Hover tooltip total only. |
| Per-minute sparkline | EV:734-745 | done | events.rs:200-233 | 40 min. |
| Load older (cursor paging) + error | EV:850-858 | missing | none | store.events_after exists; no backward paging UI. |
| Row: time, kind (tone), message, subject | EV:805-845 | done | events.rs:285-345 | |
| Selected event detail pane: kind+time, message with vuln links, facts dl, seq, vuln ids, "Open from this event" links, Payload JSON | EV:869-965 | missing | none | Keleustes shows JSON only as hover tooltip; subject click navigates by kind prefix. |
| Subject click → owning screen | FE eventLinks | done | events.rs:323-332, 348-368 | Single target per kind; no multi-link list. |
| 30-day retention note + link to Audit ledger | EV:447, 969-974 | missing | none | No event pruning found in kls-store; unbounded table. |
| Private kinds dropped from URL before request | EV:14-15 | n/a | — | Local. |

## /resolve/factory-board (Board)

Web: `website/src/pages/resolve/factory-board.vue` (FB), `components/factory/board/FactoryBoardColumns.vue` (COL), `FactoryBoardReview.vue` (REV), `FactoryBoardBacklog.vue` (BL), `FactoryBoardDone.vue` (DN), `FactoryBoardTriage.vue` (TRI), `FactoryBoardConflicts.vue` (BC), `FactoryCardDrawer.vue` (DR), `FactoryBoardRail.vue` (RAIL), `FactoryBoardDialogs.vue` (DLG), `FactorySendToWorkflow.vue` (SEND), `FactorySightingPanel.vue`, `components/belai/BelaiGithubPanel.vue` (GHP), `composables/belai-kanban-triage.ts` (TRK). Keleustes: `crates/kls-ui/src/screens/board.rs` (KB), `crates/kls-core/src/board.rs`, `crates/kls-store/src/board.rs`, `crates/kls-github/src/sync.rs`.

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Tabs Board / Review / Backlog / Done / Triage / Conflicts with counts | FB:tabs, factory-links.ts FACTORY_BOARD_TABS | partial | board.rs:30-55, 296-312 | No Conflicts (sync-conflict) tab. Lists differ: Keleustes triage/backlog/todo/doing/review/done vs web backlog/review/in_progress/blocked/done; no Blocked column (Keleustes uses `blocked` label). |
| Header: GitHub stamp (issues webhook live / poll fallback / board db), lost-access count, sync-conflict count, last synced | FB header RouterLinks | missing | none | |
| "Board events" link (events filtered to cards) | FB FactoryEventLink | missing | none | |
| New card (dialog: title, status, priority, labels, assignee, repo, body) | FB startNew, DR:584-610 | done | board.rs:317, 981-1045 | No project field. |
| Columns with drag and drop | COL:796, 856 | done | board.rs:433-525 | |
| Scope rail per repo (review/blocked counts, links to tabs) | COL:1-10, 60-125, 418 | missing | none | Repo combo filter only. |
| Search filter (title, id, label, assignee) | COL:432-439 | done | board.rs:361-370, 92-105 | |
| Repository / Assignee / Label filters | COL advanced filters | done | board.rs:371-397 | |
| Sort (updated/created/priority/title, asc/desc) | — | done (extra) | board.rs:398-420 | |
| Lens: All / Needs me / Unrouted / P1 only / From a workflow (with counts) | COL:140-165 | missing | none | |
| Group by None / Repo / Workflow / Crew (swimlanes, collapse/expand) | COL:169-180, 700-780 | missing | none | |
| Show done toggle with count | COL:464-468 | missing | none | Done always shown as column. |
| Column cap 12 + "show all" | COL:183, 887 | missing | none | |
| Per-column add-card button; per-column select all | COL:806, 828 | partial | board.rs:629-640 | New card defaults to tab status only. |
| Multi-select (Select mode, Ctrl/Cmd-click) + bulk bar: move, priority, release claim, send to workflow, delete, clear | COL:476-605 | missing | none | |
| Clear filters | COL:640 | done | board.rs:424 | |
| Card tile: key, title, labels, assignee, priority, age, workflow origin, readiness ("who would take it"), saving marker | components/belai/BelaiKanbanCard.vue | partial | board.rs:527-618 | No origin line, readiness, age, or saving state. |
| Optimistic save with "saving" + rollback on refusal | doc software-factory.md:166 | partial | board.rs:820-830 | Version conflict → "Changed elsewhere / Reload"; no optimistic marker. |
| Review tab: Confirm (→Backlog) / Dismiss (→Done not-planned), Oldest first / By session order, select-all, bulk decide, keys J/K/C/X | REV:1-10, 33-36, 139-235 | partial | board.rs:714-731 | Keleustes Approve→done, Send back→doing; semantics differ; no dismiss/not-planned, no session grouping, no keys, no bulk. |
| Backlog tab: grouped by priority, step priority ▲▼ inline, readiness text, Start/Assign, modes By priority / Unrouted / Stale >Nd | BL:34-45, 74-190 | partial | board.rs:620-760 | Flat list with Pri column; no inline step, no modes. |
| Done tab: grouped by day, evidence (verified note / obsolete / none), modes Last 7 days / Verified / Obsolete / No evidence, Reopen →Review, View evidence | DN:30-40, 57-135 | partial | board.rs:620-760 | Flat list; move-to menu can reopen. |
| Triage tab: blocked cards grouped by cause (out of passes, errored, waiting on human, claim lapsed, tool failed, other) / repo / age; focused-card panel with reason and suggested fix; actions Retry/Reassign/Edit/Dismiss/Snooze 7d; bulk action; keys J/K/R/A/E/X/Z; ?cause= | TRI:1-20, 369-640; TRK:14-60, 124-280 | missing | none | Keleustes Triage tab = cards with status "triage" (intake), a different concept. |
| Sync conflicts tab: queue, per-field three-way pick (last synced/board/GitHub), merged-fields read-only, preview, J/K, ?conflict= | BC:1-20 | missing | none | Mirror is one-way (issue→card) so no conflicts arise yet. |
| Card drawer (side panel / stacked when narrow), ?card deep link | DR:1-12, 515 | done | board.rs:198-216, 248-275, 764-975 | |
| Drawer: copy link to card | DR:542 | missing | none | |
| Drawer: title, details/body edit | DR:584-610 | done | board.rs:836-848 | |
| Drawer: list (status) / priority / assignee (worker, crew or email suggestions) / labels add-remove chips / project | DR:810-975 | partial | board.rs:850-875 | Labels as comma text; assignee free text, no suggestions; no project; adds Repo + Branch. |
| Drawer: Activity tab (moves with who: session/GitHub/website, notes) | DR:412-460 | done | board.rs:946-975 | Raw kind/actor/detail JSON; not humanised. |
| Drawer: Add a note | DR:654-675 | missing | none | |
| Drawer: Sessions tab (every session that touched the card) | DR:725; BelaiCardSessions.vue | missing | none | Dispatch notes appear in activity only. |
| Drawer: terminal commands per host (wrap in ssh) | DR:729; BelaiTerminalCommands.vue | missing | none | |
| Drawer: GitHub tab — mirrored issue state, labels, comments, both-way field map, sync events, webhook/poll binding, lost access | GHP:1-12, 283-830 | missing | none | Keleustes mirrors issue→card (sync.rs:532 issue_card) but drawer shows only PR button. |
| Drawer: Sync now (priced 1-3 calls) | DR:786-792, GHP:349 | missing | none | |
| Drawer: Unlink / Unlink and keep off GitHub / Mirror this repo / Mirror this card / Open an issue anyway | GHP:362-470 | missing | none | No card→issue creation. |
| Drawer: PR context (branch copy, commits, committers, linked issues, classification "Classify now", finding/verdict) | GHP:514-713 | partial | board.rs:932-942 | Only "Pull request" open-url button. |
| Drawer: Run tab (run that works the card) + "Open run trace" | DR:425, 745-760 | missing | none | runs carry card_id (kls-engine/ops.rs:498). |
| Drawer: Assign (sheet: worker or crew) | DR:753, DLG:1-4 | partial | board.rs:1051-1128 | "Send to a node" starts a session on an rc node; no worker/crew assign. |
| Drawer: Release claim; claim facts (claimed by, branch, PR, depends on, failed attempts, continuations) | DR:1045-1100 | missing | none | Card model has no claim/lease/depends_on/attempts. |
| Drawer: "Waiting on you" held-card decision (checkpoint:* label) Approve / Reject with reason | DR:1000-1045; .repo/belai-checkpoints.md:21-37 | missing | none | No checkpoint labels on cards. |
| Drawer: merge-conflict card ledger + Retry (checkpoint:conflicts-unresolved) | DR:975-998; ConflictLedger.vue | missing | none | conflict-resolver agent is agents auditor's; ledger UI is board's. |
| Drawer: Firewall sighting panel (Allow/Deny) | FactorySightingPanel.vue | n/a | — | Cloud firewall, out of scope. |
| Drawer: Events for this card | DR:794-797 | missing | none | |
| Drawer: Delete with confirm | DR:1133, DLG:39-80 | done | board.rs:910-930 | |
| Drawer: Revert edits / Save | DR:1145-1160 | done | board.rs:876-908 | |
| Send to workflow (start a run per card, one request per repo, card without repo refused) | SEND:1-6 | missing | none | Engine accepts card_id on run start; no board entry point. |
| Right rail (no card open): waiting on you (questions, held cards, lapsed leases), machines with workers, crews with "Start on" | RAIL:1-12 | missing | none | |
| "Waiting for a machine" readiness message on cards (factory.waiting reason) | doc software-factory.md:168 | missing | none | |
| Live re-read on card/github events (400ms) | doc software-factory.md:165 | done | board.rs:186 (Loader on "card.") | github.* events not subscribed. |
| Phone layout list picker | COL:650-660 | n/a | — | Narrow layout handled. |

## /resolve/factory-checkpoints (Checkpoints)

Web: `website/src/pages/resolve/factory-checkpoints.vue` (CP), `components/factory/checkpoints/CheckpointInspector.vue` (INS), `CheckpointConfirm.vue`, `ConflictLedger.vue`, `src/shared/factory-checkpoints.ts` (FCS), `src/shared/factory-checkpoint-kinds.ts` (KINDS), doc `website/.repo/factory-checkpoints.md` (DOC). Keleustes: `crates/kls-ui/src/screens/checkpoints.rs` (KC), `crates/kls-core/src/needs.rs` (KN).

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| One queue of everything waiting on a person, most urgent kind first | CP:1-16, FCS:123 | done | needs.rs:246-340 | |
| Kind: Approval | KINDS:37 | done | needs.rs:34-36 | |
| Kind: Question (session) | KINDS:38 | done | needs.rs:44 | |
| Kind: Gate (engine) | — | done (extra) | needs.rs:38 | |
| Kind: Step waiting for a node / paused run | — | done (extra) | needs.rs:40-42 | Retry/Give up, Resume/Cancel. |
| Kind: Draft PR (pr_draft) | KINDS:44 | missing | none | Keleustes has PullReview + PullReady only. |
| Kind: PR ready for review (pr_ready) | KINDS:45 | partial | needs.rs:46-48 | PullReview (unreviewed) / PullReady (approved, green) — different cut. |
| Kind: Blocked card | KINDS:46 | done | needs.rs:51 | via `blocked` label. |
| Kind: Held card (checkpoint:* label) | KINDS:47 | missing | none | |
| Kind: Card to confirm | KINDS:41 | partial | needs.rs:50 (CardReview) | |
| Kind: Claim lapsed | KINDS:42 | missing | none | Only ledger mapping (kls-core/src/audit.rs:175). |
| Kind: Host offline | KINDS:43 | missing | none | Offline node not a checkpoint. |
| Kind: External issue with no reply from team (authorAssociation) | KINDS:48 | missing | none | |
| Kind: Deployment waiting for approval (deploy_gate, "Review on GitHub") | KINDS:40, DOC:55 | missing | none | |
| Kind: Capability held for review (firewall_review) | KINDS:39 | n/a | — | Cloud firewall. |
| Server folds linked PR/issue into its card row | DOC:47 | missing | none | |
| Kind lens with counts (+ multi-kind option from links) | CP:381-385, DOC:57 | done | checkpoints.rs:268-285 | No multi-kind set. |
| Search (title, #number, card, author; 300ms debounce, in URL) | CP:391-400 | partial | checkpoints.rs:287-292 | Local text match. |
| Repository filter | CP:405-425 | missing | none | |
| Author filter (narrows loaded rows, notice) | CP:427-445 | missing | none | |
| Clear filters | CP:452 | done | checkpoints.rs:232 | |
| Source stamp + reload | CP:359-372 | missing | none | |
| Show more (cursor paging, 50 / max 200) | CP:764, FCS:113-114 | missing | none | Unbounded list. |
| Row: status pill, title, repo, author with External marker, age (ISO title), linked card link | CP:620-700, DOC:61 | partial | checkpoints.rs:345-430 | No author/External marker; overdue marker extra. |
| Row action menu: Resolve conflicts / Open conflict card / Retry / Mark ready / Convert to draft / Update branch / Request reviewers / Files and diff / Open on GitHub / Open on board / Unblock / Approve / Reject / File a card / Open | FCS:433-449, CP:700-730 | partial | checkpoints.rs:501-565 | Has Approve/Reject/Pass/Fail/Retry/Give up/Resume/Cancel, Merge, Answer, Open session/run/pull/card, GitHub. Missing ready/draft/update branch/reviewers here (they exist in repos.rs:954-986), Unblock, File a card, Resolve conflicts. |
| Merge from checkpoint | — | done (extra) | checkpoints.rs:512-525 | Hard-codes "squash" instead of the repo's merge_method (repos.rs:986 uses settings). |
| Bulk bar for ticked PRs (ready/draft/update/reviewers, ≤50, per-failure lines) | CP:519-540, FCS:531-540 | partial | checkpoints.rs:236-262, 369 | Bulk approve/reject of decidable kinds only; kls-github bulk_pull_action exists (sync.rs:420) unused here. |
| Inline confirm under row (reviewers input ≤15, reject reason) one at a time | CP:542, 736, CheckpointConfirm.vue | partial | checkpoints.rs:487-495 | Note field in inspector; no reviewers form. |
| Inspector: PR — state, review decision, merge state, head→base branch, changed files +/- | INS:130-200 | partial | checkpoints.rs:106-143 | Facts are workflow/run/step/repo/due/node/waiting; no PR facts. |
| Inspector: PR files changed list | INS:370-420 | missing | none | Sandbox-sourced on web; Keleustes could read via kls-github. |
| Inspector: merge-conflict card status + attempt ledger (ConflictLedger) | INS:264-300, DOC:64 | missing | none | |
| Inspector: card — list, checkpoint reason / blocked note, Open on board | INS:200-230 | partial | checkpoints.rs:553-557 | Open card yes; no blocked reason. |
| Inspector: issue — number, comment count, Open on GitHub, File a card | INS:230-245, DOC:66 | missing | none | |
| Inspector: waiting item → run/session/card/host button | INS, DOC:67 | done | checkpoints.rs:527-557 | |
| Deep link ?c= opens row; unknown key notice | DOC:53 | done | checkpoints.rs:173-176 | |
| Unblock moves blocked card to Backlog with note | DOC:72 | missing | none | |
| Approve/Reject held card via labels + history note (reject → blocked, reason required) | belai-checkpoints.md:21-37 | missing | none | |
| Fork-code hold (checkpoint:fork_code) | software-factory.md:243 | missing | none | |
| Live reload on card/github/approval events (1.5s coalesce) | DOC:77-79 | done | checkpoints.rs:157-170 (Loader) | |
| OS notification when a checkpoint starts waiting | belai-checkpoints.md:70 | partial | crates/kls-app/src/tray.rs | Verify which kinds notify. |

### Gaps

- [P0] Card claim/lease model (claimed_by, lease expiry, depends_on, attempts, continuations) + Release claim + claim_lapsed checkpoint — kls-store/src/board.rs Card + kls-core/src/board.rs; surface in board.rs drawer and needs.rs.
- [P0] Held-card checkpoints (`checkpoint:*` labels): Approve/Reject with reason in drawer and Checkpoints, reject → blocked — kls-core/src/board.rs (`decide_card`), needs.rs new `CardHeld` kind, board.rs drawer.
- [P0] Board lists out of step with web (no Blocked list; triage/todo/doing vs in_progress) — align STATUSES in kls-core/src/board.rs or document the mapping; Blocked column needed for Triage.
- [P0] Triage-by-cause for blocked cards (causes, Retry/Reassign/Edit/Dismiss/Snooze, suggested fix) — new Tab in kls-ui/src/screens/board.rs, cause parser in kls-core/src/board.rs from activity notes.
- [P0] Checkpoint PR actions (ready, draft, update branch, request reviewers) + bulk PR bar — reuse kls-github sync::pull_action/bulk_pull_action in kls-ui/src/screens/checkpoints.rs.
- [P1] Merge from Checkpoints uses hard-coded squash — read repo settings.merge_method as repos.rs:986 does (checkpoints.rs:522).
- [P1] Assign card to worker/crew (Assign sheet) and Send to workflow (one run per card) — board.rs drawer + bulk; kls-engine ops start-run with card_id.
- [P1] Multi-select + bulk bar on Board (move, priority, delete, send to workflow) — kls-ui/src/screens/board.rs.
- [P1] Review tab semantics: Confirm→Backlog, Dismiss→Done `not-planned`, keys J/K/C/X — board.rs list(); kls-core/src/board.rs label helper.
- [P1] Card GitHub tab: issue state/labels/comments, Sync now, unlink, mirror card / open issue (card→issue direction) — kls-github/src/sync.rs (create issue, push fields), board.rs drawer tab.
- [P1] Drawer Add a note, Sessions tab, Run tab, Events-for-card link, copy link — board.rs; kls-core sessions by card_id, runs by card_id.
- [P1] Events structured filters (run/card/repo/node/wf/session/needs) + deep link from other screens (`Events::focus(filter)`) — kls-ui/src/events.rs; Screen::Events arg in lib.rs.
- [P1] Events time range + Load older (backward cursor) beyond the 500-event window — kls-store `events_before(id, limit)`, `events_between`; events.rs.
- [P1] Event detail pane (facts, links, payload JSON) — events.rs side panel.
- [P1] Event retention (30 days) pruning — kls-store/src/lib.rs periodic delete; kls-core start_event_tail.
- [P1] Cockpit time range + token/spend/done-today/draft-PR/free-slot/quota/cache-hit KPIs — cockpit.rs; aggregates in kls-store/src/sessions.rs + engine.rs, quota from kls-github QuotaView.
- [P1] Running now: workflow-run lanes with node states, progress, tokens — cockpit.rs from kls-store/src/engine.rs runs.
- [P1] Checkpoint kinds: draft PRs, external issues with no team reply, host (node) offline, deploy gates — kls-core/src/needs.rs (gh_pulls.draft, gh_issues authorAssociation, offline nodes, GitHub deployment reviews in kls-github).
- [P1] Checkpoints repo + author filters, PR inspector facts (review decision, merge state, branches, +/-) and files list — checkpoints.rs; kls-github read_pull files.
- [P1] Merge-conflict card ledger + Retry, and "Resolve conflicts" file-a-card from Checkpoints — kls-core/src/board.rs (ledger parser), checkpoints.rs, board.rs drawer.
- [P1] Board lens (Needs me/Unrouted/P1/From a workflow) and Group by repo/workflow/crew swimlanes — board.rs.
- [P2] Board header GitHub stamp (mirroring mode, lost access, last synced) + Board events link — board.rs header from kls-store/src/github.rs.
- [P2] Board right rail (waiting on you, machines/workers, crews Start on) — board.rs when no drawer.
- [P2] Backlog inline priority step, Unrouted/Stale modes; Done grouped by day with evidence modes + Reopen — board.rs list views.
- [P2] Sync-conflicts tab (three-way field pick) — only needed once card↔issue sync is two-way; kls-github/src/sync.rs + new board tab.
- [P2] Cockpit HUD panels: System One decisions, token economy 14d, runs-by-hour heatmap, gates & provenance, touching — cockpit.rs; data from kls-store engine/sessions, kls-core/audit/trust.rs.
- [P2] Cockpit Fleet/Health: workers table, run graph, repo eligibility (expose kls-node gateway `eligible()`), late-heartbeat state, version skew, friction, flow queues — fleet.rs / cockpit.rs.
- [P2] Cockpit Needs-you counts-by-kind chips, need filter, secondary action, "New workflow" button, stream status indicator — cockpit.rs.
- [P2] Events saved views, Export JSONL, copy view, origin chips, fact filter — events.rs; saved views in kls-store settings.
- [P2] Checkpoints paging/"show more" and source stamp — checkpoints.rs.
# Fork B: Workforce, Agent builder, Session, Runs, Run

Paths: web = `website/src/`, K = `keleustes/crates/`. "AL" = `components/belai/BelaiAgentsList.vue`, "CT" = `components/belai/BelaiCrewsTab.vue`, "AB" = `components/factory/FactoryAgentBuilder.vue`, "SV" = `components/factory/FactorySessionView.vue`.
Context: Keleustes agents/crews are local rows (`kls-store/src/vault.rs:154,185`); nothing pushes a profile or crew to a node, and there is no per-worker control RPC. That one gap is behind many of the "missing" rows below.

## /resolve/factory-workforce

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Tabs Agents / Crews (+URL `?tab=`) | pages/resolve/factory-workforce.vue:57,137 | done | kls-ui/src/screens/workforce.rs:36,206 | K adds a third Workers tab |
| Totals strip: Agents, Crews, Live workers, Needs you, Cards held, Tokens 7d, Tool calls 7d | factory-workforce.vue:126; shared/factory-workforce.ts:118-126 | partial | workforce.rs:234-246 | K shows tab counts, "N live" and "N of M nodes online" only. No Needs you, Cards held, Tokens 7d or Tool calls 7d |
| Source stamp + "updates on event stream" | factory-workforce.vue:240 | done | kls-ui/src/model.rs:2 | K refreshes on bus events. No stamp |
| Workforce events link (kind=worker,session) | factory-workforce.vue:186 | missing | none | K Events screen has no deep-link filter (`events.rs` has no `focus`) |
| Wall mode (full-screen control wall) | factory-workforce.vue:151; CT:888 | missing | none | |
| Continue your draft (local draft persistence) | factory-workforce.vue:144,171; components/factory/factory-agent-draft.ts | missing | none | K builder state goes away when the panel closes |
| New agent | factory-workforce.vue:166 | done | workforce.rs:184 | |
| Schedules / Library links | factory-workforce.vue:255-266 | partial | kls-ui/src/lib.rs:19 | Reachable from the sidebar only |
| Reload on worker/host/card/session events, no timer | factory-workforce.vue:94 | done | model.rs | |
| **Agents list** | | | | |
| Needs-you strip (attention agents, Start, Dismiss) | AL:383-416 | missing | none | |
| State filter chips with counts | AL:87,429-440 | missing | none | K has a text filter only (workforce.rs:225) |
| Find an agent (search) | AL:447 | done | workforce.rs:225,274 | |
| "Used by" workflow filter | AL:459 | missing | none | No workforce-usage query in K |
| Grouped list (attention/working/idle/inactive) | AL:482 | missing | none | K uses a flat card grid |
| Card: avatar, name, model, live count, crews | AL:500 | done | workforce.rs:302-430 | |
| Card menu: Edit / Duplicate / Delete (+ confirm) | n/a (web edits through library) | done | workforce.rs:355-373,945 | K-only extras |
| Agent detail panel (URL `?agent=`) | AL:552 | partial | workforce.rs:128-162 | Focus opens the builder, not a read-only detail |
| Pause / Resume agent's workers on hosts | AL:205,605,615 | missing | none | No worker-control dispatch in kls-node |
| Start agent on a host (target picker) | AL:229,627,653 | missing | none | Sessions "New session" with agent covers part of it (sessions.rs:971) |
| Back up a host's agent to library | AL:252,705,730 | missing | none | |
| "How it works" (claims, hands to, crews) | AL:738-828 | missing | none | Routing is raw JSON (workforce.rs:862) |
| Used by workflows chips → workflow | AL:851 | missing | none | |
| Workers table per agent with Last log lines toggle | AL:882,948 | partial | workforce.rs:1032 | K Workers tab is per node (status, workers, agents, providers). No per-worker rows or logs |
| Reported by (hosts that report it) | AL:978 | partial | workforce.rs:302 (live_on) | |
| Install from library (inactive library agents) | AL:276 | missing | none | |
| Events for agent | AL:691 | missing | none | |
| Session / card links from a worker | AL:274-275 | missing | none | |
| **Crews (Control Center)** | | | | |
| Crew cards: faces, live counts, cards held, hosts, needs you | CT:993-1123 | partial | workforce.rs:436-530 | K card: members and live/idle. No cards held, failures or needs-you |
| Needs you strip (crew attention, Show crew) | CT:807-829 | missing | none | |
| Panes Crews / Activity / In the library | CT:337-340,856 | missing | none | |
| Search "Name, member, label or host" | CT:914 | partial | workforce.rs:225 | Name only |
| Filter toggles Working / Idle / Stopped / Has failures / Not in library | CT:922-990 | missing | none | |
| New crew | CT:896 | done | workforce.rs:191,444 | |
| Crew editor: name, description, process, members (agent, role, count) | components/belai/BelaiCrewEditor.vue | done | workforce.rs:534-680 | |
| Crew editor: claim-conflict / replica / unclaimed-label validation | composables/useBelaiAgentBuilder.ts (validateCrew, claimConflicts) | missing | none | K save does not validate |
| Back up crew / Message crew (card buttons) | CT:1104,1123 | missing | none | |
| Activity pane (recent attempts/results per crew) | CT:1186-1219 | missing | none | |
| Library pane, Install | CT:1266-1278 | missing | none | |
| Crew drawer tabs Overview / Workers / Cards held / Thread / History / Library | CT:139-145 | partial | workforce.rs:534 | K drawer = editor + "Working" list (workforce.rs:629) |
| Flow (member → member handoff by label) | CT:1450 | missing | none | |
| Start on a host / Start here | CT:1367,1602 | partial | kls-ui/src/screens/sessions.rs:1150 | New-session dialog has a crew picker |
| Message crew | CT:1377; components/belai/BelaiCrewMessage.vue | missing | none | `kls-core/src/board.rs:670 send_a2a` exists, no UI |
| Pause all / Resume all (crew) | CT:1386,1395 | missing | none | K has a factory-wide Pause all only (cockpit.rs:525) |
| Crew thread link / Events link | CT:1419,1428 | missing | none | |
| Cards held list | CT:1550 | missing | none | |
| Open in the editor (crew.json) | CT:1649 | partial | workforce.rs:534 | Form editor only, no JSON export |
| History (worker start/stop log) | CT:1684 | missing | none | |

## /resolve/factory-agent (builder)

This audit covers builder UX only. Built-in profiles and automatic agent features belong to the other auditor.

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| New / edit routes (`?new=1`, `?edit=`) | pages/resolve/factory-agent.vue; AB:706 | done | workforce.rs:128 (`new`, `edit=`) | |
| Draft from a premise (AI), cancel, reattach | AB:431,411,465,1295,1344 | partial | workforce.rs:725-780; kls-core/src/vault.rs:1092 | K drafts through a node `model` job with a 120 s timeout. No cancel button, no reattach |
| Per-field offers: Take / Drop / Undo / Take all open | AB:149-180,1285,2552,2558 | missing | none | K's `apply` overwrites the fields (workforce.rs:70) |
| Draft report (what changed, seconds) | AB:413-422 | missing | none | |
| Section nav (13 sections, error count per section, stepper) | AB:186-199,271,1381-1474 | missing | none | K is one form |
| Identity: name, display_name, description, identity, system_prompt | AB:1508 | partial | workforce.rs:785-850 | `identity` field missing |
| Personalise: palette (4 colours), generated avatar, personality | AB:1595 (BelaiCustomise) | partial | workforce.rs:872-905 | K: raw SVG paste + hashed tint. No palette or personality |
| Behaviour: mode, autonomy, max_iterations, reflection, schedule, monitor_condition | AB:1604 | missing | none | Possible only as raw `meta_json` |
| Model: provider, model, effort | AB:1733 (BelaiModelFields) | partial | workforce.rs:809-842 | Effort missing |
| Tools: groups with tri-state, read-only preset, always-on tools, known-tool check, limit_tools | AB:289-315,1761 | partial | workforce.rs:855 | Free-text CSV |
| Protections: guardrails, ask_permission (tristate) | AB:1869 | missing | none | |
| Handoffs: lists, labels, on_success/on_failure, handoff_to/labels, max_attempts, lease + Handoff contract table | AB:1928; components/factory/FactoryHandoffContract.vue | partial | workforce.rs:862 | "Routing" raw JSON only, no contract table |
| Workspace: isolation, publish | AB:2279 | missing | none | |
| Budget & memory: max passes/wall per item, memory.enabled, knowledge_paths | AB:2375 | missing | none | |
| Facts (what the agent is told) | AB:2510 (BelaiAgentFacts) | missing | none | |
| Agent files (attach, upload by sha256) | AB:2515,562 | missing | none | `kls-core/src/artifacts.rs` blobs could hold them |
| Crew section (start crew from builder, add/bump members) | AB:500-545,2524-2679 | missing | none | Crews are edited separately |
| Get the files: Markdown / JSON, Copy, Download, Share | AB:977-1002,2696-2812 | missing | none | |
| Client checks (Belai rules) | AB:1909,2889 | partial | workforce.rs:1003-1030 | K: name, prompt, routing JSON, SVG only |
| Server checks (name free, every label has a producer, tokens fit node budgets) | components/factory/FactoryAgentChecks.vue; AB:892 | missing | none | |
| Save to library (versioned) + celebration | AB:648,641,2764 | partial | workforce.rs:910-930; vault.rs:977 | K saves a versioned row ("Saved X (vN)"), but no library entry |
| Autosave (debounced) + unsaved snapshot | AB:798-843,932 | missing | none | |
| Sync / install to hosts after save | AB:769,943,1211 | missing | none | No profile push to nodes |
| Edit source (markdown import of a profile) | AB:1170 (BelaiAgentEditSource); composables/belai-profile-import | missing | none | |
| Restore / Start over | AB:1031,1072,1474 | partial | workforce.rs:934 | Cancel only |
| Used by workflows + Events link | AB:36-37 | missing | none | |
| Agent tag (avatar + display name everywhere) | components/factory/FactoryAgentTag.vue | partial | workforce.rs:1120-1160 | Avatar tile is local to workforce.rs. Sessions/runs print the raw name |

## /resolve/factory-session

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Session tab / Crew thread tab (`?tab=thread&crew=`) | pages/resolve/factory-session.vue:24; SV:1592,3038 | missing | none | No crew thread UI. `a2a_threads` exists (kls-core/src/board.rs:730) |
| Live transcript in seq order, gap refetch | SV:17-26,292 | done | sessions.rs:195,792 | |
| Transcript rendering (markdown, tool rows collapse, thinking) | components/belai/BelaiTranscript.vue | partial | sessions.rs:1223-1300 | Tool collapse and 400-line cap. Plain text, no markdown |
| Prompt composer, queued until host echoes | SV:20-22 | done | sessions.rs:729; kls-core/src/sessions.rs:297 | |
| Inbox status chips (queued/delivered/refused) | SV:20-25 | done | sessions.rs:649-690 | |
| Answer a pending question (free text) | SV:23-25 | done | sessions.rs:690 | |
| Answer with choices / multi-question / plan review answer | SV:1421-1490; components/belai/BelaiPlanView.vue:2662 | missing | none | K: single-line text only |
| Pinned asks | SV:7 | missing | none | |
| Plan view (ExitPlanMode plans, open current plan) | SV:1421-1441,2662 | missing | none | |
| Slash commands + completions in the composer | SV:47 (slashCompletions, hostCommands) | missing | none | `queue_command` takes any name (kls-core/src/sessions.rs:324), but only stop/interrupt are wired |
| Shell mode (`!cmd`) composer | SV:1223,3297 | missing | none | The "shell" role colour exists (sessions.rs:1301), no input |
| Session controls strip (mode, model, guardrails, ask, caveman, auto-commit, displays, tests, LSP) | SV:2369; components/belai/BelaiSessionControls.vue:1-6 | missing | none | Mode is set only at start (sessions.rs:1168) |
| Stop (with confirm) | SV:796-816 | done | sessions.rs:504-516 | |
| Interrupt | SV:904 | done | sessions.rs:522 | |
| Git strip: repo, branch, ahead/behind, recheck | SV:172,1776 | partial | sessions.rs:543-544 | Repo and branch shown as text |
| Branch switch (checkout request, interrupt, cancel) | SV:859-934 | missing | none | |
| Git sync toggle (rebase before turns) | SV:834 | missing | none | |
| Copy resume command | SV:940,1580 | missing | none | |
| Teleport: origin, copies, copy teleport command, push consent | SV:195,952-968 | partial | kls-ui/src/screens/audit.rs (Teleports tab); kls-core/src/audit.rs:649 | Not on the session page |
| Share link + share manager | SV:215,1696 | missing | none | Single-user. P2 at most (local export) |
| Full screen pane | SV:268-285 | missing | none | |
| Factory context card: run + step, card, crew, agent, budgets, why, PR + checks, Re-check on GitHub (priced) | components/factory/FactorySessionContextCard.vue:1-9; SV:2466 | partial | sessions.rs:560-600 | K links the card and run ids only. No step, budgets, PR/checks or recheck |
| Session facts: per-kind timeline (model, provider, mode, effort, profile, version, host) + touched (vulns, repos, PRs, commits, domains, files, commands, MCP, skills, subagents) | components/factory/FactorySessionFacts.vue:1-11; SV:2623 | missing | none | No fact index in K (trust facts in kls-store/src/audit.rs:971 are aggregates) |
| Todo panel (latest todo list + progress) | SV:2047,2488 | missing | none | |
| Kanban cards the session filed/moved/worked (open, assign) | SV:29-31,2508 | missing | none | |
| Intel: token use folded from lines | SV:211,2545 | partial | sessions.rs:1425 (usage) | Totals only |
| Session budgets | SV:2560 (BelaiSessionBudgets) | missing | none | Budgets screen exists, no per-session view |
| Timeline (board/session events) | SV:706,2578 | missing | none | |
| A2A rows: glyph, peer handle, repo/branch/PR/ticket chips, In reply to, thread expander, used repo memory | .repo/a2a-messaging.md:22-31 | partial | sessions.rs:1300 | Only the "a2a" role colour. No chips, peer link or thread |
| A2A show/hide toggle (org default + local override) | SV:239; .repo/a2a-messaging.md:30 | missing | none | |
| Crew start from session (security crew launch etc.) | SV:1318-1324,1885 | missing | none | |
| Events link filtered to session | SV:1-15 | missing | none | |
| Agent theming (colours of the agent profile) | SV:449,3120 | missing | none | |
| Compact sticky header on scroll | SV:680 | n/a | | egui layout |
| Meta line: node, dir, provider, model, mode, agent, kind, usage, entries | n/a | done | sessions.rs:540-551 | |
| New session dialog (node, trusted dir, task, model, agent/crew, mode, untrusted warning, retry) | components/belai/BelaiNewSession.vue | done | sessions.rs:971-1200 | |
| **Shared session helpers** | | | | |
| Unanswered deliveries panel (GitHub events that started nothing, templates for each) | components/factory/FactoryUnansweredPanel.vue:1-4 | missing | none | Lives on the events page. Cross-ref the events auditor |
| Review-back strip (firewall `?from=` back to card/run) | components/factory/FactoryReviewBack.vue:1-5 | n/a | | Firewall page, not SF desktop |
| Source stamp (posture/webhook/github api tiers + call count) | components/factory/FactorySourceStamp.vue:1-8 | missing | none | No data-origin pill anywhere in kls-ui |
| Priced action button ("· N GitHub calls") | components/factory/FactoryPricedAction.vue:1-7 | missing | none | Check how `kls-github/src/budget.rs` is used elsewhere |

## /resolve/factory-runs

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Tabs Workflow runs / Sessions in one page | pages/resolve/factory-runs.vue:139 | partial | kls-ui/src/screens/runs.rs, sessions.rs | Two separate screens |
| Totals: Runs today, Live sessions, Sessions 7d, Tokens 7d, Median length, Ended today | factory-runs.vue:225; shared/factory-sessions.ts:17-27 | missing | none | |
| Stream state line ("live via event stream") | factory-runs.vue:147 | missing | none | |
| Events for all runs | factory-runs.vue:206 | missing | none | |
| Run search (workflow, repo, run id, card) | factory-runs.vue:267 | done | runs.rs:279 | |
| Status filter | factory-runs.vue:274 | done | runs.rs:90-101 | Pills |
| Trigger filter | factory-runs.vue:288 | done | runs.rs:250 | |
| Workflow filter (`?workflow=`) | factory-runs.vue:104 | done | runs.rs:226 | |
| Repo filter (`?repo=`) | factory-runs.vue:105 | partial | runs.rs:279 | Search text only |
| Fact chips (run fact filters, up to 5) | factory-runs.vue:87-90,306 | missing | none | No fact index |
| Columns: Run, Workflow, Repo, Model, Trigger, Status, Sessions, Tokens, Time, Open | factory-runs.vue:324 | partial | runs.rs:320 | K: Run, Workflow, State, Trigger, Card, Started, Length, Tokens. Missing Repo, Model, Sessions count |
| Trace / Events per row | factory-runs.vue:405,410 | partial | runs.rs:385 | Trace opens in-place. No Events |
| Sessions tab: source tabs Workers / Self-hosted / Sandbox / TUI / ACP with counts | factory-runs.vue:172-174 | missing | sessions.rs:51 | K filters by state + node, not by kind (`Session.kind`). Sandbox is out of scope |
| Sessions filter (agent, crew, host, dir, name) | factory-runs.vue:465 | done | sessions.rs:84-100 | |
| Live/ended state filter | factory-runs.vue:112 | done | sessions.rs:51-69 | K adds Waiting |
| Node filter | n/a | done | sessions.rs:377 | |
| Group by crew | factory-runs.vue:121 | missing | none | |
| /tmp sessions hidden + show/hide toggle | factory-runs.vue:190, ~470-480 | missing | none | |
| Per-row teleport command copy + share link | factory-runs.vue:23-24 | missing | none | |
| Session fact chips | factory-runs.vue:494 | missing | none | |
| Session detail panel: Open session, Edit agent, Crew thread, Manage crew, Events; facts meta (provider, effort, profile, version, OS, branch, PR, commit, repo, vulns) | factory-runs.vue:573; shared/factory-sessions.ts:182-189 | partial | sessions.rs:462 | K opens the full session directly. No Edit agent, Manage crew or Events links |

## /resolve/factory-run

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Header: workflow + version link, trigger, card, repo, reason | pages/resolve/factory-run.vue:267 | done | runs.rs:468-513 | |
| Totals: Tokens, Budget used %, Est. cost, Decisions made/planned, Handoffs, Elapsed | factory-run.vue:212-224 | partial | runs.rs:508-509 | Tokens + duration only |
| Pause / Resume | factory-run.vue:172 | done | runs.rs:444-455; kls-engine/src/ops.rs:730,755 | |
| Retry, retry from a chosen step | factory-run.vue:368 | partial | runs.rs:435 | `ops::retry_run(.., from_step)` exists (ops.rs:781). UI passes `None` |
| Cancel with confirm ("Yes, cancel" / "Keep running") | factory-run.vue:407 | partial | runs.rs:426 | No confirm, fixed reason |
| Priced actions (GitHub call count on Retry/Cancel) | factory-run.vue:385,407 | missing | none | |
| Graph preview with per-node status | n/a | done | runs.rs:516-528 | K-only |
| Timeline: one row per step, opens detail | components/factory/run/RunTimeline.vue:1-6 | done | runs.rs:588,631 | |
| Decision step: question, input hash, outcome, confidence vs threshold, action, rationale | RunTimeline.vue:2-5 | missing | none | K has no decision node type. Details render as generic k/v (runs.rs:742) |
| Step output locations (S3 paths) | RunTimeline.vue:5 | partial | runs.rs:742 | details_json only. Could link blobs in kls-core/src/artifacts.rs |
| Approval inside the step: Approve / Reject with note | components/factory/run/RunApproval.vue; factory-run.vue:150 | done | runs.rs:545-580; ops.rs:893 | |
| Decided approvals list (who, note) | RunApproval.vue | done | runs.rs:600-628 | |
| Open session from step | factory-run.vue:114 | done | runs.rs:701 | |
| Evidence pack (SLSA, SBOM, image, decision ledger, gate evidence, TEA release, SARIF counts, PR checks + Re-check) | components/factory/run/RunEvidence.vue:1-6 | missing | none | |
| Download evidence | factory-run.vue:440 | missing | none | |
| Network and secrets: allow/deny + vault grant names | factory-run.vue:550-590 | missing | none | Allow/deny is sandbox (out of scope). Grant names are in scope: K has grants (kls-core/src/vault.rs:209) |
| Contacted (domains/IPs from transcripts) | factory-run.vue:594 | missing | none | Needs a fact index |
| Touched (repos, vulns, PRs, commits, files, models, tools) | factory-run.vue:614; components/factory/FactoryRunFacts.vue | missing | none | |
| Storage (prefix, folders, rows, retention) | factory-run.vue:645 | missing | none | Local equivalent would be the data-dir paths |
| Ask Pix about a failed run | factory-run.vue:22 (PixAskButton) | missing | none | Pix assistant, not a sandbox |
| Events for run | factory-run.vue:334 | missing | none | |
| Live refresh on run events | factory-run.vue:75 | done | model.rs | |

## Cockpit sub-panels in scope (HUD / Fleet / Health tabs). Cross-ref the cockpit auditor

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Touching (repos/vulns/domains/files by distinct sessions) | components/factory/FactoryTouching.vue:1-4 | missing | none | |
| Version skew (belai version/OS per host, behind/mixed) | components/factory/FactoryVersionSkew.vue:1-5 | partial | kls-ui/src/fleet.rs:169 | Fleet shows each node's belai version, with no skew flag |
| Runs on (model/mode/effort/profile × host matrix) | components/factory/FactoryRunsOn.vue:1-6 | missing | none | |
| Flow (per-stage queue depth, growing, oldest open) | components/factory/FactoryFlow.vue:1-5 | missing | none | |
| Friction (tool errors, interrupts, denied perms by tool/model/host) | components/factory/FactoryFriction.vue:1-5 | missing | none | |

### Gaps
- [P0] Agent profile → node delivery. A saved agent or crew never reaches a node, so the builder's routing, handoff and tools fields have no effect. Add a push or install RPC in `kls-proto` node.proto, served from `kls-node/src/gateway.rs`, with `kls-core::vault::save_agent` triggering the sync.
- [P0] Structured builder fields (mode, autonomy, max_iterations, effort, guardrails, ask_permission, kanban handoffs, workspace, budget/memory, knowledge_paths). Model them as typed fields in the `kls-store/src/vault.rs` AgentProfile, not `meta_json`, and give each a section form in `kls-ui/src/screens/workforce.rs::builder_ui`.
- [P1] Worker control: per-agent Pause/Resume/Start/Back up, crew Pause all/Resume all, worker log tail. Add dispatch kinds in `kls-core/src/sessions.rs` (Dispatch) and kls-node, plus buttons in `workforce.rs`.
- [P1] Crew thread / A2A UI: thread tab, Message crew, A2A rows with peer link and chips, thread expander, show/hide toggle. Build it on `kls-core/src/board.rs` `a2a_threads`/`a2a_thread`/`send_a2a`, with a Thread pane in `sessions.rs` and the drawer in `workforce.rs`.
- [P1] Session controls strip + slash commands + shell mode. `queue_command` already takes any name. Add a controls row and composer parsing (`/`, `!`) in `kls-ui/src/screens/sessions.rs::composer_ui`, sourcing commands from host caps on the Node.
- [P1] Answers with choices / plan review / pinned asks. Parse the ask meta (options, plan) from Entry meta_json in `sessions.rs::answer_ui` and send structured answers through `kls-core::sessions::queue_answer`.
- [P1] Session factory context: run step, budgets, PR + checks with recheck, plus the session's kanban cards and todo list. Add a side panel in `sessions.rs`, read from `kls-engine::ops::run_detail`, `kls-core::board` and `kls-github`.
- [P1] Per-field draft offers (Take/Drop/Undo/Take all) instead of overwrite. Keep `AgentDraft` beside the profile in `workforce.rs::Builder` and diff it per field.
- [P1] Retry from a chosen step + cancel confirm in the run trace. Add a step ComboBox feeding `ops::retry_run(.., Some(step))` and a confirm row in `kls-ui/src/screens/runs.rs` (~line 426).
- [P1] Run totals: budget %, est. cost, handoffs. Compute in `kls-engine/src/ops.rs::run_detail` from `kls-core::vault::budget_statuses` / usage rows, and show in `runs.rs::trace_body`.
- [P1] Events deep links ("Events for X" on agent, crew, session, run, runs list). Add `Events::focus(filter)` in `kls-ui/src/events.rs`, then `model.go(Screen::Events, Some("run=..."))` from each screen.
- [P1] Builder autosave / draft persistence + Continue your draft. Persist the Builder state to kls-store (settings kv) on change, and offer Continue in the workforce header.
- [P1] Workforce needs-you strip, state filter chips, crew filter toggles (Working/Idle/Stopped/Has failures). Derive them from `kls-core::vault::workers_summary` + `kls-core::needs` in `workforce.rs`.
- [P1] Crew validation (claim conflicts, unclaimed labels, replicas cap). Port validateCrew/claimConflicts into `kls-core/src/vault.rs::save_crew`.
- [P2] Fact index (model/provider/mode/effort/version timeline, touched/contacted) feeding session facts, run Touched/Contacted, fact chips on runs/sessions, and cockpit Touching/Runs on/Friction/Version skew. Add a new `kls-store` facts table folded from session entries in `kls-core/src/audit.rs` (fold_line).
- [P2] Runs list totals tiles + Repo/Model/Sessions columns. `runs.rs::table_ui`. Model comes from step sessions via `ops::run_sessions`.
- [P2] Sessions source tabs by kind (worker/tui/acp/rc), group by crew, /tmp hide toggle. `sessions.rs::list_ui` using `Session.kind`/`cwd`.
- [P2] Decision-step rendering (question, hash, confidence vs threshold, rationale). Needs an engine decision node (cross-ref the workflows auditor). Then render it in `runs.rs::step_details`.
- [P2] Evidence pack + Download evidence (SBOM/SARIF/PR checks) and vault grant names per run. Add a `kls-engine` evidence collector over `kls-core/src/artifacts.rs` blobs and a panel in `runs.rs`.
- [P2] Builder extras: Get the files (markdown/JSON export, copy), Edit source (markdown import), agent files attachments, facts, palette/personality, tool groups with read-only preset, server checks (label producers, node budget fit). Put them in `workforce.rs` + `kls-core/src/vault.rs` (reuse `library_file`/`parse_library_file` for markdown).
- [P2] Git strip actions: branch switch, git sync toggle, copy resume/teleport command; teleport on the session page. Wire `sessions.rs` to `queue_command` and `kls-core::audit::teleport_request`.
- [P2] Wall mode, full-screen session, agent theming across screens (shared avatar/tag widget in `kls-ui/src/kit.rs`), source stamps and priced-action labels (`kls-github/src/budget.rs` call estimates).
- [P2] Ask-Pix on failed run, Storage panel, Share links: low value for single-user desktop. Consider a local "Reveal in files" for run blobs instead.
## /resolve/factory-workflows (list + builder)

Web: `website/src/pages/resolve/factory-workflows.vue` (FW), `components/factory/workflow/WorkflowFlow.vue` (WF), `WorkflowInspector.vue` (WI), `WorkflowPalette.vue` (WP), `shared/factory-workflow-fields.ts` (FF). Keleustes: `crates/kls-ui/src/screens/workflows.rs` (KW), `crates/kls-engine/src/{graph,validate,dryrun,ops,cron}.rs`.

### List (no ?id)

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Workflow list table | FW:882 | done | KW:316-460 | Columns differ, see below |
| Columns On / Workflow / Version / Triggers / Repos / Runs 7d / Updated | FW:884-892 | partial | KW:376 | KW has Workflow, State, Version, Last run, Updated. Missing: trigger summary column, repo count, runs in 7 days |
| On/off switch per row (disabled when unpublished/archived/blocked) | FW:929 | missing | none | KW only toggles enabled inside the builder header (KW:877) |
| Origin filter (All / Mine / Defaults) segmented control | FW:868, FW:108-121 | missing | none | KW has a free-text filter instead (KW:320) |
| Topic sections (collapsible groups, "N of M on" summary, Core open) | FW:900-920, FW:124-135 | missing | none | Tied to the 42 defaults |
| "Default" and "Customised" badges | FW:949-960 | missing | none | No `origin: system` workflows in KW |
| "Blocked: no provider/model" line + Add-a-provider link | FW:941-947 | missing | none | KW has no blocked state on a workflow when no provider key / model is available |
| Create draft by name (inline form, Enter) | FW:813-835 | done | KW:466-505 | KW opens a dialog; starts with one manual trigger |
| "Start from a template" link | FW:836 | done | KW:274 | |
| ?new=1 focuses the name field | FW:980-984 | done (n/a) | KW:269 | Button opens dialog directly |
| Start a workflow from an event (?from=event: draft pre-seeded with a trigger for that event, link to matching templates) | FW:839-846, FW:985-993 | missing | none | Events screen should offer "Start a workflow from this event" |
| Source stamp / as-of | FW:999 | missing | none | Low value on desktop |
| Empty-state text per filter | FW:990 | done | KW:341 | |

### Builder header and lifecycle

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Token estimate in header | FW:737, FW:960 (estimate) | missing | none | Engine has the numbers (`dryrun.rs` totals, `templates.rs` estimate) but the header shows none |
| Download as code (FactoryWorkflow document) | FW:738-750 | missing | none | |
| View as YAML (live, unsaved included) | FW:764, FW:1276-1290 | missing | none | No YAML/JSON export of a workflow |
| Save draft | FW:783 | done | KW:808 | |
| Dry run | FW:773 | done | KW:820 | |
| Publish with confirm panel, problem count, note (200 chars) | FW:793, FW:1175-1215 | partial | KW:834 | KW publishes in one click; no note field, no "N problems will likely refuse" panel |
| "Triggers after publish" list (id, kind, runs on, sync state) | FW:1217-1232 | partial | KW:855 | KW shows "Published vN with N triggers" notice only |
| Discard draft | FW:1056 | done | KW:864 | |
| Version history table (version, state, when, by, nodes, note) | FW:1237-1272 | done | KW:1182-1222 | KW adds Restore (copy into draft) which web lacks |
| Archive workflow | FW:1590 | done | KW:923-940 | KW confirms ("Cancels its runs") |
| Rename / description | FW:1440 (name field) | done | KW:758, KW:1224 | |
| Unsaved-edits chip | FW:1035 | done | KW:105 (dirty) | |
| "Disabled" chip | FW:1038 | done | KW:877 | |
| Stale: newer version saved elsewhere, "Load it and drop my edits" | FW:1138-1145 | partial | KW:110-140 (sync/reload), KW:798 | Reload exists; no explicit stale banner (single-user, lower value) |
| 409 conflict "Reload the server copy" | FW:1146-1153 | n/a | none | Single user |
| Edit notice for a default ("saving stops it following the catalogue") | FW:1064 | missing | none | Tied to defaults |
| Blocked notice ("cannot start: no provider…", self re-enables) | FW:1079-1090 | missing | none | |
| Workflow enabled switch | FW:1098 | done | KW:877 | |
| Per-trigger on/off switches (event triggers) | FW:1113-1122, FW:157 setTrigger | missing | none | KW can only disable the whole workflow |
| Limits chips (scope, budgets, concurrency) | FW:1125-1133 | missing | none | |
| Events for this workflow link | FW:1043 | missing | none | Should jump to Events/Audit filtered by wf |
| Manual Run button | (not on web builder) | done (extra) | KW:888 | |

### Dry run

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Dry run options panel with repo picker (pick a scope repo) | FW:1293-1312 | missing | none | KW dry-runs against default repo resolution only |
| Valid / N problems line | FW:1318 | done | KW:1036 (Checks) | |
| Warnings list | FW:1320-1328 | done | KW:1036 | issues_ui shows warn level |
| Repositories line (mode + repos) | FW:1330 | done | KW:1150 | |
| Plan table: step, type, tokens range, GitHub calls range, simulated outcome + confidence, brief chars | FW:1335-1365 | partial | KW:1155-1169 | No GitHub calls column, no confidence, no type column; one line per step |
| Totals (tokens and GitHub calls) | FW:1368 | partial | KW:1151 | Tokens only |
| Unreached steps warning | (validator) | done | KW:1170 | |

### Problems

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Problems panel grouped by node, click selects node | FW:1396-1425 | done | KW:1036-1088 | |
| Issue code shown | FW:1420 | done | KW:1071 | |
| Server + local issues merged | FW:318-329 | done | `kls-engine/src/validate.rs` | Local only, by design |
| Per-node problem badge on card | WF (issues prop) | done | KW:2077 draw_node | |
| Quick fix "end run" for an outcome with no route | WI:323 | missing | none | |

### Graph / palette

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Palette groups | WP:1-90 | done | KW:946, graph.rs:350 | Groups differ (web: triggers, GitHub reads, System One, crews, agents, gates, state, outputs; KW: triggers, agents, logic, people, board, outputs) |
| Palette search box | WP:49 | missing | none | |
| GitHub reads group with per-read call cost (posture/S3/webhook/live API) | WP:3-6 | missing | none | |
| Disabled palette entries for background agents, with reason | WP:6-7 | missing | none | Cross-ref: agent profiles auditor |
| Click to add after selected; drag onto card | WP:83-85, WF:92-94 | done | KW:946, KW:2184 | KW free canvas with ports, drag nodes |
| Move up / down / delete selected card | WF:160-180 | done (different) | KW:2184, KW:1792 | Free canvas; Remove step button |
| Add approval after a step (one click) | WF:144, FW:382 | missing | none | Must add Approval from palette and wire |
| Artifact label on connector | WF:3 | done | KW:1508 | edge label |
| Decision card lists outcome routes | WF:4 | done | KW:1968 out_ports | |
| Empty workflow hint | FW:1452 | done | KW:2368 | |
| Workflow name field | FW:1440 | done | KW:758 | |

### Scope and limits (workflow settings)

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Repos scope textarea + "Scope: Any repository / N entries" help | FW:1462-1475 | done | KW:1454 | |
| Run budget tokens | FW:1478 | done | KW:1431 | |
| Month budget tokens | FW:1485 | done | KW:1439 | |
| Concurrency | FW:1492 | done | KW:1444 | |
| Stop-the-line attempts | FW:1499 | done | KW:1448 | |
| Agent budget fields (cost USD per card, finalize-at share, failed attempts, continuations, passes, wall minutes, tool rounds) | FW:1506-1520, FF:399-407 | missing | none | Cross-ref agent-features auditor; no per-card cost/turn caps in workflow limits |
| GitHub calls per run (typical, max) | FW:1522-1536 | missing | none | |

### Inspector: node kinds and fields

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Node title edit | WI:265 | done | KW:1544 | |
| Trigger kinds | FF:170-340 | partial | graph.rs:570 | KW: manual, cron, kanban, event, webhook, label. Missing firewall trigger (source/events/newTo/ecosystems/kinds/sandboxes/ticketRepo) |
| Cron: expression, timezone, describe + next 3 runs | WI:278-300 | done | KW:1554-1572 | Timezone is free text (web: list) |
| Cron: timer runs on (aws/cloudflare) | FF `runsOn` | n/a | none | Desktop runs locally |
| Overlap policy | FF `overlap` | done | KW:1574 | |
| Catch-up when missed while closed | (n/a web) | done (extra) | KW:1582 | |
| Webhook trigger: event | FF `event` | done | KW:1625 | Free text, no event list |
| Webhook conditions: actions, base/head branch, changed paths, ignored paths, labels any/all, author type, author allow/deny, drafts, origin (same repo/fork), subject, conclusion, severity, environment, package type, repo profile, repo properties, review state | FF:170-340 | missing | none | KW matches on event name only (`kls-engine/src/forge.rs:558`) |
| Debounce ("coalesce within seconds") and max runs per hour | FF `debounceSeconds`, `maxRunsPerHour` | missing | none | Poll-driven event storms can over-fire |
| Trigger priority 1-5 (fan-out cap) | firewall-sources.md "priority" | missing | none | |
| "Also start on" (extra triggers on one trigger node) | WI:514-556 | missing | none | |
| Kanban trigger from/to list | FF `fromList`,`toList` | done | KW:1592-1606 | |
| Scan node (scanners, fail on, SBOM format, source, CLI job, not-evaluable) | FF:340-360 | missing | none | KW has `job` node (generic) only |
| GitHub read node source (posture/webhook/live API/fresh clone/CLI) with cost | FF:341-346 | missing | none | |
| System One decision node (model, question, inputs, min confidence slider, below-threshold route, fallback model + threshold, on abstain, daily cap share, rationale retention) | WI:1-10, FF `adapter`..`rationaleRetention` | missing | none | KW has rule-based `condition` node (KW:1804) only |
| Decision node: last 30 days of decisions per outcome (stats) | FW:440-460, WI stats | missing | none | |
| Outcome table: add/remove outcome, route per outcome, "safest outcome" flag, below-threshold and unknown routes | WI:685-795 | partial | KW:1804-1920 | KW condition: outcomes, rules, default outcome; no safest flag / unknown route |
| Crew node: crew or agent | FF `crew` | done | KW:1638-1648 | suggest from built-in lists |
| Crew node: model select from org models + per-member model | FF `model`, `memberModels` | partial | KW:1651 | Free text, no per-member models |
| Crew node: mode | FF `mode` | done | KW:1649 | |
| Crew node: brief | FF `brief` | done | KW:1650 | |
| Crew node: CLI job, code model tier, tests.post_end, tests.on_fail, show_internal_work | FF crewFields | missing | none | |
| Crew node: max attempts before stop the line | FF `maxAttempts` | done | KW:1655 | |
| Crew node: run on (host select) + fallback if no host online | FF `runOn`, `runOnFallback` | partial | KW:1652 | Free text "auto, node:<id>"; no node picker, no fallback policy |
| Crew node: per-node agent budget fields | FF:457 | missing | none | |
| Node budget (tokens, GitHub calls) | WI:613-640 | partial | KW:1774 | Token budget only |
| Gate node: profile, release jail, on fail, check source (mirror/live), live-if-older-than, max calls, on quota low | FF:462-468 | partial | KW:1729-1733 | KW: checked by manual/forge, on fail. Rest missing |
| Human node: approvers editor (kind + name rows) | WI:564-606 | partial | KW:1704 | Comma-separated text |
| Human node: message, SLA hours, on timeout | FF `slaHours`,`onTimeout` | done | KW:1717-1726 | |
| Human node: notify via | FF `notifyVia` | n/a | none | Email/push out of scope; OS notification implied |
| Output node kinds | FF outputFields | partial | graph.rs:584 | KW: notify, draft_pr, issue_comment, pr_review, approve_ready, check_run, file_issues, add_label, merge |
| Output: notify message + severity | FF `message`,`severity` | done | KW:1751-1754 | |
| Output: send to chat connections (Slack, Teams, Discord, Google Chat, webhook) | FW:209-226, FF `connections` | missing | none | |
| Output: PR state (draft/ready), request reviewers, attach, base branch, if crew pushed no branch | FF `prState`..`ifNoBranch` | missing | none | KW GitHub action takes only `message` (KW:1761-1765) |
| Output: file issues (label, title, min severity, group, max issues, findings from) | FF | missing | none | Action exists, fields do not |
| Output: check run name / when | FF `name`,`when` | missing | none | |
| Output: SBOM/provenance publish (visibility, kind, ref) | FF `visibility`,`kind`,`ref` | missing | none | |
| State/card node | FF stateFields | done | KW:1742-1748 | |
| Wait node | (n/a web) | done (extra) | KW:1737 | |
| Next-step route select | WI:645-660 | done | KW:1475 edge_ui | Via wires |
| Handoff and persistence section | WI:822 | missing | none | |
| Notes about this step | WI:335 | missing | none | |
| IaC drift lock (read-only when managed by IaC) | FW:333 | n/a | none | |

## /resolve/factory-workflows?view=schedules

Web: `components/factory/workflow/WorkflowSchedules.vue` (WS). Keleustes: KW:624-730, `kls-engine/src/cron.rs`, `ops.rs:272`.

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| One table of all crons (workflow triggers + host agent schedules) | WS:512 | partial | KW:624-730 | KW lists workflow triggers (cron/card/event); no host agent schedules |
| Columns enabled, runs, kind, cron, runs on, where, last, next | WS:515-523 | partial | KW:666 | KW: On, Workflow, Kind, When, Next, Last; no Where (repos/host·dir) |
| Enable/disable a schedule | WS:537 | done | KW:679 | |
| Edit cron inline (re-time publishes a new version) | WS:561, WS:87-98 | missing | none | |
| Change runs-on per schedule | WS:584 | n/a | none | |
| Delete a schedule (with confirm) | WS:638-655 | missing | none | |
| New workflow schedule form (workflow, trigger node, cron, presets, tz, overlap, preview next 3) | WS:304, WS:345-460 | missing | none | |
| New agent schedule via host editor (profile + directory on a node) | WS:311, WS:468-500 | missing | none | Cross-ref belai auditor |
| Kind filter (workflows / agents / all) | WS:271 | missing | none | |
| Host filter | WS:285 | missing | none | |
| Blackout windows shown | WS:247 | missing | none | Cross-ref settings (A2A blackout) |
| Org timezone header | WS:240 | missing | none | |
| Last run status tone | WS:120-127 | partial | KW:708-714 | Time only, no status |
| Schedules/Triggers as code | WS:252-262 | missing | none | |

## /resolve/factory-templates

Web: `website/src/pages/resolve/factory-templates.vue` (FT). Keleustes: `crates/kls-ui/src/screens/templates.rs` (KT), `crates/kls-engine/src/templates.{rs,json}`.

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Template catalogue | vdb-site docs/factory.md:1350 | done | templates.json | Web 14 ids (security-patch-train, nightly-blackbox, feature-from-issue, bug-repro-to-fix, dependency-hygiene, malware-response, secret-leak-response, release-provenance, kanban-triage-autopilot, ui-polish-pass, delivery-sweep, pr-reviewer, ci-failure-fixer, dependabot-triage). KW 8 (feature-from-card, bug-repro-to-fix, nightly-regression, dependency-hygiene, session-follow-up, kanban-triage-autopilot, delivery-sweep, pr-reviewer). All 14 web ids are present, plus 18 event templates (35 in total). Firewall, egress, siem, deployment and registry templates are n/a (no producer). Evidence `kls-engine/src/templates.json` |
| Start from blank | FT actions | done | KW:269 (Workflows) | |
| "On by default" strip (42 defaults, grouped, On/Off/Customised) | FT defaults-strip | missing | none | |
| Suggested for your events (rank by unanswered events, last 7 days, reasons) | FT suggested | missing | none | |
| Dismiss suggestion (30 days or until count doubles) | FT dismiss | partial | KT:250-258 | KW dismisses templates permanently (+ Restore, Dismissed toggle KT:134); no ranking |
| Search (?q= with synonyms, "ci" finds check runs) | FT search form | missing | none | |
| Source chips with counts | FT source group | missing | none | |
| Category chips | FT category group | done | KT:120 | |
| Card: category, badges (new crew, GitHub), default badge, name, description, flow chips, event, files tickets | FT cards | partial | KT:160-200 | KW preview shows category, trigger, output, tokens, crews, GitHub required, steps; no flow chips list on card, no default badge |
| "Needs the X crew, add it in Workforce" | FT needsNewCrew | partial | KT:177-184 | Lists crews; does not check the library for missing ones |
| Preview graph + limits chips | FT preview | done | KT:159-200 | Steps list, not drawn graph |
| ?id= deep link to preview | FT | done | KT focus | |
| Use template form: name, repos | FT use form | done | KT:223-249 | |
| Use template form: run budget | FT use-budget | partial | KT:244 | Engine supports `run_budget` (`templates.rs:101`), UI passes `None` |
| Opens new draft after use | FT create | done | KT:245 | |

## /resolve/factory-repos

Web: `website/src/pages/resolve/factory-repos.vue` (FR), `components/factory/repos/*`. Keleustes: `crates/kls-ui/src/screens/repos.rs` (KR), `kls-github/src/{sync,poller,labels,budget}.rs`, `kls-store/src/github.rs`.

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Refresh installations/orgs (priced: 1 + 1/100 repos) | FR actions | done | KR:129 | No call-count price on the button |
| Last refresh result (calls, added, removed) | FR notices | partial | KR:875 | Notice only |
| Install on another org link | FR actions | n/a | none | gh user token; KW lists user orgs |
| Managed orgs cards with Manage switch | FactoryOrgCards.vue:77 | done | KR:196-290 | Also Enable/Disable + per-org refresh |
| Org card: App permissions today/optional/unlocks + review link | FactoryOrgCards.vue:1-8 | partial | KR:1016-1085 | KW shows token scopes + "Grant scopes · N" |
| Org card: how many of 42 defaults run + default autonomy | FactoryOrgCards.vue:7-8 | missing | none | |
| Managed orgs as code | FR [01] | missing | none | |
| Repositories table | FactoryRepoTable.vue | done | KR:291-490 | |
| Columns: opt-in signals, Pix, open findings, SLSA, workflows, scanned · enriched | FactoryRepoTable.vue:50-80 | partial | KR:381-386 | KW: Repository, Open PRs, Autonomy, Mirroring, Managed, Synced. Missing findings, SLSA, workflows attached, scanned |
| Signal filter chips (App, Scanned, Kanban bound, TEA, Watched, SBOM uploads) with counts | FR:262, shared/factory-repos.ts:242 | partial | KR:308 | KW: "Managed" checkbox only |
| Org filter select | FR org select | missing | none | |
| Sort (name, recently enriched, open findings, SLSA) | FR:56-61 | missing | none | |
| Search owner/repo | FR search | done | KR:305 | |
| Hidden count (unreachable names) | FR repos-hidden | n/a | none | |
| Show more (paging) | FR loadMore | done (n/a) | KR | All in memory |
| Skeleton rows for webhook-added repos awaiting enrichment + queue position | FactoryRepoTable.vue:5 | n/a | none | Poll model |
| "No access" pill | FactoryRepoTable.vue:154 | partial | KR:429 | Archived pill; can_push stored but no "no access" pill |
| Select all / checkboxes | FactoryRepoTable.vue:54 | done | KR:366-401 | |
| Bulk: attach workflow | BULK_ACTIONS:690 | missing | none | |
| Bulk: bind kanban (project key + mode) | BULK_ACTIONS:691 | partial | KR:543-558 | KW bulk mirror pulls/issues/labels on/off |
| Bulk: run scanners now | BULK_ACTIONS:693 | missing | none | Web disabled too |
| Bulk: stop watching | BULK_ACTIONS:694 | missing | none | Detail has Watched checkbox |
| Bulk: manage / unmanage | (n/a web) | done (extra) | KR:512-516 | |
| Bulk: sync sf: labels | (n/a web; FactoryLabelsSync in settings) | done | KR:560 | |
| Bulk outcome list (per repo done/failed + reason) | FactoryBulkBar.vue:6 | missing | none | |
| Detail: autonomy (capped by Settings ceiling, effective value shown) | FactoryRepoDetail.vue:192 | partial | KR:678 | No ceiling/effective-value message |
| Detail: kanban binding | FactoryRepoDetail.vue:211 | done | KR:693 | |
| Detail: monthly token cap | FactoryRepoDetail.vue:224 | done | KR:710 | |
| Detail: protected branches | FactoryRepoDetail.vue:242 | partial | KR:720 | Web is a branch list; KW a boolean |
| Detail: TEA publish | FactoryRepoDetail.vue:249 | done | KR:725 | |
| Detail: per-repo default workflows on/off (grouped) | FactoryRepoDetail.vue:265-300 | missing | none | `RepoSettings.defaults_off` exists in `kls-store/src/github.rs:64`, never read or shown |
| Detail: workflows attached | FactoryRepoDetail.vue:5 | missing | none | |
| Detail: why listed (signals) | FactoryRepoDetail.vue:2 | missing | none | |
| Detail: save only changed at revision, 409 stale shown, Discard | FactoryRepoDetail.vue:339-347 | done | KR:745-782 | Revision tracked |
| Detail: events for this repo link | FactoryRepoDetail.vue:116 | missing | none | |
| Detail: merge method | (n/a web) | done (extra) | KR:701 | |
| Detail: open PRs with mergeability/checks/review + Ready/Draft/Update branch/Request reviewers/Merge/Resolve | (n/a web, on board) | done (extra) | KR:888-1010 | |
| Issue mirroring: master switch | FactoryIssueMirroring.vue:415 | partial | KR:790-815 | Per repo only |
| Issue mirroring: connection (App or vault GITHUB_TOKEN) | FactoryIssueMirroring.vue:471-476 | done | KR:1016 (GitHub route) | gh / node route |
| Issue mirroring: row per board project → repo, which cards mirror, inbound (GitHub issues become cards in Review) | FactoryIssueMirroring.vue:586-627 | missing | none | KW mirrors per repo, no board-project↔repo mapping, no inbound toggle |
| Issue mirroring: Save / Stop mirroring per row, multi-checkout keys | FactoryIssueMirroring.vue:648-657 | missing | none | |
| Issue mirroring: unlinked local projects group (hide) | FactoryIssueMirroring.vue:534-543 | missing | none | |
| Issue mirroring: merge rules per field ("when X clashes") | FactoryIssueMirroring.vue:739 | missing | none | |
| Issue mirroring: hours before an open conflict escalates to owners | FactoryIssueMirroring.vue:752 | missing | none | |
| Issue mirroring: what to do with existing cards | FactoryIssueMirroring.vue:773 | missing | none | |
| Issue mirroring: delivery/health per project, open conflicts link | FactoryIssueMirroring.vue:10-13 | missing | none | |
| Webhooks and quota: rate limit per installation (bar, who used it) | FactoryWebhooksQuota.vue:80 | partial | KR:1091-1150 | One user quota; no breakdown by caller |
| Webhooks and quota: webhook events + subscribed state | FactoryWebhooksQuota.vue:3-5 | n/a | KR:1158-1200 | Replaced by poller status (last poll, why stopped, ETag hits) |
| Webhooks and quota: enrichment queue | FactoryWebhooksQuota.vue | n/a | none | |
| Poll now | (n/a web) | done (extra) | KR:134 | |

## /resolve/factory-budgets

Web: `website/src/pages/resolve/factory-budgets.vue` (FB), `components/factory/budgets/*`. Keleustes: `crates/kls-ui/src/screens/budgets.rs` (KB), `kls-core/src/vault.rs`, `kls-github/src/budget.rs`.

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| [01] How budgets nest: tier meters (tenant > org > workflow > run > node) | FB [01] tiers | missing | none | KW tiles per budget; no nesting view |
| [02] Budgets table: name, applies to, provider, scope/window, limit, used, alerts, action | FactoryBudgetTable.vue:60-90 | done | KB:190-240, KB:1415 | Tiles not a table |
| Inline scope and action selects save on change; revert on refusal | FactoryBudgetTable.vue:117-152 | partial | KB:1022-1110 | Edit dialog |
| Remove with confirm; tenant row and reserve not removable | FactoryBudgetTable.vue:167 | done | KB:965-1015 | |
| Add budget: applies to tenant / org / workflow / node / repo / interactive reserve | FactoryBudgetForm.vue:94-160 | partial | `kls-core/src/vault.rs:74` | KW scopes: all, provider, model, agent, repo. Missing workflow and node budgets here (workflow limits live in the builder), org |
| Add budget: provider list | FactoryBudgetForm.vue:191 | done | KB:1057 | |
| Add budget: window | FactoryBudgetForm.vue:205 | done | KB:1063 | day/week/month/total |
| Add budget: limit + unit (tokens / USD) | FactoryBudgetForm.vue:222-230 | done | KB:1066-1070 | |
| Add budget: multiple alert thresholds | FactoryBudgetForm.vue:248 | partial | KB:1072 | Single "Alert at %" |
| Add budget: action at limit | FactoryBudgetForm.vue:261 | done | vault.rs:66 | alert_only, downgrade_to_fast, escalate_human, pause_run, hard_stop |
| Field-level validation messages | FactoryBudgetForm.vue:5 | partial | vault.rs:683-695 | Error notice, not per field |
| Budgets as code | FB [02] | missing | none | |
| [03] Providers list with "factory may use" checkbox | FB [03] | done | KB:390-500, KB:1262 | |
| Open Secrets Vault link | FB [03] | done | KB:246 (vault panel inline) | Vault lives in Budgets in KW; cross-ref vault audit |
| [04] Routing: fixed routes (System One decisions, code smart, code fast, planner/reviewer, images) | shared/factory-budgets.ts:159 | partial | KB:615-770 | KW has priority rules (repo, agent, label, kind → provider/model) + default and fast model; no fixed route keys |
| Routing: when Pix allowance runs out | shared/factory-budgets.ts:167 | n/a | none | |
| Routing: reserve % for interactive sessions | FB reserveInteractivePct | missing | none | |
| [05] Usage range 7/30/90 days | FB range | done | KB:776-785 | |
| Usage totals (tokens, est cost) | FB totals | done | KB:787-790 | |
| Tokens by model | FB ControlBars | done | KB:811 | |
| Tokens by role | FB ControlBars | partial | KB:812 | By agent instead |
| Tokens by project | FB ControlBars | done | KB:813 | By repository |
| Tool calls by tool | FB ControlBars | missing | none | |
| Tokens by provider | (n/a web) | done (extra) | KB:810 | |
| Recent usage rows | (n/a web) | done (extra) | KB:870-960 | |
| Budget events link | FB FactoryEventLink | missing | none | |
| [06] Hosts: per-host gauge % of budget + 7d spend | FB [06] | done | KB:823-865 | |
| [07] GitHub quota: installation select | FactoryGithubQuota.vue:90 | n/a | none | One token |
| GitHub quota: bar split this hour (enrichment / factory / people) | FactoryGithubQuota.vue:111 | partial | KR:1091-1150 | Shows remaining + factory poller calls; on Repos, not Budgets |
| GitHub quota: which workflows the factory calls came from | FactoryGithubQuota.vue:4 | missing | none | `CallCount` has caller, not shown |
| GitHub quota rules: factory share/hour, reserve for people, when share runs out | FactoryGithubQuota.vue:169-199 | missing (UI) | `kls-github/src/budget.rs:86` | Settings keys `forge.quota.factory_share`/`reserve` are read but no screen edits them; no on-exhausted policy |
| Grants log + revoke | (n/a web on this page) | done (extra) | KB:520-610 | |

## /resolve/factory-trust

Web: `website/src/pages/resolve/factory-trust.vue` (FTr), `components/factory/trust/FactoryScannerMatrix.vue`, `components/factory/FirewallReviewStrip.vue`, `components/factory/control/GitHubTrafficPanel.vue`, `.repo/factory-firewall-sources.md`. Keleustes: `crates/kls-ui/src/screens/trust.rs` (KTr), `kls-core/src/audit/trust.rs`.

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| [01] SLSA level cards (count per level, names, "and N more") | FTr [01] | done | KTr:147-235, KTr:350 | Level 3 explicitly unreachable |
| Open TEA catalogue link | FTr actions | n/a | none | |
| Provenance events link | FTr actions | partial | KTr:68 | Opens Audit ledger, unfiltered |
| Export evidence index (bundle) | FTr actions | missing | none | Audit screen may export ledger (cross-ref) |
| [02] Scanner matrix: row per repo, column per scanner, clean/findings/blocking/not run with distinct glyphs | FactoryScannerMatrix.vue | done | KTr:236-312 | From gate.verified + failing checks |
| Matrix totals line | FTr matrix-totals | missing | none | |
| Matrix: SLSA level + last TEA release per row | FactoryScannerMatrix.vue:4 | partial | KTr | Level shown in separate table |
| Matrix search | FTr search | done | KTr:76 | Shared filter |
| 30 day window line | FTr | done | KTr:23-29 | |
| [03] Package firewall summary | FTr [03] | n/a | none | Platform feature |
| [04] AI firewall summary | FTr [04] | n/a | none | Platform feature |
| [05] Sandbox egress allow/deny hosts | FTr [05] | partial | KTr:437-470 | Counts of egress denied from belai; no host list |
| [06] Quality gate and release jail summary | FTr [06] | partial | KTr:29 | "Gates run" counter only |
| [07] GitHub traffic (deliveries, from hooks range) | GitHubTrafficPanel.vue | n/a | none | No webhooks |
| Guardrails by node (hazard asks/denials, egress, A2A withheld, run caps, faults) | (n/a web) | done (extra) | KTr:472-545 | |
| Firewall review strip ("N waiting for review" → board cards, template link) | FirewallReviewStrip.vue | missing | none | No firewall sightings → review cards pipeline; egress denials from belai could file sf:egress-review cards |
| Firewall-sourced trigger + sightings dedupe + auto-close card when rule added | factory-firewall-sources.md | missing | none | |

### Gaps

- [P0] Webhook trigger conditions (branches, paths, labels, author, drafts, actions, review state) — add a `conditions` map to trigger config in `kls-engine/src/graph.rs`, match it in `kls-engine/src/forge.rs` `webhook_events`/trigger match, fields in `kls-ui/src/screens/workflows.rs` node_ui. Without it every `pull_request.*` trigger fires on all PRs.
- [P0] Debounce and max runs per hour on event/webhook triggers — enforce in `kls-engine/src/exec.rs` trigger dispatch (store last-fire per trigger in `kls-store/src/engine.rs`); inspector ints in workflows.rs.
- [P1] Per-trigger enable switches — add `enabled` to trigger nodes / ScheduleSpec (`kls-engine/src/ops.rs:272`), honour in exec; checkboxes in header_ui and the Schedules table.
- [P1] Schedules view: create, re-time (publishes new version), delete, kind filter — `kls-engine/src/ops.rs` (edit trigger cron → save+publish), `workflows.rs` schedules_ui.
- [P1] List row on/off switch, trigger summary, repo count, runs 7d columns — `workflows.rs` list_ui; runs count from `kls-store/src/engine.rs`.
- [P1] Output node fields for draft PR (PR state, reviewers, base branch, if no branch), file issues (label/title/min severity/group/max), check run name — `kls-engine/src/exec.rs` output step + workflows.rs node_ui "github" arm.
- [P1] Crew node: model picker from configured providers/models, per-member models, run-on node picker + offline fallback policy — read models from `kls-core/src/vault.rs` providers and nodes from `kls-ui/src/fleet.rs`; use combos in workflows.rs.
- [P1] Agent budget limits per workflow/node (cost USD per card, finalize-at, passes, wall minutes, tool rounds, continuations) — `kls-engine/src/graph.rs` `Limits` + exec enforcement; cross-ref agent-features auditor.
- [P1] Publish panel with note + problem count, and "triggers after publish" list — `workflows.rs` header_ui; `ops.rs` publish already takes a note.
- [P1] Template catalogue parity: add ci-failure-fixer, dependabot-triage, ui-polish-pass, security-patch-train, release-provenance — `kls-engine/src/templates.json`.
- [P1] Template search box + run-budget field in Use form — `kls-ui/src/screens/templates.rs` (pass budget to `templates::use_template`).
- [P1] Issue mirroring per board project: project↔repo mapping, inbound issues → Review cards, merge rules, escalation hours, existing-cards choice — `kls-github/src/sync.rs` + `kls-store/src/github.rs` mirroring table; section in repos.rs.
- [P1] GitHub quota rules editor (factory share, reserve, on-exhausted policy) and per-workflow call breakdown — settings write of `forge.quota.*` in `kls-ui/src/screens/budgets.rs` (or settings.rs); policy in `kls-github/src/budget.rs`.
- [P1] Default workflow catalogue (origin system, Defaults filter, Customised badge, per-repo opt-out using existing `RepoSettings.defaults_off`) — seed in `kls-engine/src/templates.rs`/ops.rs; UI in workflows.rs and repos.rs detail. Cross-ref automatic-agent-features auditor.
- [P1] Repo detail: workflows attached + bulk "attach workflow" — scope match via `ops::repo_in_scope` (`kls-engine/src/ops.rs:647`); repos.rs detail + bulk_bar.
- [P2] System One decision node (model question, confidence threshold, fallback, abstain route, daily cap, rationale retention, 30-day stats) — new node kind in `kls-engine/src/graph.rs` + exec calling a provider via `kls-core/src/vault.rs` routing.
- [P2] Scan node and GitHub-read node with cost labels — `kls-engine/src/graph.rs` palette + exec dispatching a `job` on a node.
- [P2] "Also start on" extra triggers, one-click "add approval after", quick-fix "end run" for unrouted outcomes, safest outcome flag — workflows.rs inspector + graph.rs helpers.
- [P2] View/Download workflow as YAML/JSON — `kls-engine/src/graph.rs` to_json exists; add a window in workflows.rs with copy/save.
- [P2] Dry run repo picker, GitHub-call ranges, confidence, totals of calls — `kls-engine/src/dryrun.rs` (add calls range) + workflows.rs dry_run_ui.
- [P2] Header token estimate and limits chips — reuse dryrun totals in workflows.rs header_ui.
- [P2] Start a workflow from an event (Events → new draft with matching trigger) — `kls-engine/src/ops.rs` create with trigger preset; button in events/audit screen.
- [P2] Template suggestions ranked by unanswered events, source chips, defaults strip — `kls-engine/src/templates.rs` rank over `kls-store` events.
- [P2] Blocked workflow state when no provider/model available, self re-enable — `kls-engine/src/exec.rs` preflight using vault providers; badge in workflows.rs.
- [P2] Budgets: nesting view, workflow/node/org scopes in the budget form, multiple alert thresholds, interactive reserve %, tool-calls usage bars, budget events link — `kls-core/src/vault.rs` BUDGET_SCOPES + budgets.rs.
- [P2] Repos table: signal chips (watched, kanban bound, TEA), sort, org filter, findings/SLSA columns, stop-watching bulk, bulk outcome list, autonomy ceiling message, protected branch list — repos.rs + `kls-store/src/github.rs` RepoSettings.
- [P2] Trust: matrix totals, evidence index export, egress host list, filtered "provenance events" link — `kls-core/src/audit/trust.rs` + trust.rs.
- [P2] Firewall/egress review strip and sightings → review cards — consume belai `egress.denied` audit lines in `kls-core/src/audit`, file cards via `kls-core/src/board.rs`.
# Fork D: audit, settings, memory, conflicts, delivery pages

Paths: web = `website/src/`, K = `keleustes/crates/`. Status: done / partial / missing. "n/a" rows (out of scope or not applicable to desktop) are left out of the counts.

## /resolve/factory-audit

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Hash-chained ledger list, newest first | pages/resolve/factory-audit.vue:690 | done | kls-ui/src/screens/audit.rs:383 (table, 5000-row window) | K shows a flat table, not day groups |
| Day grouping with per-day entry count | factory-audit.vue:690-697 | missing | none | K table has a When column only |
| Verify chain (server recompute) + result line | factory-audit.vue:315, shared/factory-audit.ts verifyResultText | done | audit.rs:193,243 (verify_strip) | K verifies factory and host chains |
| "Last verified n ago" when idle | factory-audit.vue:157 lastVerifiedText | done | audit.rs:316-335 | |
| Broken-chain alert ("entries after cannot be trusted") | factory-audit.vue:365 | done | audit.rs:297 "Broken at" | |
| Refresh button | factory-audit.vue:321 | partial | Loader auto-reload on `audit.` events | no manual refresh; reload is event-driven |
| Cached / revalidating chip + source stamp (asOf) | factory-audit.vue:299-305 | n/a | — | local DB, no cache |
| Export **signed** JSONL | factory-audit.vue:349, ts:411 | partial | audit.rs:177, kls-core/src/audit.rs:552 | K JSONL ends with chain head line but is **not signed**; written to `<data_dir>/exports/` with no save dialog |
| Export range (30 / 90 / 400 days) | factory-audit.vue:330, shared/factory-audit.ts:411 | missing | none (LedgerQuery has from_at/to_at, unused by UI) | K exports the current filter, all time |
| Free-text search (debounced, in URL) | factory-audit.vue:396 | done | audit.rs:439 | |
| Kind filter (worker, card, claim_lapsed, commit, PR, gate, vex, finding, fault, provider_switch, crew_control …) | factory-audit.vue:408, ts:192 | done | audit.rs:420 (kinds from store counts) | |
| Vuln filter (also matches sessions/runs whose transcripts mention it) | factory-audit.vue:432 | missing | none | |
| Card filter | factory-audit.vue:445 | missing | none | text search only |
| Agent filter | factory-audit.vue:457 | missing | none | |
| Repo filter | factory-audit.vue:469 | missing | none | |
| Workflow / run filter | factory-audit.vue:481 | missing | none | |
| Session source + include /tmp (flag-gated off on web) | factory-audit.vue:495-513 | n/a | — | off on web too |
| Session filter | (via fact/links) | done | audit.rs:446 | K extra: direct session-id field |
| Host / origin filter (factory vs hosts) | — | done | audit.rs:403 | K extra |
| Fact filter chips (kind:value, all-of) + truncated warning | factory-audit.vue:532,540; components/factory/FactoryFactFilter.vue | missing | none | |
| Clear filters | factory-audit.vue:522,681 | done | audit.rs:454 | |
| "Matching / total" count | — | done | audit.rs:464 | K extra |
| Row: time, kind tone, actor + actor kind (person/agent/host/system/rule) | factory-audit.vue:723-750 | done | audit.rs:517-560 | |
| Vulnerability ids in message rendered as links | factory-audit.vue:723 (vulnSegments), 268 | missing | none | |
| Agent tag (avatar) on rows | factory-audit.vue:743,756,819 | missing | none | plain text actor |
| Expand row: hash/prev, card, agent, payload JSON | factory-audit.vue:777-840 | done | audit.rs:999 entry_detail | K adds Copy JSON |
| "Open" links from an entry to its card / run / session / workflow / events window | factory-audit.vue:826-835, shared/factory-audit.ts:322 entryLinks | missing | none | detail pane has no navigation; `Model::go` exists (kls-ui/src/model.rs:113) |
| Load more (cursor paging) | factory-audit.vue:856 | partial | audit.rs:20 LEDGER_WINDOW=5000 | newest 5000 only; "export for all" hint |
| Select mode, multi-select sessions, select all loaded | factory-audit.vue:554-572 | partial | audit.rs:631 Sessions tab | K deletes one session at a time, from a separate tab |
| Delete sessions with tombstone, confirm (Enter/Esc) | factory-audit.vue:579-614 | done | audit.rs:745, kls-core/src/audit.rs:565 | K adds optional reason; no keyboard confirm |
| Partial delete failure message | factory-audit.vue:236 | n/a | — | single delete |
| Empty / failed / API-unavailable states | factory-audit.vue:620-685 | done | audit.rs:388,494 | |
| Teleports tab (session bundle move between nodes) | — | done | audit.rs:787 | K extra |

## /resolve/factory-settings

Page chrome and shared mechanics:

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| One document, section nav, sticky bar with unsaved count | pages/resolve/factory-settings.vue:250-312 | done | kls-ui/src/screens/settings.rs:369,445 | |
| Save / Discard | factory-settings.vue:258-266 | done | settings.rs:500,509 | |
| Revision conflict: another session saved, edits rebased | composables/useFactorySettings | done | settings.rs:169-203 | |
| Export as YAML / Download as code | factory-settings.vue:222-249 | done | settings.rs:517,965 | K also imports YAML with preview (settings.rs:850) |
| "Setting change events" link to the events/audit filter | factory-settings.vue:211 | missing | none | |
| `?section=` deep link | factory-settings.vue:13 | done | settings.rs:105 focus | |
| Per-field dirty marker, inline validation error | components/factory/settings/FactorySettingField.vue:1-12 | done | settings.rs:599-760 | |
| Locked field note / "applies at next launch" note | FactorySettingField.vue:1-5 | partial | config.rs `restart` flag, settings.rs:159 | restart note only after save |
| Model fields as dropdown of allowed models | FactorySettingField.vue:5 (modelpick), FactoryModelsSection.vue | partial | config.rs:212-221 Kind::Model | free text `<provider>/<model>`; no list from vault/catalogue |
| Kill switch with confirm dialog explaining effect + GitHub-call cost, reason for ledger | components/factory/settings/FactoryKillSwitch.vue:52,93-137 | partial | settings.rs:759-805, kls-core/src/board.rs:757 | no confirm dialog; one reason box shared with A2A |
| Stop: cancel running steps, release kanban leases, revoke forge tokens, hold scheduled fires; result counts | FactoryKillSwitch.vue:3-7,70,112 | partial | kls-engine/src/exec.rs:522,664,977 | K only stops new schedule/trigger/dispatch work; running steps not cancelled, leases not released, no result summary |
| A2A blackout with confirm + reason | components/factory/settings/FactoryA2ABlackout.vue:1-6,81-116 | done | settings.rs:806-845 | no confirm dialog when turning on |
| sf: labels sync (create missing labels in named repos, priced) | components/factory/settings/FactoryLabelsSync.vue:1-4,85-106 | done | kls-github/src/labels.rs, kls-ui/src/screens/repos.rs | lives on Repos, not Settings |
| "No decision model available" empty state | factory-settings.vue (AiProviderEmptyState) | n/a | — | platform AI switches |
| Keep min-confidence for new model prompt | factory-settings.vue:443 | missing | none | no systemone.model key in K |

Settings keys (shared/factory-settings.ts). "stored" = in K's document but nothing in the code reads it (checked by grepping every key and getter).

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| scope.orgs managed orgs | shared/factory-settings.ts:143 | partial | kls-ui/src/screens/repos.rs | managed repos chosen on Repos |
| scope.interest / archived / dependencies / sync | factory-settings.ts:144-147 | missing | none | |
| autonomy.max | ts:155 | partial | kls-core/src/audit/config.rs:180 | stored; per-repo autonomy is checked (kls-store/src/github.rs:804) but the global cap is not read |
| belai.mode default agent mode | ts:156 | partial | config.rs:181 | stored, not read |
| autonomy.concurrency | ts:157 | partial | config.rs:187 | stored; engine uses per-workflow limits.concurrency only |
| autonomy.claim_ttl_minutes | ts:158 | partial | config.rs:190 | stored, not read |
| autonomy.max_attempts | ts:159 | partial | config.rs:188 | stored, not read |
| autonomy.no_host_ttl_hours | ts:160 | partial | config.rs:192 | stored; engine hard-codes 1 h (kls-engine/src/exec.rs:298) while the setting defaults to 6 h |
| autonomy.parked_concurrency | ts:161 | missing | none | |
| factory.fanoutCap (max runs one event starts) | ts:162 | missing | none | |
| belai.ask_permission | ts:163 | partial | config.rs:194 | stored, not read |
| autonomy.plan_approval | ts:164 | partial | config.rs:196 | stored, not read |
| defaults.enabled | ts:172 | partial | config.rs:203 | stored, not read; no defaults catalogue in K |
| Per-default switches (42 defaults, groups, "Customised" badge, live state) | components/factory/settings/FactoryDefaultsSection.vue:1-10,145-196 | missing | none (per-repo opt-outs in kls-store/src/github.rs:63) | see the agent-features auditor |
| autonomy.default | ts:174 | partial | config.rs:206 | stored, not read |
| forge.quota.proactive_writes_per_hour | ts:175 | missing | none | |
| approve.sensitive_paths | ts:176 | partial | config.rs:208 | stored, not read by the approval logic |
| systemone.model / fallback / below_threshold / on_abstain / retention / override | ts:184-193 | missing | none | only min_confidence exists |
| systemone.min_confidence | ts:186 | done | config.rs:238, kls-core/src/github.rs:490 | |
| belai.guardrails | ts:201 | partial | kls-core/src/audit/trust.rs:416 | shown on Trust if set; no settings field |
| belai.tests.post_end / on_fail / show_internal_work / auto_commit_per_task / a2a.show / crews.lease | ts:202-207 | missing | none | |
| agents.* per-card budgets (cost, finalize_at, attempts, continuations, passes, wall minutes, iterations) | ts:211-217 | missing | none | |
| models.default / models.fast | ts:223-232 | done | config.rs:212, kls-core/src/vault.rs:446,913 | free text (see above) |
| models.agents (model per agent) | ts:231, FactoryModelsSection.vue:174 | missing | none | |
| Model refused by AI-firewall policy shown on the field | FactoryModelsSection.vue:1-10 | n/a | — | |
| forge.branch_prefix / pr_state / protected | ts:238-240 | partial | config.rs:233-237 | stored, not read by the forge code |
| kanban.mode / opt_out_label / conflict_comment | ts:241-243 | missing | none | |
| forge.check_runs | ts:244 | partial | kls-engine/src/forge.rs (check runs) | no toggle |
| signing.mode | ts:252 | done | config.rs:247, audit/trust.rs | |
| signing.email | ts:254 | partial | config.rs:252 | stored, not read |
| signing.auto_add / rotate_days / retire_days / on_missing | ts:253-257 | n/a | — | K commits through the GitHub API, which signs them |
| Signing keys per GitHub user: status, set up, revoke inline | components/factory/settings/FactorySigningSection.vue:1-6,132-277 | n/a | — | same reason; node-git commits are not covered (see notes) |
| forge.read.default / stale.* / etag / polling.forbidden / poll.fallback | ts:265-272 | partial | config.rs:226-229 github.poll, poll_interval_s | K polls on a timer with ETag; the web locks polling off |
| forge.quota.factory_share / reserve | ts:273-274 | done | config.rs:230-232, kls-core/src/github.rs | |
| forge.calls_per_hour / quota.on_exhausted | ts:275-276 | missing | none | |
| hosts.runOn default + runOnFallback | ts:285-286 | missing | none (per-step runOn in kls-engine/src/exec.rs) | |
| pix.maintenance window (no run starts) | ts:287 | missing | none | not sandbox-only on the web |
| pix.* sandbox size / egress / version / reserve | ts:289-292 | n/a | — | sandboxes out of scope |
| security.scanners / custom / slsa_min / evidence / tea.publish / jail / firewall | ts:301-307 | partial | kls-core/src/audit/trust.rs (reads security.slsa_min) | no settings fields |
| budget.factory_share / budget.month | ts:315-316 | missing | none | |
| budget.on_exceed | ts:317 | partial | config.rs:312 | stored; budgets carry their own action (kls-core/src/vault.rs:591) |
| budget.alerts | ts:318 | partial | config.rs:313 | stored, not read |
| providers.code / vault.grant / vault.on_error | ts:326-328 | missing | none | vault itself is the parent's section |
| persistence.retention | ts:336 | partial | config.rs:296 | stored; no pruning job |
| persistence.postgres / export | ts:337-338 | partial | config.rs:294 DATABASE read-only | no scheduled export |
| schedules.tz | ts:346 | partial | config.rs:278 | stored; a schedule without tz uses UTC (kls-engine/src/ops.rs:277) |
| schedules.blackout windows | ts:347 | missing | none | |
| schedules.missed | ts:348 | partial | config.rs:282 | stored; catch-up is per schedule |
| audit.calls | ts:356 | partial | config.rs:304 | stored, not read |
| audit.sign / weekly_report | ts:357-358 | missing | none | |
| notify.events | ts:366 | partial | config.rs:268 | stored, not read; kls-app/src/desktop.rs:244 reads only notify.desktop/approvals/questions/conflicts |
| Per-event notification kinds, quietest severity, quiet hours | components/factory/settings/FactoryNotificationsSection.vue:386-485 | partial | config.rs:260-273 | 4 booleans; no severity or quiet hours (OS notifications) |
| Chat channels (Slack / Teams / Discord / webhook) route + notify.connector | FactoryNotificationsSection.vue:654-672, ts:367 | missing | none | |
| Browser push subscription + test push | FactoryNotificationsSection.vue:347 | n/a | — | out of scope |
| Delivery estimate settings (delivery.enabled, parts, multiplier, overhead, rates per role, manual baselines, companyDomain, idleGapMin, reviewMinPer100), live preview, reset to defaults | components/factory/settings/FactoryDeliverySection.vue:1-12, shared/factory-delivery-settings.ts:353-380 | missing | none | |
| Capture coverage of the lifecycle ledger | components/factory/settings/FactoryCaptureCoverage.vue:1-5 | missing | none | |
| Project overrides (per host + dir prefs, layer shown, save to host and wait for ack) | components/factory/settings/FactoryProjectOverrides.vue:1-9,144-258 | missing | none | |
| Appearance (theme), Node listener, join token TTL, teleport retention | — | done | config.rs:148-170,300 | K extras |

## /resolve/factory-memory

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Scope list with live/total counts and "All notes" | pages/resolve/factory-memory.vue:68-108 | done | kls-ui/src/screens/memory.rs:150-200 | |
| Scopes grouped Repositories then Agents (profile lessons) | factory-memory.vue:6 | partial | kls-store/src/board.rs:55 (`profile:<name>` scope) | flat list, not grouped |
| Search notes (text, tags) | factory-memory.vue:13 | done | memory.rs:103 | |
| Show deleted | factory-memory.vue:21 | done | memory.rs:97 | |
| Paged 50 at a time, "More" | factory-memory.vue:8,342 | missing | memory.rs:53 (loads all) | |
| Add note (text, tags) to the selected scope | factory-memory.vue:132,165-185 | done | memory.rs:318-391 | K lets you type any scope |
| Edit note inline | factory-memory.vue:238-254 | done | memory.rs:408-470 (dialog) | |
| Delete note with confirm | factory-memory.vue:325,404-424 | done | memory.rs:484 | |
| Clear scope with confirm | factory-memory.vue:140,365-382 | done | memory.rs:93,513 | |
| Author: agent avatar / "Written in the console" | factory-memory.vue:429-438 | done | memory.rs:282 "Written in Keleustes" / robot chip | |
| Source host of a note | factory-memory.vue:439 | missing | none | MemoryNote has no host field |
| "From A2A" link to the message in its session | factory-memory.vue:452-459 | missing | none | no A2A link stored |
| Relative time, exact on hover; deleted tag | factory-memory.vue:444-451 | done | memory.rs:55-59,242 | |
| Hosts drop deleted/cleared notes on next sync | factory-memory.vue:11 | done | memory.rs:492,521 (node pull) | |
| Read-only for non-admins | factory-memory.vue:11 | n/a | — | single user |
| `?scope=&key=&q=` deep link | factory-memory.vue:14 | done | memory.rs:82 focus(scope) | no search term carried |

## /resolve/factory-conflicts

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Open a PR's conflicts by repo + number (`?repo=&pr=`) | pages/resolve/factory-conflicts.vue:1-18 | done | kls-ui/src/screens/conflicts.rs:136-160 | |
| Server-side merge: clean hunks in, overlaps marked | factory-conflicts.vue:5-6 | done | kls-github/src/conflicts.rs:128, merge3.rs | K shows base / ours / theirs side by side |
| File list with progress count | factory-conflicts.vue:198-215 | done | conflicts.rs:409-445 | |
| Per block: accept current / incoming / both | components/factory/conflicts/ConflictFilePane.vue:1-5 | done | conflicts.rs:665-669 | K adds Base and Markers |
| Accept all / whole file per side | — | done | conflicts.rs:583-611 | K extra |
| Edit file by hand; markers block "resolved" | ConflictFilePane.vue:135 | done | conflicts.rs:732-744 | |
| Jump to conflict n | ConflictFilePane.vue:89 | done | conflicts.rs:634 | |
| J / K next / previous file | factory-conflicts.vue:17 | missing | none | |
| Lockfiles: take the base copy, ask for regeneration, say so on done | factory-conflicts.vue:7-8,104 | missing | none | |
| Files it cannot merge (binary, rename vs delete, fork) named, with Ask Pix + local git commands | factory-conflicts.vue:154-184 | partial | conflicts.rs:398,443 | fork and binary flagged; no git commands, no rename-vs-delete case |
| Commit dialog: message + extended description | components/factory/conflicts/ConflictCommitDialog.vue:110-134 | partial | conflicts.rs:307,779 | message only |
| Commits as the person, email choice | ConflictCommitDialog.vue:139-152 | partial | conflicts.rs:365 | commits as the gh user via the API; no email choice (signing.email not read) |
| Signature: required/preferred, "Commit without a signature" | ConflictCommitDialog.vue:1-8,169 | partial | conflicts.rs:361,699 | always API-signed; shows Verified / Unverified after |
| head_moved: nothing written; Reload keeps unchanged resolved files | factory-conflicts.vue:13-15, ConflictCommitDialog.vue:89 | partial | kls-github/src/conflicts.rs:378 | refuses; reload loses resolutions |
| Done panel: commit sha link, parents, signature, lockfiles, "waiting for GitHub mergeability" | factory-conflicts.vue:87-110 | partial | conflicts.rs:548-553 | notice with sha + verified only |
| Open PR on GitHub | factory-conflicts.vue (Open #n) | done | conflicts.rs:246 | |
| "Use Update branch" when nothing overlaps | factory-conflicts.vue:139 | done | conflicts.rs:414 | |

## /resolve/factory-delivery-decisions

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Range switch today / 7d / 30d / MTD / 90d | components/factory/delivery/DeliveryFrame.vue:1-8, shared/factory-links.ts FACTORY_DELIVERY_RANGES | partial | kls-ui/src/screens/delivery.rs:200 | 7/30/90 only |
| Off-in-settings empty state | DeliveryFrame.vue:4-5 | missing | none | no delivery.* settings |
| Totals: decisions, per day | pages/resolve/factory-delivery-decisions.vue (template 25-40) | done | delivery.rs:836-852 | |
| Settled without a person % | decisions.vue (template 27-28) | done | delivery.rs:852-870 | proceeded / by rule / escalated / no person |
| Median time to decide; median human wait | decisions.vue (template 29-30) | partial | delivery.rs:1029 | wait only |
| Person time replaced + $ at tech-lead rate (assumed) | decisions.vue:78 | missing | none | no estimate model |
| Charts: per day settled vs escalated, outcome breakdown, confidence | components/factory/delivery/DecisionsCharts.vue:1-5 | done | delivery.rs:898-930 | |
| By model table | decisions.vue:104-115 | done | delivery.rs:956-985 | |
| By kind table: manual minutes, hours, escalated | decisions.vue:205-292 | partial | kls-core/src/github.rs:434 (DecisionsReport.kinds) | computed but not drawn; no manual-minutes estimate |
| Tool-call checks excluded note with count | decisions.vue (template ~300) | missing | none | |
| Reached a person: per kind, approved, sent back, answered, slowest 1 in 10 | decisions.vue:312-367 | partial | delivery.rs:995-1029 | no slowest-tenth |
| Factory vs team comparison (time to decide, 3 a.m./holiday, same rule, record of why, people needed) | decisions.vue:378-418 | missing | none | |
| Export CSV | — | done | delivery.rs:154, kls-core/src/github.rs:1321 | K extra |

## /resolve/factory-people

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Totals: people, internal, actions, person time | pages/resolve/factory-people.vue:21 | done | delivery.rs:476-500 | |
| Team equivalent (assumed) | factory-people.vue:74 | missing | none | |
| Where people are detected, by source, with basis (measured / derived rule) | factory-people.vue:91-171 | partial | delivery.rs:526-560 | no basis or rule text |
| Who did what matrix (top 10 + fold + total row; source off reads "Off") | components/factory/delivery/PeopleMatrix.vue:1-4 | done | delivery.rs:572-600 | no fold row |
| Seen and left out (bots, App, Pix [bot]) | factory-people.vue:5-6 | done | delivery.rs:649 | |
| Rules table (how each time is derived, quoting Settings) | factory-people.vue:232-245 | missing | none | |
| Row click opens contributor | factory-people.vue | done | delivery.rs:584 (click sense) | |

## /resolve/factory-contributors

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Totals: contributors (int/ext), outcomes with a person, signed off, decisions answered + median | pages/resolve/factory-contributors.vue:30 | done | delivery.rs:656-704 | |
| Scope switch all / internal / external | factory-contributors.vue:94 | done | delivery.rs:213-218 | |
| Server search (name, login, repo) kept in URL | factory-contributors.vue:103-109 | done | delivery.rs:206 | |
| Directory table (works on, outcomes, started, decided, reviewed, signed off, signed %, time, last active) | components/factory/delivery/ContributorTable.vue:1-4 | done | delivery.rs:764-773 | |
| By repository (outcomes, contributors) | factory-contributors.vue:140 | done | delivery.rs:368 | |
| Where people step in (share of outcomes) | factory-contributors.vue:185 | done | delivery.rs:726 | |
| Internal by company email domain | shared/factory-delivery-settings.ts:378 | partial | kls-github/src/parse.rs:286 | K uses GitHub author association only |

## /resolve/factory-contributor

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Person header + numbers (outcomes, started, decisions, median answer, commits, signed, time) | pages/resolve/factory-contributor.vue:61 | done | delivery.rs:1049-1113 | |
| Work list | factory-contributor.vue:126-187 | done | delivery.rs:1130 | |
| Part filter (started / decided / reviewed / signed) `?part=` | factory-contributor.vue:9,135 | missing | none | WorkRow.parts exists |
| Parts breakdown by source | factory-contributor.vue:259 | done | delivery.rs:1163 | |
| Decisions answered (approved, sent back, answered, slowest tenth) | factory-contributor.vue:305-361 | partial | delivery.rs:1084-1091 | count + median only |
| Repositories | factory-contributor.vue:372-405 | done | delivery.rs:1172 | |
| Agents they worked with | factory-contributor.vue:440 | missing | none | `agent_ms_beside` only |
| Identities (GitHub, emails, aliases) | factory-contributor.vue:472 | done | delivery.rs:1066 | |
| Actions per day | factory-contributor.vue:508-526 | done | delivery.rs:1119 | |

## /resolve/factory-outcome

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| One outcome page by key (card / pr / issue / run / session) | pages/resolve/factory-outcome.vue:1-9 | missing | none | the Outcomes table rows do nothing on click |
| Trigger-to-result timeline (step, time, who, duration) | components/factory/delivery/OutcomeTimeline.vue:1-5 | missing | none | |
| With the factory vs a team by hand (people time, people, trigger to result, cost) | factory-outcome.vue:79-153, DeliveryContrast.vue | missing | none | |
| Value of the agent work by role | factory-outcome.vue:162, DeliveryRoleTable.vue | missing | none | |
| Cost by model | factory-outcome.vue:185, DeliveryModelCost.vue | missing | none | OutcomeRow.cost_micro_usd exists |
| People on this outcome | factory-outcome.vue:213 | partial | delivery.rs:419-420 (People / Reviewers columns) | |
| Outcomes ledger also counts cards, runs, sessions | DeliveryOutcomesTable.vue:1-5 | partial | kls-core/src/github.rs:276 | merged PRs and closed issues only |

### Gaps

- [P0] Settings that save but do nothing: autonomy.max, autonomy.default, concurrency, max_attempts, claim_ttl_minutes, no_host_ttl_hours (hard-coded 1 h in the engine against a 6 h default), ask_permission, plan_approval, belai.mode, defaults.enabled, approve.sensitive_paths, forge.branch_prefix/pr_state/protected, signing.email, schedules.tz (UTC fallback), schedules.missed, budget.on_exceed/alerts, persistence.retention, audit.calls, notify.events — load these into `EngineConfig` and the forge/approval paths when the engine starts and when a `settings.saved` event arrives (kls-engine/src/exec.rs, kls-engine/src/forge.rs, kls-github), read notify.events in kls-app/src/desktop.rs; until each is wired, hide or mark it in kls-core/src/audit/config.rs.
- [P0] The kill switch does not cancel running steps or release kanban leases, and has no confirm dialog — add `Core::stop_factory` in kls-core/src/board.rs that cancels running dispatches through kls-node jobs, releases leases and returns counts; add a confirm window in kls-ui/src/screens/settings.rs control_ui.
- [P1] Audit entry has no "open" links to its card, run, session, workflow or events — map subject fields to `Model::go(Screen, focus)` in kls-ui/src/screens/audit.rs entry_detail (port shared/factory-audit.ts entryLinks).
- [P1] Audit filters for card, agent, repo, workflow/run and vuln — add the fields to `LedgerQuery` (kls-store/src/audit.rs:169) as subject JSON matches, plus inputs in audit.rs ledger_ui.
- [P1] Audit export is unsigned and has no range picker or save dialog — sign the JSONL head line with the server key (kls-core/src/audit.rs audit_export_jsonl), set from_at from a 30/90/400-day picker, and use an `rfd` save dialog in kls-ui.
- [P1] Per-agent model map and model dropdowns — add `models.agents` (a map) to config.rs and a picker fed by kls-core/src/vault.rs routes; resolve step → agent → default in kls-engine.
- [P1] Crew defaults (guardrails, tests post_end/on_fail, show_internal_work, auto_commit_per_task, a2a.show, crews.lease) and agents.* per-card budgets — add a section to kls-core/src/audit/config.rs and pass the values in the dispatch payload (kls-engine/src/exec.rs → kls-node jobs).
- [P1] System One section: model, fallback, below_threshold, on_abstain, retention, override — config.rs keys read by the decision code in kls-core/src/github.rs:490 and kls-engine.
- [P1] Default run-on host + fallback, and a maintenance window — config.rs keys; kls-engine/src/exec.rs node selection and fire_schedules check.
- [P1] Schedule blackout windows (schedules.blackout) — new Kind::Blackout in config.rs; skip or hold fires in kls-engine/src/exec.rs fire_schedules.
- [P1] Conflict editor: lockfile handling (take the base copy, flag regeneration), keep resolutions across a head_moved reload, extended description — kls-github/src/conflicts.rs (lockfile list, merge into the new set) and kls-ui/src/screens/conflicts.rs.
- [P1] Outcome detail screen (timeline, cost by model, people) — add `outcome(key)` to kls-core/src/github.rs and a Tab::Outcome in kls-ui/src/screens/delivery.rs opened from an Outcomes row click.
- [P1] Project overrides (per node + directory belai prefs, layer shown, save and wait for the node's ack) — add an RPC in kls-proto/kls-node and a section in kls-ui/src/screens/settings.rs.
- [P2] Kanban binding settings (mode, opt-out label, comment on sync conflict) — config.rs keys read by kls-github/src/sync.rs.
- [P2] Forge quota extras: proactive_writes_per_hour, calls_per_hour, on_exhausted — kls-github/src/budget.rs.
- [P2] Audit: day grouping, cursor paging past 5000, multi-select session delete, fact-filter chips, vuln links, agent avatars — kls-ui/src/screens/audit.rs.
- [P2] Delivery estimate model (delivery.* rates, role multipliers, manual baselines, company domain), team-equivalent figures, Decisions by-kind person time, factory-vs-team contrast, capture coverage — a `delivery` settings section in config.rs plus an estimate fn in kls-core/src/github.rs; render in delivery.rs.
- [P2] Delivery ranges today and MTD; contributor part filter; contributor agents list; slowest-tenth answer time — kls-ui/src/screens/delivery.rs, kls-core/src/github.rs.
- [P2] Memory: page 50 at a time, group scopes into repos and agents, show the source host, link to the A2A message — add host/a2a_ref to MemoryNote (kls-store/src/board.rs:53) from node sync; kls-ui/src/screens/memory.rs.
- [P2] Conflicts J/K file navigation, git commands for files it cannot merge — kls-ui/src/screens/conflicts.rs.
- [P2] Settings "setting change events" link to the Audit kind `setting` — kls-ui/src/screens/settings.rs header → `Model::go(Screen::Audit, …)`.
- [P2] Chat-channel notifications (Slack, Teams, Discord, webhook route) and notification severity / quiet hours — kls-app/src/desktop.rs plus a connector module; low priority for one user.
- [P2] Transparency: sign the decision ledger, weekly report — kls-core/src/audit.rs.
## /resolve/vault — Provider Vault (Secrets Vault)

### How the web vault works (reference)

- **Page** (`website/src/pages/resolve/vault.vue`): header buttons *Add provider key* / *Add environment variable* (:160-180); "Entries as code (no values)" IaC export (:194); 4 stat tiles: Provider keys, Serving requests, Expiring within 14 days, Expired/retrieval denied (:240-285); an integrity banner "N of M entries verified, last X ago" with a *Run checks* button that runs `verify` on every live entry sequentially (:96-100, :290-320); tabs Provider keys / Environment variables / Connector secrets / Signing keys / Activity (:326-340), deep-linkable via `#env`, `#connectors`, `#signing`, `#activity`, `?entry=<id>` (opens detail drawer), `?add=key|env` (opens dialog) (:53-56, :128-135). After a *new* provider key is saved, `AiProviderApplyDialog` opens to apply one of its models to every SF setting / workflow that needs a model (:84-92).
- **Add/edit dialog** (`components/vault/VaultEntryDialog.vue`):
  - Kind picker: provider key / env var / management key (rotation only) / commit signing key (hands off to signing setup) (:96-102, :156-162).
  - Provider autocomplete with logos, only enabled providers (:76, :350-378); label auto-fills `<provider>-prod` until the user types (:164-168); env-var names forced upper-case, `^[A-Z_][A-Z0-9_]{0,63}$`, reserved names (PATH, LD_PRELOAD, BELAI_*, VULNETIX_*) refused (:182-187, :380-390); env values must be >= 8 chars so the scrubber works (:209-215).
  - Secret field is write-only, masked, with an eye toggle to reveal while typing (:56, :407-445); Kiro takes a JSON sign-in with its own validator (:190-207).
  - Edit mode: name locked for env vars, value untouched unless "Replace the stored value" switch is on (:395-405).
  - **Test key** (:446-472, `useVault.ts testKey`, server `vdb-site/api/internal/handler/vault_handlers.go:330` + `vault_providers.go:323-375`): a separate outlined button next to the secret, enabled when provider + value are present. It POSTs `{provider, value}` to `/v1/vault/test-key`; the server does one free GET (`/v1/models` with `x-api-key` for Anthropic, `openrouter.ai/api/v1/auth/key`, `<base>/models` Bearer otherwise) and returns `{ok, skipped, status, ms}`. Result is a tonal alert: success "accepted the key in N ms", info "address not known, not tested" (skipped), warning "refused the key (HTTP N). **You can still save it.**" Errors show inline. **`canSave` (:220) does not reference the probe at all** — the test is purely advisory and the button shows a spinner only on itself. Changing the provider clears the result (:164).
  - "Where it can be used" access editor: per surface (sandbox / host / AI firewall) a rule none / specific / all / all-except; nothing allowed by default (`VaultAccessEditor.vue`).
  - Lifetime: 24h / 7d / 30d / 90d / Custom date / Never; past expiry requests are denied with `vault_key_expired`, value kept so it can be extended (:498-550, `vault-model.ts:415-441`).
  - Auto-rotation (providers whose API can mint keys: OpenRouter, OpenAI, xAI): every 14/30/60/90 days, keep old key 5/15/60 min, pick management key, project/team id (:552-640). Server scheduler runs every 15 min (`docs/secrets-vault.md:310`).
  - Overlap guard: client `draftConflicts` then server 409 -> `VaultConflictDialog` offers carve-out / remove grant / revoke other key (:278-290, `VaultConflictDialog.vue`).
  - Save button "Encrypt and save" / "Save changes" (:650-658); success banner "saved ... cannot be viewed again" plus any server warning.
- **Keys tab** (`VaultKeysTab.vue`): table Key / Where it can be used / Expiry and rotation / Integrity (Verified / Check failed / Not checked) / actions (details, edit) (:118-215); Management keys list (:229-250); "Who serves each provider" matrix provider x surface (:254-300).
- **Env tab** (`VaultEnvTab.vue`): Name / Available to / Expiry / Last 7 days (injected, scrubbed counts) (:128-200); warning when a sandbox with vars allows all egress.
- **Entry detail drawer** (`VaultEntryDetail.vue`): state chip (Serving / Expiring soon / Expired, denied / Revoked); actions *Edit access and expiry*, *Rotate now* (auto-rotatable), *Replace with a new key* -> inline field + "Test and swap in" (server seals, probes, then swaps; old key kept for grace; on failure current key keeps serving, steps listed pass/fail) (:145-245); Provenance (origin, created by/at, version, fingerprint, envelope, expiry, rotation next/last error) (:250-288); Lineage of replaced versions with fingerprints (:289-320); **Encryption checks** (*Run checks* = envelope, KMS key, decrypt context, fingerprint match, overlap, expiry notice; *Run checks and test with <provider>* adds the live probe) (:322-394); Recent activity for this entry (25 newest) (:395-425); Revoke with confirm dialog (:426-455).
- **Connector secrets tab** (`VaultConnectorsTab.vue`): server-only credentials for Slack/Discord/Google Chat/Teams/Jira/webhook/AWS/Wiz; table Secret / Version / Used by (links) / Replace value (per-provider field set, `vault-model.ts:156`) / Revoke (confirm lists what stops working). Not grantable to agents.
- **Activity tab** (`VaultActivityTab.vue`): newest-first log, filter All / Changes / Denied; columns When / What (Added, Changed, Access changed, Value replaced, Rotated, Rotation failed, Revoked, Imported, Checked, Used, Denied, Removed from output xN, Old key deleted) / Entry / Where / Result (+ deny reason: no grant, expired, revoked, ambiguous) (`vault-model.ts:207-215, :464-480`).
- **Signing keys tab** (`VaultSigningTab.vue`): per-GitHub-user commit signing key, public half only; cross-ref settings audit (FactorySigningSection).

### Keleustes today

No vault screen of its own. Keys live in **Budgets** (`crates/kls-ui/src/screens/budgets.rs`): a "Vault" panel on the Budgets overview (:245-300) and a "Keys" tab (`Tab::Providers`, :388-600) listing providers (seeded: anthropic, openai, google, openrouter, mistral — `crates/kls-store/migrations/sqlite/0005_vault.sql:43-48`) with their keys, a "Keys without a provider" group and the grant log. Add/rotate dialog `key_dialog` (:1116-1224): Provider combo, Env var, Label, masked Key, Save (enabled when provider + value non-empty), saves via `kls_core::vault::put_secret`/`rotate_secret` (`crates/kls-core/src/vault.rs:153-189`), XChaCha20 sealed with OS-keyring master key (`crates/kls-vault/src/lib.rs`). `put` silently replaces the existing key for the same provider+env (:435-487). No HTTP client exists in the workspace (GitHub goes through `gh`), so nothing can probe a provider.

| Feature | Web source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Dedicated vault screen / nav entry | pages/resolve/vault.vue:140 | partial | kls-ui/src/screens/budgets.rs:245, :388 | Buried under Budgets; `vault.*` events route to Budgets (kls-ui/src/events.rs:361) |
| Add provider key | vault.vue:170-178, VaultEntryDialog.vue | done | budgets.rs:1116 key_dialog | Provider, env, label, value |
| Provider picker limited to known/enabled providers, with logos | VaultEntryDialog.vue:76, :350-378 | partial | budgets.rs:1136-1150 | Combo of `vault_providers`; no enabled flag (only `factory`), no logos |
| Label auto-fill `<provider>-prod` until touched | VaultEntryDialog.vue:164-168 | missing | none | Label empty by default |
| Env var name auto-filled from provider | (web n/a; per-kind) | done | budgets.rs:1150-1155 | Keleustes-specific |
| Reveal (eye) toggle while typing | VaultEntryDialog.vue:407-445 | missing | budgets.rs:1171-1176 | `password(true)` with no toggle |
| Client-side validation messages (name/value rules, inline) | VaultEntryDialog.vue:182-216 | partial | kls-vault/src/lib.rs:268-296 | Server-side bail only; error arrives as a toast after the dialog closed (dialog closes before `act` resolves, budgets.rs:1215) — typed value is lost on failure |
| **Test key (non-blocking indicator)** | VaultEntryDialog.vue:227-231, :446-472; vault_providers.go:323-375 | **missing** | none | No probe code, no HTTP client. Web: separate button, result ok/ms, skipped, refused HTTP N "You can still save it"; `canSave` ignores it |
| Probe result clears on provider change | VaultEntryDialog.vue:164 | missing | none | |
| Save never gated by test | VaultEntryDialog.vue:220 | n/a (done by absence) | budgets.rs:1178 | Must stay so when test is added |
| Edit entry without replacing value (label/access/expiry) | VaultEntryDialog.vue:395-405, :236-247 | partial | kls-vault/src/lib.rs:510 set_label | `set_label` exists in vault crate but no UI/core wrapper; UI offers Rotate (value) or Delete only |
| Rotate = replace value | VaultEntryDetail.vue:167-215 | done | budgets.rs:305-313, kls-core/src/vault.rs:180 | Web additionally seals, probes, then swaps ("Test and swap in"); Keleustes swaps blindly |
| Rotation steps report / keep old key for grace period | VaultEntryDetail.vue:216-245 | missing | none | Old value overwritten immediately |
| Auto-rotation (schedule, management key, grace) | VaultEntryDialog.vue:552-640; docs/secrets-vault.md:293-312 | missing | none | P2 for single user |
| Management key kind | VaultEntryDialog.vue:96-102; VaultKeysTab.vue:229 | missing | none | Only needed with auto-rotation |
| Delete / revoke key with confirm | VaultEntryDetail.vue:176-184, :426-455 | done | budgets.rs:314-320, :976 | |
| Expiry presets / custom date / never; denied after expiry | VaultEntryDialog.vue:498-550; vault-model.ts:415-441 | missing | none | `SecretMeta` has no `expires_at` (kls-store/src/vault.rs:42-50) |
| State chips (Serving / Expiring soon / Expired / Revoked) | vault-model.ts:443-455 | missing | none | Key row shows only added/rotated ago |
| Stat tiles (keys, serving, expiring 14d, expired) | vault.vue:240-285 | missing | none | Overview shows "Grants today: N" only (budgets.rs:285-293) |
| Last used | VaultEntryDetail.vue:395-425 | partial | budgets.rs:1404-1408 | `last_used_at` only in hover text ("last granted") |
| Integrity checks per entry (decrypt opens, fingerprint matches) | VaultEntryDetail.vue:322-394; vault_service.go:1100 | missing | none | Vault can `get` but nothing verifies + records; master key check only at open (kls-vault/src/lib.rs:344) |
| "Run checks" across all entries + banner (N of M verified, last X ago) | vault.vue:96-100, :290-320 | missing | none | |
| "Run checks and test with provider" (stored key probe) | VaultEntryDetail.vue:343-351 | missing | none | Same probe, on the stored value, server/core side |
| Integrity column (Verified / Check failed / Not checked) | VaultKeysTab.vue:29-37, :125 | missing | none | |
| Master-key source + fallback warning | (web: KMS envelope text) | done | budgets.rs:163-168, kls-core/src/vault.rs:135-150 | Keleustes-specific (keyring vs file) |
| Fingerprint / version / provenance / lineage | VaultEntryDetail.vue:250-320 | missing | none | No version history; no fingerprint column |
| Entry detail drawer | vault.vue:395-415, VaultEntryDetail.vue | missing | none | Rows have only Rotate/Delete |
| Deep link to entry / open add dialog | vault.vue:128-135 | partial | budgets.rs:113-131 focus("key:<provider>") | No per-entry focus |
| Duplicate/overlap guard + resolution dialog | VaultEntryDialog.vue:278-290; VaultConflictDialog.vue | partial | kls-vault/src/lib.rs:451-458 | Same provider+env silently replaced; no warning that an existing key is overwritten |
| Where-it-can-be-used grants (per host/node, all / all-except) | VaultAccessEditor.vue | missing | kls-vault/src/lib.rs:545 grant | Grants are per-dispatch by provider list; no per-node allow-list. Single user: relevant for nodes (belai hosts) only |
| "Who serves each provider" matrix | VaultKeysTab.vue:254-300 | partial | budgets.rs:417-495 | Keys grouped under provider; no per-node coverage |
| Environment variables (non-provider secrets, e.g. MCP tokens) | vault.vue:160-168; VaultEnvTab.vue | partial | budgets.rs:496-520 "Keys without a provider" | Any slug works via put, but UI forces a provider; no env kind, no 8-char rule, no scrub counts |
| Env var reserved-name refusal (PATH, LD_PRELOAD, BELAI_*) | VaultEntryDialog.vue:388 | missing | kls-vault/src/lib.rs:277 valid_env | Only shape check |
| Connector secrets tab (Slack/Discord/Jira/webhook...) | VaultConnectorsTab.vue | done (C, 2026-11-22) | kls-core/src/vault/connectors.rs:42 `KINDS` (slack, discord, google_chat, teams, webhook, jira, aws_iam_user, aws_sso, wiz, registry), :146 `encode`, :270 `put`, :315 `replace`, :340 `revoke`, :366 `open` (kill switch, one ledger entry per secret and surface per hour), :233 `list` (used by); never granted kls-vault/src/lib.rs:349, :915; UI kls-ui/src/screens/vault.rs:1418 Connectors tab, :1514 add/replace form; tests kls-vault `connector_secrets_are_never_granted`, kls-core `vault::kiro::tests::connector_secrets_are_kept_never_granted_and_opened_audited` | No Keleustes integration opens one yet (the chat webhook stays a sealed blob, kls-core/src/audit/notify.rs) |
| Activity tab (all vault events, filters All/Changes/Denied) | VaultActivityTab.vue | partial | budgets.rs:530-600 grants table; kls-core/src/vault.rs:170-197 events | Only grants (When/Providers/Purpose/State/Revoke). Saves/rotations/deletes are emitted as `vault.*` events and show in Audit, not on the vault; no "denied" outcome concept |
| Grant revoke | (web n/a; grants are implicit) | done | budgets.rs:585-600 | Keleustes-specific |
| Missing-key indicator on grant | (web: deny `vault_no_grant`) | done | budgets.rs:560-570 | chip "No key" |
| After add: apply one of the key's models to settings/workflows | vault.vue:84-92; ai/AiProviderApplyDialog.vue | missing | none | Keleustes has provider default/fast model + routing rules + default model (budgets.rs:620-650) but no "apply to settings/workflows blocked for want of a model" |
| List models a stored key reaches | useVault.ts models(); vault_providers.go:350-352 | missing | none | Provider `models` is a hand-typed list (budgets.rs:1258) |
| Add custom provider (id, name, env, models, factory flag) | (web: provider catalogue server-side, CustomProviderDialog) | done | budgets.rs:1226-1290 | Keleustes lacks base URL field, needed for probe |
| Kiro JSON sign-in | VaultEntryDialog.vue:190-207 | done (C, 2026-11-22) | kls-core/src/vault/kiro.rs:118 `parse_login` (refresh_token, client_id, client_secret required; social sign-ins refused; region, start_url, profile ARN, registration expiry checked; re-encoded in belai `kiroauth.Login` shape), :226 file open, :273 `put_login` (provenance on the ledger, :310), :294 kiro-cli `KIRO_API_KEY`; put/rotate re-check kls-core/src/vault.rs:245, :292; stored test mints at SSO-OIDC kiro.rs:341, :486 (kill switch first; a rotated refresh token sealed as the next version, actor `kiro-refresh`, :513); dialog test offline only :541, never gates Save; UI kls-ui/src/screens/vault.rs:745 Kiro pill, Sign-in (JSON) / API key, masked multiline, Open / drop file, :1820 summary; tests `vault::kiro::tests::*`, kls-ui `kiro_connector_ui_tests` | - |
| Commit signing keys tab | VaultSigningTab.vue | see settings section | kls-ui/src/screens/settings.rs | Cross-ref FactorySigningSection |
| Entries as code (IaC export, no values) | vault.vue:194 | missing | none | P2 |

### Recommended non-blocking test design (user request)

1. **kls-core** `vault::probe_key(provider: &Provider, value: &SecretString) -> ProbeResult {ok, skipped, status, ms, error}` and `vault::probe_stored(core, id)`; add `base_url` (and optional `probe_url`) to `Provider` (new migration `0007_vault_probe.sql`, seed: anthropic `https://api.anthropic.com/v1/models` x-api-key + anthropic-version; openai `https://api.openai.com/v1/models` Bearer; google `https://generativelanguage.googleapis.com/v1beta/models` `x-goog-api-key`; openrouter `https://openrouter.ai/api/v1/auth/key` Bearer; mistral `https://api.mistral.ai/v1/models` Bearer). Unknown URL -> `skipped`. Needs an HTTP client crate (reqwest + rustls) in kls-core; 10 s timeout; parse `/models` body to refresh `Provider.models` as a bonus.
2. **kls-ui budgets.rs `KeyForm`**: add `probe: Option<ProbeResult>`, `probing: bool` behind an `Arc<Mutex<..>>` (or a `Loader`-style one-shot) so the probe runs on `model.rt` and the window keeps rendering. A "Test" ghost button next to the key field (and optionally auto-run 800 ms after typing stops). Show a chip: green "accepted · 412 ms", amber "refused · HTTP 401", grey "not tested". Clear on provider/value change. **Do not include the probe in the Save `add_enabled_ui` condition**; Save stays enabled; if the user saves while probing, let the probe finish and drop its result.
3. **Indicator after save**: persist `last_test_at`, `last_test_status` on `vault_secrets`; `put_secret`/`rotate_secret` spawn a background `probe_stored` after sealing and emit `vault.secret_tested`; `key_row` shows a dot (green/amber/grey) with hover "HTTP 401, 2 h ago". A "Test all" button on the Vault panel runs them sequentially, still non-blocking.
4. Keep the dialog open until `put_secret` returns so validation errors (`check_value`, `valid_env`) appear inline instead of as a toast after the typed value has been discarded.

### Gaps
- [P0] Test key, non-blocking indicator (add dialog + stored-key re-test), never gating Save — kls-core/src/vault.rs `probe_key` + Provider `base_url` migration in kls-store; UI in kls-ui/src/screens/budgets.rs `key_dialog`/`key_row` (see design above).
- [P1] Key dialog closes before the save result; errors arrive as a toast and the typed value is gone — budgets.rs:1199-1216: keep the window open with a `saving` flag, close on Ok, show the error inline.
- [P1] Silent overwrite of an existing key for the same provider+env — budgets.rs key_dialog: warn "replaces the key added <ago>" when `view.secrets` already has the pair (kls-vault `put` semantics stay).
- [P1] Edit label without replacing value — wrap `kls_vault::Vault::set_label` in kls-core/src/vault.rs and add an Edit action in budgets.rs `key_row`.
- [P1] Dedicated Vault screen (or at least a nav entry "Vault" focusing Budgets › Keys) — kls-ui/src/screens/mod.rs + lib.rs nav; route `vault.*` events there (events.rs:361).
- [P1] Per-entry integrity check ("opens with master key", records `verified_at`) + "Run checks" — kls-vault `verify(id)` (open_sealed, compare stored fingerprint), kls-store column, UI column in key_row.
- [P1] Vault activity log (saved / rotated / deleted / granted / denied for missing key) on the vault page with filters — kls-core/src/vault.rs: query `vault.*` events from kls-store; budgets.rs new section replacing the grants-only table.
- [P2] Expiry (presets, custom, never) with expiring/expired state and refusal in `grant` — kls-store vault_secrets.expires_at, kls-vault grant filter, budgets.rs chips + stat tiles.
- [P2] Per-node grant scoping (which belai nodes may receive which keys) — kls-vault grant(node_id) check + kls-node dispatch; access editor in budgets.rs.
- [P2] Generic env-var secrets (MCP tokens etc.) as a first-class kind with reserved-name refusal — kls-vault kind column, `valid_env` deny-list; UI tab.
- [P2] Apply-a-model-after-add (to default model / routing rules / workflows lacking a model) and model discovery from the key — kls-core vault::models_for(id) via probe `/models`; dialog in budgets.rs after first save.
- [P2] Rotation with grace (keep previous value N minutes) and version lineage / fingerprints — kls-store vault_secret_versions table; kls-vault rotate keeps prior version.
- [P2] Reveal toggle, label auto-fill `<provider>-prod`, provider logos — budgets.rs key_dialog.
- [P2] Auto-rotation + management keys; Kiro sign-in; connector secrets; IaC export — kls-vault/kls-core; only if those integrations are ported.
## Background behaviours (server)

Web sources are vdb-site (`api/internal/handler/*`, `docs/*`) unless prefixed `website/`. Keleustes paths are under `keleustes/crates/`.

### Engine: wakes, recovery, retries, concurrency

| Behaviour | Web/API source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Event-driven run wakes (queued, cancel/pause/resume/retry, approval decided, card moved, session end) | factory_engine.go:258,334; docs/factory-engine.md:35 | done | kls-engine/src/exec.rs:632 (`on_event`) | In-process bus instead of pg_notify. |
| One-shot timers (step `wake_at`, cron next fire) drive the loop | docs/factory-engine.md:56; factory_engine.go:468 (`armTimer`) | done | kls-engine/src/exec.rs:468 (`next_wake`), :432 | Loop sleeps max 5 min. |
| Startup adoption: running steps back to pending, every active run advanced | factory_engine.go:227; docs/factory.md:3568 | done | kls-engine/src/exec.rs:481 (`recover`) | |
| Periodic recovery pass (every 15 min: queued >2 min, held, deferred, overdue approvals) | docs/factory.md:3568; docs/factory-operations.md:50 | partial | kls-engine/src/exec.rs:495 (`sweep`) | Full sweep only on bus lag or kill-switch lift; `tick` only handles due timers. A queued run blocked by a slot is woken only when a run of the same workflow finishes. |
| Retry with backoff (30 s doubling, cap 10 min) up to `maxAttempts` | docs/factory-engine.md:812 | done | kls-engine/src/exec.rs:379, walker.rs:180 | |
| Stop the line (`limits.stopTheLineAttempts`, `stopped_line:<node>`) | docs/factory-engine.md:112 | done | kls-engine/src/walker.rs:124 | |
| `autonomy.max_attempts` global default | docs/factory-engine.md (crew) | missing | none | Setting declared in kls-core/src/audit/config.rs but never read; node `maxAttempts` defaults to 1. |
| Per-workflow concurrency (`limits.concurrency`) | docs/factory-engine.md:150 | done | kls-engine/src/exec.rs:995 | |
| Factory-wide concurrency (`autonomy.concurrency`, default 4) | docs/factory-engine.md:150; docs/factory.md:3589 | missing | none | Setting inert. |
| Parked runs not counted; `autonomy.parked_concurrency` cap (50) | docs/factory-engine.md:476 (`feParkedRunSQL`) | missing | none | |
| Overlap policy per trigger (`skip_if_running`, `queue`, `cancel_previous`) | docs/factory.md:3589 | done | kls-engine/src/ops.rs:556 | Web `queue` keeps one waiting run and drops the rest; Keleustes queues every fire. |
| Run dedupe key (one run per fire / delivery) | docs/factory.md:4483 | done | kls-engine/src/ops.rs:537, exec.rs:618,738 | |
| Human reject ends run `cancelled` `declined:<node>` | docs/factory-engine.md:140 | done | kls-engine/src/walker.rs:158 | |
| Failed-run notice names step and cause in words (`fwFailureCause`) | docs/factory-engine.md:146; factory_notify_failure.go | missing | none | `run.finished` carries only the machine reason; nothing raises a notice. |

### Kill switch, holds, windows

| Behaviour | Web/API source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Kill switch: nothing starts, cron fires held not dropped, resume re-sweeps | docs/factory-operations.md:39; factory_schedules.go:31 | done | kls-engine/src/exec.rs:643,654; ops.rs:528 | |
| Kill switch modes `pause` (running steps settle) vs `stop` (cancel open runs, release, withdraw dispatches) | docs/factory-operations.md:39; docs/factory-engine.md:156 | missing | kls-store/src/board.rs:101 (`stopped` bool only) | Keleustes "stopped" freezes everything; nothing settles and nothing is cancelled. |
| Kill switch pushed to nodes | belai side | done | kls-node/src/board.rs:122 | |
| A2A blackout enforced (no A2A send/read while on) | website/.repo/a2a-messaging.md:34 | done | kls-core/src/board.rs:671,719 | Broadcast with control frame. |
| Blackout windows (`schedules.blackout`) hold work, `window_end` wake releases | docs/factory.md:3589; docs/factory-engine.md:156 | missing | none | Maintenance windows are sandbox-only (out of scope). |
| Missed-fire policy on release (`schedules.missed`: run_once/skip/run_all, cap 24) | docs/factory.md:3589 | partial | kls-engine/src/exec.rs:519 | Per-trigger `catchUp` (once/skip/all, cap 24) works; the global `schedules.missed` setting is inert. |
| Default schedule zone (`schedules.tz`) | docs/factory.md:3589 | missing | none | Setting inert; each schedule carries its own tz. |
| Held-run status `held` with `hold:<cause>` reason | docs/factory-engine.md:156 | missing | none | Runs simply do not move; the UI cannot say why. |

### Hosts (nodes): admission, no-host, claim, capacity

| Behaviour | Web/API source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Host choice that knows which node has the repo / offers the profile | docs/factory-engine.md:375 (`feClassifyNoHost`, `feRankHosts`) | partial | kls-engine/src/exec.rs:1486 (`pick_node`) | Takes the first online node (sorted id) unless `runOn: node:<id>`; ignores repo checkout, profile, free slots. |
| Admission: no run without a host for the repo (`trigger.skipped no_host_for_repo`, hourly) | docs/factory-engine.md:357 | missing | none | Event and cron triggers always create the run. |
| No-host wait with reason class and ttl `autonomy.no_host_ttl_hours` (6 h) | docs/factory-engine.md:375,470 | partial | kls-engine/src/exec.rs:1502 | Fixed 60 s retry, fixed 1 h ttl (`EngineConfig`), one generic reason; setting inert. |
| Skip instead of fail when the only host is offline (`offline_host_grace_min`) / profile not offered | docs/factory-engine.md:375 | missing | none | |
| Dispatch nobody claims fails `card_unclaimed` after `autonomy.claim_ttl_minutes` (60) | docs/factory-engine.md:420 | missing | none | Crew step waits up to `timeoutMinutes` (240) instead; setting inert. |
| Host full is a wait (`deferred:capacity`, recheck 10 min), not a failure | docs/factory-engine.md:428 (`feCapacityFull`) | missing | none | A refused dispatch fails the attempt. |
| Reserved worker slots for factory dispatches | docs/factory-engine.md:440 | missing | none | Needs node capability report. |
| Crew step follows handoffs (`agent.parent`, 5 levels, `handoff_stalled`) | docs/factory-engine.md:455 | missing | none | A repro card that hands to builder settles the step too early. |
| Live session extends step lease; transcript tokens added to step/run | docs/factory-engine.md:77 | partial | kls-engine/src/exec.rs:783 (`settle_dispatch`) | Tokens taken from event `tokens`; timeout fixed, no heartbeat extension. |
| Session lost sweep (no heartbeat 10 min → session lost/ended) | belai_session_finished.go:322,334 | missing | kls-node/src/server.rs:586 emits `node.offline` only | Sessions of a vanished node stay live; crew steps hang until timeout. |
| Durable dispatch outbox, resend on reconnect, delivered sweep | belai rc dispatch | done | kls-node/src/gateway.rs:38; server.rs:509 | |

### Approvals, gates, waits

| Behaviour | Web/API source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Approval SLA: re-notify and extend (`park_and_renotify`) or expire and fail (`end_run`) | docs/factory-engine.md:506 | done | kls-engine/src/exec.rs:1736 | |
| Re-notify actually reaches the person | docs/factory-engine.md:506 | partial | kls-app/src/desktop.rs:353 | `approval.renotified` is emitted but the notifier dedupes by checkpoint key, so the OS toast is not shown again. |
| `autonomy.plan_approval` forces a plan approval before crews | config key in docs/factory.md 2.15 | missing | none | Setting inert. |
| Gate waits on checks, woken by check/PR webhooks | docs/factory-engine.md:493 | partial | kls-engine/src/exec.rs:1430,1455 | Rechecks every 5 min on a timer; `github.pull.updated` events do not wake the gate. |
| Wait-for-event step | docs/factory-engine.md (wait) | done | kls-engine/src/exec.rs:876 | |
| Quality gate / release jail on scan findings | docs/factory-engine.md:493 | missing | none | No scan node results feed the gate. |

### Budgets

| Behaviour | Web/API source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| `budgetCheck` before crew dispatch and decisions; actions `hard_stop`, `pause_run`, `escalate_human`, `downgrade_to_fast`, `alert_only` | docs/factory-engine.md:729 | missing | kls-core/src/vault.rs:594,669 (unused) | `check_budget_now` exists and is tested but no engine/gateway path calls it. Budgets never stop anything. |
| Alert thresholds (`alertAt`, `budget.alerts`) write alert rows and notify | docs/factory-engine.md:729 | missing | none | |
| `budget.on_exceed` global action | config key | missing | none | Setting inert. |
| Run and month token caps (`limits.runBudgetTokens`, `monthBudgetTokens`) | docs/factory-engine.md:729 | missing | kls-engine/src/validate.rs:227, dryrun.rs:181 | Validated and estimated only. |
| Repo `monthly_token_cap` | docs/factory.md 2.5 | missing | kls-store/src/github.rs:58 (stored) | |
| Downgrade to `models.fast` on limit | docs/factory-engine.md:760 | missing | none | |
| Usage ledger from sessions (planned at dispatch, adjusted on settle) | docs/factory-engine.md:770 | partial | kls-core/src/vault.rs:789 (`sync_usage`) | Usage read from sessions on demand; no reservation at dispatch. |

### GitHub plane: ingest, quota, triggers

| Behaviour | Web/API source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Event ingest (App webhooks; poll fallback every 1/10 min) | docs/factory.md:3634,3684; belai_github_scheduler.go:34 | done | kls-github/src/poller.rs:45; service.rs:180 | Poller replaces webhooks; `github.poll_interval_s` default 120 s, `github.poll` on/off. |
| ETag conditional reads; 304 not counted | docs/factory.md:3692 | done | kls-github/src/client.rs | |
| Mirrored event kinds: pulls (+checks, reviews), issues, labels | docs/factory.md:3634 table | done | kls-github/src/sync.rs:234,451,613 | |
| Mirrored event kinds: push, release, workflow_run, standalone check_run/suite, pull_request_review, issue_comment, repository, dependabot/code-scanning/secret-scanning alerts, deployment_status, registry_package, merge_group, review comments | docs/factory.md:3634 table, 4667 (12.7) | missing | kls-engine/src/forge.rs:496 (`webhook_events`) maps only PR/issue/label | Webhook triggers on these events can be saved but never fire. |
| Quota guard classes `user` / `time_critical` / `deferrable`, factory share, reserve, secondary limit | docs/factory.md:3692 | done | kls-github/src/budget.rs:104 | |
| Factory live reads classed `deferrable` | docs/factory.md:3692 | partial | kls-github/src/budget.rs:24 | Every `factory:*` caller is `TimeCritical`; only the poller is deferrable. |
| Refusal action `onQuotaLow` / `forge.quota.on_exhausted` (use_cached, defer with `quota_resume` wake, escalate approval) | docs/factory-engine.md:709 | missing | none | A refused call fails the step. |
| Per-run cap `limits.githubCallsPerRun.max` (`run_cap`) | docs/factory-engine.md:709 | missing | none | |
| Quota events `share_threshold` 50/80/100, `exhausted`, `reset` | docs/factory.md:3740 | partial | kls-github/src/client.rs:200,257 | Only `observed` and `limited`. |
| `forge.quota.proactive_writes_per_hour` (60) | website/.repo/software-factory.md:206 | missing | none | |
| Webhook trigger fires on event name | docs/factory.md:4346 | done | kls-engine/src/exec.rs:713 | |
| Trigger conditions at fire time (`subject`, `origin` same_repo/fork, branches, author, paths, `profile`, `severity`, `environment`, `packageType`) | docs/factory.md:4346, 4667; website/.repo/software-factory.md:175 | missing | none | Only the event name (or label) is matched. |
| Safeguard: own work (`own_branch` via `forge.branch_prefix`, `own_identity`) | docs/factory.md:4483 | missing | none | Factory's own PRs can retrigger workflows (loops). |
| Safeguard: coalescing window (`debounceSeconds`, 30 s for push/synchronize) | docs/factory.md:4483 | missing | none | Each poll diff starts a run. |
| Safeguard: `maxRunsPerHour` per trigger (60) | docs/factory.md:4483 | missing | none | |
| Safeguard: fork PR must pass a human node (`fork_needs_human`) | docs/factory.md:4483 | missing | none | |
| Managed repo / enabled org gate | docs/factory.md:4483 | done | kls-github/src/poller.rs:69; kls-engine/src/ops.rs:555 | Unmanaged repos produce no events; scope checked at start. |
| Label trigger (case-insensitive) | docs/factory.md:4695 | done | kls-engine/src/exec.rs:718 | |
| Label trust (`label_untrusted_sender`, `allowBots`, `fromFactory`) | docs/factory.md:4695 | missing | none | Poller does not record who added a label. |
| Slash command `/sf <name>` from OWNER/MEMBER/COLLABORATOR | docs/factory.md:4695 | missing | none | Comments not mirrored. |
| Unlabel cancels queued runs; `sf:stop` cancels every open run on the subject | docs/factory.md:4695 | missing | none | |
| Fan-out order by trigger `priority`, `factory.fanoutCap` (6) | docs/factory.md:4695 | missing | none | |
| `trigger.skipped` events with reason, hourly dedupe | docs/factory.md:4483 | partial | kls-engine/src/exec.rs:578,763 | `schedule.skipped` for overlap/missed/errors only. |
| `sf:` label set (34) synced into managed repos | docs/factory.md:4723 | done | kls-github/src/labels.rs; poller.rs:85 | |
| Cron fan-out: one run per in-scope repo that an online host has, staggered over 6 h | factory_cron_stagger.go:21,41; docs/factory-engine.md:357 (`fwPlanFanout`) | missing | none | A cron fire makes one run with no repo. |

### Defaults, autonomy, outputs

| Behaviour | Web/API source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| 42 default workflows seeded per managed org/repo, upgraded with the catalogue, gated by `defaults.enabled`, `defaults.<key>.enabled`, `defaultsOff`, autonomy | docs/factory.md:4741,4780; website/.repo/software-factory.md:190 | missing | kls-engine/src/templates.json (8 templates); kls-store/src/github.rs:64 (`defaults_off` stored) | `defaults.enabled` inert; nothing runs out of the box. |
| Autonomy enforced on outputs (observe / draft_pr / approve-when-ready), `autonomy.default` for repos without settings, `autonomy.max` cap | docs/factory.md:4780 (13.2); docs/factory-engine.md:514 | missing | kls-store/src/github.rs:56 (stored only) | `draft_pr`, `approve_ready`, `merge` run regardless of the repo's autonomy. |
| Factory never merges | website/.repo/software-factory.md:192 | partial | kls-engine/src/forge.rs:321 (`merge` action) | Keleustes adds a merge output: a deliberate divergence or a safety gap; at minimum gate it on autonomy `merge`. |
| `approve.sensitive_paths` blocks approve-when-ready | website/.repo/software-factory.md:206 | missing | none | |
| `forge.branch_prefix`, `forge.pr_state`, `forge.protected` honoured by outputs | config keys docs/factory.md 2.15 | missing | none | Inert. |
| `conflict_cards` output with mergeability rechecks (0/1/3/10 min), one card per PR | docs/factory.md:4878 | missing | kls-engine/src/forge.rs:177 reads `mergeable` only | |
| Repo `kanban_mode` drives issue mirroring | docs/factory.md 2.5 | missing | kls-store/src/github.rs:58 | Poller uses `mirror_issues` flags; `kanban_mode` is not read. |

### Kanban ↔ GitHub sync

| Behaviour | Web/API source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Inbound: open issue → triage card; close → done; reopen → triage; title follows | belai_github_scheduler.go:221; website/.repo/belai-github-sync.md | done | kls-github/src/sync.rs:532 (`issue_card`) | |
| Outbound: card → issue (open the issue, push title/body/list/priority/labels, every 30 s) | belai_github_scheduler.go:112,131 | missing | none | One-way only. |
| Retry queue for failed pushes (15 s), `lost_access` backoff (15 min) | belai_github_scheduler.go:120,192 | missing | none | |
| Edit conflicts held per field with policies (priority follows board, rest ask) | website/.repo/belai-github-sync.md:57 | missing | none | Inbound sync overwrites the card title. |
| Conflict escalation after `escalateHours` (24) with a notice | belai_github_scheduler.go:284 | missing | none | |

### Notifications (OS notifications replace web push/email)

| Behaviour | Web/API source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Approval waiting / gate / session question → notice | docs/factory-engine.md:506 | done | kls-app/src/desktop.rs:353 | Gated by `notify.desktop`, `notify.approvals`, `notify.questions`. |
| PR merge conflict → notice | belai-github-sync / conflicts | done | kls-app/src/desktop.rs (`conflict_note`) | `notify.conflicts`. |
| Workflow `notify` node delivers its message | docs/factory-engine.md (notify) | missing | kls-engine/src/exec.rs:1262 emits `run.notify` | Nothing consumes `run.notify`; the node is silent. |
| Run failed notice | factory_notify_failure.go | missing | none | |
| Session finished notice (headline, result line, duration) | belai_session_finished.go:290; docs/factory-events.md:26 | missing | none | |
| Budget limit / alert notices | docs/factory-engine.md:729 | missing | none | |
| Event filter for notices (`notify.events`) | config key | missing | none | Setting inert. |
| Outbound connectors (Slack/Teams/webhook) with routes, digests, hourly rate limit, retries, prune | messaging_dispatcher.go:45-52,441,757 | missing | none | P2: only if the desktop should reach chat. |

### Retention, ledgers, sweeps

| Behaviour | Web/API source (file:line) | Keleustes | Keleustes file | Notes |
|---|---|---|---|---|
| Event pruning after 30 days | docs/factory-events.md:66; docs/factory.md:3568 | missing | none | `events` table grows without bound. |
| `persistence.retention` (sessions, transcripts) | config key | missing | none | Inert. |
| Teleport retention sweep | n/a (Keleustes M6) | done | kls-core/src/audit.rs:893 | |
| Audit ledger fold from events and host audit lines | docs/factory-engine.md:776 | done | kls-core/src/audit.rs:893 | |
| `audit.calls` (record each GitHub call in the ledger) | config key | missing | none | Inert. |
| Lifecycle ledger sweep (stage episodes, `lifecycle` entered/exited events, Flow reads, 5 min) | factory_lifecycle_sweep.go:25,44; docs/factory-lifecycle.md:131 | missing | none | Cockpit Flow panel has no data source. |
| Delivery derivation (outcomes, decisions, commit links, people) | factory_delivery_sweep.go:45,290 | done | kls-core/src/github.rs:276,485,637; kls-github/src/sync.rs:718 | Computed on read plus capture at poll; no 3-day backfill window needed locally. |
| Firewall sightings subscriber (package seen/blocked, capability, egress → triage; 10 min batch; 7 d retention) | factory_sources.go:46-50,173 | missing | none | Keleustes has no firewall source; P2. |
| Vault auto-rotation when due and deletion of the old key after grace (15 min) | vault_derived.go:293; vault_service.go:1492 | missing | kls-core/src/vault.rs:180 (manual rotate only) | |
| Vault entry expiry (expiring in 14 d, expired → retrieval denied) | website/src/pages/resolve/vault.vue:67 | partial | kls-vault/src/lib.rs:616 | Grants expire; entries do not. |
| Cross-process event pickup (CLI writes seen by the app) | n/a | done | kls-core/src/lib.rs:163 | |

Counts: done 30, partial 13, missing 59.

### Gaps

- [P0] Budget enforcement: no budget ever stops or pauses work — in `kls-engine/src/exec.rs` `dispatch()` call `kls_core::vault::check_budget_now` with the node's planned tokens; map Block → `hard_stop` fail / `pause_run` (ops::pause_run) / `escalate_human` approval; also enforce `limits.runBudgetTokens` against `run.tokens`.
- [P0] Autonomy enforcement on outputs (`observe`/`draft_pr`/approve; `autonomy.default`, `autonomy.max`; gate or drop `merge`) — check `RepoSettings.autonomy` in `kls-engine/src/forge.rs::action` before `draft_pr`/`approve_ready`/`merge`/`pr_review`.
- [P0] Trigger safeguards: own-branch/own-identity, coalescing (`debounceSeconds`), `maxRunsPerHour` — in `kls-engine/src/exec.rs::fire_event_triggers`; poll events need `headRef`/`author` in data from `kls-github/src/sync.rs`.
- [P0] Webhook trigger conditions (subject, origin/fork, branches, author, paths) evaluated at fire time — add a condition matcher in `kls-engine/src/forge.rs` next to `webhook_matches`; include fork-needs-human check.
- [P0] Session lost when a node vanishes — in `kls-node/src/server.rs` on `NODE_OFFLINE` (+10 min grace) end its live sessions via `kls-store/src/sessions.rs`, emitting `session.ended` so crew steps settle.
- [P1] Notify node, run-failed and session-finished OS notices — extend `kls-app/src/desktop.rs::start_notifications` to consume `run.notify`, `run.finished` (failed, with cause text from walker reason) and `session.ended`; honour `notify.events`.
- [P1] Default workflows catalogue + seeding per managed repo (`defaults.enabled`, per-key, `defaults_off`) — `kls-engine/src/templates.rs` seed on `github.org.updated`/managed repo; gate in `ops::start_run`.
- [P1] Kill switch pause vs stop — add `mode` to `FactoryControl` (`kls-store/src/board.rs`), let running steps settle under pause, and on stop cancel open runs/withdraw dispatches in `kls-engine/src/ops.rs`.
- [P1] Repo-aware node choice + admission (`no_host_for_repo`) — `kls-engine/src/exec.rs::pick_node` should use node-reported repos/profiles (kls-node hub capabilities) and skip run creation when none qualifies.
- [P1] Read inert engine settings: `autonomy.concurrency`, `autonomy.max_attempts`, `autonomy.claim_ttl_minutes`, `autonomy.no_host_ttl_hours`, `schedules.tz`, `schedules.missed`, `budget.on_exceed` — load into `EngineConfig` from settings in `kls-engine/src/exec.rs::start`.
- [P1] Card claim TTL (`card_unclaimed`) and capacity wait — in `kls-engine/src/exec.rs::settle_dispatch`/`on_wake`, fail retryably when no session after claim TTL; treat node "full" refusals as `deferred:capacity`.
- [P1] Crew handoff following (`agent.parent`) — `kls-engine/src/exec.rs::settle_dispatch` should check child cards via `kls-core/src/board.rs` before succeeding the step.
- [P1] Outbound card → issue sync with retry and field conflicts — new `kls-github/src/sync.rs` outbound pass in `poller::poll_once`; conflict rows in `kls-store/src/github.rs`; escalation notice.
- [P1] Gate woken by PR/check updates — in `exec.rs::on_event`, on `github.pull.updated` with `checksCompleted` wake waiting gate steps for that repo/PR.
- [P1] Event retention (30 d) and `persistence.retention` — daily prune in `kls-core/src/audit.rs::start` sweep arm, store helpers in `kls-store/src/lib.rs`.
- [P1] Mirror more GitHub event kinds (push, release, workflow_run, review submitted, issue_comment, alerts) — `kls-github/src/sync.rs` + `kinds` in `kls-core/src/github.rs`, mapped in `kls-engine/src/forge.rs::webhook_events`.
- [P1] Label controls: unlabel cancels queued runs, `sf:stop`, `/sf <name>` slash commands, label trust — `kls-engine/src/exec.rs::fire_event_triggers`; needs `labelsRemoved` and comments from `kls-github/src/sync.rs`.
- [P2] Quota refusal actions (`onQuotaLow`, `on_exhausted`, `run_cap`), deferrable factory reads, threshold events — `kls-github/src/budget.rs` (class by call kind), `kls-engine/src/forge.rs` (defer with wake at reset).
- [P2] Cron fan-out per repo with 6 h stagger — `kls-engine/src/exec.rs::fire_one`, iterate `g.scope.repos` × managed repos.
- [P2] Blackout windows and `held` run status with reason — `kls-engine/src/exec.rs::stopped()` → hold check returning a cause; window_end as a step/schedule wake.
- [P2] Fan-out priority and cap per event — sort schedules by `priority` in `fire_event_triggers`, cap at `factory.fanoutCap`.
- [P2] `conflict_cards` output with mergeability rechecks — `kls-engine/src/forge.rs` action + wakes at 1/3/10 min.
- [P2] `approve.sensitive_paths`, `forge.branch_prefix`, `forge.pr_state`, `forge.protected`, `proactive_writes_per_hour` — read in `kls-engine/src/forge.rs`.
- [P2] Budget alerts/downgrade-to-fast and alert notices — `kls-core/src/vault.rs` record alert crossings; desktop notice on `budget.limit`.
- [P2] Vault auto-rotation sweep and entry expiry — periodic task in `kls-core/src/vault.rs` started from `kls-app/src/main.rs`; expiry column in `kls-store/src/vault.rs`.
- [P2] Lifecycle ledger (stage episodes for the Flow panel) — new fold in `kls-core/src/audit.rs` sweep, table in `kls-store`.
- [P2] Approval re-notify shows the OS toast again — key notes by `approval id + slaAt` in `kls-app/src/desktop.rs::needs_notes`.
- [P2] Periodic full sweep (e.g. every 15 min) to catch lost wakes and queued runs blocked by global concurrency — `kls-engine/src/exec.rs::run_loop`.
- [P2] Outbound chat connectors (Slack/Teams/webhook) and firewall sightings — only if desired; new module in `kls-core`.


## Prioritized gaps (consolidated, deduplicated)

- **P0:** the core workflow is broken without it.
- **P1:** commonly used.
- **P2:** nice to have.

Each item is tagged with its area. The section-level `### Gaps` lists above hold the same items with more detail.

### P0

1. **[Vault] Key test that only indicates and never blocks saving.** Add `probe_key(provider, value)` and `probe_stored(id)` in `kls-core/src/vault.rs`, with a reqwest/rustls client, a 10 s timeout, and a `base_url`/`probe_url` column on `Provider` via a kls-store migration. In `kls-ui/src/screens/budgets.rs`, `key_dialog` gets a Test button and a result chip (accepted N ms / refused HTTP N / not tested). Save stays enabled. After save, a background re-test stores `last_test_*`, shown as a dot in `key_row`.
2. **[Background] Budget enforcement.** In `kls-engine/src/exec.rs` `dispatch()`, call `check_budget_now`. Map Block to a hard fail, `ops::pause_run` or an approval. Also enforce `limits.runBudgetTokens`.
3. **[Settings/Background] Wire the inert settings into the engine.** Load them into `EngineConfig` at engine start and on `settings.saved` (`kls-engine/src/exec.rs`, `forge.rs`, kls-github). Read `notify.events` in `kls-app/src/desktop.rs`. Until a key is wired, hide it in `kls-core/src/audit/config.rs`. Keys:
   - `autonomy.max`, `autonomy.default`, `autonomy.concurrency`, `autonomy.max_attempts`, `autonomy.claim_ttl_minutes`, `autonomy.no_host_ttl_hours`
   - `schedules.tz`, `schedules.missed`
   - `budget.on_exceed`, `budget.alerts`
   - `forge.branch_prefix`, `forge.pr_state`, `forge.protected`
   - `approve.sensitive_paths`, `persistence.retention`, `notify.events`, `defaults.enabled`
4. **[Settings/Background] Kill switch.** Add pause vs stop, a confirm dialog, cancelling of running dispatches and release of kanban leases. Put it in `Core::stop_factory` (`kls-core/src/board.rs`) via kls-node jobs, with a `mode` on `FactoryControl` (kls-store `board.rs`) and the UI in `kls-ui/src/screens/settings.rs`.
5. **[Background] Repo autonomy on outputs.** Check `RepoSettings.autonomy` in `kls-engine/src/forge.rs::action` before `draft_pr`, `approve_ready`, `merge` and `pr_review`. Gate or drop `merge`, which the web factory never does.
6. **[Workflows/Background] Webhook trigger conditions.** Conditions are branches, paths, labels, author, drafts, actions, review state and fork-needs-human. Add a `conditions` map to the trigger in `kls-engine/src/graph.rs` and match it next to `webhook_matches` in `forge.rs`. Inspector fields go in `kls-ui/src/screens/workflows.rs`.
7. **[Workflows/Background] Trigger safeguards.** Add `debounceSeconds`, `maxRunsPerHour` and an own-branch/own-identity check in `kls-engine/src/exec.rs::fire_event_triggers`, with last-fire per trigger in `kls-store/src/engine.rs`.
8. **[Background] Lost sessions.** On `NODE_OFFLINE` plus a 10-minute grace, end that node's live sessions. Do it in `kls-node/src/server.rs`, write through `kls-store/src/sessions.rs` and emit `session.ended` so crew steps settle.
9. **[Board] Card claim/lease model.** Add `claimed_by`, lease expiry, `depends_on`, attempts, continuations, a Release-claim action and a `claim_lapsed` checkpoint. Use `kls-store/src/board.rs` Card and `kls-core/src/board.rs`, and surface them in the board drawer and `needs.rs`.
10. **[Checkpoints] Held-card checkpoints.** For `checkpoint:*` labels, add Approve, and Reject with a reason (reject moves the card to blocked). Use `kls-core/src/board.rs` `decide_card`, a `CardHeld` need kind in `needs.rs`, and the board drawer.
11. **[Board] Lists out of step with the web.** There is no Blocked list, and Keleustes uses triage/todo/doing where the web uses in_progress. Align `STATUSES` in `kls-core/src/board.rs`.
12. **[Board] Triage by cause for blocked cards.** Cards need a cause plus Retry, Reassign, Edit, Dismiss, Snooze and a suggested fix. Add a new tab in `kls-ui/src/screens/board.rs`, with a cause parser in `kls-core/src/board.rs`.
13. **[Checkpoints] PR actions and bulk PR bar.** Actions are ready, draft, update branch and request reviewers. Reuse `kls-github` `sync::pull_action`/`bulk_pull_action` in `kls-ui/src/screens/checkpoints.rs`.
14. **[Workforce] Saved agents and crews never reach a node.** Add a push/install RPC in `kls-proto` `node.proto`, served from `kls-node/src/gateway.rs` and triggered by `kls-core::vault::save_agent`.
15. **[Agent builder] Structured builder fields.** Fields are mode, autonomy, max_iterations, effort, guardrails, ask_permission, kanban handoffs, workspace, budget/memory and knowledge_paths. Make them typed fields on AgentProfile in `kls-store/src/vault.rs` instead of `meta_json`, with section forms in `kls-ui/src/screens/workforce.rs`.

### P1

- **[Vault]**
  - Keep the key dialog open until the save returns, and show errors inline in `budgets.rs` `key_dialog`.
  - Warn before silently replacing an existing key for the same provider and env.
  - Edit a key's label without replacing its value: wrap `kls_vault::Vault::set_label` in kls-core and add an Edit action in `key_row`.
  - Give the vault its own screen or nav entry, and route `vault.*` events there (`kls-ui/src/events.rs:361`).
  - Per-entry integrity check plus "Run checks": a kls-vault `verify(id)`, a `verified_at` column, and an Integrity column.
  - Vault activity log of saved, rotated, deleted, granted and missing-key events, with All/Changes/Denied filters, built from `vault.*` events in kls-store.
- **[Board]**
  - Assign sheet (worker or crew), and Send to workflow (one run per card) via kls-engine ops `start_run` with `card_id`.
  - Multi-select with a bulk bar (move, priority, delete, send).
  - Review tab semantics: Confirm moves to Backlog, Dismiss moves to Done `not-planned`; keys J/K/C/X.
  - Card GitHub tab: issue state, labels, comments, Sync now, unlink, mirror card / open issue. Use `kls-github/src/sync.rs`.
  - Drawer: add a note, Sessions and Run tabs, an Events-for-card link, copy link.
  - Board lens (Needs me / Unrouted / P1 / From a workflow) and swimlanes by repo, workflow or crew.
- **[Events]**
  - Structured filters (run, card, repo, node, wf, session, needs) and an `Events::focus(filter)` deep link that every screen uses (`kls-ui/src/events.rs`).
  - Time range and Load older via `events_before`/`events_between` in kls-store.
  - Event detail pane (facts, links, payload JSON).
  - 30-day retention prune in `kls-core/src/audit.rs`.
- **[Cockpit]**
  - Time range, plus KPIs: tokens, spend, done today, draft PRs, free slots, quota, cache hit.
  - Running-now workflow lanes with node states.
- **[Checkpoints]**
  - Missing checkpoint kinds: draft PRs, external issues with no team reply, node offline, deploy gates (`kls-core/src/needs.rs`).
  - Repo and author filters, PR inspector facts and file list.
  - Merge-conflict ledger with Retry, and "Resolve conflicts" to file a card.
  - Merge uses the repo's `merge_method` instead of hard-coded squash (`checkpoints.rs:522`).
- **[Workforce/Session]**
  - Worker control: pause, resume, start, back up, crew pause/resume all, and a log tail.
  - Crew thread and A2A UI on `kls-core/src/board.rs` `a2a_*`.
  - Session controls strip with slash commands and `!` shell mode in `sessions.rs::composer_ui`.
  - Answers with choices, plan review and pinned asks in `sessions.rs::answer_ui`.
  - Session factory context: run step, budgets, PR checks, cards, todo list.
  - Workforce needs-you strip and state filters.
  - Crew validation (claim conflicts, unclaimed labels, replicas cap) in `kls-core/src/vault.rs::save_crew`.
- **[Agent builder]**
  - Per-field draft offers (Take / Drop / Undo / Take all).
  - Autosave and "Continue your draft".
- **[Runs]**
  - Retry from a chosen step, and a cancel confirm in `kls-ui/src/screens/runs.rs`.
  - Run totals: budget %, estimated cost, handoffs (`kls-engine/src/ops.rs::run_detail`).
- **[Workflows]**
  - Per-trigger enable switches.
  - Schedules view: create, re-time, delete, kind filter (`ops.rs` plus `workflows.rs` `schedules_ui`).
  - List columns: on/off, trigger summary, repo count, runs in the last 7 days.
  - Output fields for draft PR, file issues and check run.
  - Crew node: model picker, per-member models, run-on node and offline fallback.
  - Per-workflow agent limits (`graph.rs` `Limits`).
  - Publish panel with a note and problem count.
- **[Templates]**
  - Add ci-failure-fixer, dependabot-triage, ui-polish-pass, security-patch-train and release-provenance to `kls-engine/src/templates.json`.
  - Template search, and a run budget in the Use form.
- **[Repos]**
  - Issue mirroring per board project (`kls-github/src/sync.rs` plus a mirroring table).
  - Repo detail: attached workflows and bulk attach.
  - Default workflow catalogue, seeding per managed repo, and honouring `RepoSettings.defaults_off` (`kls-engine/src/templates.rs`).
- **[Budgets]** GitHub quota rules editor (share, reserve, on-exhausted) with a per-workflow call breakdown (`kls-github/src/budget.rs`).
- **[Audit]**
  - Open links from an entry to its card, run, session, workflow or events.
  - Filters for card, agent, repo, workflow/run and vuln (`LedgerQuery`, `kls-store/src/audit.rs:169`).
  - Signed export with a range picker and save dialog.
- **[Settings]**
  - Per-agent model map.
  - Crew defaults (guardrails, tests, show_internal_work, auto_commit_per_task, `a2a.show`, lease).
  - System One section.
  - Default run-on node, fallback and maintenance window.
  - Schedule blackout windows.
  - Project overrides per node and directory, with ack.
- **[Conflicts]** Lockfile handling, keeping resolutions across a `head_moved` reload, extended description (`kls-github/src/conflicts.rs`).
- **[Delivery]** Outcome detail screen (timeline, cost by model, people): `outcome(key)` in `kls-core/src/github.rs` and a tab in `delivery.rs`.
- **[Background]**
  - OS notices for notify nodes, failed runs and finished sessions (`kls-app/src/desktop.rs::start_notifications`).
  - Repo-aware node choice and `no_host_for_repo` admission (`exec.rs::pick_node`).
  - Card claim TTL and capacity wait.
  - Crew handoff following (`agent.parent`).
  - Outbound card-to-issue sync with retry and field conflicts.
  - Gates woken by PR/check updates.
  - Mirror more GitHub event kinds: push, release, workflow_run, review submitted, issue_comment, alerts.
  - Label controls: unlabel cancels queued runs, `sf:stop`, `/sf <name>`, label trust.

### P2

- **[Vault]**
  - Expiry (presets / custom / never) with expiring and expired states.
  - Per-node grant scoping.
  - Generic env-var secrets (MCP tokens) with a reserved-name deny-list.
  - Apply a model after add, and discover models from the key.
  - Rotation grace period and version lineage with fingerprints.
  - Reveal toggle, `<provider>-prod` label auto-fill, logos.
  - Auto-rotation with management keys, Kiro sign-in, connector secrets, IaC export.
- **[Board]**
  - Header GitHub stamp.
  - Right rail.
  - Backlog inline priority, Unrouted/Stale modes, Done grouped by day with Reopen.
  - Sync-conflicts three-way tab.
- **[Cockpit]**
  - HUD panels: System One, token economy, runs-by-hour heatmap, gates and provenance.
  - Fleet/Health: workers table, run graph, repo eligibility, late heartbeat, version skew, friction, flow queues.
  - Needs-you counts by kind, plus a stream status indicator.
- **[Events]** Saved views, JSONL export, origin chips, fact filter.
- **[Checkpoints]** Paging and source stamp.
- **[Workforce/Session/Runs]**
  - Fact index: model, provider, mode, effort, version timeline, touched/contacted.
  - Runs list totals and Repo/Model columns.
  - Session source tabs and grouping by crew.
  - Decision-step rendering.
  - Evidence pack download.
  - Builder extras: export/import markdown, files, palette, tool groups.
  - Git strip actions: branch switch, sync toggle, teleport.
  - Wall mode and agent theming.
- **[Workflows]**
  - System One decision node.
  - Scan and GitHub-read nodes.
  - "Also start on" extra triggers, add-approval-after, quick-fix for unrouted outcomes.
  - YAML/JSON view and download.
  - Dry-run repo picker and call ranges.
  - Header token estimate.
  - Start a workflow from an event.
  - Ranked template suggestions.
  - Blocked state when no model is available.
- **[Budgets]**
  - Nesting view.
  - Workflow, node and org scopes.
  - Multiple alert thresholds.
  - Tool-call bars.
  - Alert crossings, and downgrade to the fast model.
- **[Repos]** Signal chips, sort, org filter, findings/SLSA columns, stop-watching bulk, protected-branch list.
- **[Trust]**
  - Matrix totals, evidence index export, egress host list.
  - Firewall/egress review strip, and sightings turned into review cards.
- **[Settings]** Kanban binding settings, forge quota extras, a "setting change events" link, chat-channel notifications and quiet hours.
- **[Audit]** Day grouping, cursor paging past 5000, multi-select delete, fact chips, signed decision ledger, weekly report.
- **[Delivery/Memory/Conflicts]**
  - Estimate model (rates, role multipliers, baselines) and team-equivalent figures.
  - Today and MTD ranges.
  - Contributor part filter.
  - Memory paging, grouping, source host and A2A link.
  - J/K file navigation.
- **[Background]**
  - Quota refusal actions and deferrable reads.
  - Cron fan-out per repo with stagger.
  - `held` run status during blackout.
  - Fan-out priority and cap.
  - `conflict_cards` output.
  - Lifecycle ledger.
  - Approval re-notify.
  - Periodic full sweep (every 15 min).


## Status re-check (2026-11-22)

Every item in the consolidated P0/P1/P2 list above was re-verified against the code on 2026-11-22. The request docs were treated as hints only. A row is `done` only when cited code implements it, and a UI item only when a screen calls that code. P0 #3 is split into one row per settings key, plus a row for the stale `config::PENDING` list. A bullet that bundles several features is `partial` unless every part exists. The section-level P2 item "outbound chat connectors" is optional and was not re-checked.

Totals (157 rows):

| priority | done | partial | missing |
|---|---|---|---|
| P0 | 20 | 7 | 5 |
| P1 | 41 | 20 | 8 |
| P2 | 21 | 26 | 9 |

Counted by the 15 numbered P0 items: 10 done (1, 2, 5, 6, 8, 9, 10, 11, 12, 15) and 5 partial (3, 4, 7, 13, 14).

Cross-cutting findings:

- **Stop never releases card claims.** `Core::release_all_claims` (`kls-core/src/board/cards.rs:479`) and `ops::stop_all_runs` (`kls-engine/src/ops.rs:791`) are called only from tests. `kls-app/src/control.rs` still loops `cancel_run`.
- **The engine ignores schedule blackout windows.** Cron, event triggers and dispatch use `guard::halted` (`kls-engine/src/guard.rs:16`), which reads only the pause/stop mode. Only provision steps and conflict cards call `control::check`, which knows about the windows.
- **`config::PENDING` is stale in both directions.** It marks keys as pending that are already read, while `autonomy.max_attempts`, `schedules.missed`, `forge.pr_state`, `forge.protected` and `approve.sensitive_paths` have no reader at all.
- **Workflow inspector fields with no effect.** The engine never reads `reviewers` (draft_pr), `minSeverity` / `group` / `maxIssues` (file_issues) or `when` (check_run).
- **ci-fix stacks onto a branch literally named `pr_head`.** `forge.rs:238-247` resolves only `""` and `"default"`.

| item | area | priority | status | evidence (file:line, under `crates/`) | what's left |
|---|---|---|---|---|---|
| Key test, indicate only, never blocks save | Vault | P0 | done | kls-core/src/vault/probe.rs:23 (10 s TIMEOUT), :157 `probe_key`, :250 `probe_stored`; kls-store/migrations/sqlite/0010_governance.sql:19 (`probe_url`); kls-ui/src/screens/vault.rs:714 Test button, :724 `test_chip`, :731 "Save depends on the form only"; kls-core/src/vault.rs:217 `after_save` -> `spawn_probe`; kls-ui/src/screens/vault.rs:1205 chip in `key_row` | - (lives in vault.rs screen, not budgets.rs) |
| Budget enforcement in dispatch + runBudgetTokens | Budgets | P0 | done | kls-engine/src/exec/policy.rs:174 `budget_check` (run cap `run_budget_exceeded`, `budget_gate_scoped`, Block → `budget_exceeded`, Pause → wait + `ops::pause_run`, Escalate → a `budget` approval, approve re-runs past the gate ops.rs:1181, Downgrade → fast tier) before dispatch, jobs and decisions; test `escalate_budget_files_an_approval` | - |
| autonomy.max | Settings | P0 | done | kls-engine/src/guard.rs:146 (cap in `effective_autonomy`); callers exec.rs:2159, defaults.rs:459, conflicts.rs:63 | - |
| autonomy.default | Settings | P0 | done | kls-engine/src/guard.rs:141 | - |
| autonomy.concurrency | Settings | P0 | done | kls-engine/src/exec.rs:1761-1766 (factory-wide cap) | - |
| autonomy.max_attempts | Settings | P0 | done | kls-engine/src/exec.rs:1958 (`Effective.max_attempts` is the stop-the-line limit of a workflow that sets none, before `walker::plan`); test exec/tests/engine_policy.rs `max_attempts_setting_stops_the_line` | Governance: drop from `config::PENDING` (requested) |
| autonomy.claim_ttl_minutes | Settings | P0 | done | kls-engine/src/exec.rs:2758-2770 (route-card unclaimed wake, `claim_ttl_ms`), `card_unclaimed` fail exec.rs:3328 | - (WP1: dropped from `config::PENDING`) |
| autonomy.no_host_ttl_hours | Settings | P0 | done | kls-engine/src/exec.rs:2377-2382 | - |
| schedules.tz | Settings | P0 | done | a cron trigger with no zone keeps `tz` empty and follows the setting: kls-engine/src/ops.rs:294 `schedule_zone`, :324 `trigger_specs_in` (publish), exec.rs:731-748 (fire time); defaults no longer pin UTC (defaults.rs `cron`, defaults_cli.rs:1227); test `schedule_settings_fill_zone_and_catch_up` | Governance: drop from `config::PENDING` (requested) |
| schedules.missed | Settings | P0 | done | kls-engine/src/exec.rs:736 (`run_once`/`skip`/`run_all` → `once`/`skip`/`all` when a trigger sets no `catchUp`); test `schedule_settings_fill_zone_and_catch_up` | Governance: drop from `config::PENDING` (requested) |
| budget.on_exceed | Settings | P0 | done | kls-core/src/vault.rs:1217 `default_budget_action`, used :1258 and :1384 | - |
| budget.alerts | Settings | P0 | done | kls-core/src/vault.rs:1252-1257, :1272-1279 | - |
| forge.branch_prefix | Settings | P0 | done | own-branch guard kls-engine/src/exec.rs:1116-1121; names the factory branch in `draft_pr` (kls-engine/src/forge.rs:229 `branch_name`, :338-345); test forge/tests.rs:1057 | - (PENDING entry removal requested from governance) |
| forge.pr_state | Settings | P0 | done | kls-engine/src/forge.rs:397-401 (node `prState`, else `Effective.pr_ready`); test kls-engine/src/forge/tests.rs:1057 | - (PENDING entry removal requested) |
| forge.protected | Settings | P0 | done | kls-github/src/forge/guard.rs:110-147 (`protection_on` with the repo switch, `protected_branch`: default branch or forge-protected); `draft_pr` refuses + audits kls-engine/src/forge.rs:358-362; conflict commits kls-github/src/conflicts.rs (`commit`); test forge/tests.rs:1057 | - |
| approve.sensitive_paths | Settings | P0 | done | kls-engine/src/forge/outputs.rs:596-625 (built-in globs + setting), used by `approve_ready` kls-engine/src/forge.rs:499; test forge/tests.rs:954 | - |
| persistence.retention | Settings | P0 | done | kls-core/src/sweeps.rs:122-141 (deletes ended sessions), started kls-app/src/main.rs:569 | - |
| notify.events | Settings | P0 | done | kls-app/src/desktop.rs:454 `notify_allows(&eff.notify_events, ..)` | - |
| defaults.enabled | Settings | P0 | done | kls-engine/src/defaults.rs:453-456, conflicts.rs:65 | - |
| PENDING list accuracy (hourglass in Settings) | Settings | P0 | done | kls-core/src/audit/config.rs:522 `PENDING` now holds only keys with no reader (belai.mode, belai.ask_permission, autonomy.plan_approval, audit.calls, systemone.override, kanban.*); test `pending_keys_have_no_reader_outside_config` (config.rs:1521) walks every crate's sources for the key literal or its `Effective` field and fails when a listed key gains a reader | - (WP1, 2026-11-22; a key wired later must be dropped from the list or that test fails) |
| Kill switch pause/stop, confirm, cancel dispatches, release leases | Settings | P0 | done | kls-app/src/control.rs:26 `EngineStopper` calls `kls_engine::ops::stop_all_runs` and `Core::release_all_claims`, then stops factory-dispatched sessions outside runs (one audited `session.stop` each); `claims_released` set; pause undoes nothing (`set_mode` runs stoppers on stop only). Drill test control.rs:184 `stop_drill_cancels_runs_releases_claims_and_audits_each` (one `run.cancel` and one `claim.release` ledger entry each, `factory.mode` carries the counts) | - (WP1, 2026-11-22) |
| 9 Card claim/lease model | Board | P0 | done | `kls-core/src/board/cards.rs:38` Claim (claimedBy, leaseUntil, attempts, continuations, dependsOn in meta_json `belai.agent`), `:60` lapsed, `:446` release_card_claim; `kls-core/src/needs.rs:57,340` ClaimLapsed; `kls-ui/src/screens/board/claims.rs:135-185` drawer claim grid + Release claim; `kls-ui/src/screens/checkpoints.rs:732` | - (stored in meta_json, not Card columns; belai sets claims, Keleustes reads/releases) |
| 10 Held-card checkpoints | Checkpoints | P0 | done | `kls-core/src/board/cards.rs:496` decide_held_card (reject -> belai.list blocked); `kls-core/src/needs.rs:55,338` CardHeld; `kls-ui/src/screens/board/claims.rs:101-137` drawer Approve/Reject w/ required why; `kls-ui/src/screens/checkpoints.rs:713-730` | - |
| 11 Lists out of step / Blocked list | Board | P0 | done | `kls-core/src/board.rs:40-46` STATUSES + documented web mapping (in_progress->doing, blocked->todo+belai.list); `kls-core/src/board/cards.rs:138` is_blocked; `kls-ui/src/screens/board.rs:23-33` Blocked lane; `:706-712` drag in/out of Blocked | - (by mapping, not renaming statuses) |
| 12 Triage by cause | Board | P0 | done | `kls-core/src/board/cards.rs:184-237` Cause + fix() + triage_cause; `kls-ui/src/screens/board.rs:66,400` Triage tab; `kls-ui/src/screens/board/claims.rs:346-455` groups w/ fix, Snooze, Dismiss, Edit/reassign (opens drawer), Release, Retry | - (Reassign is via drawer assign_menu `board.rs:1197`, not inline) |
| 13 Checkpoint PR actions + bulk PR bar | Checkpoints | P0 | done | inspector Ready/Draft/Update branch/Request reviewers via pull_action `kls-ui/src/screens/checkpoints.rs:1058`; bulk bar Update branch/Draft/Ready and Request reviewers (1 to 15, `PullAction::RequestReviewers` through `bulk_pull_action`) `kls-ui/src/screens/checkpoints.rs:502`; only pull rows take PR actions (`is_pull`, `:61`) | - |
| 14 Saved agents/crews reach a node | Workforce | P0 | done | WP5 2026-11-22: save backs up and sends the existing `library_install` (no proto change) to the picked nodes or every node holding it: `kls-core/src/vault/workforce.rs:311` publish_agent, `:373` publish_crew (crew + its saved members in one dispatch), `:201` install_targets, via `hosts::library_dispatch` (console-only, kill switch, A2A blackout, audited with origin). Crews enter the library: `CREW_KIND` `kls-core/src/vault.rs:100`, `crew_to_library` workforce.rs:356. Reconcile answers "install" when the host holds an older library version (never for hooks/MCP): `kls-core/src/belai/library.rs:220-250`. UI: builder Save + "Install on" picker `kls-ui/src/screens/workforce.rs:1255,1337`, crew save :925. Tests: `vault::workforce::tests` (saving_an_agent_installs_it_on_its_nodes, kill_switch_and_blackout_refuse_the_install, crews_enter_the_library_with_their_members, reconcile_answers_install_when_the_library_is_ahead) | - (no automatic install loop on reconcile; the verdict is available to belai) |
| 15 Structured builder fields | Agent builder | P0 | done | Typed `ProfileSpec` has mode, autonomy, max_iterations, effort, guardrails, ask_permission (`kls-engine/src/catalogue.rs:136-171`). Section forms (Behaviour/Protections/Handoffs/Workspace/Budget and memory/Knowledge paths) are in `kls-ui/src/screens/workforce/spec.rs:185-319`, wired at `kls-ui/src/screens/workforce.rs:1084` and saved at `:1142` | - (storage is still `AgentProfile.meta_json` text, `kls-store/src/vault.rs:214`. Typed in the engine/UI, not as store columns) |
| P0-5 Repo autonomy on outputs; merge gated/dropped | Background | P0 | done | `kls-engine/src/exec.rs:2158-2176` (required vs effective autonomy, step skipped + `guard::refuse`); `kls-engine/src/guard.rs:124` (`required_autonomy`), `guard.rs:135` (repo level capped by `autonomy.max`); `kls-engine/src/forge.rs:350-353` (merge bails); `kls-engine/src/graph.rs:655-664` (no `merge` in GITHUB_ACTIONS) | - |
| P0-6 Webhook trigger conditions + inspector fields | Workflows/Background | P0 | done | `kls-engine/src/triggers.rs:33-57` (Conditions: actions, base/head branches, paths/ignore, labels any/all, author type/allow/deny, drafts, origin, review states), `triggers.rs:347` (`check`); `kls-engine/src/exec.rs:1068` (used in `hold_reason`), `exec.rs:1123-1134` (fork needs human); `kls-ui/src/screens/workflows.rs:1740-1796` (`conditions_ui`), `workflows.rs:2110-2119` (webhook/label arms) | - (conditions live in triggers.rs, not graph.rs) |
| P0-7 Trigger safeguards (debounce, maxRunsPerHour, own-branch/identity) | Workflows/Background | P0 | done | rate_check / own_branch as before; own identity for `webhook` triggers kls-engine/src/exec.rs:1313-1322 (`own_identity`) via triggers.rs:476 `factory_authored` (`fromFactory`, the `<!-- keleustes:` marker in a body or comment, `keleustes[bot]`/`belai[bot]`, logins in setting `forge.identity` exec.rs:402); test `factory_authored_webhook_events_never_trigger` | Forge: set `fromFactory` on pulls/issues/comments the factory authored (requested); governance: add `forge.identity` to config.rs (requested) |
| P0-8 Lost sessions on node offline + 10 min grace | Background | P0 | done | `kls-node/src/lost.rs:12-13` (grace setting, default 10), `lost.rs:53-90` (sweep ends sessions, emits `session.lost`); `kls-node/src/server.rs:273` (started); `kls-core/src/sessions.rs:294` (`session_end` emits `session.ended`) | - (sweep every 60 s, not driven by NODE_OFFLINE event) |
| Key dialog stays open until save, inline errors | Vault | P1 | done | kls-ui/src/screens/vault.rs:486-503 (settle save), :551 disabled while saving, :738 inline error | - |
| Warn before replacing existing key | Vault | P1 | done | kls-ui/src/screens/vault.rs:527-534, :694-706 "Replaces the key added ..." chip | - |
| Edit label without replacing value | Vault | P1 | done | kls-core/src/vault.rs:249 `set_secret_label` (wraps `Vault::set_label`); kls-ui/src/screens/vault.rs:1262-1265 Edit label act, :793 save path | - |
| Vault own screen + route vault.* events | Vault | P1 | done | kls-ui/src/lib.rs:55,140 `Screen::Vault`; kls-ui/src/events.rs:889 `"vault" => S::Vault` | - |
| Per-entry integrity check + Run checks | Vault | P1 | done | kls-core/src/vault.rs:274 `verify_secret`, :401 `verify_all`; kls-store/src/vault.rs:273 `verified_at`; kls-ui/src/screens/vault.rs:149 Run checks, :1226-1241 Verified/Check failed chip | - (a chip, not a separate column) |
| Vault activity log with All/Changes/Denied | Vault | P1 | done | kls-core/src/vault.rs:433 `vault_activity`, :441 `activity_of`; kls-ui/src/screens/vault.rs:401-421 | - |
| GitHub quota rules editor + per-workflow call breakdown | Budgets | P1 | done | on_exhausted enforced kls-github/src/client.rs:356 `on_exhausted` (defer / use_cached / escalate, kill switch first, every outcome audited); per-workflow count public kls-github/src/budget.rs:172 `calls_this_hour` / `workflow_calls_this_hour`; shown in Budgets > Usage kls-ui/src/screens/budgets.rs:581 "GitHub calls · this hour" | - (WP1; the editor itself stays in Settings > GitHub quota) |
| Audit entry links to card/run/session/workflow/events | Audit | P1 | done | kls-ui/src/screens/audit.rs:1314 `entry_links`, :779-780 `model.go` | - |
| Audit filters card/agent/repo/workflow/run/vuln | Audit | P1 | done | kls-store/src/audit.rs:183-185 `facts`; kls-ui/src/screens/audit.rs:575-580 facts field, :217 | - (one free-text facts field, not per-type pickers) |
| Signed export with range picker and save dialog | Audit | P1 | done | kls-ui/src/screens/audit.rs:224-271 export dialog (path field, 30/90/400/All pills, `audit_export_to`), :277-301 | - (path text field; no native file picker in the repo) |
| Per-agent model map | Settings | P1 | done | engine dispatch: a step with no model takes `kls_core::vault::route_for` (rules, `models.agents`, profile, default) kls-engine/src/exec/policy.rs:64 `step_model`, used exec.rs:2817; crew members exec/policy.rs:82; test `a_step_with_no_model_takes_the_per_agent_model_map` | - |
| Crew defaults (guardrails, tests, show_internal_work, auto_commit, a2a.show, lease) | Settings | P1 | done | kls-engine/src/exec.rs:281-317 `crew_options` puts them on `DispatchSpec.options` for crew/worker/scan dispatches | - (WP1: dropped from PENDING). belai-side use is outside this repo. |
| System One section | Settings | P1 | done | decisions stored kls-store/src/engine.rs:1565 (`engine_decisions`, migration 0200_wp2_engine_decisions.sql) and added to the day stats (`record_decision`) exec/policy.rs:403; `systemone.retention` prunes :465; daily cap `systemone.daily_cap` (default 500) exec/policy.rs:471, exec.rs:3018 | `systemone.override` still unread; governance: add `systemone.daily_cap` to config.rs and prune PENDING (requested) |
| Default run-on node, fallback, maintenance window | Settings | P1 | done | kls-engine/src/exec.rs:2338-2356 (`Effective.run_on` / `run_on_fallback` as the step default); maintenance via kls-core/src/control.rs:302-320, enforced at kls-node/src/gateway.rs:381 | - (WP1: dropped from PENDING) |
| Schedule blackout windows | Settings | P1 | done | kls-engine/src/guard.rs:33 `window`, :42 `held`; cron fires exec.rs:743 and event triggers exec.rs:1477 inside a blackout create `held` runs; dispatch/job/decision held by a maintenance window (guard::held) | - |
| Project overrides per node/dir with ack | Settings | P1 | done | kls-core/src/audit/overrides.rs:173 `set_project_prefs`, :240 `settle_acks` (called sweeps.rs:208); kls-ui/src/screens/project_overrides.rs:42,190; kls-node/src/gateway.rs:395 | - |
| Assign sheet + Send to workflow | Board | P1 | done | `kls-ui/src/screens/board/bulk.rs:317` assign_menu (crews/workers), used `kls-ui/src/screens/board.rs:1197`; `kls-ui/src/screens/board/claims.rs:291-340` Send to workflow (start_run w/ card_id); `bulk.rs:230-290` one run per card | - |
| Multi-select bulk bar | Board | P1 | done | `kls-ui/src/screens/board.rs:216,343,694` selection + bulk_bar; `kls-ui/src/screens/board/bulk.rs:175` Move, `:192` Priority, `:249` Send, `:293` Delete | - |
| Review tab semantics + keys | Board | P1 | done | Confirm -> backlog, Dismiss -> dismiss_card; J/K move a row cursor, C confirms, X dismisses (`kls-ui/src/screens/board.rs:862`, keys `kls-ui/src/screens/board/tabs.rs:376` `review_key`, ignored while a text field has focus; test `review_keys_through_egui`) | - |
| Card GitHub tab | Board | P1 | done | drawer GitHub tab `kls-ui/src/screens/board/github.rs:72`: issue state, labels, comment count and mirrored comments (`Core::issue_comments` `kls-core/src/board.rs:610`), Sync now (`sync_issues`), Unlink (`Core::unlink_card_issue` `:705`, audited), Open issue mirrors the card to a new issue and links it (`github.rs:197`, `Core::link_card_issue` `:648`, audited); test `link_unlink_and_three_way_sync` | kls-github `issue_card` should honour the `unlinked:` marker (request WP4 #1 in requests-engine.md) |
| Drawer: note, Sessions/Run tabs, Events link, copy link | Board | P1 | done | drawer tabs Card/GitHub/Sessions/Run/Activity `kls-ui/src/screens/board/tabs.rs:27`, Sessions `:91`, Run `:127` (runs and sessions with the card id); Copy link `keleustes://board/<key>` and Events for this card (`card:<id>`) `kls-ui/src/screens/board.rs:1126,1133`; Add note `board/claims.rs` | - |
| Board lens + swimlanes | Board | P1 | done | `kls-ui/src/screens/board/bulk.rs:17-70` Lens (Needs me/Unrouted/P1/From a workflow), `:73-127` Group (repo/workflow/crew); `kls-ui/src/screens/board.rs:330,390,405-424` | - |
| Structured filters + Events::focus deep link | Events | P1 | done | `kls-ui/src/events.rs` key:value filters and `focus`; deep links from Board (drawer `card:<id>`, `kls-ui/src/screens/board.rs:1133`), Checkpoints (inspector Events, `events_filter` `kls-ui/src/screens/checkpoints.rs:66`), Cockpit (latest events `kind:<kind>` `cockpit.rs:667`, HUD panels `kind:decision.` / `needs`, Fleet/Health `kind:node.`), Audit | - |
| Time range + Load older | Events | P1 | done | `kls-ui/src/events.rs:129-145` Range, `:381-424` range tabs, `:700` Load older; uses `events_query` before_id/from_at (`:330-337`) | - |
| Event detail pane | Events | P1 | done | `kls-ui/src/events.rs:727-838` detail_window: facts grid, payload links, payload JSON + copy | - |
| 30-day retention prune | Events | P1 | done | `kls-core/src/sweeps.rs:40,104-119` retention_at (default 30 d, setting), daily tick `:179`; started `kls-app/src/main.rs:569`; setting `kls-core/src/audit/config.rs:422` | - (lives in sweeps.rs, not audit.rs) |
| Time range + KPIs | Cockpit | P1 | done | `kls-ui/src/screens/cockpit.rs:262-275` range pills, `:281` Tokens, `:292` Spend, `:300` Done, `:308` Draft PRs, `:318` Free slots, `:330` GitHub quota, `:344` Cache hits | - |
| Running-now lanes w/ node states | Cockpit | P1 | done | `kls-ui/src/screens/cockpit.rs:502-505,820-860` lane_row with per-step state glyphs; `kls-core/src/estimates.rs:714` RunLane | - |
| Missing checkpoint kinds | Checkpoints | P1 | done | `kls-core/src/needs.rs`: `PullDraft` (open drafts), `IssueExternal` (open issue, author not owner/member/collaborator or a bot, no team comment, `answered_issues` `:551`), `NodeOffline` (`offline_nodes` `:621`, gateway or last node event), `DeployGate` (gate steps that deploy, `is_deploy_step` `:238`, decidable; GitHub environment reviews from `github.deployment_review.*`, `deploy_reviews` `:574`); test `new_kinds_are_produced` `:888` | GitHub plane must emit `github.deployment_review.*` (request WP4 #2) |
| Repo/author filters, PR inspector facts + file list | Checkpoints | P1 | done | repo and author combos `kls-ui/src/screens/checkpoints.rs:418` (`Checkpoint.author` set for pulls, issues, deploy reviews); inspector PR facts author/checks/mergeable/review/branch/size/reviewers/fork/merge method (`pull_facts` `:83`, `pull_detail` `:852`) and the file list (`ForgeApi::pull_files`, `:831`) | - |
| Merge-conflict ledger + Retry + Resolve conflicts | Checkpoints | P1 | done | conflicting open pulls are listed (`Checkpoint.conflict`); inspector ledger with Retry and Open card (`ledger_ui` `kls-ui/src/screens/checkpoints.rs:253`, for the pull's open conflict card `Core::conflict_card_of` or a card row); Resolve conflicts files a `merge-conflict` card (`:911`, `Core::file_conflict_card` `kls-core/src/board/cards.rs:764`, fork held, deduped, audited; test `resolve_conflicts_files_one_card`); Merge uses the repo's `merge_method` | - |
| Merge uses repo merge_method | Checkpoints | P1 | done | `kls-ui/src/screens/checkpoints.rs:632-645` reads `settings.merge_method`, squash only as fallback | - |
| Worker control (pause/resume/start/backup/crew pause-resume all/log tail) | Workforce | P1 | done | WP5: core `worker_control` `kls-core/src/vault/workforce.rs:736` (pause/stop wind down; start/resume refused under the kill switch; crew resume also under the A2A blackout; audited with origin). Workers tab controls `kls-ui/src/screens/workforce/controls.rs:71` (pause/resume, back up :135, log tail :235, stop, crew Pause all/Resume all :200, start :279), wired `kls-ui/src/screens/workforce.rs:373`; Fleet uses it too `kls-ui/src/fleet.rs:671`. Tests: crew_resume_respects_the_a2a_blackout, kill_switch_refuses_start_and_resume_but_not_pause | - |
| Crew thread + A2A UI | Workforce/Session | P1 | done | Per-session tab as before; crew thread `CrewThread` `kls-ui/src/screens/harness_a2a.rs:189` (members from node status `crew_agents` :152; nothing read or sent under the blackout; sends via `send_owner_a2a`, refused and audited by core), opened per crew run in Workforce controls (`workforce/controls.rs` Thread chip). Test: crew_thread_is_its_members | - (uses the A2A message log, not `a2a_threads`) |
| Session controls strip, slash cmds, `!` shell | Session | P1 | done | Core `queue_composer` `kls-core/src/sessions.rs:550` (`/name args` command checked against `Accepts.commands`/controls, `!cmd` only when the session takes shell, kill switch first, audited), `parse_composer` :226, completions :244. UI composer with `/` completion pills and shell mode `kls-ui/src/screens/sessions.rs:1543-1590`. Tests: slash_and_shell_commands, slash_and_shell_refused_under_the_kill_switch | - |
| Answers with choices, plan review, pinned asks | Session | P1 | done | `ask_ui` option buttons + plan_review notes: `kls-ui/src/screens/sessions.rs:1040-1118`. Clarify/mode_choice form: `:1120-1234`. Ask pinned in bottom composer panel: `:813-823` | - |
| Session factory context (run step, budgets, PR checks, cards, todo) | Session | P1 | done | `Core::session_context` `kls-core/src/sessions/context.rs:116` (run step, applicable budgets, open PR checks on the branch, card, latest TodoWrite list); panel `context_ui` `kls-ui/src/screens/sessions.rs:2178` | - |
| Workforce needs-you strip + state filters | Workforce | P1 | done | `needs_strip`: `kls-ui/src/screens/workforce.rs:1420-1460`. State chips Working/Idle/Inactive/Not in library: `:396-426` | - |
| Crew validation (claim conflicts, unclaimed labels, replicas cap) | Workforce | P1 | done | Core `crew_problems` `kls-core/src/vault/workforce.rs:109` (saved profiles, then builtins), enforced in `save_crew` `kls-core/src/vault.rs:1963` as `Refusal::Invalid`; UI blocks on the same problems `kls-ui/src/screens/workforce.rs` crew drawer. Test: an_invalid_crew_is_refused_in_core | - (an agent Keleustes does not know is allowed; it may live on a node) |
| Per-field draft offers (Take/Drop/Undo/Take all) | Agent builder | P1 | done | `kls-ui/src/screens/workforce/offers.rs:105,114,131,137`, wired at `kls-ui/src/screens/workforce.rs:1006` | - (offers cover the 7 base fields, not spec sections) |
| Autosave + "Continue your draft" | Agent builder | P1 | done | `DRAFT_KEY`/`AUTOSAVE_MS` at `kls-ui/src/screens/workforce/offers.rs:9-11`. Autosave at `kls-ui/src/screens/workforce.rs:1210-1226`. "Continue draft" at `:258-261`. Cleared on save at `:1197` | - |
| Retry from chosen step + cancel confirm | Runs | P1 | done | "Retry from here" `ops::retry_run(.., Some(&sid))`: `kls-ui/src/screens/runs.rs:782-791`. Two-click cancel: `:461-479` | - |
| Run totals (budget %, est. cost, handoffs) | Runs | P1 | done | `RunDetail::totals` at `kls-engine/src/ops.rs:1324-1359`. UI: budget % `kls-ui/src/screens/runs.rs:561`, handoffs `:570`, model cost `:584` | - |
| Lockfile handling, keep resolutions across head_moved, extended description | Conflicts | P1 | done | kls-github/src/conflicts.rs (`LOCKFILES`, `regenerate_command`, `unchanged_files`, `describe`); kls-ui/src/screens/conflicts.rs (lockfile Keep ours/theirs, Head moved chip, multi-line message); tests kls-github/src/conflicts_tests.rs | - |
| Outcome detail screen (timeline, cost by model, people) | Delivery | P1 | done | `kls-core/src/github.rs:1601` (`outcome(core, key)`: timeline, cost by model, people, links), test `:2414`; Outcome tab `kls-ui/src/screens/delivery.rs:423`, `:2093` (`outcome_view`), opened from an Outcomes row (`:233`) | - |
| Per-trigger enable switches | Workflows | P1 | done | `kls-engine/src/ops.rs:465` (`set_schedule_enabled`); `kls-ui/src/screens/workflows.rs:857-861` (checkbox in Schedules table) | - (no switch in builder header itself) |
| Schedules view: create, re-time, delete, kind filter | Workflows | P1 | done | `kls-ui/src/screens/workflows.rs:768` (kind filter), `workflows.rs:811` (`add_cron_schedule`), `workflows.rs:896` (`retime_schedule`), `workflows.rs:956` (`delete_schedule`); `kls-engine/src/ops.rs:1204,1245,1299` | - |
| List columns: on/off, trigger summary, repo count, runs 7d | Workflows | P1 | partial | `kls-ui/src/screens/workflows.rs:444` (columns Workflow/State/Version/Last run/Updated), `workflows.rs:487-506` (on/off toggle) | Trigger summary, repo count and runs-in-7-days columns missing (`Row` at `workflows.rs:38` holds only w/last/blocked) |
| Output fields for draft PR, file issues, check run | Workflows | P1 | done | engine reads every field: `reviewers` kls-engine/src/forge.rs:459, `base` incl. pr_head :291, `prState` :397; `minSeverity`/`group`/`maxIssues`/`kinds`/`skipKinds`/`from` :766; check `name`/`minSeverity`/`when` :695; tests kls-engine/src/forge/tests.rs:620-1127 | - |
| Crew node: model picker, per-member models, run-on, offline fallback | Workflows | P1 | partial | engine: per-member models on crew dispatches (`DispatchSpec.member_models`) from the step's `memberModels` or `models.agents` kls-engine/src/exec/policy.rs:82, exec.rs:2831; test `dispatch_carries_budget_tier_and_member_models` | No `CrewMember.model` field (adding it breaks struct literals in kls-core/src/vault.rs tests, WP1/WP5 files); UI editor for `memberModels` (requested) |
| Per-workflow agent limits (graph.rs Limits) | Workflows | P1 | partial | kls-engine/src/graph.rs:88-104 (cost USD per card, finalize-at, attempts, continuations, passes, wall minutes, tool rounds), validated validate.rs `check_limits`, sent as `Dispatch.budget` exec/policy.rs:19 | UI fields in `workflow_settings_ui` (kls-ui, requested) |
| Publish panel with note and problem count | Workflows | P1 | done | `kls-ui/src/screens/workflows.rs:1106-1110` (200-char note), `workflows.rs:1116-1120` (problem count blocks), `workflows.rs:1126-1153` (confirm with warnings + triggers list), `workflows.rs:1165` (`ops::publish` with note) | - |
| Template ci-failure-fixer | Templates | P1 | done | `kls-engine/src/templates.json:840` (webhook check_suite.completed, belai:bug, draft PR) | - |
| Template dependabot-triage | Templates | P1 | done | `kls-engine/src/templates.json:931` (webhook trigger, comment verdict) | - |
| Template ui-polish-pass | Templates | P1 | done | `kls-engine/src/templates.json:1020` (manual trigger, approval, draft PR) | - |
| Template security-patch-train | Templates | P1 | done | `kls-engine/src/templates.json:1124` (cron trigger, gate, draft PR) | - |
| Template release-provenance | Templates | P1 | done | `kls-engine/src/templates.json:1228` (manual trigger, files ticket) | - (all templates validated by `kls-engine/src/templates.rs:135`) |
| Template search + run budget in Use form | Templates | P1 | done | `kls-ui/src/screens/templates.rs:114,382` (search with synonyms), `templates.rs:278-303` (run budget passed); `kls-engine/src/templates.rs:96-110` (`use_template` sets `run_budget_tokens`) | - |
| Issue mirroring per board project | Repos | P1 | partial | `github.issue_mirror` rows (project, repo, inbound list, outbound, merge rules, escalate hours) kls-core/src/board/sync.rs:378, `save_issue_mirrors` :435; poller kls-github/src/poller.rs:98,235; test kls-github/src/tests.rs:1418 | No "existing cards" choice; no editor screen for the rows |
| Repo detail: attached workflows + bulk attach | Repos | P1 | done | `kls-ui/src/screens/repos.rs:1045` (detail lists workflows whose scope names the repo, `attached_workflows` `:1133`); bulk Attach menu `:768` -> `attach_repos` `:1274` (scope, save_draft, publish, audited `workflow.attach` actor owner), test `:1958` | - |
| Default workflow catalogue, per-repo seeding, defaults_off | Repos | P1 | done | `kls-engine/src/defaults.rs:91` (core + 34 CLI defaults), `defaults.rs:422-440` (`gate` honours defaults_off/autonomy), `defaults.rs:447-545` (`seed` per managed repo, upgrade unless customised); `kls-engine/src/exec.rs:885-891` (re-seed on repo/settings events); UI `kls-ui/src/screens/repos.rs:709-790`, list chips `workflows.rs:475-484` | - |
| OS notices: notify nodes, failed runs, finished sessions | Background | P1 | done | `kls-app/src/desktop.rs:364-381` (`run.notify`, failed `run.finished`, `session.ended`), `desktop.rs:491-520` (`start_notifications`), `desktop.rs:440-454` (notify.events filter) | - |
| Repo-aware node choice + no_host_for_repo admission | Background | P1 | done | kls-engine/src/route.rs:137 `NO_HOST_FOR_REPO`, :205 (every offering node lists dirs, none holds the repo, none can clone); exec.rs:2786 refuses (audited) and fails the step (not retried, walker.rs); tests route.rs `node_choice_prefers_offer_and_repo`, `no_host_for_repo_is_refused` | - |
| Card claim TTL and capacity wait | Background | P1 | done | `deferred:capacity` on a full node; an unclaimed routing card waits (`waiting:unclaimed`, `route.unclaimed` audited) each claim TTL until the step timeout, then `card_unclaimed` kls-engine/src/exec.rs:3600-3648 | - |
| Crew handoff following (agent.parent) | Background | P1 | done | `kls-engine/src/exec.rs:1429-1434` (handoffWait), `exec.rs:1461-1484` (`open_children` via claim parent), `exec.rs:1538-1554` | - |
| Outbound card-to-issue sync with retry and field conflicts | Background | P1 | done | three-way `plan` kls-core/src/board/sync.rs:283; `sync_cards` / `push` / `retry_backoff` kls-github/src/sync.rs:1041,907,792 (kill switch first, retry 1 min..1 h, close on done, ISSUE_PUSHED / ISSUE_PUSH_FAILED, audited); tests kls-github/src/tests.rs:1208,1325,1363 | - |
| Gates woken by PR/check updates | Background | P1 | done | kls-engine/src/exec.rs:998 `wake_gates` (`github.pull.*` / `github.workflow_run` for the run's repo and pull wake `deferred:checks` gates now); test `forge_gate_wakes_on_a_pull_update` | - |
| Mirror more GitHub event kinds | Background | P1 | done | REVIEW_SUBMITTED kls-github/src/sync.rs:341, PACKAGE_PUBLISHED events.rs:306, RISK events.rs:468,554; webhook names kls-engine/src/forge.rs:1166-1172; tests kls-github/src/tests.rs:1478,1522 | - |
| Label controls: unlabel cancels, sf:stop, /sf, label trust | Background | P1 | done | `kls-engine/src/exec.rs:1203-1252` (`label_controls`: removed label cancels queued, `sf:stop`), `kls-engine/src/triggers.rs:263,276` (`/sf <name>`), `triggers.rs:298` + `exec.rs:1092-1113` (trust/bot checks) | - |
| Expiry presets/custom/never, expiring/expired | Vault | P2 | done | custom expiry kls-core/src/vault/lineage.rs:121 `parse_expiry` (date, date+time, Nd/Nh, never) and `set_expiry`; UI kls-ui/src/screens/vault.rs:1035 Custom field beside the presets; tests `expiry_text`, `custom_expiry_is_kept_and_never_leaks` (lineage.rs:657) | - (WP1) |
| Per-node grant scoping | Vault | P2 | done | kls-core/src/vault.rs:323 `set_secret_scope`, :565 `grant_for_node`; kls-node/src/gateway.rs:188,260,643; kls-ui/src/screens/vault.rs:673-690, :922 | - |
| Generic env-var secrets + reserved deny-list | Vault | P2 | done | kls-core/src/vault.rs:296-321 `put_env_secret`; kls-vault/src/lib.rs:289 `reserved_env`; kls-ui/src/screens/vault.rs:565-575 Env var pill | - |
| Apply model after add, discover models from key | Vault | P2 | done | kls-core/src/vault/probe.rs:89 `models_from` reads the models list an accepted test returns, stored on the key (`models_json`, migration 0211_wp1_vault_lineage.sql); :348 `apply_discovered_models` adds them to the provider (audited `vault.models_applied`); UI chip + "Models" button kls-ui/src/screens/vault.rs:1494; test probe.rs:494 (no plaintext in events, ledger, settings, logs or the data dir) | - (WP1) |
| Rotation grace period, version lineage with fingerprints | Vault | P2 | done | kls-vault/src/lib.rs:578 `rotate_with` (old value sealed until grace end, lineage row per version with fingerprint/origin/dates), `rollback`, `sweep_grace`; store `vault_secret_versions` (kls-store/src/vault.rs, migration 0211); core kls-core/src/vault/lineage.rs:49 `rotate_with_grace` (setting `vault.rotation_grace_hours`), `rollback`; UI lineage window kls-ui/src/screens/vault.rs:1120 with Roll back; tests lib.rs:1056, lineage.rs:568 | - (WP1) |
| Reveal toggle, -prod label auto-fill, logos | Vault | P2 | done | logos as Phosphor glyphs kls-ui/src/screens/vault.rs:107 `provider_logo` (provider sections and key rows) | - (WP1) |
| Auto-rotation, Kiro sign-in, connector secrets, IaC export | Vault | P2 | done | auto-rotation kls-core/src/vault/lineage.rs:187 `Minter` + `OpenRouterMinter` (provisioning API), :379 `tick` (due keys minted and rotated in with grace; retired minted keys revoked at the provider after grace; kill switch `provision` checked, failures audited `vault.rotate_failed`); management keys filed under `<provider>-management` and never granted (kls-vault `is_management_provider`); run every 10 min by `audit::governance_tick` (kls-core/src/audit.rs:919); UI Auto-rotate field in Access; test lineage.rs:708; Kiro sign-in and connector secrets (C, 2026-11-22): see the Vault rows "Kiro JSON sign-in" and "Connector secrets tab" | Providers other than OpenRouter cannot mint keys from a management key, so they rotate by hand |
| Budget nesting view | Budgets | P2 | done | kls-ui/src/screens/budgets.rs:1138 `nesting`, called :226 | - |
| Workflow, node and org scopes | Budgets | P2 | done | `org` scope (the whole factory, every provider) kls-core/src/vault.rs:115 BUDGET_SCOPES, :969 `budget_applies_in`, `usage_filter`; UI form + nesting tier kls-ui/src/screens/budgets.rs; test vault.rs:2826 `org_budget_covers_the_factory_and_tool_call_bars` | - (WP1) |
| Multiple alert thresholds | Budgets | P2 | done | kls-ui/src/screens/budgets.rs:782, :809; kls-core/src/vault.rs:1272-1279 (own + `budget.alerts`) | - |
| Tool-call bars | Budgets | P2 | done | kls-core/src/vault.rs:1584 `tool_calls_by_agent` (session facts' `tool_calls` per agent) into `BudgetsView.tool_calls`; bars kls-ui/src/screens/budgets.rs:556 | - (WP1) |
| Alert crossings, downgrade to fast model | Budgets | P2 | done | kls-store/src/vault.rs:754 `budget_alert_once`; kls-core/src/vault.rs:1280-1293 `budget.alert`, :1297-1325 Downgrade; kls-engine/src/exec.rs:2586-2592 | - (crossings are events, no list on the Budgets screen) |
| Matrix totals, evidence index export, egress host list | Trust | P2 | done | kls-core/src/audit/trust.rs:439, :489, :524; kls-ui/src/screens/trust.rs:73-80, :157-183, :284 | - |
| Firewall/egress review strip, sightings -> review cards | Trust | P2 | done | kls-core/src/audit/review.rs:74 `egress_review` (pending until reviewed, back when refused again), :133 `file_card` (card with the sighting as origin, label `egress-review`), `dismiss`; each audited (`trust` ledger kind, `trust.egress_reviewed`); strip kls-ui/src/screens/trust.rs:162 with Card / Dismiss / card link | - (WP1) |
| Kanban binding, forge quota extras, setting-change link, chat notifications, quiet hours | Settings | P2 | done | kanban binding (agent A, 2026-11-22): settings kls-core/src/audit/config.rs:309 read by `Binding::from_settings` (kls-core/src/board/kanban.rs), dropped from `PENDING` (config.rs:527); three-way merge per field against a stored base `plan` kanban.rs:469, pass `sync_with` :607 (kill switch `control::check("job")` first, refusal audited; import of new outside items into triage with cleaned text `clean_body` :375, label `kanban:<provider>`, origin URL + SHA-256 in card meta `import` :888; conflicts held, never overwritten, `resolve_conflict` :1035), tick `start` :1159 every 5 min started from `kls_github::start` (kls-github/src/service.rs:84 via kls-github/src/kanban.rs:31); providers: GitHub Projects + GitLab boards over the forge CLI (kls-github/src/kanban.rs:218, :436; writes kill-switch-checked, write-capped and audited by `Client::request`), Linear and Jira Cloud over HTTPS with the token sealed in the vault (kls-core/src/board/kanban/linear.rs:117, jira.rs:180, `set_token` kanban.rs:319); links + last pass in migration 0243_a_kanban_links.sql (kls-store/src/board/kanban.rs); UI status chip, Sync now, token Seal/forget and held-field Keep board / Take outside kls-ui/src/screens/settings.rs:1184 `kanban_ui`; tests kls-core/src/board/kanban/tests.rs (11, fake remote and fake HTTP), kls-github/src/kanban.rs tests (3, fake gh/glab), kls-store/src/board/kanban.rs (1); chat channel `notify.chat`/`notify.chat_events` + sealed webhook kls-core/src/audit/notify.rs (`set_webhook` seals the address in the vault, `post` :191 checks the kill switch first, honours quiet hours, audits every outcome; started from `audit::start`), UI kls-ui/src/screens/settings.rs:1090 `chat_ui` (Seal / Send test / forget); quota extras enforced (on_exhausted, proactive write cap kls-github/src/client.rs:318); quiet hours as before | Kanban: comments/notes are not mirrored, labels and priority are not synced (title, body, column only); GitHub Projects reads go through `POST graphql`, so each read counts toward the proactive write cap |
| Day grouping, paging past 5000, multi-delete, fact chips, signed decision ledger, weekly report | Audit | P2 | done | decision ledger kls-core/src/audit/report.rs:54 `export_decisions` (approval, decision, factory_control, override entries in the signed export format; `verify_export` checks it); weekly report :97 `weekly_report`, `render_markdown`, :228 `weekly_tick` (last week's report written once to `<data>/reports/`, audited `audit.report`); UI tabs kls-ui/src/screens/audit.rs:390 Decisions, :457 Weekly | - (WP1) |
| Quota refusal actions and deferrable reads | Background | P2 | done | kls-github/src/client.rs:356 `on_exhausted`: defer (refuse until the reset), use_cached (answer from the ETag cache, else defer), escalate (refuse and notify once per reset window, `github.quota.escalated` counts as an escalation for notifications); each refused under the kill switch first and audited; proactive write cap :318 `check_write_cap`; tests client.rs:937, :957 | - (WP1) |
| Lifecycle ledger | Background | P2 | partial | Stored: `kls-core/src/github.rs:2053` (`lifecycle_sweep`: card/pr/issue episodes into the hash-chained ledger, kind `lifecycle`, deduped by ref, `lifecycle.entered/exited` events within the hour, kill-switch gated), `:2097` (`ledger_flow` read); tests `:2488`, `:2528`; UI `kls-ui/src/screens/delivery.rs:2336` (ledger table, Record button) | No five-minute timer yet (requested in requests-governance.md, WP3: `sweeps.rs`); cockpit Flow panel still reads the engine's computed flow. |
| Approval re-notify | Background | P2 | done | kls-engine/src/exec.rs:3272-3291 (`approval.renotified` on SLA); kls-app/src/desktop.rs:517-519 re-shows notice | - |
| Header GitHub stamp | Board | P2 | done | `kls-ui/src/screens/board.rs:313-329` "poll mirror · N repos · synced X" | - |
| Right rail | Board | P2 | done | `kls-ui/src/screens/board.rs:358,1455-1505` Held / Claims lapsed / In review | - |
| Backlog priority, Unrouted/Stale, Done by day + Reopen | Board | P2 | done | backlog inline priority (DragValue, `kls-ui/src/screens/board.rs:929`) plus up/down; Unrouted and Stale lenses (`board/bulk.rs:42`, `is_stale` 14 days `board/tabs.rs:21`); Done tab grouped by local day with Reopen (`done_by_day` `board/tabs.rs:227`); header GitHub stamp `board.rs:509` | - |
| Sync-conflicts three-way tab | Board | P2 | done | Base / Board / GitHub per field with a changed-side mark (`kls-ui/src/screens/board/tabs.rs:292`); base from `keleustes.syncBase` (`Core::sync_conflicts_3way` `kls-core/src/board.rs:737`), written on each resolution (`resolve_sync_field` `:753`) | base unknown ("—") until a first resolution; request WP4 #3 asks the mirror to write it |
| HUD panels | Cockpit | P2 | done | `kls-ui/src/screens/cockpit.rs:1238` `hud_panels`: System One (`decision.made`, `system_one` `:942`), token economy (tokens, spend, per done card, by hand), gates and provenance (gates waiting, refusals, A2A withheld, audit entries 24 h), runs-by-hour heatmap (`runs_heatmap` `:965`); test `hud_rules` | - |
| Fleet/Health | Cockpit | P2 | partial | `kls-ui/src/screens/cockpit.rs:1349` `health`: workers table from node status reports with late heartbeats (>5 min, `worker_rows` `:994`), belai version skew (`version_skew` `:1024`), friction (failed runs, refusals, blocked cards, late beats), flow queues per column with the oldest wait (`flow_queues` `:1048`) | Run graph and repo eligibility |
| Needs-you counts by kind + stream status | Cockpit | P2 | done | counts by kind, every kind, each opening Checkpoints filtered (`kind:<key>`, `kls-ui/src/screens/cockpit.rs:477`, `Checkpoints::focus`); stream status (live n/min or stream error, opens Events) `cockpit.rs:1167`, in the header `:182` | - |
| Saved views, JSONL export, origin chips, fact filter | Events | P2 | done | saved views, JSONL export; origin chips row and Origin column (`origin_of` `kls-ui/src/events.rs:183`, filter `origin:`), fact filter `key=value` from the Facts column and the detail window (`fact_matches` `:209`); test `origin_chips_and_fact_filters` | - |
| Paging and source stamp | Checkpoints | P2 | done | `kls-ui/src/screens/checkpoints.rs:33,529` Show more (+50); `:339-347` "live · read X ago" stamp + reload | - |
| Fact index (model, provider, mode, effort, version timeline, touched/contacted) | Workforce/Session | P2 | partial | Session header provider/model/mode/agent plus effort and outcome chips (WP5): `kls-ui/src/screens/sessions.rs:840-841`. Run facts: `kls-ui/src/screens/runs.rs` | Version timeline, touched/contacted index |
| Runs list totals + Repo/Model columns | Runs | P2 | done | Repo and Model columns `kls-ui/src/screens/runs.rs:408` (models from the run's sessions), list totals line `:1016`, `run_axes` in the trace `:199` | - |
| Session source tabs + grouping by crew | Session | P2 | done | Source tabs Console/Runs/Workers/Host `kls-ui/src/screens/sessions.rs:92`, By crew grouping `:594`, `group_by_crew` :2279 (crew from node status) | - |
| Decision-step rendering | Runs | P2 | done | `decision_card`: `kls-ui/src/screens/runs.rs:850-854,906` | - |
| Evidence pack download | Runs | P2 | done | `ops::write_evidence` button: `kls-ui/src/screens/runs.rs:573-581` | - |
| Builder extras (export/import md, files, palette, tool groups) | Agent builder | P2 | done | WP5: Markdown copy/import `kls-ui/src/screens/workforce/markdown.rs` wired `kls-ui/src/screens/workforce.rs:1284,1352`; palette editor :1145; tool groups :1134; files (B, 2026-11-22): `kls-ui/src/screens/workforce/files.rs` (Agent files: attach from a local path under a listed path, remove, caps shown) wired `workforce.rs:1174`; `kls-core/src/attachments.rs:555` (`add_agent_file`: UTF-8 text ≤ 256 KiB, ≤ 32 files, ≤ 2 MiB, path cannot climb out, no private key/token, blob content-addressed, audited `agent` either way), :658, :682; store `kls-store/src/attach.rs` + migration `0252_b_attach_agent_files.sql`; `kls-core/src/vault.rs:2315` (publish lists them in the agent document and saves them as the library version's files), `kls-core/src/belai/library.rs:213` (`Library.Fetch` ships them with the profile; kill switch as for every fetch); belai `internal/rc/factorylibrary.go:513` (`profileFiles`: hash, bounds, path, secret checked before saving), :165 (`agentfiles.Install` into the profile's own dir, audited `profile_files`; paths become knowledge paths); tests kls-core `attachments::tests::agent_files_checked_stored_audited_and_published`, belai `TestKeleustesProfileFiles` | Files are attached from a local path (no file dialog in the desktop app); workspace.sync sharing of a file is not set from the builder |
| Git strip actions (branch switch, sync toggle, teleport) | Session | P2 | done | Core `session_git` `kls-core/src/sessions.rs:629` (branch/sync as listed session commands, teleport via `teleport_request`; kill switch, audited); strip `git_strip_ui` `kls-ui/src/screens/sessions.rs:1452`. Test: git_strip_actions_are_listed_checked_and_audited | - |
| Wall mode + agent theming | Workforce | P2 | done | `wall_ui`: `kls-ui/src/screens/workforce.rs:333,1369`. Palette tint: `:1535-1545` | - (tint is local to workforce.rs) |
| Estimate model + team-equivalent | Delivery | P2 | done | `EstimateSettings` rates/roles/multiplier/baselines: `kls-core/src/estimates.rs:113-130`. Settings section: `kls-core/src/audit/config.rs:428-445`. Estimate tab: `kls-ui/src/screens/delivery.rs:1597`. People/month: `kls-core/src/estimates.rs:413`, `kls-ui/src/screens/budgets.rs:1213` | - |
| Today and MTD ranges | Delivery | P2 | done | `kls-core/src/github.rs:1466,1475` (`Range::today`, `month_to_date`), test `:2359`; chips `kls-ui/src/screens/delivery.rs:325`, `:2076` (`RANGES`) | - |
| Contributor part filter | Delivery | P2 | done | part chips filter Latest work `kls-ui/src/screens/delivery.rs:1336` | - |
| Memory paging, grouping, source host, A2A link | Memory | P2 | done | Paging and scope grouping as before; source host (node name) and A2A ref link in the note card `kls-ui/src/screens/memory.rs:304-330`, `host_name` :101 | - (the A2A link copies the reference) |
| J/K file navigation | Conflicts | P2 | done | kls-ui/src/screens/conflicts.rs (J next / K previous file, only without text focus) | - |
| System One decision node | Workflows | P2 | done | budget gate and daily cap before the job (exec.rs:3018-3050), each decision stored with its rationale and in the day stats (exec/policy.rs:403), retention pruning; test `system_one_decision_row_is_written_and_capped` | 30-day stats UI reads `decision_days` (no change needed) |
| Scan and GitHub-read nodes | Workflows | P2 | partial | scan: `kls-engine/src/graph.rs:478-484,628`, `kls-engine/src/exec.rs:2095` | No GitHub-read node kind (NODE_TYPES `graph.rs:627-642`) |
| Also start on / add-approval-after / quick-fix unrouted | Workflows | P2 | partial | `kls-ui/src/screens/workflows.rs:2354-2368`, `workflows.rs:3121` (approval after) | "Also start on" extra triggers and quick-fix for unrouted outcomes missing (issues panel `workflows.rs:1349-1400` only selects node) |
| YAML/JSON view and download | Workflows | P2 | partial | `kls-ui/src/screens/workflows.rs:1079-1086` (copy JSON to clipboard) | No YAML, no view window, no save/download |
| Dry-run repo picker and call ranges | Workflows | P2 | done | `kls-ui/src/screens/workflows.rs:1087-1090` (repo field), `workflows.rs:1468-1484` (calls totals + per step); `kls-engine/src/dryrun.rs:37,58,63` | - (repo is free text, not a scope picker) |
| Header token estimate | Workflows | P2 | done | `kls-ui/src/screens/workflows.rs:1070-1078` (`dryrun::estimate`) | - |
| Start a workflow from an event | Workflows | P2 | done | `kls-engine/src/ops.rs:1368` (`create_from_event`); `kls-ui/src/events.rs:766` | - |
| Ranked template suggestions | Workflows | P2 | done | `kls-engine/src/delivery.rs:92-122` (`rank_templates` by unanswered events); `kls-ui/src/screens/templates.rs:44-70` | - |
| Blocked state when no model available | Workflows | P2 | partial | `kls-ui/src/screens/workflows.rs:3092-3107` (`model_block`), `workflows.rs:466-473` (blocked chip) | UI-only; no engine preflight refusing/blocking runs when no provider/model |
| Repos table: signal chips, sort, org filter, findings/SLSA, stop-watching bulk, protected branches | Repos | P2 | done | signal chips `kls-ui/src/screens/repos.rs:601`, `:1206`; sortable headers (name, PRs, synced, findings) `sort_rows` `:1182`; org filter (existing); Findings column from `github.alert`/`github.risk` events `:1159`; SLSA from the scout profile `:1244`; Stop watching bulk `:754`; protected-branch list `:1257` | SLSA is read from profile signals (sigstore / release-artifacts), not a measured level. |
| Cron fan-out per repo with stagger | Background | P2 | done | `kls-engine/src/exec.rs:780-827` (scope fan-out, per-repo dedupe, `staggerMinutes`), `exec.rs:364` (`stagger_offset`), `exec.rs:1815` | - |
| `held` run status during blackout | Background | P2 | done | run status `held` (kls-store/src/engine.rs:247 ACTIVE_RUN_STATUSES); `StartRun.held` kls-engine/src/ops.rs:559, `run.held` event + audit; `release_held` ops.rs:773 queues them when the window ends (not under the kill switch), called each tick exec.rs:727; stored, so it survives a restart; test `cron_fire_in_a_blackout_is_held_survives_restart_and_is_audited` | UI: a Held filter in Runs (requested) |
| Fan-out priority and cap | Background | P2 | done | `kls-engine/src/exec.rs:1015-1016` (sort by priority), `exec.rs:1139-1146` (`factory.fanoutCap`, default 6); `kls-engine/src/triggers.rs:484`; UI priority `kls-ui/src/screens/workflows.rs:1802` | - (cap not exposed in Settings UI) |
| `conflict_cards` output | Background | P2 | done | `conflict_cards` in GITHUB_ACTIONS kls-engine/src/graph.rs:681, run by exec/policy.rs:289 (rechecks 0/1/3/10 min, up to 20 open pulls) | - |
| Periodic full sweep (15 min) | Background | P2 | done | `kls-engine/src/exec.rs:518` (`FULL_SWEEP_MS`), `exec.rs:564-586` (run_loop), `exec.rs:639` (`sweep`) | - |
