# Library search: discover and import from public sources

Date: 2026-11-21. Every endpoint below was probed that day without credentials, except where a row says `gh`.

**Goal.** For each library type, search the public places where agent profiles, skills, commands, hooks, MCP servers and rules are published (Claude Code, Codex, Hermes, OpenClaw, Gemini CLI, Cursor, Cline/Roo, Copilot, MCP registry). Preview the result, convert it to the Keleustes/belai format, review it and import it with provenance. Search must work with **no credentials**. A signed-in local `gh` is an optional boost.

**Today.**
- `kls_core::vault::LIBRARY_KINDS = [prompt, skill, command, process]` (`crates/kls-core/src/vault.rs:65`).
- Agents live in `agent_profiles` (`crates/kls-store/src/vault.rs:154`).
- Import reads local `.md` files only (`import_library`, `vault.rs:1485`). Its front matter is `kind, name, description`, with no validation beyond the name rule.
- There is no `hook` kind, no `mcp` kind and no file bundle. The workspace has no HTTP client: GitHub goes through `gh` (`kls-github`).

**Specs this follows.**
- `vdb-site/docs/belai-library-items.md`: kinds, limits, canonical form and hash, the Skill, Command, Prompt, Hook and MCP rules, and the scan report's `notes` vocabulary.
- `belai/internal/agentimport` (read only): adapters for Claws, Nemoclaw, Hermes, mini-SWE and harness Markdown. Each foreign definition becomes a single-mode, supervised profile. Tools come only from a fixed table, and the fallback is read-only.
- `belai/internal/harness/harnesses.json`: 81 harnesses, each with a per-kind dir and a shape (`md-file`, `skill-dir`, `json-key`, `json-file`, `doc-file`).

---

## 1. Searchable types

| Library type | Keleustes store today | Searchable | Import target |
|---|---|---|---|
| agent | `agent_profiles` | yes | `vault::save_agent` (profile Markdown → `AgentProfile`) |
| skill | `library_items` kind `skill` | yes | belai Skill rules, 32 KiB, single `SKILL.md` |
| command | kind `command` | yes | belai Command rules, 32 KiB |
| hook | **missing** (WP0) | yes | belai Hook bundle: JSON doc of 16 KiB or less, plus up to 32 files of 256 KiB, 2 MiB in all |
| mcp | **missing** (WP0) | yes | belai MCP rules, 16 KiB, no literal secrets |
| prompt | kind `prompt` | yes (rules, AGENTS.md-style docs) | belai Prompt rules: `name, description, order, enabled` |
| process, crew, repo, budget, provider, rewrite | partial | no | No public ecosystem publishes these. Out of scope. |

---

## 2. Source catalogue

Legend:
- **Auth**: ∅ means none needed. `gh` means it needs a token.
- **Mode**: `I` means indexed (crawled into the local FTS index). `R` means remote (queried live and merged in).
- Limits are per IP unless marked otherwise.

### 2.1 Registries and static indexes

| # | Source | Types | Endpoint | Auth | Limits / caching | Response | Licence metadata | Change detection | Mode |
|---|---|---|---|---|---|---|---|---|---|
| S1 | MCP Registry (official) | mcp | `GET https://registry.modelcontextprotocol.io/v0.1/servers?search=&version=latest&limit=&cursor=&updated_since=`; also `/v0.1/servers/{name}/versions[/{v}]`; spec at `/openapi.yaml` (`/v0` also live) | ∅ | None documented; `x-registry-cache: HIT` | `{servers:[{server: server.json (schema 2025-12-11), _meta:{"io.modelcontextprotocol.registry/official":{status,isLatest,updatedAt}}}], metadata:{nextCursor,count}}` | none (only `repository.url`) | `updated_since` incremental, `isLatest` | I + R |
| S2 | Hermes skills index | skill | `GET https://nousresearch.github.io/hermes-agent/docs/api/skills-index.json` (the `hermes-agent.nousresearch.com/docs/api/…` URL redirects here with a 301) | ∅ | GitHub Pages: `max-age=600`, ETag, Last-Modified. 45 MB raw / 11.4 MB gzip | `{version:1, generated_at, skill_count:104593, skills:[{name,description,source(official\|github\|clawhub\|skills-sh\|lobehub),identifier,trust_level,repo,path,tags,extra}]}` | none (fetch `SKILL.md`) | ETag; `generated_at` | I (opt-in, streamed) |
| S3 | ClawHub (OpenClaw) | skill (also packages, plugins) | `GET https://clawhub.ai/api/v1/search?q=&limit=&nonSuspiciousOnly=true`; `/skills?search=`; `/skills/{slug}?owner=`; `/skills/{slug}/file?path=SKILL.md&owner=`; `/skills/{slug}/versions`; `/download?slug=&version=` (ZIP); `/skills/{slug}/scan`; spec at `/api/v1/openapi.json` | ∅ for reads | read 3000/min, download 1200/min (per IP); 429 + `Retry-After`; `cache-control: no-store` | `{results:[{id,displayName,canonicalUrl,downloads,install:{kind,reference:"owner/slug"},native:{skill:{slug,summary,tags,stats,isSuspicious,badges},owner}}]}`. A bare slug can return **409 `AMBIGUOUS_SKILL_SLUG`** with `matches[]`; always send `owner` | the `SKILL.md` `license:` key (seen: `Proprietary` on a re-upload of an Anthropic skill) | `latestVersionId`, `/resolve?slug=&hash=` | R |
| S4 | skills.sh (Vercel) | skill | `GET https://skills.sh/api/search?q=&limit=` (undocumented, Algolia-backed). The documented `/api/v1/skills/search` returns **401** (needs a bearer token) | ∅ (legacy path) | `timings_ms.rate_limit` shows a server-side limiter; 429/503 documented | `{skills:[{id:"owner/repo/skill",source:"owner/repo",skillId,name,installs}]}`. Content comes from GitHub at `skills/`, `.agents/skills/` or `.claude/skills/` | GitHub | GitHub ref | R (fragile: feature-flag it) |
| S5 | Gemini CLI gallery | skill, command, hook, mcp, prompt (context) | `GET https://geminicli.com/extensions.json` | ∅ | `max-age=3600`, ETag, Last-Modified; 1.8 MB | `[{id,rank,url,fullName,stars,lastUpdated,extensionName,extensionVersion,extensionDescription,hasMCP,hasSkills,hasHooks,hasCustomCommands,hasContext,isGoogleOwned,licenseKey}]`. 2198 entries: 1357 have skills, 1069 MCP, 358 hooks | `licenseKey` (e.g. `mit`) | ETag; `extensionVersion` | I (catalogue), then repo crawl on demand |
| S6 | aitmpl / claude-code-templates | agent, command, hook, mcp, skill (+settings) | `GET https://www.aitmpl.com/components.json`; content at `raw…/davila7/claude-code-templates/main/cli-tool/components/<type>s/<path>` | ∅ | `max-age=300, stale-while-revalidate=3600`, ETag; 2.5 MB | `{agents[423],commands[288],hooks[64],mcps[143],skills[945],settings[86],…}`, each `{name,path,category,type,description,author,repo,version,license,downloads,skillspector:{score,severity,recommendation,findings}}` | per entry (often empty) → repo MIT | ETag | I |
| S7 | LobeHub agents | agent | `GET https://chat-agents.lobehub.com/index.json`, `/{identifier}.json` | ∅ | static | `{schemaVersion,agents[505]:{identifier,meta:{title,description,tags,avatar,category},author,homepage}}`; detail has `config.systemRole` | repo `lobehub/lobe-chat-agents` MIT | index ETag | I |
| S8 | Cline MCP marketplace | mcp (discovery only) | `GET https://api.cline.bot/v1/mcp/marketplace` | ∅ | — | `[{mcpId,githubUrl,name,author,description,…}]`, no server config | GitHub | — | I (shows link + "README setup") |
| S9 | `/.well-known/skills/index.json` | skill | `GET https://<site>/.well-known/skills/index.json` (e.g. `https://www.mintlify.com/docs/.well-known/skills/index.json`) | ∅ | site | `{skills:[{name,description,…}]}` | per site | ETag | I (user-added) |
| S10 | npm registry | mcp (package facts) | `GET https://registry.npmjs.org/{pkg}`, `/-/v1/search?text=` | ∅ | soft | package JSON: `license`, `dist-tags.latest`, `repository` | `license` | version | lookup |
| S11 | PyPI JSON | mcp (package facts) | `GET https://pypi.org/pypi/{pkg}/json` | ∅ | soft | `info.license`, `info.version` | `license` / classifiers | version | lookup |

### 2.2 Plugin marketplaces (one JSON file in a GitHub repo, fetched raw)

| # | Source | Types | File | Size / entries | Entry shape | Licence |
|---|---|---|---|---|---|---|
| M1 | `anthropics/claude-plugins-official` (Apache-2.0) | agent, command, skill, hook, mcp | `.claude-plugin/marketplace.json` | 317 plugins | `{name,description,category,author,homepage,source}`. `source` is a `"./plugins/x"` relative path (55), `{source:"url",url,sha}` (163), `{source:"git-subdir",url,path,ref,sha}` (98) or **`{source:"command",command,…}` (1). Refuse that one: it runs a program** | plugin `LICENSE`, `plugin.json` |
| M2 | `anthropics/skills` (no root licence) | skill | `.claude-plugin/marketplace.json` + `skills/*/SKILL.md` (19) | — | `{name,source:"./",skills:["./skills/pdf",…]}` | per-skill `LICENSE.txt`. The docx/pdf/pptx/xlsx skills are **proprietary** |
| M3 | `openai/plugins` (no root licence) | skill, mcp | `.agents/plugins/marketplace.json` | 65 | `{name,source:{source:"local",path},policy:{installation,authentication},category}`; per plugin `.codex-plugin/plugin.json`, `.app.json`, `skills/*/SKILL.md` | per plugin |
| M4 | `wshobson/agents` (MIT) | agent 199, command 103, skill ≈150 | `.claude-plugin/`, `.agents/plugins/` and `.cursor-plugin/marketplace.json` (all three dialects) | 91 / 52 | as M1 / M3 (+`license` per entry) | MIT |
| M5 | any repo with `.claude-plugin/marketplace.json`, `.agents/plugins/marketplace.json` or `.cursor-plugin/marketplace.json` | all | same | — | same | per repo |

### 2.3 Curated GitHub repos (crawled by tarball, §4.2)

