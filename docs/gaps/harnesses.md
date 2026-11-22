# Gap: third-party harness sessions and cross-node messaging

Status: design, 2026-11-21. Nothing here is built. Protocol changes need the
coordinator (`docs/WORKSTREAMS.md`, "Shared contracts"), and belai security
invariant changes need sign-off on `belai/AGENTS.md`.

The two asks, verbatim:

1. "send messages to belai nodes from sessions started in a keleustes shell"
2. "detect other harnesses on the machine (support all 41 we know about in the
   rest of our products) and offer them as detected agents to choose when
   starting a sesion shell (carefully integrate with them so that their own
   native jsonl files are used, and the prompts are input into their
   interactive tui/gui rather than non-interactive cli use)"

Contents

- [0. Findings in one screen](#0-findings-in-one-screen)
- [A. Sessions, shells and messaging today](#a-sessions-shells-and-messaging-today)
- [B. The harness list](#b-the-harness-list)
- [C. Design](#c-design)
- [D. Work packages, tests, risks](#d-work-packages-tests-risks)

---

## 0. Findings in one screen

- **There is no list of 41.** "41" comes from a marketing line,
  `vdb-site/website/data/faq.yaml:14` ("works with 41+ agents that support
  SKILLS.md"), added by vdb-site commit `7c5c18f` ("chore: bump agents to
  41+", 2026-04-08), which changed no list. The website has since dropped the
  claim on purpose (`website/src/shared/coding-agents.ts:7`,
  `website/src/shared/agent-surface.ts:74`). The only other "41" is test fixture
  data, `website/src/components/belai/BelaiHostScan.test.ts:203`,
  `BelaiScanReport.test.ts:33` and `belai-host-scan.test.ts:468` ("Checked 41
  harnesses"). The real value of `libscan.Report.Checked` is
  `len(harness.All())+1` = 85 (`belai/internal/libscan/scan.go:195`).
- **The canonical list has 84 harnesses:** `belai/internal/harness/harnesses.json`
  (generated, 84 `"id"` records, `adal` at line 3 to `zenflow` at line 2269).
  The table that names them all is `belai/docs/harnesses.md:164-255`. It is built
  by `belai/tools/gen-harnesses` from `cli/internal/agent/hosts.go` (10 hosts),
  `cli/cmd/skills.go:423` (`agentDirs`, 71 ids) and
  `cli/internal/aibom/catalog/tools.json` (38 `cli-agent`, 9 `ide`, 21
  `ide-extension`), plus `belai/tools/gen-harnesses/overlay.json`. 70 of the 84
  have a `detect` path.
- **The registry knows where skills live, not how to run a harness.** It has no
  binary names, launch argv, transcript paths or resume flags. Transcript
  knowledge exists for 9 harnesses only, in `belai/internal/agentstore/registry.go:64-119`
  (belai, claude-code, codex, pi, goose, opencode, copilot-vscode, copilot-cli,
  antigravity), with parsers in `agentstore/jsonl.go`, `sqlite.go` and `json.go`.
- **`detect` paths give false positives.** On this host `~/.hermes`,
  `~/.openclaw`, `~/.vibe`, `~/.kiro`, `~/.junie`, `~/.augment`, `~/.forge`,
  `~/.iflow` and `~/.config/crush` all exist but hold only `skills/`, which our own
  skills installer created. None of those binaries is on PATH. Launch
  detection has to probe the binary.
- **Nothing in Keleustes or belai rc has a PTY.** `creack/pty` is only in
  `belai/tools/shot/main.go:95`, a screenshot tool. Keleustes has no terminal
  widget. "Session shell" today means the Sessions screen's **New session**
  dialog (`crates/kls-ui/src/screens/sessions.rs:971-1200`) plus its composer.
  That dialog dispatches a belai `rc-session` and runs no shell.
- **A2A across nodes already works for belai sessions.** Keleustes stores and
  relays it (`kls-core/src/board.rs:670-728`, `kls-node/src/board.rs:240-320`).
  belai rc sessions send and receive it through `a2alink` and
  `factorylink.A2ARelay`, with `KindPeer` on receipt. What is missing: a
  Keleustes view or composer for A2A, a peer directory, and any A2A for a
  third-party harness.

The rest of this document designs the following:

- **Detection:** every node advertises the harnesses it can actually launch.
- **Start:** Keleustes offers them in **New session**.
- **Run:** belai rc runs the chosen harness's own TUI in a PTY, types prompts
  into it with bracketed paste, and mirrors the harness's own native
  transcript.
- **Messaging:** the harness gets A2A through a belai MCP bridge, with
  `KindPeer` kept intact.

---

## A. Sessions, shells and messaging today

### A1. How Keleustes starts a session

```
Keleustes UI / CLI                kls-node Hub              belai rc daemon              belai rc-session (child)
──────────────────                ────────────              ───────────────              ───────────────────────
New session ▶ Start  ──DispatchSpec──▶ outbox, ServerFrame.Dispatch (seq) ──Link──▶ handleFactory
 (sessions.rs:103 spec)           (gateway.rs:148)                                 factoryStart (rc/factory.go:193)
`keleustes sessions dispatch`                                                       startGrant (rc/daemon.go:952)
 (kls-app/src/cli.rs:117)                                                           childArgs  (rc/daemon.go:1138) ─▶ agent.Session
                                  ◀────────── Jobs.Complete{session_id} ─────────────┘                    │
                                  ◀── Sessions.Upsert/Append (mirror) ─────────────────────────────────────┤
                                  ── Sessions.Inbox (Prompt/Answer/Command) ──────────────────────────────▶│
                                  ◀▶ Board.A2ASend / A2AReceive ──────────────────────────────────────────▶│ (a2alink)
```

- `Dispatch.kind` is `session | worker | crew` (`node.proto`, `message
  Dispatch`). Its `agent` field names a **belai agent profile**, not a
  third-party harness.
- `SessionUpsert.kind` is `tui | worker | rc | acp | headless`.
- The inbox is all a session takes in. `Prompt` passes `CleanPrompt` and the
  classifier and waits for idle. `Answer` resolves an open ask. `Command` can
  be `stop`, `interrupt`, a control, `key <key>`, `checkout`, `shell <line>`
  (only with `--web-shell`) or `command` (`belai/docs/keleustes.md`, "Sessions";
  `factorylink/sessions.go:476` `ToCommand`, `case "key"` at `:488`).
- `Capabilities.terminal` (`node.proto`, `message Capabilities`) only reports
  whether the belai process has a terminal (`factorylink/host.go:52`). Nothing
  attaches to it remotely.

### A2. Shell or PTY

- belai: `rcshell.go` (`cmd/belai/rcshell.go`) and `--web-shell` run **one
  shell line** as a checked command, not an interactive PTY. No harness runs
  inside a PTY.
- Keleustes: no terminal emulator. The transcript is the only view
  (`sessions.rs:792` `transcript_ui`). The `"shell"` role is coloured at
  `sessions.rs:1301`.

### A3. Messaging that exists

| Piece | Where | Behaviour |
|---|---|---|
| Keleustes relay | `kls-core/src/board.rs:670` `send_a2a`, `:718` `a2a_inbox`, `:218` `a2a_for` | Stores envelopes and is idempotent on id. Refused during a blackout. Ids are `<nodeId>:<sessionId>`. `to` can be empty (broadcast), `<node>`, `<node>:*` or an exact id. |
| Node RPCs | `kls-node/src/board.rs:240` `a2a_send` (checks that `from` is the caller's node, `own_agent` at `:107`), `:275` `a2a_receive` (replays, then goes live) | mTLS node certificate |
| belai client | `factorylink/a2a.go` (`A2ARelay`), `internal/a2a` (relay, bus, mailbox), `internal/a2alink` (rc, headless and ACP sessions) | `a2a.enabled` gates it. Text from a peer reaches the model only as a classified `KindPeer` attachment (`belai/AGENTS.md:47`, `:162`). |
| Model tools | `belai/internal/tools/a2a.go:21-22` `A2AMessage`, `A2AAck` | Only to an agent whose broadcast reached the session. There is no directory. |
| Keleustes UI | none | `send_a2a` and `a2a_thread` have no caller in `kls-ui`. The owner cannot read or send A2A. |

So for ask 1:

- A belai session that Keleustes dispatched can already message belai sessions
  on other nodes, through Keleustes.
- Three gaps remain. (a) The session can only reply to peers whose broadcast it
  saw. (b) The owner cannot watch or join the conversation. (c) A third-party
  harness session has no A2A at all.

### A4. Design for ask 1

**A4.1 Peer directory (proto, additive).** The relay knows which sessions
are live, so the directory can come from the server:

```proto
service Board {
  // Live sessions any node may message (excludes ended), newest first.
  rpc A2APeers(A2APeersRequest) returns (A2APeerList);
}
message A2APeersRequest { string repo = 1; uint32 limit = 2; }
message A2APeer {
  string agent_id = 1;      // <nodeId>:<sessionId>
  string node_name = 2;
  string title = 3;         // session title, sanitized server-side, ≤120
  string repo = 4;
  string branch = 5;
  string harness = 6;       // "belai", "claude-code", ...
  string agent = 7;         // belai profile, if any
  int64 updated_at_ms = 8;
}
message A2APeerList { repeated A2APeer peers = 1; }
```

On the belai side:

- `A2AMessage`'s `to` accepts any id in `A2APeers`, not only a broadcaster.
- A new read-only tool `A2APeers` (kind `KindPeer`, so it is classified)
  returns the list. Titles are other models' text, so the list counts as
  remote content.

**A4.2 Owner as a participant.** Keleustes gets the reserved agent id
`keleustes:owner`.

- `Core::send_a2a` takes it only from the UI or CLI path, never from a node.
  `own_agent` already refuses it there.
- belai treats envelopes from `keleustes:owner` as `KindPeer` too. The policy
  is no exemption: an owner prompt goes through the inbox; A2A stays peer
  content.
- UI: an **A2A** tab in session detail (thread view plus composer) and an
  **A2A** page listing threads (`a2a_threads`).

**A4.3 Third-party harness sessions** get A2A through the MCP bridge in
[C5](#c5-a2a-for-a-harness-session-mcp-bridge).

**A4.4 Keep:** blackout in both directions, the 4 KiB text cap
(`tools/a2a.go` `MaxA2AText`), audit of each envelope, and `KindPeer`
classification on receipt on every belai surface.

---

## B. The harness list

### B1. Sources searched

| Source | Count | Holds |
|---|---|---|
| `belai/internal/harness/harnesses.json` (generated; doc `belai/docs/harnesses.md:164-255`) | **84** | id, name, aliases, home-relative `detect`, skill, command, agent, prompt, doc and hook dirs, import format |
| `cli/internal/aibom/catalog/tools.json` | 38 cli-agent + 9 ide + 21 ide-extension (plus 16 cloud-agent, 73 service, 2 convention) | env var names, project paths, commit patterns, model config extractors (no binaries) |
| `cli/cmd/skills.go:423` `agentDirs` | 71 | id → user skills dir |
| `cli/internal/agent/hosts.go` | 10 | verified install hosts |
| `ai-firewall/internal/client/client.go:28-39` | 12 client names | HTTP client fingerprints (claude-code, codex-cli, cursor, kiro, copilot, continue, aider, ...) |
| `belai/internal/agentstore/registry.go:64-119` | 9 + generic | native session, prompt and memory globs plus dialects (the only transcript knowledge) |
| `belai/internal/agentscan/scan_*.go` | 7 (claude, codex, copilot, goose, kiro, opencode, pi) + other | credential discovery in other agents' config |
| `website/src/shared/coding-agents.ts` | 11 pages | per-agent marketing pages |
| `vdb-site/website/data/faq.yaml:14` | "41+" | claim only, no list |

**Decision.** Use the 84-entry belai registry as the one list. Do not invent a
41. Detect all 84. Offer "Start" only for the subset with a verified launch
profile (B3). If the owner wants exactly 41, that becomes a filter on the
registry (`launch.tier`), not a second list.

### B2. Tiers across the 84

| Tier | Meaning | In the picker | Ids |
|---|---|---|---|
| **T1 PTY, profile verified** | TUI CLI, native transcript dialect known, flags checked against the binary | ▶ Start | claude-code, codex, pi, opencode, goose, github-copilot (copilot CLI), antigravity-cli (agy). Binaries present on this host: `claude`, `codex`, `pi`, `opencode`, `goose`, `copilot`, `agy`. |
| **T2 PTY, profile from vendor docs** | TUI CLI. The launch, resume and transcript details below are from vendor docs as of mid-2026 and must be checked in WP H0 before going to T1. | ▶ Start (after H0) | gemini-cli, qwen-code, iflow-cli, cursor (`cursor-agent`), droid, crush, kimi-code-cli, kiro-cli, amazon-q, augment (`auggie`), aider, amp, cline (CLI), continue (`cn`), openhands, hermes-agent, openclaw, mistral-vibe, forgecode, letta-code, codebuff, gptme, vtcode, nanocoder, kode, neovate, junie, qoder (`qodercli`), rovodev, tabnine-cli, cortex, codebuddy, command-code, adal, kilo (CLI), deepagents, autohand-code, dexto, g3, octofriend, bob |
| **T3 GUI app** | Desktop or IDE app with no stdin channel | 👁 detected; "Open in app" only (launches the app on the directory; no prompt injection, no mirroring except read-only transcript where agentstore has a dialect) | cursor (IDE), windsurf, zed, trae, trae-cn, antigravity, qoder (IDE), warp, mux, jetbrains-ai-assistant, firebender, lingma, zencoder, zenflow, sourcegraph-cody, tabnine, roo, pochi, traycer, codestudio, codemaker, codearts-agent, aider-desk, mcpjam |
| **T4 not local** | Cloud agent, convention or framework | hidden | devin, ona, replit, universal, promptscript, loaf, astrbot, inference-sh, jazz, moxby, reasonix, terramind, tinycloud |

Counts: T1 7, T2 41, T3 24, T4 13. cursor and qoder appear in both T2 (CLI)
and T3 (IDE), so the four tiers hold 85 rows for 84 ids. The T2 count of 41
is a coincidence. The registry does not mark it, and H0 will move ids between
tiers.

**Interactive input.** Only a T1 or T2 harness gets prompts typed into its own
TUI, which is what ask 2 wants. A T3 GUI or IDE has no supported way to inject
input.

- VS Code extension commands and IDE plug-in APIs exist, but each is a separate
  integration against a separate vendor API. They are left out of this design
  (risk R9).
- ACP (`--experimental-acp`, `opencode acp`, `goose acp`, `copilot --acp`,
  `kimi --acp`, `auggie --acp`, `vibe-acp`) is interactive and structured, but
  it **replaces** the TUI. The ask rules that out as the main path. ACP is
  recorded per harness as a fallback for approvals only (C4.6).

### B3. Per-harness launch facts (T1 and T2)

Legend:

- **Conf**:
  - H: verified on this host or in our code
  - M: vendor docs
  - L: unverified, H0 must confirm
- **Pre-id**: the harness can be told its session id or file at launch, so the
  native file is known before the first line. Without it, belai locates the
  file by a new-file watch plus cwd match, under a per-(harness, cwd) start
  lock.
- **Int**: interrupt key. **Exit**: graceful exit sequence.
- **Paste**: bracketed paste is honoured. Every Rust/ratatui, Ink and Bubble
  Tea TUI enables DECSET 2004, and belai detects it at run time anyway (C4.3).

| id | bin | interactive argv (never `-p`/`exec`/`run`) | Pre-id | native transcript (dialect) | resume argv | Int / Exit | MCP wiring for the A2A bridge | key env (vault grant names) | Conf |
|---|---|---|---|---|---|---|---|---|---|
| claude-code | `claude` | `claude --session-id <uuid> [--permission-mode default\|plan\|acceptEdits] [--model M] --mcp-config <f> --settings <f>` | yes (`--session-id`) | `~/.claude/projects/<slug(cwd)>/<uuid>.jsonl`, where slug = cwd with `/` and `.` as `-`. One JSON object per line: `type` (user, assistant, attachment, system, summary, ...), `uuid`, `parentUuid`, `sessionId`, `cwd`, `gitBranch`, `timestamp`, `message.role`, `message.content[]` (text, tool_use, tool_result, thinking), `isSidechain` (`jsonl-claude`) | `claude --resume <uuid>` | Esc / `/exit`⏎ | `--mcp-config` JSON file; hooks (Stop, Notification, UserPromptSubmit) through `--settings` | ANTHROPIC_API_KEY (or the user's own OAuth) | H (format keys checked on host; parser `agentstore/jsonl.go:207-268`) |
| codex | `codex` | `codex [-m M] [-a on-request\|untrusted] [-s read-only\|workspace-write] -c mcp_servers.belai.command=… -c notify=[…]` | no | `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`. Line 1 is `type:"session_meta"` (`payload.id`, `cwd`, `cli_version`, `originator`). Then `response_item` (`payload.type` message, function_call, function_call_output, reasoning), `event_msg` (`task_started`, `task_complete`, `token_count`, `item_completed`), `turn_context` (`jsonl-codex`) | `codex resume <uuid>` | Esc / Ctrl-C ×2 | `-c mcp_servers.<n>.command/args`; `notify` program gets agent-turn-complete | OPENAI_API_KEY | H (keys and event types checked on host; parser `jsonl.go:270-324`) |
| pi | `pi` | `pi [--provider P --model M] --session <file>` | yes (`--session <path>` in the native dir) | `~/.pi/agent/sessions/--<cwd>--/<ts>_<uuid>.jsonl`. Line 1 is `{type:"session",id,cwd}`, then `message` (`message.role`, `content[]`), `model_change`, `thinking_level_change`, `custom`, `custom_message` (`jsonl-pi`) | `pi --session <file>` / `--continue` | Esc / Ctrl-C ×2 | no native MCP. A2A via a pi extension or skill calling `belai a2a` (L) | per provider (ANTHROPIC_API_KEY, OPENAI_API_KEY, …) | H (types checked on host; parser `jsonl.go:326-378`) |
| opencode | `opencode` | `opencode [--model P/M] [<dir>]` | no (session row) | SQLite `~/.local/share/opencode/opencode.db`: `session(directory)`, `message`, `part.data` JSON (`sqlite-opencode`) | `opencode --session <id>` / `-c` | Esc / Ctrl-C | `OPENCODE_CONFIG=<file>` holding `mcp` | per provider | H (parser `sqlite.go:178-300`; read via the `sqlite3` binary, read-only) |
| goose | `goose` | `goose session --name kls-<sid>` | yes (name) | SQLite `~/.local/share/goose/sessions/sessions.db`: `sessions(working_dir, name)`, `messages(role, content_json)` (`sqlite-goose`) | `goose session --resume --name kls-<sid>` | Ctrl-C / `/exit` | `--with-extension "<cmd>"` (stdio MCP) | GOOSE_PROVIDER plus provider key | H (parser `sqlite.go:37-176`) |
| github-copilot | `copilot` | `copilot [--model M] --additional-mcp-config <json>` | no | `~/.copilot/session-state/` (JSON or JSONL, schema not guaranteed: `agentstore` "generic") | `copilot --resume [<id>]` / `--continue` | Esc / Ctrl-C ×2 | `--additional-mcp-config` (M) | COPILOT_GITHUB_TOKEN / GH_TOKEN | M (dir checked on host; empty) |
| antigravity-cli | `agy` | `agy` (M) | no | `~/.gemini/antigravity-cli/conversations/*`, prompts in `history.jsonl` (`generic`) | L | L | L | GEMINI_API_KEY (L) | M (binary and dirs checked on host) |
| gemini-cli | `gemini` | `gemini [-m M] [--approval-mode default\|auto_edit]` | no | `~/.gemini/tmp/<sha256(cwd)>/chats/session-<ts>-<id>.json`, one JSON document rewritten per turn (`messages[]`: `type` user, gemini, tool; `content`; `toolCalls`) | `gemini --resume <id\|latest>` | Esc / `/quit`⏎ | settings `mcpServers` (system settings path through env, L) | GEMINI_API_KEY, GOOGLE_API_KEY | M |
| qwen-code | `qwen` | as gemini | no | `~/.qwen/tmp/<hash>/…` (Gemini fork) | `qwen --resume` (L) | Esc / `/quit` | as gemini | OPENAI_API_KEY, DASHSCOPE_API_KEY | M/L |
| iflow-cli | `iflow` | as gemini | no | `~/.iflow/…` (Gemini fork) | L | Esc | as gemini | IFLOW_API_KEY | L |
| cursor | `cursor-agent` | `cursor-agent [--model M]` | no | `~/.cursor/chats/<hash>/<id>/store.db` (SQLite) | `cursor-agent --resume <id>` | Esc / Ctrl-C | `~/.cursor/mcp.json` (user file; do not edit, L) | CURSOR_API_KEY | M |
| droid | `droid` | `droid` | no | `~/.factory/sessions/…/<id>.jsonl` (L) | `droid --resume <id>` (L) | Esc | `droid mcp add` (persistent; avoid) or config env (L) | FACTORY_API_KEY | L |
| crush | `crush` | `crush [-c <dir>]` | no | `<repo>/.crush/crush.db` (SQLite: sessions, messages) | session picker in the TUI | Esc / Ctrl-C | `crush.json` `mcp` (project file, L) | provider keys | M |
| kimi-code-cli | `kimi` | `kimi` | no | `~/.kimi/sessions/…` (L) | `kimi --continue` (L) | Esc | `--mcp-config-file` (L) | MOONSHOT_API_KEY / KIMI_API_KEY | L |
| kiro-cli | `kiro-cli` | `kiro-cli chat` | no | SQLite under `~/.local/share/kiro-cli/` (L; agentscan has `scan_kiro.go`) | `kiro-cli chat --resume` | Ctrl-C / `/quit` | agent config `mcpServers` (L) | (AWS Builder ID login) | L |
| amazon-q | `q` | `q chat` | no | SQLite `~/.local/share/amazon-q/data.sqlite3` (L) | `q chat --resume` | Ctrl-C / `/quit` | `~/.aws/amazonq/mcp.json` (L) | (Builder ID) | L |
| augment | `auggie` | `auggie` | no | `~/.augment/sessions/*.json` (L) | `auggie --resume` (L) | Esc | `--mcp-config` (L) | AUGMENT_SESSION_AUTH | L |
| aider | `aider` | `aider [--model M] --no-auto-commits?` (owner choice) | n/a | `<repo>/.aider.chat.history.md` (Markdown, append-only) and `.aider.input.history` | `aider --restore-chat-history` | Ctrl-C / `/exit` | none (no MCP); A2A unavailable | provider keys | M |
| amp | `amp` | `amp` | no | server-side threads. No local transcript (L), so mirroring is PTY-only. | `amp threads continue <id>` | Esc / Ctrl-C | `amp.mcpServers` in settings (L) | AMP_API_KEY | L |
| cline | `cline` | `cline` (CLI TUI, L) | no | `~/.cline/data/tasks/<id>/{api_conversation_history,ui_messages}.json` (L) | `cline task open <id>` (L) | Esc | L | provider keys | L |
| continue | `cn` | `cn` | no | `~/.continue/sessions/<id>.json` (L) | `cn --resume` (L) | Esc | config `mcpServers` | provider keys | L |
| openhands | `openhands` | `openhands` | no | `~/.openhands/conversations/<id>/…` (L) | `openhands --resume <id>` (L) | Ctrl-C | config (L) | LLM_API_KEY | L |
| hermes-agent | `hermes` | `hermes` | no | `~/.hermes/sessions/…` (L) | `hermes --resume` (L) | Ctrl-C | config `mcp_servers` (L) | provider keys | L |
| openclaw | `openclaw` | `openclaw tui` (L) | no | `~/.openclaw/agents/<agent>/sessions/<id>.jsonl`, likely pi-format (L) | L | Esc | L | provider keys | L |
| mistral-vibe | `vibe` | `vibe` | no | `~/.vibe/logs/session/*.json` (L) | `vibe --continue` (L) | Esc / Ctrl-C | `config.toml` `mcp_servers` (L) | MISTRAL_API_KEY | L |
| gptme | `gptme` | `gptme --name kls-<sid>` | yes (name, L) | `~/.local/share/gptme/logs/<name>/conversation.jsonl` (`role`, `content`, `timestamp`) (M) | `gptme --name <name>` (L) | Ctrl-C | none (L) | provider keys | M/L |
| forgecode, letta-code, codebuff, vtcode, nanocoder, kode, neovate, junie, qoder, rovodev, tabnine-cli, cortex, codebuddy, command-code, adal, kilo, deepagents, autohand-code, dexto, g3, octofriend, bob | per H0 | per H0 | — | unknown | — | — | — | per `tools.json` `env` | L: not offered until H0 fills a profile |

`tools.json` already records each agent's env var names (field `env`, for
example claude-code's `ANTHROPIC_API_KEY`, `CLAUDE_CODE_*`). H0 joins them into
the profile's allowlist for vault grants.

### B4. Parser code to reuse

| Need | Reuse | Cite |
|---|---|---|
| Stream JSONL lines with a cap | `scanJSONL` | `belai/internal/agentstore/jsonl.go:22` |
| Flatten Anthropic/OpenAI `content[]` | `contentText` | `jsonl.go:47` |
| Timestamps of many shapes | `parseTime`, `promptTime`, `numberTime` | `jsonl.go:90`, `:615`, `:634` |
| Claude, Codex and pi turn pickers | `claudeAdapter`, `codexAdapter`, `piAdapter` (`Sources`, `Turns`), `streamJSONLTurns` | `jsonl.go:207-401` |
| Goose and opencode SQL | `gooseTurns`, `opencodeTurns`, `sqliteQuery` (shells out to `sqlite3`, no Go dep) | `sqlite.go:19`, `:104`, `:233` |
| VS Code Copilot Chat JSON (T3 read-only) | `vscodeAdapter` | `json.go:31-140` |
| Registry and expansion | `harness.All`, `ByID`, `Expand` | `belai/internal/harness/harness.go` |
| Native credential discovery (whether the harness is logged in) | `agentscan` | `belai/internal/agentscan/scan_claude.go` etc. |
| belai Entry → node Entry role mapping | `factorylink.ToEntry`, `roleFor` | `belai/internal/factorylink/sessions.go:396`, `:419` |

Today these adapters return whole `Turn`s for search. C4.4 factors the
per-line "pick" functions into exported pure functions that both the search
tools and the live tailer call. Only one parser exists per dialect.

---

## C. Design

### C1. Where detection runs

- **On the belai node, in `belai rc`.** That is where the harness would run,
  and with whose `$HOME`, PATH and login.
- **Not on the Keleustes host.** Keleustes "runs no gh and calls no model
  itself" (`belai/docs/keleustes.md`, "Jobs"), and it has no PTY or agent
  runtime. On a desktop the owner runs `belai rc` on the same machine, and it
  shows up as a node like any other. The picker says so when no node offers
  harnesses: `🤖 No harnesses · run belai rc here`.

Detection, in new package `belai/internal/harnessdetect`:

1. For each registry harness with a `launch` profile: `exec.LookPath` each
   `launch.bins` entry on the daemon's PATH. Then, once per daemon start, on
   the PATH of the user's login shell (`$SHELL -lc 'printf %s "$PATH"'`, 3 s
   timeout). A GUI-started daemon often lacks `~/.local/bin` and nvm or brew
   paths. The resolved absolute path is kept, and later launches exec that
   path, not a PATH lookup.
2. Version probe: `<abs> <launch.version_args>` with no shell, a scrubbed env,
   a 3 s timeout and 4 KiB of output. Keep the first line, `sanitize.Line(…,64)`.
   A failing probe means "broken", which is advertised so the owner sees why.
3. Login probe (optional): use `agentscan` to report `logged_in` (an OAuth or
   credential file exists) without reading secrets beyond what agentscan
   already reads. A grant is then needed only when it is false.
4. `detect` dirs alone mean `configured` (T3/T4, or a T1/T2 with no binary). The
   picker shows that state disabled, never as installed.
5. Re-run on SIGHUP, on `rc` start, and every 10 min (cheap). Send a new
   `HostAdvert` on the link when the result changes.

### C2. Protocol changes (additive; `buf breaking` clean)

```proto
// ---- HostAdvert ----------------------------------------------------------
message HostAdvert {
  // ... fields 1-9 unchanged
  // Third-party agent harnesses on the host (belai registry ids).
  repeated Harness harnesses = 10;
}

message Harness {
  string id = 1;              // registry id, e.g. "claude-code"
  string name = 2;            // display name from the registry
  string version = 3;         // first line of --version, sanitized, ≤64
  // "launch": a PTY profile and a working binary; "broken": binary found,
  // probe failed; "gui": an app only (open, no injection); "configured":
  // config dir only.
  string state = 4;
  string detail = 5;          // why broken/configured, harness text ≤200
  bool resume = 6;            // the profile can resume a native session
  bool a2a = 7;               // the profile can wire the belai A2A MCP bridge
  bool logged_in = 8;         // native credentials present (no values)
  // Env var names a vault grant may set for this harness (no values).
  repeated string grant_env = 9;
  // Permission modes the profile maps ("default", "plan", "accept_edits").
  repeated string modes = 10;
  string transcript = 11;     // dialect id, e.g. "jsonl-claude"; "" = PTY only
}

message Capabilities {
  // ... fields 1-5 unchanged
  // The rc daemon can run harness sessions in a PTY and stream their screen.
  bool harness_sessions = 6;
}

// ---- Dispatch ------------------------------------------------------------
message Dispatch {
  // ... fields 1-15 unchanged
  // A third-party harness to run instead of belai (kind "session" only).
  // Empty = belai. The node refuses an id it did not advertise as "launch".
  string harness = 16;
  // Resume this native session id (from a SessionUpsert.harness_session_id).
  string harness_resume = 17;
  // Permission mode from Harness.modes; "" = the harness default.
  string harness_mode = 18;
}

// ---- Sessions ------------------------------------------------------------
message SessionUpsert {
  // ... fields 1-21 unchanged
  // kind gains "harness".
  string harness = 22;             // registry id
  string harness_version = 23;
  string harness_session_id = 24;  // the harness's own id (Claude uuid, Codex rollout id)
  // "native" (mirrored from the harness's own transcript) or "screen"
  // (no transcript dialect: entries are screen snapshots only).
  string transcript_source = 25;
}
// Entry: unchanged. meta_json carries
//   {"harness":"claude-code","native":{"type":"assistant","id":"<uuid>","file_off":40213}}
// Roles map onto the existing set: user, assistant, tool, tool_result, system, a2a, error.

// ---- Screen (new service) ------------------------------------------------
// The live terminal of a harness session. The node dials; Keleustes is the
// server, as for every other RPC. Output only: keys go through the inbox
// as Command{name:"key"} so they are acked, audited and gated.
service Screen {
  rpc Stream(stream ScreenUp) returns (stream ScreenDown);
}
message ScreenUp {
  string session_id = 1;
  oneof body {
    ScreenOpen open = 10;   // first frame
    bytes output = 11;      // raw PTY bytes, ≤32 KiB per frame
    ScreenSize size = 12;   // after a resize
    ScreenExit exit = 13;
  }
  uint64 offset = 2;        // byte offset of `output` in the PTY stream
}
message ScreenOpen { ScreenSize size = 1; bytes replay = 2; /* last ≤256 KiB */ }
message ScreenSize { uint32 cols = 1; uint32 rows = 2; }
message ScreenExit { int32 code = 1; string signal = 2; }
message ScreenDown {
  oneof body {
    ScreenSize resize = 10;   // viewer asks for a size (clamped by node)
    bool pause = 11;          // no viewer: the node stops sending output
  }
}

// ---- Board (from A4.1) ---------------------------------------------------
// rpc A2APeers(A2APeersRequest) returns (A2APeerList);
```

Keleustes store: `HostInfo.harnesses: Vec<HarnessInfo>`
(`kls-store/src/lib.rs`, coordinator-owned), filled in `host_info`
(`kls-node/src/server.rs:316`). `DispatchSpec` gains `harness`,
`harness_resume` and `harness_mode` (`kls-core/src/gateway.rs`).
`sessions` gains `harness`, `harness_version`, `harness_session_id` and
`transcript_source` (migration in the M1 workstream).

Inbox `Command` names gain no new values. `key <name>` is reused, and the
allowlist per harness is in its profile (C4.6).

### C3. Registry: the `launch` block

This extends `belai/tools/gen-harnesses/overlay.json`, hand-written and
checked like the existing dirs, and so `harnesses.json`:

```json
"claude-code": {
  "launch": {
    "tier": "pty",
    "bins": ["claude"],
    "version_args": ["--version"],
    "argv": ["{bin}", "--session-id", "{native_id}"],
    "model_flag": ["--model", "{model}"],
    "modes": {"default": [], "plan": ["--permission-mode", "plan"],
              "accept_edits": ["--permission-mode", "acceptEdits"]},
    "resume_argv": ["{bin}", "--resume", "{native_id}"],
    "native_id": "uuid4",
    "transcript": {"dialect": "jsonl-claude",
                   "path": "~/.claude/projects/{cwd_slug}/{native_id}.jsonl"},
    "mcp": {"how": "flag-file", "flag": "--mcp-config"},
    "hooks": {"how": "flag-file", "flag": "--settings", "dialect": "claude-code"},
    "input": {"paste": "bracketed", "submit": "\r", "submit_delay_ms": 80,
              "multiline": "paste"},
    "keys": {"interrupt": "esc", "exit": ["/exit", "enter"],
             "allow": ["enter", "esc", "up", "down", "tab", "1", "2", "3"]},
    "idle": {"transcript": "assistant-end", "quiet_ms": 1500},
    "grant_env": ["ANTHROPIC_API_KEY"],
    "never_flags": ["--dangerously-skip-permissions", "-p", "--print"]
  }
}
```

Rules for the generator and its tests:

- Every `argv` element is a literal or a `{placeholder}` from a closed set:
  `bin`, `native_id`, `model`, `cwd`, `cwd_slug`, `sid`, `mcp_file`,
  `settings_file`.
- A `{model}` value must match `^[A-Za-z0-9._:/@-]{1,128}$`.
- `never_flags` must not appear in any produced argv.
- Each `grant_env` must be a name from the tool's `tools.json` `env`, and none
  may match `*_BASE_URL|*_ENDPOINT|*_PROXY|HTTP*_PROXY|*_HOST`.
- The parity test of `belai/docs/harnesses.md` gains a **Launch** column.

### C4. belai side: running a harness session

New hidden child `belai rc-harness`, the analogue of `rc-session`
(`rc/daemon.go:1138` `childArgs`). New packages: `internal/harnessrun` (PTY,
input, keys, lifecycle) and `internal/harnesstail` (dialect tailers).

#### C4.1 Start

`factoryStart` (`rc/factory.go:193`), with `fd.harness` set, runs the checks
already made today:

- trusted or offered dir, factory not stopped, session cap, deadline,
  branch worktree;
- the harness advertised as `launch`;
- `harness_mode` in the profile's modes;
- `harness_resume` matching `^[A-Za-z0-9-]{1,64}$` and present in the
  dialect's store for this cwd.

Then it starts `belai rc-harness -dispatch … -session-id … -harness <id>
[-mode …] [-model …] [-resume …]`. The fixed argv comes only from the
registry. The prompt goes over stdin, as now, and the grant over fd 3, as now.

`rc-harness` then:

1. Mints `native_id` when the profile pre-sets one, or takes the start lock
   `~/.vulnetix/belai/harness/locks/<id>-<sha(cwd)>` for the locate phase.
2. Writes per-session config under
   `~/.vulnetix/belai/harness/<sid>/` (0700): the MCP bridge config (C5) and the
   hook settings (C4.5). The harness's own config files are **never edited**.
3. Builds the env:
   - start from the scrubbed base env belai uses for tools;
   - keep the user's `HOME`, `PATH` (the resolved one), `TERM=xterm-256color`,
     `COLORTERM`, `LANG`;
   - add the grant's allowed `grant_env` vars (C6.3);
   - set `BELAI_HARNESS_SOCK=<unix socket>` for the bridge and hooks.
   - Nothing else of belai's env is passed, and no `BELAI_*` secret is passed.
4. `pty.StartWithAttrs` (creack/pty, already in go.mod) at 120×40, with
   `Setsid` and `Setctty`, in its own process group. The cwd is the dispatch cwd.
5. Starts the PTY reader. It feeds:
   - a ring buffer (256 KiB) for `ScreenOpen.replay`;
   - the `Screen.Stream` uplink while a viewer is attached;
   - a mode tracker (DECSET 2004 bracketed paste, alt screen);
   - the vaultenv scrubber pass. Bytes that match a granted value are masked
     before they leave the host. This is a stream scrubber that keeps state
     across chunk boundaries.
6. Starts the tailer (C4.4) and the mirror (`SessionUpsert` with kind
   `harness`, `harness_session_id`, then `Append`).
7. Delivers the first prompt (C4.3) once the TUI is ready. Ready means
   bracketed paste is enabled, or no PTY output for 800 ms after the first
   output, with a 20 s ceiling.

#### C4.2 Session identity

- belai session id `sid`: the one Keleustes sees.
- `harness_session_id`: the native id, known at launch (pre-id) or after
  locate. The locate rules per dialect:
  - **codex**: a new `rollout-*.jsonl` under today's (and yesterday's) date
    dir, mtime after spawn, whose `session_meta.payload.cwd` equals cwd.
  - **gemini**: a new `chats/session-*.json` under `tmp/<sha256(cwd)>/`.
  - **opencode**: a `session` row with `directory = cwd` and
    `time_created ≥ spawn`.
  - **copilot**: a new entry under `session-state/`.
  - The lock is held until it matches, at most 60 s. A miss downgrades
    `transcript_source` to `screen`.
- `~/.vulnetix/belai/harness/<sid>/ledger.jsonl` maps every Keleustes entry
  index to its source: `{"i":12,"src":"native","off":40213,"len":812}` or
  `{"i":13,"src":"own","type":"a2a","text":…}`. Own lines are belai's A2A
  notices, system lines and errors. The ledger makes `Append` indexes
  deterministic across restarts and lets teleport and search work. No copy
  of the native transcript is kept: the harness's own file is the record, as
  the ask requires.

#### C4.3 Prompt input (interactive, into the TUI)

Each inbox `Prompt` is admitted by the code path a belai rc prompt takes:
`sync.remote_prompts`, `CleanPrompt`, the classifier, and factory-stopped
refusal. Then:

1. **Wait for idle.** In order of preference:
   - a hook `Stop` event on the socket (Claude);
   - the transcript turn end (Claude: an assistant entry whose `stop_reason`
     is `end_turn`; Codex: `event_msg.task_complete`; pi: an assistant
     `message` with a stop reason; gemini: last message `type:"gemini"` with
     no pending tool call);
   - the fallback: PTY quiet for `idle.quiet_ms` and not in a known
     "approval" screen (C4.6).
   - A prompt queued behind a running turn stays unacked, the same rule as
     today, so a crash re-delivers it.
2. **Neutralise.** Strip every C0/C1 control byte except `\n` and `\t`. That
   removes ESC, so a prompt cannot hold `ESC[201~` and end the paste early to
   inject keystrokes (risk R1). Normalise `\r\n` to `\n`, and cap at 64 KiB.
3. **Write.** If bracketed paste is on: `ESC[200~` + text + `ESC[201~`, wait
   `submit_delay_ms`, then `submit`. If it is off, the profile's `multiline`
   decides: `paste` is refused with "harness does not accept multi-line input",
   `backslash` sends a `\`+`\r` per line, and `join` replaces newlines with
   spaces. Writes are chunked at 4 KiB with a 2 ms gap, because some Node TUIs
   drop large single writes.
4. **Confirm.** Wait up to 15 s for a native `user` entry whose text starts with
   the first 200 normalised characters of the prompt. On a match, ack `done`
   with that entry's index as detail. On a miss, ack `failed`
   ("harness did not take the prompt") and add an own `error` line. No
   retry, since a retype could double-submit.

`Answer` items are refused for harness sessions ("this harness asks in its
own screen; use keys"). Approvals go through keys (C4.6).

#### C4.4 Transcript tailing (native files → Entries)

The tailer interface lives in `internal/harnesstail`:

```go
type Dialect interface {
    ID() string                                   // "jsonl-claude"
    Locate(ctx context.Context, s LocateSpec) (Source, error)
    // Follow emits native records from off onward, and keeps going
    // (inotify/kqueue on the file or dir, 500 ms poll fallback, handles
    // rename, truncate and whole-file rewrite).
    Follow(ctx context.Context, src Source, off int64, emit func(Record)) error
}
type Record struct {
    Off, Len int64       // byte span (JSONL) or rowid/version (SQLite, JSON doc)
    Role     string      // user|assistant|tool|tool_result|system|error
    Text     string      // flattened content, capped at 64 KiB
    At       int64       // ms
    NativeID string
    Meta     map[string]any // tool name, args (truncated), usage, stop_reason
    TurnEnd  bool
}
```

- **JSONL** (claude, codex, pi, gptme, droid): this is a byte-offset tail of an
  append-only file. Torn last lines are held until `\n`. Each line goes
  through the dialect's pick function, factored out of `agentstore/jsonl.go`.
  - Claude's `isSidechain:true` lines (subagents) are kept, with
    `meta.subagent`.
  - `attachment`, `api-request-*`, `mode` and bookkeeping types are mapped to
    `system` and hidden by the existing `is_noise` filter in Keleustes
    (`sessions.rs:1256`).
- **Single JSON documents** (gemini, copilot, cursor-agent, auggie): the file
  is rewritten per turn. On change, re-read it (cap 32 MiB) and diff by message
  index. `Off` is the message index.
- **SQLite** (opencode, goose, crush, kiro, q, cursor-agent): run
  `sqlite3 -readonly -json` with a query keyed on `rowid > last`
  (`agentstore/sqlite.go:19` `sqliteQuery`). It is triggered by mtime changes
  of the db and `-wal`, with a 1 s poll and a 250 ms debounce.
- **Markdown** (aider): tail `.aider.chat.history.md`. `#### ` lines are user,
  and the rest is assistant text.
- **No dialect** (amp, L-tier ones): `transcript_source:"screen"`. On each
  turn end (PTY quiet), one `system` entry holds a plain-text screen snapshot
  (vt100 → text, 200 lines). Search and teleport are limited for these.

Mapping to `Entry`: `role`, `text` and `at_ms` come from the Record.
`meta_json` is `{"harness","native":{type,id,off}, "tool":{name,args≤2KiB},
"usage":{…}}`. Usage is summed into `SessionUpsert.input_tokens` and
`output_tokens` where the dialect has it (Claude `message.usage`, Codex
`token_count`).

#### C4.5 Hooks (where the harness has them)

Hooks are used only through a per-session file passed by flag (Claude
`--settings`, Codex `-c notify=`). The user's own settings are never written.

- The hook command is `belai harness-hook --sock $BELAI_HARNESS_SOCK --event <name>`.
  It reads the hook JSON on stdin and forwards it, capped at 64 KiB, to the
  runner.
- Its output is always empty and its exit code is 0, so a hook never blocks or
  steers the harness.
- Events used:
  - `Stop` → idle;
  - `Notification` (`permission_prompt`, `idle_prompt`) → session `waiting` and
    `pending_question` (harness text, `sanitize.Line`, ≤300);
  - `UserPromptSubmit` → prompt confirmation.

#### C4.6 Controls: stop, interrupt, mode, model, approvals

| Inbox Command | Harness action |
|---|---|
| `interrupt` | write `keys.interrupt` (Esc for most; Ctrl-C for goose and aider) |
| `stop` | send `keys.exit`, wait 5 s, SIGTERM the process group, wait 5 s, SIGKILL. Then `Sessions.End{reason}`. |
| `key <name>` | only names in `keys.allow`, mapped to bytes from a fixed table (`enter`=`\r`, `esc`=`\x1b`, `up`=`\x1b[A`, `tab`, digits, `y`, `n`, `ctrl-c`). Anything else is refused. Each key is audited. This is how the owner answers an approval prompt they see on the Screen view. |
| `mode <m>` / `model <m>` | only when the profile defines a slash command for it (Claude `/model`). Typed as a prompt through C4.3 with no classifier exemption. Otherwise refused. |
| `checkout`, `shell`, `command` | refused for harness sessions |

All of these need `--web-controls` on the daemon, the same gate as today.

Modes from the dispatch map only to the harness's **own** permission modes in
the profile. `auto` is the harness default. The `never_flags` set (skip
permissions, yolo, `-p`/`exec`) can never be produced, whatever the dispatch
holds.

**ACP fallback** (out of scope for v1): for harnesses with ACP, a later
profile flag `approvals: "acp"` could run ACP side by side. It is not used,
because only one client can drive a process.

#### C4.7 Resume and teleport

- Resume: `Dispatch.harness_resume` uses `resume_argv` with the native id.
  Keleustes offers **Resume** on an ended harness session whose node still
  advertises that harness and whose `harness_session_id` is set.
- Teleport is not supported for harness sessions in v1. The native file
  belongs to the harness's home on that host. `Teleport` refuses them with a
  reason.

### C5. A2A for a harness session: MCP bridge

The goal is to let the third-party model talk to belai peers through a
**tool**, so peer text arrives as a tool result (data), never as typed user
input.

- `belai a2a-mcp --sock $BELAI_HARNESS_SOCK` is a stdio MCP server, wired
  through the profile's `mcp` method. It exposes:
  - `peers()` → `A2APeers` (A4.1), sanitized;
  - `send(to, text)` → `a2a.Relay` → `factorylink.A2ASend` from
    `<node>:<sid>`. Text capped at 4 KiB. Refused in a blackout.
  - `inbox(after?)` → envelopes addressed to `<node>:<sid>`, each **classified
    as `KindPeer` before it is returned** (refused ones become
    `{"refused": "<reason>"}`). Every result is wrapped as
    `{"untrusted_peer_message": {...}}`.
  - `ack(id, decision)` → as `A2AAck`.
- **Notification without text:** when a direct message arrives and the harness
  is idle, the runner types this harness line through C4.3:
  `[belai] 1 peer message waiting. Call the belai inbox tool to read it.`
  The peer's text never goes into the PTY. While busy, it waits for idle.
  Broadcasts do not notify.
- The runner records each sent and received envelope as an own `a2a` ledger
  line, so the Keleustes transcript shows them (violet, `sessions.rs:1300`).
- A harness without MCP (aider, pi without an extension) gets `a2a:false`.
  Keleustes shows A2A as unavailable for it. Peer text is never typed into
  it.
- The bridge is a belai process, so its classifier runs with the host's
  posture and provider. If the classifier is unavailable, `inbox` fails
  closed.

### C6. Security

The rules are added to `belai/AGENTS.md` under a new bullet, "Third-party
harness sessions". It needs sign-off.

1. **Harness output is untrusted.** This covers PTY bytes, native transcript
   text, hook payloads and `--version` output.
   - In belai it never reaches a belai model, except through the existing
     `SearchSessions` and `ReadSession` tools (`KindAgentStore`).
   - It reaches Keleustes as data. The transcript is rendered as plain text
     with no link auto-open (the existing caps apply). The Screen view
     parses bytes with a VT emulator (`vt100` crate) into a cell grid. It
     drops OSC 52 (clipboard), OSC 0/2 titles and OSC 7, and shows OSC 8
     links as text. The grid is never passed to a real terminal.
   - A Keleustes `model` job never reads it without a `classify` job first.
2. **Inputs to the harness.**
   - Prompts pass the existing remote-prompt gates, then C0/C1 stripping
     (C4.3).
   - Keys come only from the allowlist.
   - Peer text goes only through the MCP tool result, after `KindPeer`
     classification.
   - No dispatch field is copied into argv except through the profile
     placeholders, which are validated. The model id is pattern-checked.
     The cwd is the checked trusted dir.
3. **Vault grants for third-party harnesses** (this changes an invariant).
   Today a grant "never [goes into] the process environment or a tool's". A
   harness reads keys only from its env, so the rule needs a narrow,
   explicit exception:
   - It applies only to `rc-harness` and only to env names in the profile's
     `grant_env`. The generator refuses URL, endpoint, proxy and host names.
   - The env is set only on the harness child, never in belai's own env.
     `rc-harness` reads the grant from fd 3, builds the child env slice and
     drops its copy.
   - Every value is added to the vaultenv scrubber, and the PTY stream and
     ledger own-lines are scrubbed. Native transcript text is scrubbed before
     `Append`.
   - The grant is dropped at `expires_at_ms`. The harness is then stopped,
     because its env cannot be revoked.
   - Disclosed limits, shown in the UI: a third-party harness and its tools
     (its own Bash) can read the key, the same user can read
     `/proc/<pid>/environ`, and the harness may log it. The picker
     therefore defaults to **Own login**, and **Vault key** needs an explicit
     choice.
   - Windows is refused, as today.
4. **Config isolation.**
   - belai writes only its per-session MCP and hook files under
     `~/.vulnetix/belai/harness/<sid>/` (0700, removed at end).
   - Harness user and project config are not edited. A profile whose only MCP
     method is "edit a user file" gets `a2a:false`.
   - Native transcripts are read-only to belai. The tailer opens them
     `O_RDONLY` and refuses symlinks out of the expected root (same rule as
     `agentscan`).
5. **Sandboxing.** The harness runs with the user's rights, like a terminal
   the user opened. belai's sandbox and Landlock do not wrap it in v1. The
   picker shows the harness's own mode, and the session row is tagged
   `🛡 harness rules`.
6. **Audit.** Each harness start, prompt, key, A2A envelope, stop and grant
   use is a `host_dispatch` or new `harness_*` audit fact. It goes to the
   Keleustes ledger (`Sessions.Audit`).

### C7. Keleustes side

#### C7.1 New session dialog

The dialog is `sessions.rs:971`. An **Agent** row is added first. The current
"Agent" (a belai profile) becomes **Profile** and only shows for belai.

```
┌ ⌨ New session ─────────────────────────────────────────────┐
│ 🖴 Node       [ xeon ▾ ]                                     │
│ 🤖 Agent      [ ◉ belai ▾ ]                                  │
│               ┌──────────────────────────────────────────┐  │
│               │ ◉ belai                 v0.160           │  │
│               │ ◎ Claude Code           2.3.1   ✓ login  │  │
│               │ ◎ OpenAI Codex          0.70    ✓ login  │  │
│               │ ◎ opencode              1.4               │  │
│               │ ◎ Pi                    0.9               │  │
│               │ ◎ Goose                 1.12              │  │
│               │ ◎ GitHub Copilot        0.4    ⚠ no A2A  │  │
│               │ ─ detected ─────────────────────────────  │  │
│               │ ⊘ Cursor (app)          open only         │  │
│               │ ⊘ Gemini CLI            ⚠ broken          │  │
│               │ ⊘ Hermes Agent          config only       │  │
│               └──────────────────────────────────────────┘  │
│ 📁 Directory  [ ~/GitHub/Vulnesis/keleustes        ▾ ]       │
│ 💬 Prompt     [ Task                                   ]     │
│ ⚙ Mode       ( default ) ( plan ) ( accept edits )          │
│ 🧠 Model      [ harness default ]                           │
│ 🔑 Keys       ( ◉ Own login ) ( ○ Vault key ▾ )             │
│ ↺ Resume     [ none ▾ ]   (native sessions in this dir)     │
│                                              [ ▶ Start ]    │
└─────────────────────────────────────────────────────────────┘
```

- The rows come from `node.host.harnesses`. The `launch` state is selectable.
  `gui`, `broken` and `configured` are listed under "detected" and greyed,
  with `detail` on hover.
- With a harness chosen: Provider, Crew and belai Profile are hidden. Mode shows
  `Harness.modes`. Keys shows Vault only when `grant_env` is non-empty.
  Choosing Vault lists the vault entries whose env name is in `grant_env`.
- **Resume** lists the node's ended harness sessions for that cwd and harness
  (from the store, `harness_session_id` set).
- `DispatchSpec { kind:"session", harness, harness_mode, harness_resume,
  vault_providers }`.
- CLI: `keleustes sessions dispatch --harness claude-code --mode plan …`
  (`kls-app/src/cli.rs:24` `Dispatch`). Add `keleustes nodes harnesses <node>`.

#### C7.2 Session detail

```
┌ ⌨ Fix flaky test  ·  🤖 Claude Code 2.3.1  ·  🖴 xeon  ·  ▶ live ┐
│ [ 💬 Transcript ] [ ⌨ Screen ] [ 📡 A2A ]      [ ✋ Interrupt ] [ ⏹ Stop ] │
├──────────────────────────────────────────────────────────────────┤
│ Screen tab:                                                      │
│ ┌──────────────────────── 120×40 ─────────────────────────────┐  │
│ │  (read-only VT render of the harness TUI)                   │  │
│ │  Do you want to make this edit?  1 Yes  2 Yes, always  3 No │  │
│ └─────────────────────────────────────────────────────────────┘  │
│ Keys: [⏎] [Esc] [↑] [↓] [1] [2] [3]          ⚠ waiting: approval │
├──────────────────────────────────────────────────────────────────┤
│ 💬 [ Prompt                                          ] [ ➤ Send ] │
└──────────────────────────────────────────────────────────────────┘
```

- Transcript tab: the existing `transcript_ui` plus per-harness labels.
  `meta.native.type` picks the role title. Tool rows use `meta.tool.name`.
  `role_title` and `role_label` (`sessions.rs:1271`, `:1284`) gain a harness
  table. Rendering is identical for every dialect, because the node already
  normalised the roles.
- Screen tab: opens `Screen.Stream` interest through the Hub. The node sends
  output only while some viewer is attached (`pause`). Rendering uses the
  `vt100` crate with an egui monospace grid at ≤10 fps, a 256 KiB replay and
  no input capture. The key buttons come from the profile's `keys.allow`,
  sent in the advert per harness as `allow_keys` (add to `Harness`).
- A2A tab (A4.2): the thread list for `<node>:<sid>`, and a composer that
  sends as `keleustes:owner`.
- Session row chip: `🤖 Claude Code`. Filter: Agent ▾.

#### C7.3 Fleet

The node card gains a `🤖 7` chip, the count of launchable harnesses. Hovering
lists their names and versions.

### C8. Non-negotiable rules (`docs/WORKSTREAMS.md:21-39`)

Every path in this design has to satisfy rules 1 to 5. The design adds no
exemption for built-ins, crews, harnesses or host-to-host traffic.

**Rule 1, provenance.** Every item records where it came from.

| Item | Origin stored | Where it is shown |
|---|---|---|
| Harness dispatch | the actor (`owner`, a workflow run or step id, a card id), the node, the harness id and version, the mode, the key source (`own_login` or a vault grant id; names only, no values), and `harness_resume` | session row and header: `👤 owner · 🤖 Claude Code 2.3.1` |
| Each prompt typed into a harness | the inbox item id, its sender (owner, workflow run, card), the classifier verdict, and the matched native entry index. If the bridge typed the notice line, the origin is `harness:a2a-notice` and the envelope id. | an origin chip on the user entry in the Transcript tab |
| Each key sent | the inbox command id, the sender, the key name | an own `system` ledger line, `⌨ key 1 · owner` |
| Each A2A envelope | `from` (`<node>:<sid>` or `keleustes:owner`), `to`, thread, the sending surface (`belai`, `harness:<id>` via MCP, `owner`), the KindPeer verdict on receipt, and the receiving session | the A2A tab and the transcript `a2a` row |
| Native transcript entries | `meta.native` (harness id, file, offset, native id), so every row traces to a byte span in the harness's own file | hover on the row |
| Detected harness | the binary's absolute path, version output hash, and probe time | Fleet hover |

New `SessionUpsert` fields 22-25 and `Entry.meta_json` carry these. Add
`origin_json` to `Dispatch` (field 19: `{"actor","run_id","step_id","card_id"}`)
so the node records the same origin in its own audit lines.

**Rule 2, audit.** Every action writes a hash-chained ledger entry, refusals
included.

- Keleustes emits these events, and each maps to a ledger kind in
  `ledger_kind_for_event` (`kls-core/src/audit.rs:142`):
  - `harness.dispatched` and `harness.refused`;
  - `harness.prompt.queued`, `harness.prompt.typed`, `harness.prompt.failed`
    and `harness.prompt.refused`;
  - `harness.key.sent` and `harness.key.refused`;
  - `harness.stopped`, with its reason (owner, kill switch or grant expiry);
  - `a2a.sent`, `a2a.withheld` and `a2a.refused`;
  - `screen.attached` (who watched and when).
- belai sends `harness_*` lines on `Sessions.Audit`. These are folded by
  `fold_line` (`audit.rs:278`) and verified in the host stream by
  `verify_host_stream` (`:314`):
  - start, the env var names granted, and argv with the model redacted to a
    pattern;
  - each prompt written (sha256 of the neutralised text, not the text);
  - each key, each A2A envelope in or out, and the classifier verdicts;
  - stop and escalation steps, and tailer downgrades to `screen`.
- Each entry has actor, action, target (session, envelope, node), origin
  (rule 1) and outcome.

**Rule 3, kill switch.** The factory kill switch is checked first, then the
action is refused, an event is emitted and the refusal is audited.

- Keleustes side:
  - `NodeGateway::dispatch` refuses a harness dispatch while the factory is
    stopped.
  - `queue_prompt`, `queue_command("key", …)` and the owner's A2A send also
    refuse while stopped. The exceptions are `stop` and `interrupt`, which
    still go out, as today.
  - Outbound Link frames for harness work are held.
- belai side, which is stricter than belai sessions today, because a
  third-party harness has none of belai's own gates:
  1. `rc-harness` subscribes to the control state (`control.json` and
     `Board.State`).
  2. When `stopped` turns on, it refuses new prompts, keys and bridge `send`
     calls.
  3. It then **stops every harness session**: the exit sequence, then SIGTERM
     to the process group, then SIGKILL. Each session ends with reason
     `factory stopped`.
  4. The stop is audited per session.
  5. A resume after the kill switch lifts is a new dispatch, so it passes the
     same checks.
- If a separate pause state is added to `FactoryControl` later, pause refuses
  input and A2A and keeps processes running. Stop always kills.

**Rule 4, A2A.** The A2A blackout and policy apply on every path, with
`KindPeer` intact.

- Every cross-node message from a harness session goes through `a2a-mcp` →
  `a2a.Relay` → `Board.A2ASend`. The relay refuses during the blackout, and the
  server checks again (`send_a2a` blackout check, `kls-core/src/board.rs:670`).
- The bridge's `inbox` is empty during the blackout, the receive stream is
  closed, and no notice line is typed. `peers` still answers, because it
  carries no message text.
- Owner messages (`keleustes:owner`) pass the same blackout check in
  `send_a2a`.
- Every received envelope is classified as `KindPeer`. That includes ones
  from the owner, from belai sessions, and from other harnesses. Peer text
  reaches a harness only as a classified MCP tool result, never typed into the
  PTY.
- A2A policy (`a2a.enabled`, per-profile allowances) applies to harness
  sessions as to belai sessions. When A2A is disabled, the bridge is not
  wired, and the advert sets `a2a:false` for that session.
- No other channel exists between sessions: no shared files, no prompt
  relaying through the inbox, and no Screen input path.

**Rule 5, tests.** Each WP adds tests that prove rules 1 to 4. See the
"Rules" rows in D2.

---

## D. Work packages, tests, risks

### D1. Work packages

Each is independently mergeable behind the `harness_sessions` capability.
Owners follow `docs/WORKSTREAMS.md`.

| WP | Repo / owner | Scope | Depends |
|---|---|---|---|
| **H0 Profiles** | belai `tools/gen-harnesses`, `internal/harness` | `launch` block schema, generator rules and tests (C3). Fill and verify T1 (7) against real binaries. Fill T2 from vendor docs at `conf:"L"` (not advertised as `launch` until verified on a real install). Add a "Launch" column to `docs/harnesses.md`. | — |
| **H1 Detect** | belai `internal/harnessdetect`, `factorylink/host.go`, `rc/factory.go:107` | PATH and login-shell PATH, version probe, agentscan login, states, re-advert on change | H0, P1 |
| **P1 Proto** | coordinator: `node.proto`, `kls-node/src/server.rs`, `kls-store/src/lib.rs` | C2 messages. `HostInfo.harnesses`. `buf breaking`. Regenerate `factorylink/nodev1`. | — |
| **H2 PTY runner** | belai `internal/harnessrun`, `cmd/belai` `rc-harness` | spawn, env, process group, readiness, input with bracketed paste and stripping, keys, stop and interrupt, ledger | H0 |
| **H3 Tailers** | belai `internal/harnesstail`, refactor `internal/agentstore` | the Dialect interface. jsonl-claude, jsonl-codex and jsonl-pi first, then sqlite-opencode and sqlite-goose, then gemini JSON. Factor the pick functions out of agentstore so search and tail share one parser. | — |
| **H4 Mirror** | belai `factorylink/sessions.go`, `sessionsync` | Upsert kind `harness`. Append from the ledger. Inbox mapping (prompt confirm, refused answer, key allowlist). Usage roll-up. | H2, H3, P1 |
| **H5 Hooks** | belai `cmd/belai harness-hook` | Claude `--settings` hooks and Codex `notify`, socket protocol | H2 |
| **H6 A2A bridge** | belai `cmd/belai a2a-mcp`, `internal/a2a`, `tools/a2a.go` | the MCP server, KindPeer classification on `inbox`, idle notification line, `A2APeers` client and tool | P1, H2 |
| **K1 Dispatch and picker** | Keleustes M1: `kls-core/src/gateway.rs` (coordinator), `kls-ui/src/screens/sessions.rs`, `kls-app/src/cli.rs`, store migration | C7.1, CLI flags, `nodes harnesses` | P1 |
| **K2 Transcript labels** | Keleustes M1: `sessions.rs` | harness role and tool labels, agent filter and chip | P1 |
| **K3 Screen** | Keleustes M1 and coordinator (`kls-node` Screen service) | `Screen.Stream` server with fan-out to UI subscribers, `vt100` render, key buttons through `queue_command("key", …)`, OSC filtering | P1, H2 |
| **K4 A2A UI and peers** | Keleustes M2: `kls-core/src/board.rs`, `kls-node/src/board.rs`, new A2A tab and page | `A2APeers` RPC, `keleustes:owner` sender, thread views | P1 |
| **S1 Security sign-off** | belai `AGENTS.md`, `docs/keleustes.md` | C6 rules, the grant-env exception, docs | before H2 merges |

Order: P1 ∥ H0 ∥ H3 → H1, H2 → H4, H5, K1, K2 → H6, K3, K4.

### D2. Test plan

**Fake harnesses.** `belai/internal/harnessrun/testdata/fakeharness/` is a
small Go program, built in `TestMain`, run as several profiles through a
`-dialect` flag. It:

- turns on bracketed paste (`ESC[?2004h`), draws a prompt line, and reads raw
  PTY input;
- on a pasted block followed by `\r`, appends a native `user` record, prints
  output, then appends an `assistant` record with turn end, in the chosen
  dialect: claude-like JSONL under a temp `HOME`, codex-like rollout (line 1
  `session_meta`), pi-like, a SQLite db (opencode schema subset, built with
  the `sqlite3` binary, skipped when it is absent), or a rewritten JSON doc
  (gemini-like);
- has switches for: no bracketed paste, a slow start (3 s), a torn last line,
  file rewrite or truncation, a permission screen ("1 Yes 2 No") that blocks
  until a key arrives, ignoring Esc, crashing mid-turn, echoing an env var (to
  test scrubbing), printing OSC 52 and OSC 8, and writing a session file only
  after 2 s (locate);
- calls a configured MCP command once (bridge test).

Unit tests:

- **H0**:
  - the profile schema, placeholders, `never_flags` and the `grant_env` deny
    pattern;
  - golden argv for each mode, including that the model pattern refuses
    `--foo`;
  - the generator's determinism.
- **H1**:
  - a fake PATH with a script `claude` that prints a version gives `launch`;
  - a dir only gives `configured`;
  - an exit-1 probe gives `broken`;
  - the login-shell PATH merge;
  - probe timeout.
- **H2** (with the fake harness):
  - the first prompt is pasted and submitted, and confirmation matches the
    native user entry;
  - a prompt that contains `\x1b[201~rm -rf ~\r` arrives as one pasted block
    with ESC stripped, and no extra submit;
  - multi-line with no paste is refused;
  - keys outside the allowlist are refused;
  - interrupt and stop escalation (Esc ignored → SIGTERM → SIGKILL), the exit
    reason, and process group cleanup (no orphans);
  - the env holds only allowed vars;
  - a granted value echoed on the PTY is masked across a chunk boundary.
- **H3**:
  - each dialect is tested against recorded fixtures. Real-shape fixtures
    come from `agentstore/testdata`. Add Claude sidechain, Codex `event_msg`,
    pi `custom`.
  - tailing covers appends, torn lines, truncation, rotation, rewrite-diff,
    the SQLite WAL, and a restart from the ledger offset that yields the same
    indexes;
  - a symlink out of root is refused.
- **H4**:
  - against the existing factorylink fakes (`factorylink/fake_test.go`):
    Upsert kind `harness`, idempotent Append after a restart, a queued
    prompt not acked while busy, an Answer refused, and a `key` command
    audited.
- **H6**:
  - an MCP `inbox` returns classified items, with a stub classifier refusing
    one;
  - blackout refuses `send`;
  - the notification line contains no peer text;
  - an envelope from `keleustes:owner` is classified.
- **P1 and K**:
  - `kls-node` tests: advert round-trip with harnesses; a dispatch with an
    unadvertised harness is still sent, and the node refuses it with a
    reason shown;
  - the Screen fan-out with pause when no viewer;
  - the `vt100` render drops OSC 52;
  - `A2APeers` excludes ended sessions and sanitizes titles;
  - UI: `new_session_spec_picks_harness` beside the existing
    `new_session_spec_picks_session_or_crew` (`sessions.rs:1484`).

**Rules tests (WORKSTREAMS rule 5), required in every WP that touches the
path:**

- **Provenance**:
  - a harness dispatch stores actor, node, harness, version and key source;
  - a typed prompt's native entry carries the inbox id and sender;
  - an envelope stores its sending surface;
  - the UI chip renders the origin.
- **Audit**:
  - each action writes a ledger entry, and so does each refusal: kill switch,
    blackout, key not allowlisted, classifier refusal, unadvertised harness;
  - `audit_verify` passes over a fake harness session's host stream;
  - prompt text appears only as a hash.
- **Kill switch**:
  - with the factory stopped, a harness dispatch, prompt, key and owner A2A
    send are refused, emitted and audited;
  - turning the kill switch on with a running fake harness ends it within
    15 s, with reason `factory stopped` and no orphan processes;
  - `stop` and `interrupt` still pass.
- **A2A**:
  - under blackout, bridge `send` is refused, `inbox` is empty, no notice line
    is typed, and owner send is refused;
  - every inbound envelope, including one from `keleustes:owner`, goes through
    KindPeer, and a stub-refused one never reaches the harness;
  - with `a2a.enabled` off, the bridge is not wired.

**Manual end-to-end (one real harness: Claude Code, T1, installed at
`~/.local/bin/claude`).**

1. Run `belai connect <token>` and then `belai rc --web-controls` on the
   desktop.
2. Keleustes Fleet shows `🤖 n` and lists Claude Code with its version and
   ✓ login.
3. New session → 🤖 Claude Code, the repo dir, Mode plan, Own login, prompt
   "List the crates in this workspace". ▶ Start.
4. Screen tab shows Claude's TUI with the prompt pasted.
   `~/.claude/projects/<slug>/<uuid>.jsonl` exists with the same uuid as
   `harness_session_id`.
5. Transcript tab fills from that file and roles look right. Send a second
   prompt from the composer, and it shows up in Claude's TUI, typed.
6. Ask for an edit in default mode. Session goes ⚠ waiting. Press `[1]` on the
   Screen tab, and the edit proceeds.
7. Interrupt mid-turn: Esc stops the turn. Stop: the session ends, and
   `claude` is gone from `ps`.
8. Resume from Keleustes: `claude --resume <uuid>`, and the transcript
   continues at the next index.
9. A2A: start a belai session on a second node. In Claude, ask it to call
   `belai.peers` and `belai.send`. The message appears in the other session's
   transcript as a classified peer message. Reply from there. Claude gets the
   notice line and reads the reply through `belai.inbox`.
10. Turn on the A2A blackout in Keleustes: `send` is refused, and both sides
    show it.
11. Stop the factory in Keleustes: the Claude process exits, the session ends
    with reason `factory stopped`, and the Audit screen shows the stop and
    every prompt, key and envelope from steps 3 to 10 with their origins.
    `keleustes audit verify` passes.

Repeat steps 3 to 7 with Codex (no pre-id, so it exercises locate) before
calling the T1 set done.

### D3. Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | **Keystroke injection through paste.** Prompt text holding `ESC[201~` or control keys could escape the paste and drive the TUI (approve, run commands). | Strip C0/C1 before writing. Keys are allowlisted and audited. There is a test for it (D2 H2). |
| R2 | **The TUI changes under us.** Prompt readiness, approval screens, submit key and multi-line handling vary by version. | Readiness and turn end come from native transcripts and hooks, not screen scraping. Profiles pin `min_version`. Confirmation-by-transcript turns a silent failure into a `failed` ack. The Screen view lets the owner recover by hand. |
| R3 | **Native formats are undocumented and change** (Claude adds types often; Codex and opencode changed storage before). | Dialects ignore unknown types (mapped to `system`) and are shared with agentstore, so one fix covers both. Fixture tests come from real files. On a parse failure the session falls back to `screen`. |
| R4 | **Locate ambiguity** for harnesses that cannot preset an id: two starts in one cwd. | A per-(harness, cwd) start lock with a 60 s ceiling, cwd plus mtime plus originator match, and a downgrade to `screen` on a miss. |
| R5 | **Vault key exposure** to the third-party harness, its tools, logs and `/proc`. | Own login is the default. The grant env allowlist, scrubber, expiry stop and UI disclosure (C6.3). It needs security sign-off. |
| R6 | **No belai sandbox or posture** around the harness's own tools. | The harness's own permission modes are mapped, never bypassed (`never_flags`). The UI says "harness rules". This is the same trust as the user running it in a terminal. |
| R7 | **Peer text reaches a model that has no belai classifier** after our pre-filter. | `KindPeer` before return, the untrusted wrapper, notice-only typing, and size caps. This is no worse than the harness's own WebFetch. |
| R8 | **"41" mismatch with the user's expectation.** | Report the finding (section 0). Detect all 84 and launch the verified tiers. Leave a tier filter if a fixed set is wanted. |
| R9 | **GUI and IDE harnesses** (Cursor, Windsurf, Zed, extensions) cannot take typed prompts. | Out of scope: "open in app" only. A later WP could use VS Code extension APIs per vendor. |
| R10 | **The daemon's PATH lacks user installs** (nvm, brew, `~/.local/bin`). | Login-shell PATH probe plus absolute paths. Detail shown for `broken`. |
| R11 | **Windows**: no creack/pty ConPTY parity, and no fd grant. | v1 is Linux and macOS. On Windows the advert sets `harness_sessions:false`. |
| R12 | **Terminal escape attacks on the Keleustes renderer.** | A VT parser into a grid, an OSC filter, no clipboard and no title. Bytes are never written to a real tty. |
| R13 | **Load**: PTY output volume. | Stream only while a viewer is attached. 32 KiB frames. 10 fps render. Ring buffer replay. |

## Status re-check (2026-11-22)

Read-only check of the current code in both repos. belai paths are prefixed
`belai/`; Keleustes paths are relative to this repo. "done" means code (and
tests where noted) exists at the cited lines.

| item | priority | status | evidence (file:line) | what's left |
|---|---|---|---|---|
| D1 H0 Profiles | P0 | done (P1, 2026-11-22) | belai/internal/harness/launch.go:96 (`min_version`), :186 (`Launchable`: `H`, or `M` after `CheckInstall`), :247 (`CheckInstall`), :297 (`VersionOK`); belai/internal/harnessdetect/detect.go:173,184 (install check, pin); belai/tools/gen-harnesses/launch.go:109 (`H` pins `min_version`, `grant_env` ⊂ tools.json `env`); launch.json pins on the 7 `H` profiles; tests launch_test.go, detect_test.go:168,188, gen-harnesses/launch_test.go | droid `FACTORY_API_KEY` and augment `AUGMENT_SESSION_AUTH` dropped from `grant_env` (tools.json records neither); belai/docs/harnesses.md does not yet describe the install check |
| D1 H1 Detect | P1 | done | belai/internal/harnessdetect/detect.go:121,237,294,320,357; belai/internal/rc/factoryharnessadvert.go:42-68; belai/internal/factorylink/host.go:38 | — |
| D1 P1 Proto | P0 | done | proto/keleustes/node/v1/node.proto:75,122,277,299,388-393,566-569,944-1040; crates/kls-node/src/server.rs:296,421; crates/kls-store/src/lib.rs:198,259; belai/internal/factorylink/nodev1/node_grpc.pb.go:548 | `buf breaking` not re-run in this check |
| D1 H2 PTY runner | P0 | done | belai/internal/harnessrun/session.go:173; pty_unix.go:32; input.go:37; runner.go:187,206,243; belai/cmd/belai/rcharness.go:89; belai/internal/rc/factoryharness.go:30,77; belai/internal/rc/factory.go:509 | start lock: see R4 |
| D1 H3 Tailers | P1 | done | belai/internal/harnesstail/tail.go:121,219,237,281,328,367 | P2: copilot, cursor-agent, auggie, crush, kiro, q, droid, gptme dialects not tailed |
| D1 H4 Mirror | P1 | done | belai/internal/factorylink/sessions.go:173,416; belai/internal/harnessrun/session.go:422,457,760,881 | tests: see D2 H4 |
| D1 H5 Hooks | P2 | partial | belai/internal/harnessrun/config.go:100-110; bridge.go:210; belai/cmd/belai/rcharness.go:211 | Codex `-c notify=` not wired (launch.json:26 has no `hooks`); `Notification` is an own line, not session `waiting`/`pending_question` |
| D1 H6 A2A bridge | P0 | done | belai/internal/harnessrun/bridge.go:140-200,253,296,341; belai/cmd/belai/rcharness.go:166,183; belai/internal/factorylink/a2a.go:311 | — |
| D1 K1 Dispatch and picker | P1 | done | crates/kls-core/src/gateway.rs:68-78; crates/kls-core/src/harness.rs:349; crates/kls-ui/src/screens/harness_picker.rs:24,74,150-230; crates/kls-app/src/cli.rs:64,97,354; crates/kls-node/src/gateway.rs:177,605 | CLI is `sessions harnesses`, not `nodes harnesses` |
| D1 K2 Transcript labels | P2 | done (P3, 2026-11-22) | crates/kls-core/src/harness.rs:592,621; crates/kls-ui/src/screens/sessions.rs:719 (Agent filter: belai or each harness seen), :2320 (`agent_choices`); test sessions.rs:3080 `harness_rows_tag_origin_and_agent` | none |
| D1 K3 Screen | P1 | done | crates/kls-node/src/screen.rs:44-54,94,128; crates/kls-ui/src/screens/harness_screen.rs:18,59,238; sessions.rs:1065; belai/internal/harnessrun/screen.go:27 | no explicit ≤10 fps cap |
| D1 K4 A2A UI and peers | P1 | done | crates/kls-core/src/harness.rs:469,516; crates/kls-node/src/board.rs:397; crates/kls-ui/src/screens/harness_a2a.rs:43,139; sessions.rs:1048 | — |
| D1 S1 Security sign-off | P0 | partial (P1, 2026-11-22) | belai/AGENTS.md:161-172 (grant exception), :177 ("Third-party harness sessions" bullet: C6.1, 6.2 with the classifier, profile checks, 6.4, 6.5, audit hashes; written as the sign-off) | a person's review of the AGENTS.md text; belai/docs/keleustes.md has no harness section |
| C1 Detection steps 1-5 | P1 | done | belai/internal/harnessdetect/detect.go:55-56,167,253,294,320; belai/internal/rc/factoryharnessadvert.go:49,68 | — |
| C2 HostAdvert.harnesses + Harness (incl. allow_keys, provenance) | P0 | done | node.proto:277,944-983 | — |
| C2 Capabilities.harness_sessions | P1 | done | node.proto:299; crates/kls-node/src/server.rs:393 | — |
| C2 Dispatch harness/mode/resume/keys | P0 | done | node.proto:388-391 | — |
| C2 Dispatch.origin_json | P1 | done | node.proto:393; crates/kls-node/tests/belai.rs:819 | — |
| C2 SessionUpsert harness fields | P1 | done | node.proto:566-569; crates/kls-node/src/sessions.rs:126 | numbered 26-29, not 22-25 |
| C2 Screen service | P1 | done | node.proto:122,1007-1040; crates/kls-node/src/server.rs:296 | — |
| C2 Board.A2APeers | P1 | done | node.proto:75,989-1005; crates/kls-node/src/board.rs:397 | — |
| C2 Store: HostInfo.harnesses, DispatchSpec, session columns | P1 | done | crates/kls-store/src/lib.rs:259; crates/kls-store/migrations/sqlite/0111_node_harnesses.sql, 0112_session_harness.sql (+postgres); crates/kls-core/src/gateway.rs:68 | — |
| C3 Launch block + generator rules | P1 | done (P1, 2026-11-22) | belai/internal/harness/launch.go:96,247,297; belai/tools/gen-harnesses/launch.go:26,109 | none |
| C4.1 Start checks | P1 | partial | belai/internal/rc/factoryharness.go:30-56,77-100; belai/cmd/belai/rcharness.go:98-116; belai/internal/harnessrun/session.go:177-200 | no per-(harness, cwd) start lock; no branch worktree for harness starts |
| C4.1 Env, PTY, ring, scrubber, readiness | P0 | done | belai/internal/harnessrun/config.go:122; pty_unix.go:32; runner.go:34,76,164; session.go:252 | — |
| C4.2 Session identity + ledger | P1 | partial | belai/internal/harnesstail/tail.go:44,141,191; belai/internal/harnessrun/session.go:402,422 | copilot locate missing; no start lock (R4) |
| C4.3 Prompt input | P0 | done (P1, 2026-11-22) | belai/internal/harnessrun/session.go:80 (`Config.Admit`, nil refuses), :844 (`admit`: dispatch, remote and answer prompts classified before typing; refusal acked, own line, audited by hash); belai/cmd/belai/rcharness.go:220 (`rcHarnessAdmit`: host classifier + posture via `Pipeline.Admit`); tests session_test.go:719,777 | an owner's answer is acked on receipt, so a classifier refusal of it shows in the transcript and audit, not the answer ack |
| C4.4 Transcript tailing | P1 | done | belai/internal/harnesstail/tail.go:237-400; belai/internal/harnessrun/session.go:457 | extra dialects (P2, see H3) |
| C4.5 Hooks | P2 | partial | see H5 | Codex notify |
| C4.6 Controls | P1 | partial | belai/internal/harnessrun/session.go:881-919; runner.go:232,243; belai/internal/harness/launch.go:164 | `mode`/`model` mid-session refused, not mapped to profile slash commands |
| C4.7 Resume | P1 | done | belai/internal/rc/factoryharness.go:47; belai/internal/harnessrun/session.go:189; crates/kls-ui/src/screens/harness_screen.rs:269,286; crates/kls-app/src/cli.rs:70 | — |
| C4.7 Teleport refusal | P2 | done | crates/kls-core/src/audit.rs:713,769 (refused, event + ledger entry); crates/kls-node/src/teleport.rs:258 (node-started upload); belai/internal/rc/factoryteleport.go:88,127; belai/cmd/belai/teleportcmd.go:174; tests: kls-core audit.rs `teleport_of_a_harness_session_is_refused_and_audited`, crates/kls-node/tests/teleport_needs.rs, belai/internal/rc/factoryteleport_needs_test.go | — |
| C5 A2A MCP bridge | P0 | done | belai/internal/harnessrun/bridge.go:22,79,154,161,174; session.go:52,822; belai/cmd/belai/rcharness.go:166,183 | — |
| C6.1 Output untrusted, VT + OSC filter | P0 | done | crates/kls-ui/src/screens/harness_screen.rs:18-59,335; belai/internal/harnessrun/grid.go | — |
| C6.2 Inputs to the harness | P0 | done | belai/internal/harnessrun/input.go:37,62; session.go:844; belai/internal/harness/launch.go:333; belai/internal/rc/factoryharness.go:50 | none (classifier: C4.3) |
| C6.3 Vault grant exception | P0 | done | belai/internal/harnessrun/keys.go:58; session.go:233-283; crates/kls-ui/src/screens/harness_picker.rs:172,204; belai/AGENTS.md:161 | sign-off (S1) |
| C6.4 Config isolation | P0 | done | belai/internal/harnessrun/session.go:205; config.go:100; belai/internal/harnesstail/tail.go:219,290; input_test.go:66 | — |
| C6.5 Sandboxing disclosure | P2 | done (P3, 2026-11-22) | crates/kls-ui/src/screens/sessions.rs:2821 (row meta `harness rules` with the shield icon, hover names the harness's own rules), :1159, :2339 (amber chip in the detail header); test sessions.rs:3080 | none |
| C6.6 Audit | P0 | done | belai/internal/harnessrun/session.go:388-396; belai/cmd/belai/rcharness.go:127-133; crates/kls-core/src/harness.rs:283 | — |
| C7.1 New session dialog | P1 | done | crates/kls-ui/src/screens/harness_picker.rs:24,74-127,150-230; sessions.rs:177 | — |
| C7.2 Session detail | P1 | done (P3, 2026-11-22) | crates/kls-ui/src/screens/sessions.rs:1826 (origin chip on typed entries: dispatch, console, answer, A2A), :2350 (`origin_chip`), :2365 (`source_hover`: native type, id, byte offset, or belai line kind and prompt id), entry detail chip; harness_screen.rs:238; test sessions.rs:3080 | none |
| C7.3 Fleet chip | P2 | done (P6, 2026-11-22) | crates/kls-ui/src/fleet.rs:249 (hover per harness from `probe_provenance`); crates/kls-core/src/belai/fleet.rs:698 | — |
| C8 Rule 1 Provenance | P1 | done (P6, 2026-11-22) | crates/kls-core/src/harness.rs:356,408; node.proto:393; belai/internal/harnessrun/session.go:457; crates/kls-ui/src/screens/sessions.rs:2350, :2365 (P3: origin chip and native hover); crates/kls-core/src/belai/fleet.rs:698 (`probe_provenance`: absolute path, version-output hash, probe time; test :727), crates/kls-ui/src/fleet.rs:249 (Fleet chip hover) | — |
| C8 Rule 2 Audit | P1 | partial (P1, 2026-11-22) | crates/kls-core/src/harness.rs:36-42 (kinds), :606 (`line_event`: host line → `harness.prompt.queued/typed/failed/refused`, `harness.stopped` with cause; ids and hashes only), :649 (`Core::harness_lines`); belai/internal/harnessrun/session.go:317,917 (`prompt_queued` facts); test harness.rs:1126 | nothing calls `harness_lines` yet: needs one call in `Core::record_audit` (kls-core/src/sessions.rs, not P1's file) for the lines the store took as new; requested in requests-governance.md. The ledger row stays the folded host line (kind `host`) |
| C8 Rule 3 Kill switch | P0 | done | crates/kls-core/src/harness.rs:357,431; crates/kls-core/src/sessions.rs:481,532; belai/internal/harnessrun/session.go:587,760,857,909; bridge.go:195 | — |
| C8 Rule 4 A2A | P0 | done | crates/kls-core/src/harness.rs:474; belai/internal/harnessrun/bridge.go:79,154,174; belai/cmd/belai/rcharness.go:166; crates/kls-node/tests/belai.rs:824 | — |
| D2 H0 tests | P1 | done | belai/internal/harness/launch_test.go:9-196; harness_test.go:466 | — |
| D2 H1 tests | P1 | done | belai/internal/harnessdetect/detect_test.go:39-165 | — |
| D2 H2 tests (fake harness) | P0 | done | belai/internal/harnessrun/session_test.go:211,236,249,279,348,371; input_test.go:20,79 | — |
| D2 H3 tests | P1 | done | belai/internal/harnesstail/tail_test.go:64-219 | — |
| D2 H4 tests | P1 | partial | belai/internal/factorylink/sessions_test.go:438 (end facts only) | Upsert kind `harness`; idempotent Append after restart; queued prompt not acked while busy |
| D2 H6 tests | P0 | partial | belai/internal/harnessrun/session_test.go:427,471 | stub classifier refusing an inbound envelope on the harness path; owner envelope classified |
| D2 P1/K tests | P1 | partial | crates/kls-node/tests/belai.rs:784; harness_screen.rs:335; harness_picker.rs:421; crates/kls-core/src/harness.rs:890-965 | Screen fan-out/pause test; A2APeers excludes ended and sanitizes titles; node refusal of an unadvertised harness shown in Keleustes |
| D2 Fake harness dialects | P2 | partial | belai/docs/harness-sessions.md:150 | writes Claude-shaped JSONL only; no codex/pi/sqlite/gemini modes, torn line, OSC 52/8 or late-file switches |
| D2 Rules tests | P0 | partial (P1, 2026-11-22) | crates/kls-core/src/harness.rs:1126 (`audit_verify` over a harness host stream, refusal in the ledger, events hash-only); belai/internal/harnessrun/session_test.go:719 (classifier refusal audited, audit facts hold hashes only), :777 (no classifier refuses), :785 (version pin) | `a2a.enabled` off leaves the bridge unwired |
| D2 Manual E2E (Claude, Codex) | P1 | n-a | — | manual; no recorded run in either repo |
| R1 Paste injection | P0 | done | belai/internal/harnessrun/input.go:37; session_test.go:236; belai/internal/harness/launch.go:164; crates/kls-core/src/harness.rs:423 | — |
| R2 TUI drift | P1 | done (P1, 2026-11-22) | belai/internal/harnessrun/session.go:192 (start refused below the pin); belai/internal/harnessdetect/detect.go:184; belai/internal/harness/launch.go:297 | none |
| R3 Format drift | P1 | partial | belai/internal/harnesstail/tail.go:282 (agentstore parsers); belai/internal/harnessrun/session.go:621 | downgrade to `screen` only on a locate miss, not on a parse failure |
| R4 Locate ambiguity | P1 | partial | belai/internal/harnesstail/tail.go:44,191 | per-(harness, cwd) start lock |
| R5 Vault key exposure | P0 | done | belai/internal/harnessrun/keys.go:58; session.go:233-283; runner.go:76; crates/kls-ui/src/screens/harness_picker.rs:172,204 | sign-off (S1) |
| R6 No belai sandbox | P1 | done (P3, 2026-11-22) | belai/internal/harness/launch.go:301,333; crates/kls-ui/src/screens/sessions.rs:2821, :2339 (`harness rules` tag on row and detail) | none |
| R7 Peer text to model | P0 | done | belai/internal/harnessrun/bridge.go:161,253; session.go:52 | — |
| R8 "41" mismatch | P2 | done | §0; belai/internal/harnessdetect/detect.go:121 | — |
| R9 GUI/IDE harnesses | P2 | done | belai/internal/harness/launch.go:18; belai/internal/harnessdetect/detect.go:48 | — |
| R10 Daemon PATH | P1 | done | belai/internal/harnessdetect/detect.go:167,320,359 | — |
| R11 Windows | P1 | done | belai/internal/harnessrun/pty_windows.go:14; belai/internal/rc/factoryharness.go:31 | — |
| R12 Terminal escapes in renderer | P0 | done | crates/kls-ui/src/screens/harness_screen.rs:18,335 | — |
| R13 PTY load | P2 | done | belai/internal/harnessrun/screen.go:21,99-132; runner.go:34; crates/kls-node/src/screen.rs:128 | no fps cap |

Counts: done 47, partial 20, missing 0, n-a 1 (68 items; P3 2026-11-22 moved K2, C6.5, C7.2 and R6 to done).