| # | Repo | Licence | Stars | Types → paths |
|---|---|---|---|---|
| G1 | `openai/skills` | none at root; per-skill `LICENSE.txt` | 28.9k | skill: `skills/.curated/*/SKILL.md` (39), `skills/.system/*/SKILL.md` (5); `agents/openai.yaml` (Codex UI metadata) |
| G2 | `NousResearch/hermes-agent` | MIT | 265k | skill: `optional-skills/<cat>/<skill>/SKILL.md`, `skills/<cat>/…`. **The repo is 1.5 GB: never tarball it**; use S2 + raw |
| G3 | `VoltAgent/awesome-claude-code-subagents` | MIT | 26.7k | agent: `categories/*/*.md` (minus `README.md`) |
| G4 | `obra/superpowers` | MIT | 323k | skill `skills/*/SKILL.md`, command, agent, hook `hooks/hooks.json` (also Gemini gallery #1) |
| G5 | `github/awesome-copilot` | MIT | 41.4k | agent `agents/*.agent.md` (229); prompt `instructions/*.instructions.md` (195); skill `skills/*/SKILL.md`; hook `hooks/<name>/{hooks.json,*.sh}` (8, Copilot dialect) |
| G6 | `disler/claude-code-hooks-mastery` | **none** | 4.0k | hook `.claude/settings.json#hooks` + `.claude/hooks/**`; agent `.claude/agents/**.md`; command `.claude/commands/*.md` |
| G7 | `PatrickJS/awesome-cursorrules` | CC0-1.0 | 41.2k | prompt `rules/*/.cursorrules` |
| G8 | `sanjeed5/awesome-cursor-rules-mdc` | CC0-1.0 | 3.6k | prompt `rules-mdc/*.mdc` |
| G9 | `VoltAgent/awesome-agent-skills` | MIT | 37.4k | discovery list (repo links) |
| G10 | `hesreallyhim/awesome-claude-code` | NOASSERTION | 58.5k | discovery: `THE_RESOURCES_TABLE_NEW.csv` (`ID,Display Name,Category,Sub-Category,Link,Author Name,Author Link,Active,Date Added,Last Checked,Description,Stale`) → candidate repos |
| G11 | `davila7/claude-code-templates` | MIT | 34k | via S6 (`cli-tool/components/{agents,commands,hooks,mcps,settings,skills}`) |
| G12 | `cline/prompts` | none | 1.2k | prompt `.clinerules` files |
| G13 | `gemini-cli-extensions/*`, `google-gemini/gemini-cli` | Apache-2.0 | — | via S5 |
| G14 | `openclaw/openclaw` | MIT | 395k | 16 GB: never tarball. Bundled skills via S2/S3 |
| Gx | user taps `owner/repo[@ref]` + globs | per repo | — | generic harness-path detector (§4.3) |

### 2.4 GitHub transport, no credentials

| Channel | URL | Counts against REST 60/h? | Notes |
|---|---|---|---|
| REST core | `api.github.com/…` | **yes, 60/h** (`x-ratelimit-resource: core`) | Use only for `gh` mode or one-off licence lookups. A conditional request that gets 304 is free |
| REST search: repos | `api.github.com/search/repositories?q=topic:claude-skills` | separate, **10/min** | Works without auth (17k results for that topic) |
| REST search: code | `api.github.com/search/code?q=filename:SKILL.md` | — | **401 without auth.** `gh` only |
| smart HTTP refs | `GET https://github.com/{o}/{r}.git/info/refs?service=git-upload-pack` (pkt-line) | no | Does `ls-remote` without git: HEAD and refs → commit SHA |
| codeload tarball | `https://codeload.github.com/{o}/{r}/tar.gz/{sha}` | no | Immutable by SHA, ETag; streamed and filtered |
| raw | `https://raw.githubusercontent.com/{o}/{r}/{sha}/{path}` | no | `max-age=300`, ETag |
| all of the above | — | — | Since 2025-05, GitHub applies undocumented limits to unauthenticated raw/git traffic. Treat 429 and 403 as backoff signals and honour `Retry-After` |

### 2.5 Checked and excluded

| Source | Result 2026-11-21 | Reason |
|---|---|---|
| Glama `glama.ai/api/mcp/v1/servers` | 401: needs an API key, plus a data licence | not credential-free |
| Smithery `registry.smithery.ai/servers` | 500 / key-gated | not credential-free |
| PulseMCP `api.pulsemcp.com/v0beta` | 410 `API_SUNSET` | sunset |
| cursor.directory API | 429 Vercel Security Checkpoint (bot wall) | not usable from a client. Use `cursor/community-plugins` and G7/G8 |
| Roo marketplace `app.roocode.com/api/marketplace/*` | DNS fails | gone. `.roomodes` and `.roo/commands` come from repos instead (Gx) |
| skills.sh `/api/v1/*` | 401 | bearer token. S4 legacy path only |
| GitHub code search without auth | 401 | `gh` only (WP8) |

---

## 3. Format mapping

Rules that apply to every type:
- **Name.** Keleustes: `^[a-z0-9._:-]{1,64}$` (`vault::valid_name`). belai skill and command: `^[a-z0-9][a-z0-9._-]{0,63}$`. Slug every source name with `slugify`, and keep the original name in notes.
- **Canonical form and hash.** belai `belaiCanonicalMarkdown` / `belaiCanonicalJSON`, sha256 lower hex. A hook uses `HashBundle`. Pin belai's golden vectors in tests (WP0) so the hash Keleustes records equals the hash a host advertises.
- **Notes.** Use belai's scan vocabulary: `mapped`, `metadata`, `dropped`, `warning`. Keep at most 8 per item shown and the full list in provenance.
- **Text.** Strip control characters, bidi and zero-width characters, and flag them (§6). CRLF becomes LF.

### 3.1 skill → belai Skill (`name, description, license, compatibility, metadata, allowed-tools, disable-model-invocation`)

| Ecosystem | Source file | Mapping | Dropped / notes |
|---|---|---|---|
| Agent Skills spec (agentskills.io) | `<dir>/SKILL.md` | 1:1 | `description` over 300 B is cut at a char boundary, with a `warning` |
| Claude Code | `skills/*/SKILL.md`, plugin `skills/` | 1:1 + `allowed-tools` (string or list) → one-line list | `when_to_use` is appended to the description when it fits, else `metadata`. `argument-hint`, `user-invocable`, `model`, `context`, `agent`, `hooks`, `paths` are `dropped` (hooks are never carried inside a skill) |
| Codex | `skills/.curated/*/SKILL.md` + `agents/openai.yaml` | 1:1; `interface.display_name` / `short_description` → `metadata` | `icon_*`, `default_prompt` dropped |
| Hermes | `SKILL.md` | `version, author, platforms, metadata.hermes.{tags,category}` → one-line `metadata` (1024 B max) | `required_environment_variables`, `requires_*`, `fallback_for_*`, `config` → `metadata` note, never env |
| OpenClaw / ClawHub | `SKILL.md` | `metadata` (inline JSON `{"openclaw":{…}}`) → one-line `metadata`; `homepage` → provenance | `requires.bins/env` → `warning` note |
| Gemini CLI ext | `skills/*/SKILL.md` | 1:1 | — |
| Copilot | `skills/*/SKILL.md` | 1:1 | — |
| LobeHub | — | n/a (agents only) | — |
| **Supporting files** | `scripts/`, `references/`, `assets/` | **not carried** (a belai skill is one document) | `dropped` with the count. A `warning` when the body links `scripts/…` or `references/…` |

### 3.2 command → belai Command (`name, description(req, ≤1024), argument-hint, allowed-tools, license, compatibility, metadata, disable-model-invocation`)

| Ecosystem | Source | Mapping | Dropped / flags |
|---|---|---|---|
| Claude Code | `commands/*.md`, `.claude/commands/**.md` | Name from the path (`a/b.md` → `a:b`). Front matter 1:1. `$ARGUMENTS`, `$1` kept | `model` dropped. **Flags:** ``!`cmd` `` (pre-exec shell), `@path` |
| Codex prompts | `~/.codex/prompts/*.md` | `description, argument-hint` 1:1; `$1..$9`, `$ARGUMENTS`, `$NAMED` kept | — |
| Gemini CLI | `commands/**/*.toml` (`description`, `prompt`) | TOML → Markdown: `prompt` becomes the body; `{{args}}` → `$ARGUMENTS`; `gcs/sync.toml` → `gcs:sync` | **Flag** `!{…}` (shell) and `@{…}` (file) and keep them verbatim. belai marks Gemini commands `unsupported`, so this converter is new in Keleustes |
| Cursor | `.cursor/commands/*.md` (plain) | description = first heading or line (≤1024) | — |
| Cline | `.clinerules/workflows/*.md` | as Cursor | — |
| Roo | `.roo/commands/*.md` (`description, argument-hint`) | 1:1 | — |
| Windsurf | `.windsurf/workflows/*.md` | as Cursor | — |
| Copilot | `*.prompt.md` (`description, mode/agent, model, tools`) | `description`; `tools` → `allowed-tools` (mapped names) | `mode`, `model` dropped |
| opencode | `.opencode/command/*.md` (`description, agent, model`) | `description` | `agent`, `model` dropped |
| Hermes, OpenClaw | — | n/a (skills serve as slash commands) | — |

### 3.3 agent → `AgentProfile` / belai profile Markdown (single mode, supervised)

Tool table: use the belai `agentimport` fixed table (`Read Grep Glob LS Edit Write MultiEdit Bash WebFetch WebSearch Task TodoWrite …`). An unknown tool is dropped. If no known tool is left, the profile gets the read-only set. Never carry `permissionMode`, `hooks`, `mcpServers`, endpoints or env names.

| Ecosystem | Source | name / description | prompt | tools | model / provider | Dropped |
|---|---|---|---|---|---|---|
| Claude Code | `agents/*.md` (`name, description, tools, model, color, permissionMode, skills, …`) | 1:1 | body | comma string or list → table | `sonnet\|opus\|haiku` → provider `anthropic`, model blank (routing picks), alias in `meta_json`; `inherit` → blank | `color`, `permissionMode` (warning), `hooks`, `mcpServers`, `memory` |
| Copilot | `*.agent.md` (`name, description, tools, model`) | `name` slugged → `display_name` | body | mapped | `meta_json` | — |
| opencode | `.opencode/agent/*.md` (`description, mode, model, temperature, tools:{write:false}, permission`) | file stem | body | map `true` keys | `meta_json` | `permission`, `temperature` |
| Gemini CLI | ext `agents/*.md` (preview) | 1:1 | body | mapped | `meta_json` | — |
| Roo | `.roomodes` YAML/JSON `customModes[]` (`slug,name,roleDefinition,whenToUse,customInstructions,groups`) | `slug` / `whenToUse` | `roleDefinition` + `\n\n` + `customInstructions` | groups: `read` → Read/Grep/Glob; `edit` → Edit/Write; `command` → Bash; `browser` → WebFetch; `mcp` dropped | — | `fileRegex` (note) |
| Hermes | profile dir / `.tar.gz` (`SOUL.md`, `profile.yaml`, `config.yaml`, `skills/`) | `profile.yaml` | `SOUL.md` | belai table | — | **convert on a node** (WP9: belai `agentimport` Hermes adapter); Rust fallback reads the same files (`config.yaml` model, `distribution.yaml` version, `skills/`, `.tar.gz` exports); `mcp.json`, `cron/`, `memories/` and credential files are named only |
| OpenClaw / Claws | `CLAW.md` (+ `SOUL.md`, sidecars) | `agent.id` / `agent.name` | body or `SOUL.md` | table | — | MCP (by name) and cron entries named in notes only; `packages`, `subagents`, `memory`, `workspace` dropped. Node convert (WP9) or the Rust port |
| LobeHub | `{id}.json` | `identifier` / `meta.description`; `meta.title` → `display_name` | `config.systemRole` | read-only set | — | `avatar` emoji → note |
| Codex, Cursor, Cline | — | n/a (no agent file; AGENTS.md is a prompt, §3.6) | — | — | — | — |
| Bundled skills | plugin `skills/`, Hermes `skills/` | import alongside as skill items, only where no skill of that name exists (belai `agent` import rule) | — | — | — | — |

### 3.4 hook → belai Hook bundle (`{name, description, hooks:{Event:[{matcher?, hooks:[{type:"command", command, timeout?}]}]}}` + files)

The allowed belai events are `PreToolUse PostToolUse UserPromptSubmit SessionStart SessionEnd Stop SubagentStop Notification PreCompact`. Handlers are `type: command` only: one line of 512 B or less, with a timeout of 1–600 s.

| Ecosystem | Source | Event map | Handler map | Files | Compatibility |
|---|---|---|---|---|---|
| Claude Code | settings `hooks` key, plugin `hooks/hooks.json`, aitmpl `{description, hooks}` | 1:1. `PostToolUseFailure`, `PermissionRequest`, `SubagentStart` and other events beyond the 9 are `dropped` | `command` 1:1. `prompt`, `agent`, `http` and `mcp_tool` handlers are `dropped` | `${CLAUDE_PLUGIN_ROOT}/x` and `$CLAUDE_PROJECT_DIR/.claude/hooks/x` → bundle path `x`, command rewritten | **native** |
| Codex | `hooks.json`, plugin `hooks/hooks.json` | 1:1 for shared events. `PermissionRequest`, `Pre/PostModelRequest`, `PostCompact`, `SubagentStart`, `Interrupt` dropped | `command`; `statusMessage`, `async`, `additionalContextLimit`, `commandWindows` dropped; `mcp_tool` dropped | as Claude (Codex sets `CLAUDE_PLUGIN_ROOT` too) | **native** |
| Gemini CLI | ext `hooks/hooks.json` | `BeforeTool` → PreToolUse, `AfterTool` → PostToolUse, `BeforeAgent` → UserPromptSubmit, `AfterAgent` → Stop, `SessionStart`/`SessionEnd` 1:1, `PreCompress` → PreCompact, `Notification` 1:1; `BeforeModel`/`AfterModel`/`BeforeToolSelection` dropped | `command` | `${extensionPath}` → bundle root | **converted: stdin/stdout protocol differs** (flag `ProtocolMismatch`) |
| Copilot | `hooks/<n>/hooks.json` `{version:1, hooks:{sessionStart:[{type,bash,powershell,cwd,env,timeoutSec}]}}` | camelCase → Pascal (`preToolUse` → PreToolUse, `userPromptSubmitted` → UserPromptSubmit, `sessionEnd` → SessionEnd); `errorOccurred` dropped | `bash` → `command`, `timeoutSec` → `timeout`; `powershell`, `cwd`, `env` dropped (warning) | `.github/hooks/<n>/x.sh` → `x.sh` | converted, `ProtocolMismatch` |
| Cursor | `.cursor/hooks.json` `{version:1, hooks:{beforeShellExecution:[{command}]}}` | `beforeShellExecution` → PreToolUse `Bash`; `beforeReadFile` → PreToolUse `Read`; `afterFileEdit` → PostToolUse `Edit\|Write`; `beforeSubmitPrompt` → UserPromptSubmit; `stop` → Stop; `beforeMCPExecution` dropped | `command` | relative | converted, `ProtocolMismatch` |
| Hermes, OpenClaw | plugin / `HOOK.md` + `handler.ts` | — | — | — | n/a (in-process code, not a shell hook) |

A program named by its first word (`uv`, `python3`, `node`) stays as written, with a `warning` that it needs `hooks.allowed_programs` on the host. A script that is a link, binary, over the size limit or holds a secret makes the item invalid (belai rule).

### 3.5 mcp → belai MCP (`name ≤32 [A-Za-z0-9_-], transport stdio|http, command,args,env | url,headers, tools?, timeout_ms?`)

| Ecosystem | Source | Mapping |
|---|---|---|
| MCP Registry | `server.json` | `name`: last segment of `io.github.x/y`, slugged, 32 max (full name kept in provenance). `packages[registryType=npm]` → `command:"npx", args:["-y","<id>@<version>", …packageArguments]`. `pypi` → `uvx`, `["<id>==<version>"]`. `oci` → `docker`, `["run","-i","--rm","<image>"]`. `nuget`/`mcpb` → unsupported (note). `remotes[streamable-http\|sse]` → `transport:"http"`, `url` (https only). `environmentVariables[]` / `remotes[].headers[]`: when `isSecret` or the name looks secret (`(?i)secret\|token\|password\|passwd\|credential\|private\|key\|auth\|cookie`) → `"cred:NAME"`; else `default`, or omitted with an `isRequired` note. Versions are always pinned |
| Claude / Codex plugin | `.mcp.json` `{mcpServers:{n:{command,args,env}\|{type:"http",url,headers}}}` | 1:1 + the secret rule. `${CLAUDE_PLUGIN_ROOT}` → refuse (needs the plugin's files) |
| Gemini CLI ext | `gemini-extension.json` `mcpServers` | 1:1; `${extensionPath}` → refuse; `trust` never carried; `settings[].sensitive` → `cred:` |
| aitmpl | `mcps/*.json` `{mcpServers:{…}}` | as Claude |
| Cline marketplace | `githubUrl` only | no config → "Open README" + manual draft |
| npm / PyPI (S10/S11) | package JSON | licence and latest version for the preview only |

### 3.6 prompt (rules and instruction docs) → belai Prompt (`name, description?, order 0–999, enabled`)

| Ecosystem | Source | Mapping | Dropped |
|---|---|---|---|
| Cursor | `.cursor/rules/*.mdc` (`description, globs, alwaysApply`), legacy `.cursorrules` | body; `description`; `alwaysApply:false` → `enabled:false` | `globs` → `dropped` (belai prompts are global) |
| Copilot | `*.instructions.md` (`applyTo`), `copilot-instructions.md` | body | `applyTo` dropped |
| Claude / Codex / Gemini / AGENTS.md family | `CLAUDE.md`, `AGENTS.md`, `GEMINI.md` | body; name from repo + file | — |
| Cline / Roo / Windsurf | `.clinerules`, `.roorules`, `.windsurfrules` | body | — |

---

## 4. Architecture

### 4.1 Crates and ownership

```
crates/kls-libsearch/            NEW (pure + IO; no egui, no kls-core)
  src/lib.rs                     types, re-exports
  src/source.rs                  Source trait, Query, Hit, Locator
  src/fetch.rs                   Fetcher trait; HttpFetcher (reqwest+rustls); FakeFetcher (tests)
  src/cache.rs                   HTTP cache (ETag/Last-Modified/max-age), blob store
  src/index.rs                   SQLite FTS5 index (own file: <data>/libsearch.db)
  src/git.rs                     smart-HTTP ls-refs (pkt-line), codeload tarball, bounded extract
  src/catalogue.rs + catalogue.json   curated sources (include_str!), user taps
  src/sources/{mcp_registry,marketplace,github_repo,clawhub,skills_sh,
               hermes_index,lobehub,gemini_gallery,aitmpl,well_known,
               awesome_csv,cline_mcp,gh_code}.rs
  src/convert/{mod,canonical,skill,command,agent,hook,mcp,prompt,tools}.rs
  src/scan.rs                    risk flags
  src/licence.rs                 SPDX detection + policy
  tests/fixtures/<source>/…      recorded bodies; tarballs built in-test
crates/kls-store/src/libsearch.rs + migrations/{sqlite,postgres}/0008_library_sources.sql
crates/kls-core/src/libsearch.rs domain: search, preview, import, update check, events
crates/kls-ui/src/screens/library_search.rs   Discover pane (called from library.rs)
```

Workspace dependencies to add: `reqwest` (`rustls-tls`, `gzip`, `stream`), `flate2`, `tar`, `zip` (ClawHub download), `toml` (Gemini commands), `globset`, `bytes`. YAML is already covered by `serde_norway`. **FTS5 is already compiled in**: sqlx `sqlite` bundles `libsqlite3-sys 0.40`, whose `build.rs` sets `-DSQLITE_ENABLE_FTS5`. So no tantivy.

The index lives in its own SQLite file, not in `Store`. It is a rebuildable cache, `Store` may be Postgres, and FTS5 is SQLite-only. Provenance and bundles live in `Store`, written for both dialects.

### 4.2 Fetching strategy, no credentials

Requests go cheapest first:

```
 cache ─▶ static index ─▶ refs check ─▶ tarball@sha ─▶ raw@sha ─▶ remote API ─▶ (gh, optional)
  304       S1 S2 S5–S9     info/refs     codeload        per file    S3 S4 S1     WP8
```

1. **HTTP cache.** Every GET is conditional (`If-None-Match`, `If-Modified-Since`) and honours `max-age`. A 304 is free on GitHub and on Pages.
2. **Static indexes** (S1 cursor pages with `updated_since`, S2 streamed, S5–S9, M1–M5 raw). One request per source per refresh.
3. **Repo crawl** (G*, M* path sources, `git-subdir` entries):
   - `ls-refs` over smart HTTP gives `HEAD` (or the pinned ref) → commit SHA. If that is unchanged since the last crawl, stop.
   - Otherwise GET `codeload…/tar.gz/{sha}`. Stream gzip into tar. Keep only entries matching the source globs plus `LICENSE*`, `plugin.json` and `marketplace.json`.
   - Caps follow belai `agentimport`: 1000 files, 1 MiB per file, 8 MiB kept, depth 6, 256 MiB expanded (bomb cut). Skip links and specials. Skip credential file names (`auth.json`, `.env`, `.npmrc`, `id_*`, …).
   - Repos marked `huge` in the catalogue (hermes-agent, openclaw) skip the tarball and fetch raw by path from index entries.
4. **Raw.** Single files at an immutable `{sha}` path (`SKILL.md` for an S2/S4 hit, preview before crawl).
5. **Remote APIs** (S3, S4, S1 `search=`): queried live when the query changes, debounced 300 ms, deadline 4 s per source. Results are merged with local FTS hits and written into the index as `seen` rows so they work offline later.
6. **Budget.**
   - A token bucket per host: `api.github.com` core at 55/h (5 kept back), search at 9/min; `clawhub.ai` at 600/min.
   - 429 or 403 with `Retry-After` / `x-ratelimit-reset` parks the host. Its source shows a clock chip with the reset time.
   - Background refresh runs at most every 6 h per source (S2 at its ETag cadence), only when the Library is opened, and never on start-up.
7. **`gh` boost** (optional, WP8). When `kls-github` reports `gh auth status` ok (`select.rs` route detection):
   - enable `gh_code` (code search: `filename:SKILL.md`, `path:.claude/agents`, `path:.claude/commands`, `filename:hooks.json`, `filename:.mcp.json`, 10/min);
   - use `git/trees?recursive=1` for huge repos;
   - use `repos/{o}/{r}/license` for an unknown licence.
   All of it goes through `kls_github::budget` as `CallClass::User` (interactive) or `Deferrable` (refresh). Without `gh` locally, a belai node's `gh` can be used through the existing `github` job route, off by default.

### 4.3 Generic repo adapter (user taps)

A tap is `owner/repo[@ref]` plus optional globs. With no globs, the adapter detects items from the known harness paths (a vendored data copy of belai's `harnesses.json` dirs, kept equal by a test that pins its hash):
- `.claude/{agents,commands,skills}`, `.codex/{prompts,skills}`, `.agents/skills`, `skills/*/SKILL.md`
- `.gemini/commands/**.toml`, `gemini-extension.json`
- `.cursor/{rules,commands}`, `.roo/commands`, `.roomodes`, `.clinerules/workflows`
- `.github/{agents,prompts,instructions,hooks}`, `.opencode/{agent,command}`, `.windsurf/workflows`
- `*/hooks/hooks.json`, `.mcp.json`
- `.claude-plugin|.agents/plugins|.cursor-plugin/marketplace.json`

### 4.4 Types and traits

```rust
// kls-libsearch/src/source.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ItemType { Agent, Skill, Command, Hook, Mcp, Prompt }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Ecosystem { ClaudeCode, Codex, Hermes, OpenClaw, GeminiCli, Cursor, Cline, Roo,
                     Copilot, OpenCode, Windsurf, McpRegistry, LobeHub, AgentSkills, Generic }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trust { Official, Trusted, Community }          // Official = vendor's own repo/registry

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode { Indexed, Remote, Both }

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Locator {
    GitHub { owner: String, repo: String, git_ref: Option<String>, path: String },
    ClawHub { owner: String, slug: String, version: Option<String> },
    Mcp { name: String, version: String },
    LobeHub { id: String },
    Url { url: String },                                  // well-known, aitmpl raw
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Upstream {
    pub commit: Option<String>,      // 40-hex, when git-backed
    pub version: Option<String>,     // registry/semver/ClawHub version
    pub sha256: String,              // over fetched source files (sorted path\0bytes)
    pub urls: Vec<String>,           // exact immutable URLs fetched (provenance, §6a)
    pub fetched_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hit {
    pub source: String,              // "mcp-registry", "clawhub", "gh:anthropics/skills"
    pub locator: Locator,
    pub item_type: ItemType,
    pub ecosystem: Ecosystem,
    pub name: String,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub licence: Licence,
    pub trust: Trust,
    pub popularity: Option<u64>,     // stars / installs / downloads
    pub updated_at: Option<i64>,
    pub flags: Vec<Flag>,            // upstream signals (isSuspicious, skillspector)
}

pub struct Query<'a> {
    pub text: &'a str,
    pub types: &'a [ItemType],
    pub ecosystems: &'a [Ecosystem],
    pub sources: &'a [String],
    pub licence: LicencePolicy,      // Any | Permissive | KnownOnly
    pub hide_flagged: bool,
    pub limit: usize,
}

pub struct Ctx {
    pub http: Arc<dyn Fetcher>,
    pub cache: Arc<Cache>,
    pub deadline: std::time::Instant,
}

pub struct Refresh { pub hits: Vec<Hit>, pub cursor: Option<String>, pub not_modified: bool }

#[async_trait::async_trait]
pub trait Source: Send + Sync {
    fn id(&self) -> &str;
    fn label(&self) -> &str;
    fn types(&self) -> &'static [ItemType];
    fn ecosystems(&self) -> &'static [Ecosystem];
    fn mode(&self) -> Mode;
    fn needs_gh(&self) -> bool { false }
    /// Indexed sources: crawl into the local index. `cursor` is what the last call returned.
    async fn refresh(&self, cx: &Ctx, cursor: Option<&str>) -> anyhow::Result<Refresh> {
        let _ = (cx, cursor); Ok(Refresh { hits: vec![], cursor: None, not_modified: true })
    }
    /// Remote sources: live query.
    async fn search(&self, cx: &Ctx, q: &Query<'_>) -> anyhow::Result<Vec<Hit>> {
        let _ = (cx, q); Ok(vec![])
    }
    /// Source files of one item, rooted at the item (SKILL.md dir, plugin dir, file).
    async fn fetch(&self, cx: &Ctx, loc: &Locator) -> anyhow::Result<Fetched>;
    /// Newest upstream for an imported item; `None` if unknown.
    async fn latest(&self, cx: &Ctx, loc: &Locator) -> anyhow::Result<Option<Upstream>>;
}

pub struct Fetched {
    pub files: std::collections::BTreeMap<String, bytes::Bytes>, // relative, sanitised
    pub upstream: Upstream,
    pub licence_files: Vec<(String, bytes::Bytes)>,              // LICENSE* up the tree
}
```

```rust
// kls-libsearch/src/fetch.rs
pub enum Class { User, Background }

pub struct Req<'a> { pub url: &'a str, pub accept: &'a str, pub class: Class, pub max_bytes: u64 }

pub enum Resp {
    Body { status: u16, headers: http::HeaderMap, body: bytes::Bytes },
    NotModified,                                   // served from cache by the caller
    Limited { retry_at_ms: i64 },
}

#[async_trait::async_trait]
pub trait Fetcher: Send + Sync {
    async fn get(&self, req: Req<'_>) -> anyhow::Result<Resp>;   // conditional via Cache
    async fn ls_refs(&self, owner: &str, repo: &str) -> anyhow::Result<Vec<(String, String)>>; // (ref, sha)
    async fn tarball(&self, owner: &str, repo: &str, sha: &str,
                     keep: &globset::GlobSet, lim: Limits) -> anyhow::Result<Tree>;
}

pub struct Limits { pub files: usize, pub file_bytes: u64, pub kept_bytes: u64,
                    pub expanded_bytes: u64, pub depth: usize }      // 1000/1MiB/8MiB/256MiB/6

/// Allowed hosts. Redirects are followed only within this list; anything else is an error.
pub const HOSTS: &[&str] = &["registry.modelcontextprotocol.io", "clawhub.ai", "skills.sh",
  "geminicli.com", "www.aitmpl.com", "chat-agents.lobehub.com", "api.cline.bot",
  "nousresearch.github.io", "registry.npmjs.org", "pypi.org", "github.com", "api.github.com",
  "codeload.github.com", "raw.githubusercontent.com"]; // + user well-known hosts
```

```rust
// kls-libsearch/src/convert/mod.rs
pub enum Target { Library { kind: &'static str }, Agent }   // "skill"|"command"|"prompt"|"hook"|"mcp"

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum NoteKind { Mapped, Metadata, Dropped, Warning }    // belai scan vocabulary
pub struct Note { pub kind: NoteKind, pub field: String, pub text: String }

pub struct BundleFile { pub path: String, pub data: Vec<u8>, pub sha256: String }

pub struct Converted {
    pub target: Target,
    pub name: String,
    pub description: String,
    pub doc: Vec<u8>,                 // canonical bytes (belai rules)
    pub sha256: String,               // libitem hash / HashBundle for hooks
    pub files: Vec<BundleFile>,       // hooks only
    pub profile: Option<kls_store::vault::AgentProfile>, // Target::Agent
    pub extra_skills: Vec<Converted>, // skills bundled with an agent/plugin
    pub notes: Vec<Note>,
    pub flags: Vec<Flag>,
    pub licence: Licence,
}

pub fn convert(item_type: ItemType, eco: Ecosystem, src: &Fetched, opts: &Opts)
    -> anyhow::Result<Converted>;      // pure: no IO, no clock (fetched_at passed in)

pub mod canonical {
    pub fn markdown(text: &str) -> anyhow::Result<String>;        // belaiCanonicalMarkdown
    pub fn json(v: &[u8], max_depth: usize) -> anyhow::Result<Vec<u8>>; // belaiCanonicalJSON
    pub fn hash(doc: &[u8]) -> String;
    pub fn bundle_hash(doc: &[u8], files: &[(&str, &str)]) -> String; // "\n--files--\n" form
}
```

`canonical::json` needs serde_json `arbitrary_precision` (numbers kept as written). The sorted map is the default `BTreeMap`. The writer is custom: no HTML escaping, and U+2028/U+2029 escaped.

```rust
// kls-libsearch/src/scan.rs
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Flag {
    RunsCode,              // any hook; any MCP stdio command
    ShellExpansion,        // Claude !`…`, Gemini !{…}
    FileInclude,           // @path / @{…}
    PipeToShell,           // curl|wget … | sh/bash, iwr|iex
    RemoteUrl(String),     // first non-allowlisted URL host in body/commands
    HiddenText,            // bidi / zero-width / tag chars (stripped)
    SecretLike,            // key-shaped literals (belai agentfiles.HoldsSecret patterns)
    PromptInjection,       // "ignore previous instructions", role-swap markers, hidden HTML comments
    LongBase64,            // ≥ 200 base64 chars
    Upstream(String),      // ClawHub isSuspicious/moderation, aitmpl skillspector != SAFE
    ProtocolMismatch,      // hook dialect converted (Gemini/Copilot/Cursor)
    FilesNotCarried(u32),
    NeedsProgram(String),  // hook first word not a bundled file
    LicenceUnknown, LicenceRestricted(String),
    Refused(String),       // marketplace source:"command", ${PLUGIN_ROOT} MCP, path escape
}
pub fn scan(c: &Converted) -> Vec<Flag>;
```

```rust
// kls-libsearch/src/index.rs  (own SqlitePool, WAL)
pub struct Index { /* pool */ }
impl Index {
    pub async fn open(path: &Path) -> anyhow::Result<Self>;
    pub async fn upsert(&self, source: &str, hits: &[Hit]) -> anyhow::Result<()>;
    pub async fn retire(&self, source: &str, seen_before: i64) -> anyhow::Result<u64>;
    pub async fn search(&self, q: &Query<'_>) -> anyhow::Result<Vec<Hit>>;
    pub async fn facets(&self, q: &Query<'_>) -> anyhow::Result<Facets>; // counts per type/eco/source
    pub async fn state(&self) -> anyhow::Result<Vec<SourceState>>;
}
```

```sql
-- <data>/libsearch.db (cache; deleting it is safe)
CREATE TABLE src_state (source TEXT PRIMARY KEY, cursor TEXT, etag TEXT, last_modified TEXT,
  refreshed_at INTEGER, ok_at INTEGER, error TEXT, items INTEGER, parked_until INTEGER);
CREATE TABLE hits (rowid INTEGER PRIMARY KEY, id TEXT UNIQUE,  -- source|locator_json
  source TEXT, item_type TEXT, ecosystem TEXT, name TEXT, title TEXT, description TEXT,
  tags TEXT, licence TEXT, trust TEXT, popularity INTEGER, updated_at INTEGER,
  locator TEXT, flags TEXT, seen_at INTEGER);
CREATE VIRTUAL TABLE hits_fts USING fts5(name, title, description, tags,
  content='hits', content_rowid='rowid',
  tokenize='porter unicode61 remove_diacritics 2', prefix='2 3');
-- triggers keep hits_fts in sync
CREATE TABLE http_cache (url TEXT PRIMARY KEY, etag TEXT, last_modified TEXT,
  max_age INTEGER, fetched_at INTEGER, blob TEXT);              -- blob = sha256
CREATE TABLE git_refs (repo TEXT, ref TEXT, sha TEXT, checked_at INTEGER, PRIMARY KEY (repo, ref));
-- blobs on disk: <data>/libsearch/blobs/ab/abcdef… (content-addressed)
```

Ranking: `bm25(hits_fts, 8.0, 6.0, 2.0, 3.0)`, then `+ trust boost (Official 2, Trusted 1)` and `+ 0.3·ln(1+popularity)`. Prefix match applies to the last token. Exact name match comes first.

```sql
-- kls-store migrations/{sqlite,postgres}/0008_library_sources.sql
CREATE TABLE library_provenance (
  item_kind TEXT NOT NULL,            -- library kind or 'agent'
  item_id TEXT NOT NULL, version BIGINT NOT NULL,
  source TEXT NOT NULL, ecosystem TEXT NOT NULL, locator TEXT NOT NULL,   -- JSON
  upstream_commit TEXT NOT NULL DEFAULT '', upstream_version TEXT NOT NULL DEFAULT '',
  source_urls_json TEXT NOT NULL,     -- every URL fetched for this version, exact (incl. {sha}/version)
  upstream_sha256 TEXT NOT NULL,      -- source files: sha256 over sorted (path \0 bytes)
  doc_sha256 TEXT NOT NULL,           -- canonical doc / bundle hash (belai-equal)
  files_json TEXT NOT NULL DEFAULT '[]', -- [{path, sha256, url}] per source file
  licence TEXT NOT NULL DEFAULT '', notes_json TEXT NOT NULL DEFAULT '[]',
  flags_json TEXT NOT NULL DEFAULT '[]', ack_json TEXT NOT NULL DEFAULT '{}',
  actor TEXT NOT NULL,                -- "user" (imports are person-only, §6a)
  converted_by TEXT NOT NULL DEFAULT 'kls', -- 'kls' | 'node:<id>' (WP9)
  audit_seq BIGINT NOT NULL,          -- ledger entry that recorded the import
  reviewed_at BIGINT NOT NULL,
  PRIMARY KEY (item_kind, item_id, version));
CREATE TABLE library_updates (item_kind TEXT NOT NULL, item_id TEXT NOT NULL,
  latest_commit TEXT NOT NULL DEFAULT '', latest_version TEXT NOT NULL DEFAULT '',
  latest_sha256 TEXT NOT NULL DEFAULT '', checked_at BIGINT NOT NULL,
  state TEXT NOT NULL,                -- current | behind | gone | error
  PRIMARY KEY (item_kind, item_id));
-- WP0 (hooks): bundle manifest + content-addressed blobs, mirrors BelaiLibraryItemFile
CREATE TABLE library_files (item_id TEXT NOT NULL, version BIGINT NOT NULL, path TEXT NOT NULL,
  sha256 TEXT NOT NULL, size_bytes BIGINT NOT NULL, PRIMARY KEY (item_id, version, path));
CREATE TABLE library_blobs (sha256 TEXT PRIMARY KEY, size_bytes BIGINT NOT NULL,
  data BLOB NOT NULL);                -- BYTEA in postgres
```

```rust
// kls-core/src/libsearch.rs
pub mod kinds {
    pub const LIBSEARCH_REFRESHED: &str = "libsearch.refreshed";   // {source, items, notModified}
    pub const LIBSEARCH_LIMITED:   &str = "libsearch.limited";     // {host, until}
    pub const LIBRARY_IMPORTED_FROM: &str = "library.imported_from"; // {kind,id,version,source}
    pub const LIBRARY_UPDATE:      &str = "library.update";        // {kind,id,state}
}
pub struct Preview { pub hit: Hit, pub source_files: Vec<(String, String)>, pub converted: Converted,
                     pub existing: Option<(String, i64, String)> /* id, version, body */ }
pub async fn search(core: &Core, q: &Query<'_>) -> Result<Vec<Hit>>;          // index ∪ remote
pub async fn preview(core: &Core, hit: &Hit) -> Result<Preview>;               // fetch + convert + scan
pub async fn import(core: &Core, p: &Preview, ack: &Ack) -> Result<(String, i64)>;
pub struct Ack { pub flags: Vec<Flag>, pub licence: bool, pub rename: Option<String> } // must cover p.converted.flags
pub async fn check_updates(core: &Core) -> Result<usize>;                      // latest() per provenance row
pub async fn refresh(core: &Core, source: Option<&str>) -> Result<()>;

/// Rules 1–3 of WORKSTREAMS "Non-negotiable rules" (§6a). Ok(()) or a refusal
/// that has already been emitted (`library.refused`) and chained in the ledger.
pub enum Action<'a> { Refresh(&'a str), RemoteSearch(&'a str), Preview(&'a Locator),
                      Import(&'a Locator), Update(&'a str), CheckUpdates, NodeConvert,
                      SourceChange(&'a str) }
pub async fn guard(core: &Core, a: Action<'_>) -> Result<()>;
```

Every function that touches the network or the library calls `guard(core, Action)` first (§6a).

`import` refuses in four cases:
- `ack.flags` does not cover every flag that needs review;
- a `Refused` flag is present;
- the kill switch is on;
- the fetched bytes no longer hash to `Preview.converted.upstream.sha256`.

Every refusal is audited. On success, `import` writes through `vault::save_library` / `save_agent` with note `imported from <url>@<sha>`, then writes the provenance row and the ledger entry. The provenance row carries `audit_seq`. Hooks are stored only. Nothing reaches a node except through an explicit per-host install, which is the existing belai `item_install` rule for hooks.

**Agent conversion on a node (WP9, optional).** Add job kind `library` with payload `{"task":"convert","kind":"agent","format":"hermes|claws|nemoclaw|mini-swe","files":{path:base64}}` → belai stages the files in a temp dir → `agentimport.ImportItem` → `{doc, sha256, notes, skills[]}`. The `Job.kind` comment in `node.proto` must change, which needs the coordinator. When no node is online, the Rust fallback converts SOUL.md / CLAW.md bodies only and notes that.

---

## 5. UX

Icon legend (`kls_ui::icons` constants): ⌕ MAGNIFYING_GLASS · ▤ BOOKS · ◎ GLOBE · ⎇ GITHUB_LOGO · ⬇ DOWNLOAD_SIMPLE · ↻ ARROWS_CLOCKWISE · ⚠ SHIELD_WARNING · ✓ SEAL_CHECK · 🔒 LOCK_SIMPLE · ⧉ GIT_DIFF · ⌗ FINGERPRINT · ↗ ARROW_SQUARE_OUT · ⏲ CLOCK · ⊘ PROHIBIT · ⚡ LIGHTNING (hook) · ⏚ PLUG (mcp) · ◆ SPARKLE (skill) · ▸ TERMINAL (command) · ☺ ROBOT (agent) · ≡ SCROLL (prompt) · ☰ FUNNEL · ⌂ TRAY (offline) · ★ popularity.

### 5.1 Library, Discover tab

```
┌ ▤ Library ───────────────────────────────── [▤ Items] [⌕ Discover] ─ [↻ Refresh] [◎ Sources 11/14] ┐
│ ⌕ [ code review________________________ ]  ☺ Agent  ◆ Skill  ▸ Command  ⚡ Hook  ⏚ MCP  ≡ Prompt    │
│                                            12      48       9          3       7      5            │
├─ ☰ Filters ──────────┬─ Results 84 ───────────────────────────────────┬─ ◆ code-review ─────────────┤
│ Ecosystem            │ Name            Type Eco     Source  Lic  ★   Upd│ ✓ Trusted · MIT · ⎇ a1b2c3d │
│ [x] Claude Code   31 │ code-review     ◆   Claude  ⎇ wsh.. MIT 42k 3d  │ wshobson/agents             │
│ [x] Codex          9 │ pr-review       ▸   Claude  aitmpl  MIT 1.4k 1w │ plugins/code-review/skills/…│
│ [x] Hermes        14 │ reviewer        ☺   Copilot ⎇ awe.. MIT 41k 2w  │ [Source] [Converted] [⧉ Diff]│
│ [ ] OpenClaw      20 │ review-guard    ⚡   Claude  ⎇ dis.. ?   4k  ⚠  │ [Notes 3] [Files 4]         │
│ [x] Gemini         6 │ github          ⏚   MCP     mcp-reg  —   —  1d  │ ┌─────────────────────────┐ │
│ [ ] Cursor         4 │ review          ◆   OpenClaw clawhub ? 52k ⚠   │ │---                      │ │
│ Source               │ …                                               │ │name: code-review        │ │
│ [x] ⎇ Curated  ●  2h │                                                 │ │description: …           │ │
│ [x] ⏚ MCP reg  ●  2h │                                                 │ │---                      │ │
│ [x] ◎ ClawHub  ● live│                                                 │ └─────────────────────────┘ │
│ [ ] ◎ Hermes idx ○   │                                                 │ ⚠ ▸ ShellExpansion          │
│ [x] ◎ skills.sh ●live│                                                 │ ⓘ dropped: model            │
│ Licence  (•)Any ( )Permissive ( )Known                                 │ ⓘ files not carried: 3      │
│ Trust    [x]Official [x]Trusted [x]Community                           │ [⬇ Import] [↗ Open] [⌗ Pin] │
│ [x] ⊘ Hide flagged   │                                                 │                             │
│ ⎇ anon 41/60 ⏲ 38m   │                                                 │                             │
└──────────────────────┴─────────────────────────────────────────────────┴─────────────────────────────┘
```

- Type pills come from `ItemType` and their counts from `Index::facets`. Choosing a type pre-filters the ecosystems that publish it.
- The status dot next to each source is ● ok, ○ never refreshed, ⚠ error or parked (hover shows the reason). Next to it: the age, or `live` for a remote source.
- The GitHub budget line reads `⎇ anon 41/60 ⏲ 38m`, or `⎇ gh ✓ 4980/5000`.
- The Flags column shows ⚠ or ⊘. Hover lists the flags.
- The preview tabs:
  - **Source**: upstream text, read-only.
  - **Converted**: canonical text, editable before import.
  - **⧉ Diff**: against an existing item of the same kind and name, or against the provenance version on update.
  - **Notes**: `mapped`/`dropped`/`metadata`/`warning`.
  - **Files**: bundle for a hook, the dropped list for a skill.

### 5.2 Review dialog (opens from ⬇ Import)

```
┌ ⬇ Import ◆ code-review ───────────────────────────┐
│ ⎇ wshobson/agents @ a1b2c3d   ⌗ 9f3e…  MIT ✓      │
│ Name   [ code-review______ ]  ( ) New  (•) v3 of ▸│
│ ⚠ Review                                           │
│  [ ] ▸ ShellExpansion   !`git diff`   line 14      │
│  [ ] ◎ RemoteUrl        example.com   line 31      │
│  [ ] 🔒 Licence: unknown                           │
│ ⓘ dropped: model · files not carried: 3            │
│            [⬇ Import]  [✕ Cancel]                  │
└────────────────────────────────────────────────────┘
```

- ⬇ Import is enabled only when every box is ticked.
- A ⊘ `Refused` flag replaces the checklist with one line, `⊘ <reason>`, and there is no Import button.
- Hooks always show `⚡ RunsCode · stored, not installed`.
- An MCP stdio item shows its argv (`⏚ npx -y @x/y@1.2.3`).

### 5.3 Items tab additions

```
 Name          Type  Version  Saved   ⎇ Source            Update
 code-review   ◆     v3       3d      wshobson/agents      ↻ behind (b7e…)
 github        ⏚     v1       1d      mcp-registry         ✓
 review-guard  ⚡     v1       2w      disler/…  🔒?         —
```

- `↻ behind` opens the Diff tab between the provenance upstream and the latest upstream, re-runs conversion and the review, and saves a new version with note `updated from <sha|version>`.
- Never auto-update.

### 5.4 Sources panel (◎ Sources)

```
┌ ◎ Sources ──────────────────────────────────────────────────────────┐
│ On  Source               Types        Mode   Last    Items  Status   │
│ [x] ⏚ MCP registry       ⏚           I+R    2h      4.1k   ●        │
│ [x] ◎ ClawHub            ◆           R      live    —      ●        │
│ [x] ◎ skills.sh          ◆           R      live    —      ●        │
│ [ ] ◎ Hermes index 11MB  ◆           I      —       —      ○        │
│ [x] ◎ Gemini gallery     ◆▸⚡⏚≡      I      1h      2.2k   ●        │
│ [x] ◎ aitmpl             ☺◆▸⚡⏚      I      5m      1.9k   ●        │
│ [x] ⎇ anthropics/skills  ◆           I      2h@e4bb 19     ●        │
│ [x] ⎇ openai/skills      ◆           I      2h@…    44     ●        │
│ …                                                                    │
│ [+ Add] ⎇ [owner/repo@ref] [globs] [type▾] [eco▾]   ◎ [well-known URL]│
│ ⎇ GitHub: anon 41/60 · search 10/min      [⎇ Use gh] (gh ✓ signed in) │
└──────────────────────────────────────────────────────────────────────┘
```

### 5.5 Offline

- A failed network call is never fatal. The index and blob cache answer searches.
- Sources that cannot be reached show `⌂ cached 2d`. Remote sources show `⌂ offline` and are left out.
- A preview or import works from cached blobs when the item's files are there. Otherwise ⬇ Import is disabled with `⌂ not cached`.
- Update checks are skipped and the row is left `—`.

---

## 6. Security

Everything imported is untrusted.

| Rule | Where |
|---|---|
| Nothing fetched is executed, sourced or installed by search or import. Hooks and MCP stdio servers are stored only. A host gets them through the per-host install the user starts (belai: hook is `InstallOnly`, never synced or backed up) | `kls-core::libsearch::import` |
| Marketplace `source:"command"`, MCP configs using `${CLAUDE_PLUGIN_ROOT}`/`${extensionPath}`, and http MCP URLs that are not https → `Refused` | converters |
| Review step is mandatory: each flag ticked, licence acknowledged when it is not permissive | `Ack` |
| Kill switch, audit, provenance, person-only actor | §6a, `guard` |
| Pinning: fetch by commit SHA (tarball/raw `{sha}`) or exact registry version, never a branch. Record `upstream_commit`, `upstream_sha256` (source files) and `doc_sha256` (canonical) | provenance |
| Upstream integrity: `git-subdir`/`url` marketplace entries that carry `sha` are fetched at that SHA, and a mismatch is refused | marketplace adapter |
| Bounded readers: file, total, depth and expansion caps; no symlinks, hard links or devices; no `..`, absolute or backslash paths; credential file names skipped; UTF-8 text only for docs | `git.rs`, `fetch.rs` |
| Host allowlist; https only; redirects only within the allowlist; response caps per request (index 64 MiB, file 1 MiB) | `HttpFetcher` |
| Text sanitising: strip bidi, zero-width and tag characters (and flag them); CRLF → LF; one-line fields flattened | `convert::canonical` |
| Secrets: MCP env/header values that look secret become `cred:NAME`; any literal secret-shaped string → `SecretLike` and must be ticked. Never write values from `.env`/`auth.json` | `convert::mcp`, `scan` |
| Agents: single mode, supervised; tools only from the fixed table; read-only fallback; `permissionMode`/`hooks`/`mcpServers` never carried | `convert::agent` |
| Upstream signals (ClawHub `isSuspicious`/moderation/scan, aitmpl `skillspector`) are shown as flags but never trusted as "safe" | `Flag::Upstream` |
| Prompt injection heuristics only flag, and the preview shows the text. Keleustes never sends imported text to a model during import | `scan` |

### 6a. Provenance, audit, kill switch, A2A (WORKSTREAMS non-negotiable rules)

**1. Provenance.** Every imported version stores its origin in `library_provenance`:
- `source` and `locator`;
- `source_urls_json`: the exact immutable URLs fetched, such as `https://codeload.github.com/o/r/tar.gz/<sha>`, `https://raw.githubusercontent.com/o/r/<sha>/<path>`, `…/v0.1/servers/<name>/versions/<v>` or `clawhub…/download?slug=&version=`;
- `files_json`: path, sha256 and URL for each source file;
- `upstream_sha256`, `doc_sha256`, the licence, the ack, the actor, the converter (`kls` or `node:<id>`) and `audit_seq`.

Items saved by hand keep `origin = user` with no row. The UI shows the origin wherever the item appears:
- the Items tab `⎇ Source` column;
- the editor header chip `⎇ owner/repo@sha7 ⌗ hash7`, which opens `↗` the source URL;
- the agent card on the Workforce screen.

The library file export (`library_file`) gains `source:` and `source_sha256:` front matter keys. A re-import from file keeps them as a note and never as trusted provenance.

**2. Audit.** Each action below writes one hash-chained entry through `Core::audit_record(LedgerDraft)`:
- `actor`: `user`, or `system` for background refresh;
- `actor_kind`: `user` or `system`;
- `subject`: `{kind, id, version}` or `{source}`;
- `payload`: `{urls, upstream_sha256, doc_sha256, flags, ack, licence, outcome}`;
- `origin`: `factory`, or `host:<node>` for WP9;
- `ref_key`: `libsearch:<action>:<id>:<version|ts>`, so a retry is never chained twice.

| Ledger `kind` | When | Outcome values |
|---|---|---|
| `library.imported` | import saved | `created` · `new_version` · `unchanged` |
| `library.updated` | update from upstream saved | `new_version` · `unchanged` |
| `library.refused` | any refusal: kill switch, unticked flag, `Refused` flag, hash mismatch, licence not acknowledged, size/validation | `kill_switch` · `review` · `refused:<reason>` · `hash_mismatch` · `invalid:<detail>` |
| `library.update_checked` | update check found `behind` or `gone` (not `current`, to keep the ledger lean) | `behind` · `gone` |
| `libsearch.source_changed` | a source/tap is added, removed, enabled, disabled, or the gh boost is toggled | `added` · `removed` · `on` · `off` |
| `libsearch.refreshed` | an indexed refresh that changed rows | `{items, added, retired}` |

Search queries and previews are not audited, because they change no state. A preview's fetch URLs go to the debug log only.

**3. Kill switch.** `guard` reads `core.factory_control()` (`FactoryControl.stopped`) before any of these:
- a network fetch: refresh, remote search, preview fetch, update check, licence lookup, `gh` call;
- any library write from search: import, update;
- the WP9 node `convert` job. The gateway also checks this, because it is a Job.

When `stopped`:
- Refuse the action, emit `library.refused` with `{reason:"kill_switch"}` and chain the audit entry.
- The Discover tab shows a `⊘ Factory stopped` bar.
- ⬇ Import and ↻ Refresh are disabled.
- Search answers from the local index only, marked `⌂ cached`.
- Background refresh and update checks are not scheduled.
- Lifting the switch resumes nothing automatically. The person presses ↻ again.

A refresh already in flight checks between requests and aborts at the next boundary.

**4. A2A.** Search and import are person-only.
- No agent, crew, workflow step, built-in, node or host can call `search`, `preview` or `import`. The core functions take no agent or session context. A run that asks for one is refused and audited (`library.refused`, `refused:agent_actor`).
- No A2A path is introduced. The WP9 convert job is a factory → node Job and carries no agent identity.
- An imported agent profile can never carry A2A or peer settings: no `a2a`, `peers`, `allow_agents`, `crew`, `subagents`, `Task`-to-named-agent bindings, worker blocks or schedules. Such keys are `dropped`, with a warning. This matches belai `agentimport`: "single-mode, supervised, no worker blocks, no schedule".
- Imported skills, commands, prompts and hooks are stored only. They do not change the A2A blackout or policy, and the classifier kinds (KindPeer and the others) are untouched.

**Licence detection.** The first match wins:
1. item front matter `license`
2. `LICENSE*` in the item dir
3. `plugin.json` / `gemini-extension.json` / marketplace entry `license`
4. index metadata (`licenseKey`, aitmpl `license`, npm/PyPI)
5. repo root `LICENSE` from the tarball
6. with `gh`, the licence API

SPDX is matched by normalised-text fingerprints for MIT, Apache-2.0, BSD-2/3, ISC, CC0, Unlicense, MPL-2.0, GPL/LGPL/AGPL and CC-BY(-SA).

Policy:
- Permissive → ✓.
- Copyleft → 🔒 amber, acknowledge.
- `Proprietary`, `NOASSERTION`, none or unknown → 🔒 red, acknowledge. This covers `anthropics/skills` document skills, `disler/claude-code-hooks-mastery` and ClawHub re-uploads.

---

## 7. Tests

No network in unit tests. Every source is driven through `FakeFetcher` serving `tests/fixtures/<source>/…`.

| Area | Tests |
|---|---|
| Fixtures | Trimmed real bodies captured 2026-11-21: MCP registry page + cursor, ClawHub search/detail/409-ambiguous/file, skills.sh search, Gemini gallery (20 rows), aitmpl (5 per type), LobeHub index + detail, Hermes index head (streaming parser over a 2 000-row file), M1/M3/M4 marketplace.json (incl. `command`, `git-subdir`, `url` sources), `info/refs` pkt-line response. Tarballs are built in the test from `fixtures/repos/<name>/` with the `tar` crate |
| Converters | Golden in/out per (type × ecosystem) row of §3: `fixtures/convert/<type>/<eco>/{in/…, out.doc, out.notes.json}`. Re-converting canonical output is a fixed point |
| Canonical / hash | belai's pinned vectors (`libitem/validate_test.go`, `HashBundle` golden) copied as constants. Rust must match byte for byte |
| Security | tar with `../x`, absolute path, symlink, hard link, device, 300 MiB gzip bomb, 1 001 files, deep nesting; bidi/zero-width in name and body; secret-shaped literals; `source:"command"`; redirect to a host off the allowlist; `http://` MCP remote |
| Index | FTS ranking order (exact > prefix > bm25), facets, retire stale rows, upsert idempotence, corrupt db → rebuild |
| Cache / budget | ETag round-trip → `NotModified`; `Retry-After` parks a host; budget never exceeds 55/h in a simulated hour; ls-refs unchanged → no tarball request |
| Store | `library_provenance` / `library_updates` / `library_files` CRUD on SQLite (Postgres with `KLS_TEST_POSTGRES_URL`), per WORKSTREAMS conventions |
| Core | `import` refuses an incomplete `Ack`; a hook import writes files and installs nothing; update check flips `behind`; events emitted |
| Rules 1–4 (WORKSTREAMS rule 5) | import writes a `library_provenance` row with `source_urls_json` + `upstream_sha256` + `doc_sha256` and a `library.imported` ledger entry whose seq equals `audit_seq` (`audit_verify` ok). Each refusal reason writes `library.refused`. With `set_factory_stopped(true)`: refresh, remote search, preview fetch, import, update, check_updates and the node convert job are refused, `FakeFetcher` records **zero** requests, and the refusal is audited. An agent or run actor is refused (`refused:agent_actor`). A2A keys in a source profile are dropped. A hash mismatch between preview and import is refused |
| UI | Discover pane against a `FakeSource` (egui_kittest if present, else state unit tests) |
| Live smoke | `#[ignore]` tests gated by `KLS_LIVE_NET=1`: one call per source, asserting shape only (catches upstream drift such as skills.sh's v1 auth) |

---

## 8. Rollout: independent work packages

| WP | Scope | Depends on | Done when |
|---|---|---|---|
| **WP0** Kinds parity | Add `hook` and `mcp` to `LIBRARY_KINDS`; validators per belai rules (Skill/Command/Prompt strict front matter, Hook, MCP); `canonical` + hash with belai vectors; `library_files` + `library_blobs`; Library editor shows a bundle file list for hooks | — | Validators reject what belai rejects; hashes equal belai golden |
| **WP1** Search core | `kls-libsearch` crate: types, `Source`/`Fetcher` traits, `FakeFetcher`, `HttpFetcher`, HTTP cache, blob store, FTS5 index, budget | — | Index + cache tests green, no network |
| **WP2** GitHub transport | smart-HTTP `ls_refs`, bounded codeload tarball, raw by SHA, curated `catalogue.json` (G1–G13, M1–M5), generic tap adapter with harness paths | WP1 | Crawl of fixture repos yields the expected hits; caps enforced |
| **WP3** Static indexes | S1 (cursor/`updated_since`), S2 (streamed, opt-in), S5, S6, S7, S8, S9, G10 CSV | WP1 | Fixture refresh → hits; ETag → not-modified |
| **WP4** Remote APIs | S3 ClawHub (owner-qualified, `nonSuspiciousOnly`), S4 skills.sh (feature flag), S1 `search=`; merge + debounce + deadline | WP1 | Remote hits merged and cached; 409 ambiguity resolved |
| **WP5** Converters + scan + licence | §3 tables as pure functions; `scan`; SPDX fingerprints | WP0 (for hook/mcp output), none for skill/command/prompt/agent | Golden tests per row |
| **WP6** Core + store | `kls-core::libsearch` (search/preview/import/check_updates/refresh, `guard`), migration `0008`, events, ledger entries (§6a) | WP1, WP5 | Import writes item + provenance + audit; kill switch and actor refusals audited; hooks stored only; rule 1–4 tests green |
| **WP7** UI | Discover tab, preview tabs, review dialog, Sources panel, Items provenance/update columns, origin chip in editor and Workforce, `⊘ Factory stopped` bar, offline chips | WP6 (build against a fake first) | `READY`, clippy clean |
| **WP8** gh boost | `gh_code` source, trees for huge repos, licence API through `kls_github::budget` | WP1, kls-github | Toggles on when `gh auth status` is ok; off otherwise |
| **WP9** belai convert job | belai `library` job kind (`convert`) wrapping `agentimport.ImportItem`; proto `Job.kind` comment (coordinator) | belai team | Hermes/Claws/nemoclaw/mini-SWE profiles import with belai's notes |

Order of value: WP0 ∥ WP1 → WP2 + WP3 (skills, agents and commands searchable offline) → WP5 → WP6 → WP7 → WP4 → WP8 → WP9.

## 9. Open questions

- Should Keleustes keep skill supporting files (`scripts/`, `references/`)? That needs a belai skill-bundle change, since a belai skill is one document today.
- Is the Hermes index (11 MB gzip, 104 593 rows, mostly ClawHub mirrors) worth enabling by default? S3 live search already covers ClawHub.
- `skills.sh /api/search` is undocumented. Drop it if it starts returning 401 like `/api/v1`.
- Should the catalogue be refreshable from a pinned URL in the Keleustes repo, rather than shipping only with releases?

## Status re-check (2026-11-22)

Read-only pass over the current tree (no build run). `ls/` = `crates/kls-libsearch/src/`, `lt/` = `crates/kls-libsearch/tests/`, `core/` = `crates/kls-core/src/`, `ui/` = `crates/kls-ui/src/screens/`, `mig/` = `crates/kls-store/migrations/`.

| item | priority | status | evidence (file:line) | what's left |
|---|---|---|---|---|
| **WP0** `hook` + `mcp` in `LIBRARY_KINDS` | P0 | done | core/vault.rs:104 | — |
| WP0 belai validators (Skill/Command/Prompt/Hook/MCP) | P1 | done | core/vault.rs:2394 (`validate_library_body`, called by `save_library` :2193, so the editor, the `.md` import and host backups too); refusal emits `library.refused` and a ledger entry; tests core/vault.rs:3788 (invalid MCP refused with the validator's error), :3844 | Plain Markdown (no front matter) skills/commands/prompts are kept as local text, unvalidated |
| WP0 canonical form + hash, belai golden vectors | P0 | done | ls/convert/canonical.rs:726 (`hash_vectors`), :618, :662 | — |
| WP0 `library_files` + `library_blobs` (both dialects) | P1 | done | mig/sqlite/0012_library_sources.sql:53 :63; mig/postgres/0012_library_sources.sql:53; crates/kls-store/src/libsearch.rs:292, test :517 | Migration is numbered 0012, not the 0008 the doc names |
| WP0 editor bundle file list for hooks | P2 | done | ui/library.rs:505; ui/library_search.rs:1624 | — |
| **WP1** crate, `Source`/`Fetcher` traits, `FakeFetcher`, `HttpFetcher` | P0 | done | ls/lib.rs:11; ls/source.rs; ls/fetch.rs:640 | — |
| WP1 HTTP cache (ETag/max-age) + blob store | P1 | done | ls/cache.rs:50 :73; ls/index.rs:647 (test) | — |
| WP1 FTS5 index: rank, facets, retire, rebuild | P0 | done | ls/index.rs:193 :317 :370 :285; tests :541 :575 :597 :631 | — |
| WP1 host budgets (55/h, 9/min, 600/min) + park | P1 | done | ls/fetch.rs:441 :488 :521; tests `budget_never_exceeds_55_in_any_hour`, :1025 | — |
| **WP2** smart-HTTP `ls_refs` | P1 | done | ls/fetch.rs:184; ls/git.rs:342 (test) | — |
| WP2 bounded codeload tarball | P0 | done | ls/fetch.rs:213; ls/git.rs:390 :430 :464 | — |
| WP2 raw by SHA | P1 | done | ls/fetch.rs:346; lt/sources.rs:137 | — |
| WP2 curated `catalogue.json` | P1 | done | ls/catalogue.json:5–109; ls/catalogue.rs:408 | — |
| WP2 generic tap + harness paths (hash pinned) | P1 | done | ls/catalogue.rs:286 :393 :442; lt/sources.rs:237 | — |
| **WP3/WP4** S1 MCP Registry (cursor, `updated_since`, `search=`, exact version) | P0 | done | ls/sources/mcp_registry.rs:165 :206 :218; lt/sources.rs:344 | — |
| S2 Hermes index (streamed, opt-in) | P2 | done | ls/sources/hermes_index.rs:196 :200; lt/sources.rs:697 | — |
| S3 ClawHub (owner, `nonSuspiciousOnly`, 409, ZIP) | P1 | done | ls/sources/clawhub.rs:110 :204 :252; lt/sources.rs:464 | — |
| S4 skills.sh (feature flag) | P2 | done | ls/sources/skills_sh.rs:40; ls/sources/mod.rs:107; lt/sources.rs:552 | — |
| S5 Gemini gallery (+ repo crawl on demand) | P1 | done | ls/sources/gemini_gallery.rs:74 :160; lt/sources.rs:435 | — |
| S6 aitmpl (+ skillspector flag) | P1 | done | ls/sources/aitmpl.rs:41 :108; lt/sources.rs:588 | — |
| S7 LobeHub | P2 | done | ls/sources/lobehub.rs:80; lt/sources.rs:588 | — |
| S8 Cline MCP marketplace (discovery) | P2 | done | ls/sources/cline_mcp.rs:75 :114 | — |
| S9 `/.well-known/skills` (user-added, host allowlisted) | P2 | done | ls/sources/well_known.rs:90; core/libsearch.rs:404 :559 | — |
| S10/S11 npm + PyPI facts | P2 | done | ls/sources/pkg_facts.rs:35 :60; ls/sources/mcp_registry.rs:259; lt/sources.rs:763 | — |
| M1 claude-plugins-official (`source:"command"` refused) | P0 | done | ls/catalogue.json:73; ls/sources/github_repo.rs:454; lt/sources.rs:168 | — |
| M2 anthropics/skills | P1 | done | ls/catalogue.json:90 | — |
| M3 openai/plugins | P2 | done | ls/catalogue.json:95 | — |
| M4 wshobson/agents (3 dialects) | P2 | done | ls/catalogue.json:103 :109 | — |
| M5 any repo's marketplace via tap | P2 | done | ls/sources/github_repo.rs:112; ls/catalogue.rs:359 | — |
| G1, G3–G8, G12 curated repos | P1 | done | ls/catalogue.json:5–65 | — |
| G2, G14 huge repos never tarballed | P0 | done | ls/catalogue.json:13 :69; lt/sources.rs:274 | — |
| G9 discovery list (README) | P2 | done | ls/catalogue.json:57; ls/sources/github_repo.rs:146 | — |
| G10 awesome CSV | P2 | done | ls/sources/awesome_csv.rs:207; lt/sources.rs:697 | — |
| G11 via S6, G13 via S5 | P2 | done | ls/sources/aitmpl.rs:63; ls/sources/gemini_gallery.rs:160 | — |
| WP4 merge, 300 ms debounce, 4 s deadline, seen rows | P1 | done | core/libsearch.rs:60 :753–809; ui/library_search.rs:28 | — |
| **WP5** 3.1 skill: AgentSkills, Claude, Codex, Hermes, OpenClaw, Gemini, Copilot | P1 | done | lt/fixtures/convert/skill/*; ls/convert/skill.rs:15 :93; lt/convert_golden.rs:102 | — |
| 3.1 skill LobeHub | — | n-a | doc row is n/a | — |
| 3.1 supporting files not carried + warning | P1 | done | ls/convert/skill.rs:54 :61 | — |
| 3.2 command: Claude, Codex, Gemini TOML, Cursor, Cline, Roo, Windsurf, Copilot, opencode | P1 | done | lt/fixtures/convert/command/* (9); ls/convert/command.rs:61 :77 | — |
| 3.2 command Hermes/OpenClaw | — | n-a | doc row is n/a | — |
| 3.3 agent: Claude, Copilot, opencode, Gemini, Roo, LobeHub | P1 | done | lt/fixtures/convert/agent/*; ls/convert/agent.rs:112 :499 :585 | — |
| 3.3 agent Hermes, OpenClaw (Rust fallback) | P2 | done (P3, 2026-11-28) | Ports of belai `agentimport/hermes.go` and `claws.go`: ls/convert/agent.rs:585 (`hermes`: SOUL.md prompt, profile.yaml, config.yaml model incl. `{default, provider}`, distribution.yaml version, `mcp.json` / `cron/` / `memories/` named only, `skills/` alongside), :699 (`claws`: `schemaVersion`, `agent.{id,name,model.primary,fallbacks,tools.allow/alsoAllow/deny,fs,subagents,memory}`, `packages`, `mcpServers` by name only, `cronJobs`, `workspace.bootstrapFiles.SOUL.md` as a clean relative path, unknown keys); :502 (`.tar.gz` profile export read by ls/git.rs:303 `extract_archive`: caps :437, links/specials/credential names/over-cap skipped, any `..`/absolute path refuses the import); :552 (credential files dropped before any read); goldens lt/fixtures/convert/agent/{hermes,hermes-archive,openclaw,openclaw-soul}; security lt/convert_security.rs:234 (escape refused), :255 (links, credentials, 1 MiB cap), :286 (file-count cap), :309 (no hooks, MCP secrets, schedules or A2A; bootstrap escape not followed) | — (a node conversion stays offered: core/libsearch.rs `node_format`) |
| 3.3 agent Codex/Cursor/Cline | — | n-a | doc row is n/a | — |
| 3.3 bundled skills (only when the name is free) | P2 | done | core/libsearch.rs:1062 | — |
| 3.4 hook: Claude, Codex, Gemini, Copilot, Cursor (+ ProtocolMismatch) | P1 | done | ls/convert/hook.rs:20 :34 :39 :52 :314; lt/fixtures/convert/hook/* | — |
| 3.4 hook Hermes/OpenClaw | — | n-a | doc row is n/a | — |
| 3.4 `NeedsProgram` and script rules | P1 | done | ls/convert/hook.rs:201; lt/convert_security.rs:78 | — |
| 3.5 mcp Registry (npx/uvx/docker, pinned, `cred:`) | P0 | done | ls/convert/mcp.rs:70 :150 :161 :177; lt/fixtures/convert/mcp/mcp-registry | — |
| 3.5 mcp Claude/Codex plugin, Gemini ext, aitmpl | P1 | done | ls/convert/mcp.rs:90 :399 :436; ls/sources/aitmpl.rs:63 | — |
| 3.5 mcp Cline marketplace | P2 | done | ls/convert/mcp.rs:472 (manual draft: stdio, empty command, README link note; refused by the validator until completed); test :545 | — |
| 3.6 prompt: Cursor (.mdc + legacy), Copilot, AGENTS.md family, Cline/Roo/Windsurf | P2 | done | ls/convert/prompt.rs:63 :71; ls/catalogue.rs:286–307; lt/fixtures/convert/prompt/* | Roo/Windsurf have no golden fixture |
| WP5 `scan` risk flags | P0 | done | ls/scan.rs:103; ls/scan_rules.rs:323 :337; lt/convert_security.rs:186 | — |
| WP5 SPDX fingerprints + licence policy | P1 | done | ls/licence_text.rs; ls/licence.rs:63 :249 | — |
| **WP6** `search`/`preview`/`import`/`check_updates`/`refresh`/`update_preview` | P0 | done | core/libsearch.rs:602 :753 :838 :957 :1201 :1321 | — |
| WP6 `guard` / `guard_as` / `guard_actor` | P0 | done | core/libsearch.rs:323 :337 :341 | — |
| WP6 events (`libsearch.*`, `library.*`) | P1 | done | core/libsearch.rs:42–57 | — |
| WP6 ledger kinds: imported, updated, refused, update_checked, source_changed, refreshed | P0 | done | core/libsearch.rs:1089 :294 :1276 :492 :692 | — |
| WP6 store CRUD (provenance, updates, sources) | P1 | done | crates/kls-store/src/libsearch.rs:145 :242 :400; tests :483 :540 | — |
| **WP7** Discover tab: search, type pills + facet counts | P1 | done | ui/library_search.rs:386 (`toggle_type` pre-filters ecosystems), ls/catalogue.rs:477 (`ecosystems_for`); tests ls/catalogue.rs:507, ui/library_search.rs:1937 | — |
| Discover filters (eco, source dots, licence, trust, hide flagged) | P1 | done | ui/library_search.rs:484–596 :1669 | — |
| Discover GitHub budget line (anon n/55 ⏲ / gh ✓) | P2 | done | ui/library_search.rs:1720 | — |
| Discover results table (flags column ⚠/⊘) | P1 | done | ui/library_search.rs:603 :705 :708 | — |
| Preview tabs Source/Converted (editable)/Diff/Notes/Files | P1 | done | ui/library_search.rs:844–960 :889 :917 | The UI does not cap notes at 8 |
| Preview actions Import / Open / Pin | P2 | done | ui/library_search.rs:1026 :1047 :1055 | — |
| Review dialog (ticks gate Import, ⊘ Refused, RunsCode line, MCP argv, licence ack, rename) | P0 | done | ui/library_search.rs:1070 :1119 :1140 :1149 :1179 :1199 | Flag rows show no line number |
| Items tab Origin + Pinned + update chip | P1 | done | ui/library.rs:286 :318–356 | The "update" chip cannot be clicked (the Imported view's "behind" link can: ui/library_search.rs:1550) |
| Imported view: ↻ behind → Diff → update; no auto-update | P1 | done | ui/library_search.rs:1433 :1550 :744; core/libsearch.rs:1321 | — |
| Sources panel (toggle, add tap/well-known, Use gh, skills.sh flag) | P1 | done | ui/library_search.rs:1238 :1357 :1384 :1400 :1416 | — |
| Offline: ⌂ cached/offline chips, ⌂ not cached, checks skipped | P1 | done | ui/library_search.rs:411 :414 :783 :1686; core/libsearch.rs:1246 | — |
| ⊘ Factory stopped bar; Refresh/Import/Check disabled | P0 | done | ui/library_search.rs:362 :319 :1031 :1454 | — |
| Origin chip in editor + Workforce agent card | P1 | done | ui/library.rs:453; ui/library_search.rs:1590; ui/workforce.rs:545 | — |
| Export `source:`/`source_sha256:`; re-import keeps them as a note | P2 | done | core/vault.rs:2462 (`library_file_source`), :2655 (version note); test core/vault.rs:3916 (note kept, no provenance row) | — |
| Background refresh only on Library open, ≤6 h | P2 | done | ui/library_search.rs:238; core/libsearch.rs:62 :608 | — |
| **WP8** `gh_code` search, trees, licence API, gated | P2 | done | ls/sources/gh_code.rs:25 :34 :48 :69; ui/library_search.rs:123; lt/sources.rs:813 | — |
| WP8 toggles on with `gh auth status` | P2 | done | ui/library_search.rs:245; ls/sources/mod.rs:100 | — |
| WP8 belai node `gh` fallback (off by default) | P2 | done (P3, 2026-11-28) | ls/fetch.rs:266 (`NodeGh`), :299 (`gh_args_for`: raw → `gh api -H Accept:…raw repos/{o}/{r}/contents/{path}?ref=`, api.github.com passed through, endpoint last, `..`/space/other hosts refused; codeload tarballs not routed), :357 (`via_node`: only on offline or a parked budget, gate checked first), :459 (`ls_refs` through the node: default branch + `git/refs`); core/libsearch.rs:198 (`NodeGhRoute`: `github` job with `JobNeeds.gh` so the gateway routes by capability, `gh api` only, refused under the kill switch, each fetch chained as `libsearch.node_fetch` with origin `host:<node>`), :186 / :169 (setting `setting:node_gh`, off by default, set via `set_setting` :682), Status :598; UI toggle ui/library_search.rs:1457; tests lt/sources.rs:975, core/libsearch/tests.rs:794 (off by default, routed, 404, audited, kill switch) | — |
| **WP9** proto `Job.kind` comment | P2 | done | proto/keleustes/node/v1/node.proto:485 | — |
| WP9 belai `library` convert job | P2 | done (P2, 2026-11-22) | belai/internal/libraryjob/convert.go:78 (`Parse`: formats, paths, limits, credential names), :170 (`Convert`: private temp dir, `agentimport.ImportItem`, removed); belai/internal/rc/factoryjobs.go:167, :57 (advertised in `factoryJobKinds`); tests belai/internal/libraryjob/convert_test.go, belai/internal/rc/factorylibraryjob_test.go (audited, refused under the kill switch) | — |
| WP9 kls `node_convert` | P2 | done (P2, 2026-11-22) | crates/kls-core/src/libsearch.rs:1505 (guard, node with `library` in `job_kinds`, `gateway().job`, doc re-validated, `library.converted` origin `host:<node>`, each refusal audited); :1187 (`converted_by` from the note, `node:<id>`); tests in crates/kls-core/src/libsearch/tests.rs | No screen calls it yet (library screens are the library owner's). |
| §6 nothing executed/installed; hooks + MCP stored only | P0 | done | core/libsearch.rs:1034; core/libsearch/tests.rs:384 | — |
| §6 `source:"command"`, `${PLUGIN_ROOT}`/`${extensionPath}`, non-https MCP → Refused | P0 | done | ls/sources/github_repo.rs:454; ls/convert/mcp.rs:90 :288; lt/convert_security.rs:62 | — |
| §6 mandatory review (flags + licence ack) | P0 | done | core/libsearch.rs:962–976; core/libsearch/tests.rs:244 | — |
| §6 pinning by SHA/version; commit + both hashes recorded | P0 | done | core/libsearch.rs:1129–1149 :1175 | — |
| §6 marketplace `sha` integrity, mismatch refused | P0 | done | ls/sources/marketplace.rs:117 :127; ls/sources/github_repo.rs:479; lt/sources.rs:168 | — |
| §6 bounded readers (caps, links, devices, `..`, credential names) | P0 | done | ls/git.rs:390 :430 :464 :494 | — |
| §6 host allowlist, https only, redirects in list, response caps | P0 | done | ls/fetch.rs:115 :122 :140 :577 :743–758; tests :972 :984 | — |
| §6 text sanitising (bidi/zero-width, CRLF) | P0 | done | ls/convert/canonical.rs; lt/convert_security.rs:19 | — |
| §6 secrets → `cred:` / SecretLike | P0 | done | ls/convert/mcp.rs:70; ls/scan_rules.rs:323; lt/convert_security.rs:36 | — |
| §6 agents single-mode, fixed tool table, read-only fallback | P0 | done | ls/convert/agent.rs:4 :186 :371 | — |
| §6 upstream signals shown as flags, never trusted | P1 | done | ls/sources/clawhub.rs:50 :243; ls/sources/aitmpl.rs:41 | — |
| §6 prompt injection: flag only, no model call | P1 | done | ls/scan_rules.rs:337 | — |
| §6a.1 provenance row (urls, files, hashes, licence, ack, actor, converter, audit_seq) | P0 | done | core/libsearch.rs:1128–1151; core/libsearch/tests.rs:195 | — |
| §6a.2 audit (actor, subject, payload, ref_key; refusals) | P0 | done | core/libsearch.rs:244 :279 :1100 | — |
| §6a.3 kill switch (stop + pause) before every fetch/write, in-flight abort | P0 | done | core/libsearch.rs:155 :147 :345 :705 :1234; ls/fetch.rs:300 | Done (P1, 2026-11-22): core/libsearch.rs:324 (`stop_window`), :332 (`refused_key`: one remote-search refusal per stop window, other actions once per target), :797 (later queries in the window skip the guard); test core/libsearch/tests.rs:569 |
| §6a.4 A2A: person-only actors; no agent path; A2A keys stripped | P0 | done | core/libsearch.rs:323 :926 :1050; no `libsearch::` callers outside core/ui | — |
| §6a licence detection order | P1 | done | ls/licence.rs:249 (`detection_order`) | — |
| §7 fixtures + FakeFetcher, no network | P1 | done | lt/fixtures/*; lt/sources.rs:16 | — |
| §7 converter goldens + fixed point | P1 | done | lt/convert_golden.rs:102 :127 | — |
| §7 security tests (tar escapes, bomb, 1001 files, bidi, secrets, command source, redirect, http MCP) | P0 | done | ls/git.rs:390–494; lt/convert_security.rs; ls/fetch.rs:984 | — |
| §7 cache/budget (ETag, Retry-After, 55/h, refs unchanged → no tarball) | P1 | done | lt/sources.rs:101 :406; ls/fetch.rs:1025 | — |
| §7 store CRUD on SQLite (+PG when set) | P1 | done | crates/kls-store/src/libsearch.rs:439 :483 | — |
| §7 core: incomplete Ack, hook stores only, update flips behind, events | P0 | done | core/libsearch/tests.rs:244 :384 :437 | — |
| §7 rule 1 test: provenance row + `audit_seq` = ledger seq, `audit_verify` ok | P0 | done | core/libsearch/tests.rs:195–225 | — |
| §7 rule 2 test: each refusal reason writes `library.refused` | P0 | done (P1, 2026-11-22) | core/libsearch/tests.rs:244 (review, refused:, hash_mismatch), :300 (kill_switch), :352 (agent_actor), :532 (`invalid:<detail>` and `invalid:name`) | — |
| §7 rule 3 test: kill switch refuses all 7 actions, zero requests, audited | P0 | done | core/libsearch/tests.rs:300–349; lt/sources.rs:314 | — |
| §7 rule 4 test: agent/run/node actors refused; A2A keys dropped | P0 | done | core/libsearch/tests.rs:352 :410; lt/convert_security.rs:118 | — |
| §7 UI test: Discover pane against a FakeSource | P2 | done | ui/library_search.rs:1852 (FakeSource; search, type pill narrows ecosystems and hits, renders) | — |
| §7 live smoke (`KLS_LIVE_NET=1`, ignored) | P2 | done | lt/live.rs:13–203 | — |

Counts: done 93 · partial 7 · missing 3 · n-a 4 (107 rows). Recounted from the table on 2026-11-28, after the Hermes / OpenClaw converters and the node `gh` fallback: done 104 · partial 0 · missing 0 · n-a 4 (108 rows).
