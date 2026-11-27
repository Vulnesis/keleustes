# Host management: select a belai node, open it, manage everything on it

Design date: 2026-11-21. Status: proposal. Code is unchanged.

The ask: in Fleet, pick a connected belai host and open it. From that view,
manage everything on the host that can be configured, managed or dispatched:

- provisioning
- the Vulnetix products (CLI, package firewall, AI firewall, sensor) and their live status
- schedules
- built-in agents and detections per repo, with deep links to Repos
- config, library, sessions and diagnostics

This doc does not repeat `belai-counterparts.md` (WP-A to WP-K), `sf-parity.md`,
`forges.md` or `builtin-agents.md`. It points to them where it uses them.
Package ids are `HM-n`.

Paths: `belai/…`, `cli/…`, `keleustes/…` are relative to
`/home/chris/GitHub/Vulnesis/`. The other repos are named in full.

---

## 0. Rules this design satisfies (docs/WORKSTREAMS.md "Non-negotiable rules")

| Rule | How every host action meets it |
|---|---|
| 1 Provenance | Each `HostAction` row stores an `origin`: the person (the owner), the surface (`fleet.host/<tab>`), and the plan, run, workflow or template id if any. The origin goes to the node on the wire (`HostAction.origin`). belai writes it into its host audit stream. The Provision history and every action row show it. Imports such as release assets record their URL and sha256. |
| 2 Audit | Every state change of an action is a `host.action.*` event, and the ledger chains each one (§6.4). Refusals by Keleustes and by belai are chained too. belai also writes `host.action.*` lines to its own audit stream, which reaches Keleustes through WP-C's host audit stream. |
| 3 Kill switch | `kls-core::host::request_action` checks `factory_control()` first. If the factory is stopped, every mutating class (`config`, `provision`, `shell`) is refused, the refusal is emitted and audited, and nothing is queued. belai checks again on receipt (`factorylink/control.go` `Stopped`) and refuses. Read-only actions follow §6.3. A provision plan checks before every step, not only at start. |
| 4 A2A | Host actions come from the console only. `Fleet.Request` (WP-I) must refuse every `host_*` kind, so no agent or peer node can manage a host, even when the A2A blackout is off. While the blackout is on, config writes that touch `a2a.*`, `sync.remote_*` or library installs for agents are refused (they are tighten-only). Text Keleustes sends never reaches a model through this path; §6.2 lists the classifier kind for each payload. |
| 5 Tests | Every package lists its rule-1 to rule-4 tests (§8). |

---

## 1. Products found

All three repos are still `vulnetix*` in binary names, paths and environment
variables after the "surface-only rebrand". Hostnames become `*.vulnes.is`
when `VULNETIX_BRAND=vulnesis` is set (`cli/pkg/brand/brand.go:4-15,39-40`).
`/home/chris/GitHub/Vulnetix/` holds a parallel copy of the same repos.

| Product | Repo | Kind | On a host |
|---|---|---|---|
| Vulnetix CLI | `cli` (Go, cobra, `github.com/vulnetix/cli/v4`) | binary | `vulnetix` |
| Package firewall | `package-firewall` (hosted proxy service), configured by `cli/cmd/package_firewall.go` | hosted service + host config | per-ecosystem registry config files |
| AI firewall | `ai-firewall` (hosted LLM gateway), configured by `cli/cmd/ai_firewall.go` | hosted service + host config | base-URL environment variables and client config files |
| Runtime sensor | `cli/cmd/vulnetix-sensor`, `cli/internal/sensor/*`, docs `cli/deploy/sensor/README.md` | Linux daemon | `vulnetix-sensor` + systemd units |

`vdb-hostprobe` is a host-facts library (`vdb-hostprobe/catalogue.go:333` has
a `vulnetix version` probe). Nothing imports it yet. It is not the sensor.
`siem`, `cloudscan` and `malscan-engine` are not host products.

### 1.1 Vulnetix CLI

**Install**
- `cli/install.sh`: installs to `/usr/local/bin/vulnetix`, falling back to `~/.local/bin` (`:5-7,212`), and verifies `checksums.txt` fail-closed (`:348-374`).
- Homebrew: `homebrew-tap/Formula/vulnetix.rb:1-36`.
- Scoop: `scoop-bucket/bucket/vulnetix.json:1-38`.
- Docker and `flake.nix` builds also exist.

**Detect**
- Installed: `exec.LookPath("vulnetix")`.
- Version: `vulnetix version --short` prints the bare version (`cli/cmd/version.go:23-24`).
- Facts: `vulnetix env -o json` gives `cli.version`, `commit` and `build_date` (`cli/cmd/env.go:49-55`).
- Auth: `vulnetix auth status` is text only (`cli/cmd/auth.go:78-174`). `auth verify` checks the credential against the API.
- Credential order (`cli/pkg/auth/credentials.go:96-111,181-201`):
  1. `VULNETIX_API_TOKEN`
  2. `VULNETIX_API_KEY` + `VULNETIX_ORG_ID`
  3. `.vulnetix/credentials.json` in the project
  4. `~/.vulnetix/credentials.json`
  5. the keyring service `vulnetix`

**Upgrade:** `vulnetix update`, which refuses source builds (`cli/cmd/update.go:12-24`). Otherwise `brew upgrade` or `scoop update`.

**Auth without a browser:** `vulnetix auth login --noninteractive --store keyring` with `VULNETIX_API_KEY`/`VULNETIX_ORG_ID` in the environment (`cli/cmd/auth.go:47,71,239-244`).

**belai already has**
- `belai/internal/vulnetixcli/detect.go:59-64`: `Detect`
- `capabilities.go:18-33,66-160`: `Probe` runs `version`, `env`, `auth status` and `auth verify`
- `auth.go:65-125`: `ParseAuthStatus`
- `install.go:24-95`: `DetectInstall` (brew, scoop, winget, go, direct)
- `install_cli.go:44-70`: `PlanInstall`
- `devicelogin.go:256-270`: device login

### 1.2 Package firewall

**Service:** `package-firewall/main.go:60-63,140`. `PORT=3000`, and `GET /health` returns `200 OK`. It is hosted at `packages.vulnetix.com`. Nothing on the host runs it.

**Enable per ecosystem:** `vulnetix package-firewall <eco>` (`cli/cmd/package_firewall.go:56,1060`).
- The ecosystem list is in `cli/pkg/packagefirewall/ecosystem.go:28-53`.
- npm writes `~/.npmrc` (`config.go:51,146-155`).
- pip writes `~/.config/pip/pip.conf` (`:58,158-163`).
- Go writes the netrc (`cli/pkg/auth/netrc.go:12-16`) and `GOPROXY`/`GOAUTH` in the shell rc (`package_firewall.go:33,64-73`).

**Disable:** `vulnetix package-firewall uninstall [--all|--except|--purge|--remove-credentials] [--dry-run]` (`:634,1070-1075`).

**Detect configured:** the proxy host appears in the per-ecosystem files (`cli/pkg/packagefirewall/detect.go:18-95`).
- The "Package Firewall" section of `auth status` lists the configured ecosystems.
- There is no JSON status command. HM-2b adds one.
- Entitlement: `GET <site>/billing/addons` (`cli/cmd/addon_status.go:27-30`).

**belai today:** nothing.

### 1.3 AI firewall

**Service:** `ai-firewall`. `GET /healthz` returns `{"status":"ok"}` and `GET /readyz` returns `ready`, or 503 (`ai-firewall/internal/api/server.go:150-151`, `handlers.go:42-53`). It is hosted at `guardrails.vulnetix.com`. Image: `ghcr.io/vulnesis/ai-firewall` (`.goreleaser.yml:73-95`).

**Host wiring:** `vulnetix ai-firewall install|uninstall|status|get|policy|key|settings|snippet|baseline|inventory|apply|export|kiro` (`cli/cmd/ai_firewall.go:51-63`).
- It sets `ANTHROPIC_BASE_URL`, `OPENAI_BASE_URL` and `GROQ_BASE_URL` (`cli/pkg/aifirewall/provider.go:84-100`).
- Clients it configures (`clients.go:46-187`): the shell rc, `.env`/`.envrc`, `~/.claude/settings.json`, codex, continue and aider.

**Machine status:** `vulnetix ai-firewall status -o json [--strict]` returns `{gateway, providers, guardrails, clients[{id,name,scope,path,state,baseUrl,model}], checks, summary{errors,warnings}}` (`cli/cmd/ai_firewall.go:216-298`).

**belai today**
- `belai/internal/vulnetixcli/aifirewall.go:27-45`: `ai-firewall get -o json`
- `belai/internal/firewall/vulnetix.go:13-158`: the gateway adapter
- Settings `firewall` (`belai/internal/config/settings.go:170`) and `vulnetix.firewall_enabled` (`:543`). Defaults are in `config/firewall.go:85,91`.

### 1.4 Runtime sensor

**Install:** `cli/deploy/sensor/install.sh:43-60` puts the release asset `vulnetix-sensor-linux-{amd64,arm64}`, verified against `checksums.txt`, at `/usr/local/bin/vulnetix-sensor`.

**Enroll:** `sudo vulnetix-sensor enroll --from-cli-login | --api-key/--org-id | --mdm-key …` (`cli/deploy/sensor/README.md:242`). It writes (`README.md:248-258`; `internal/sensor/provision/provision.go:20`; `units.go:12-25,111-117,149-150`):
- `/etc/vulnetix-sensor/credential` (mode 0600)
- `/etc/vulnetix-sensor/config.json`, which the running sensor overrides with `/var/lib/vulnetix-sensor/config.json`
- `vulnetix-sensor.service` (`Type=notify`, `WatchdogSec=30`)
- `vulnetix-sensor-guard.{service,timer}`
- a polkit rule

**Detect**
- Version: `vulnetix-sensor --version` (`main.go:179,278-279`).
- Status: `vulnetix-sensor status` prints text lines (enrolled, profile, protection, mode, config etag, service is-active/is-enabled) and exits 3 when not active (`provision_cmds.go:419-449`).
- `/var/lib/vulnetix-sensor/health.json` holds `{version, at}` (`updatehealth.go:32,62-69`).
- `/var/lib/vulnetix-sensor/update.json` holds the self-update state (`selfupdate.go:33-62`).
- `systemctl is-active` / `is-enabled`, and `vulnetix-sensor selfcheck`.

**Upgrade:** the guard updates the sensor itself every 6 h and rolls back if needed. A profile can set `autoUpdate=false`.

**Remove:** `vulnetix-sensor uninstall [--code]`.

**Root needed** for enroll, uninstall and unit changes. belai does not run as root. See §5.4.

**belai today:** nothing.

### 1.5 How the web console manages them (to mirror)

**Website (Vue):**
- `website/src/pages/resolve/sensor.vue` (`composables/useRuntimeSensors.ts:14`)
- `sensor-provisioning.vue` (`useSensorProvisioning.ts:20-202`)
- `/resolve/package-firewall` and `/resolve/ai-firewall` (`website/src/stores/vdb-auth.ts:4837-5126`, `5161-5242`)

**vdb-site API (`vdb-site/api/main.go`):**
- `/runtime/sensors…` and `/runtime/provisioning/*` (`:1187-1240`)
- `/package-firewall…` (`:1488-1563`)
- `/ai-firewall/*` (`:1570-1615`)
- `/billing/addons` (`:480`)
- Docs: `vdb-site/docs/sensor-provisioning-api.md`; `docs/runtime-sensor-api.md:325-338` (`tamperState`, `updateState`)

**What Keleustes mirrors:** host-side state and host-side actions only. Org policy stays in the Vulnetix console. That covers the AI firewall provider and model lists, package-firewall ecosystem policy, sensor feeds, profiles and MDM. The Products tab links out to each page (`ARROW_SQUARE_OUT`) and never stores Vulnetix org credentials beyond the vault grant in §5.3.

---

## 2. Everything on a belai host that can be managed

Paths are relative to `belai/`. "Remote" means what Keleustes may do. The tier
is the action class from §6.1.

| Aspect | Where (file:line) | Read | Write from Keleustes | Tier | Owner |
|---|---|---|---|---|---|
| belai version, install method, update available | `internal/selfupdate/selfupdate.go:1-7,102,306`; `vulnetixcli.DetectInstall` | yes | upgrade (§5.5) | provision | HM-8 |
| Global settings `<root>/settings.json` | `internal/config/paths.go:14-35`; struct `settings.go:34-310`; `LoadGlobal :2442`, `SaveGlobal :2487` | redacted | allowlisted keys (§5.2) | config | HM-5 |
| Project settings `.vulnetix/settings.json`, `.vulnetix/belai/` | `paths.go:92,123,158`; `LoadProject settings.go:2452` | redacted | no (repo-owned file) | none | HM-5 |
| Project prefs `projectprefs/<key>.json` | `config/projectprefs_keys.go:32-89`; `rc/prefs.go:14,56` | yes | keys in `ProjectPrefs` | config | HM-5 (shares WP-K) |
| Providers, models, labels, routing | `settings.go:36-40,80-101,1350-1371` | names and auth state, never keys | model/effort defaults only | config | HM-5 |
| Provider keys | `factorylink/grant.go:51,134,143`; `internal/credentials/keychain.go:72-112` | present: yes/no | per-dispatch grant only | none | WP-F |
| Agent profiles, crews | `agentprofile/store.go:20`, `crew.go:102,105`; ~55 built-ins `agentprofile/builtin/*.json` (`profile.go:211-213`) | names, hashes | install/backup | config | WP-E |
| Skills, prompts, commands, processes, repos, budgets, rewrites, providers, mcp | `cmd/belai/libcmd.go:26-36`; `internal/libitem`, `libstore` | names, hashes | install/backup | config (hook, mcp: provision) | WP-E |
| Hooks | `settings.go:105,551-559` (`hooks.enabled`, `allowed_programs`) | names | install via WP-E with per-install confirmation. `allowed_programs` is never remote. | provision | WP-E |
| MCP servers | `settings.go:113,660-688`; built-ins `mcp.go:47-70` | names, transport | install via WP-E; secrets via WP-F | provision | WP-E/F |
| Plugins | `internal/plugins/plugins.go:269-283`; `cmd/belai/plugin.go:83-135` | list | no (row 39, by design) | none | HM-11 |
| Trusted dirs, project registry | `projectregistry/registry.go:81-87,115`; `trustgate.go:36,74`; `rc/dirs.go:54` | yes | trust/untrust a dir under `workspace_dirs` | config | HM-5 |
| Workspace dirs, git repos list | `settings.go:175,209`; `gitrepos.go:12-34` | yes | add/enable a repo; clone (§5.6) | provision | HM-7 |
| Worktrees | `fleetpaths.go:30-37`; `cmd/belai/worktreecmd.go:83-89` | list, disk | drop a stale worktree | config | HM-13 |
| Schedules `schedules.json` | `schedule/store.go:44-49`; `rc/scheduler.go`; off in factory mode `cmd/belai/rccmd.go:333-336` | yes | via WP-G | config | WP-G → HM-10 |
| Built-in agent enablement | global only: `agents.enabled settings.go:2535-2559`; project layer tighten-only `:2564` | yes | global switch + new per-repo keys (§5.7) | config | HM-6 |
| Repo detections | `builddetect/detect.go:53`, `lintdetect/detect.go:75`, `testdetect/detect.go:85`; `vulnetix_sweep_enabled/roots settings.go:160-163`, `projectregistry/sweep.go:34`; `vulnetix.subcommands`, `dep_watch` `settings.go:529-547` | yes (new report) | per-repo scanner set, sweep roots | config | HM-6 |
| Worker limits | `--max` (`rc/daemon.go:38`), `--max-workers`, `--idle` (`:42`), `agents.max_workers` (`settings.go:2521`), `factory_reserved_workers`, `maxFactoryJobs` (`rc/factoryjobs.go:50-62`) | yes | `agents.*` limits only; rc flags no | config | HM-5 |
| rc gates (`--web-controls`, `--web-shell`, `--web-project-settings`, `--web-allow-guardrails-off`, `--no-clone`, `--allow-private-cidr`) | `cmd/belai/rccmd.go:120-134` | yes | **never**: these are the host's final gate | none | HM-1 |
| Sandbox | `settings.go:109,718-733`; `sandbox.go:32-34,283,327` | mode, backend found | tighten only (`off`→`auto`→`required`) | config | HM-5 |
| Permissions, posture | `permissions.go:14`; `settings.go:45-59`; `main.go:396-419` | yes | tighten only, unless the host has `--web-allow-guardrails-off` | config | HM-5 |
| Sessions, retention | `session/store.go:66`; `session_retention_days` (default 28) | count, disk | prune now; retention days | config | HM-13 (list: WP-A) |
| Memory | `repomemory.go:155` | yes | via Board memory (done) | config | done |
| Kanban | `factorylink/kanban.go:61` | yes | done | n/a | done |
| Teleport | `cmd/belai/teleportcmd.go`; `teleport.push` | yes | via teleport (done) / WP-I target | n/a | done |
| A2A settings | `settings.go:151` | yes | tighten only; refused under blackout | config | HM-5 |
| Sync settings `sync.*` | `settings.go:155,314-360` | yes | tighten only (`remote_*`, `hooks`) | config | HM-5 |
| rc daemon state `rc/rc.json`, `rc/rc.log` | `rc/record.go:51-59`; `rccmd.go:478` | yes, log tail scrubbed | restart after drain (§5.5) | provision | HM-8/13 |
| Preflight | `rc/preflight.go:118,183,198-230` | yes, structured | rerun | read | HM-13 |
| Audit log | `audit/recorder.go:25,170` | via WP-C | n/a | n/a | WP-C |
| External tools (`gh`, `git`, `bwrap`, `rg`, `llama-server`, `docker`) | `factorylink/host.go:30-40,78`; `forge/commit.go:265`; `machineprobe.go:101` | yes | no | read | HM-1 |
| Factory control on the host `<profile>/control.json` | `factorylink/control.go:29-85` | yes | via the factory kill switch (done) | n/a | done |
| Dispatch a session/worker/crew here | `rc/factory.go:193,201-231`; `rc/workers.go:32` | n/a | existing `NodeGateway::dispatch` | dispatch | done + WP-D |

There is no `belai doctor`. `rc.Preflight` is the closest thing. The self-update code only checks for a new version and never downloads.

---

## 3. UI

Icons are Phosphor, from `kls-ui/src/icons.rs`. Icons marked `+` are new
constants to add: `PACKAGE`, `WRENCH`, `STETHOSCOPE`, `FOLDER_SIMPLE`,
`SLIDERS`, `CLOUD_ARROW_DOWN`, `SHIELD_WARNING`, `RADAR`. Labels are
functional with minimal prose (`ui-copy-style`).

### 3.1 Entry and routing

**Fleet table**
- Clicking a row (the table already senses clicks, `fleet.rs:104`) opens the host.
- A `CARET_RIGHT` button on the right of each row does the same.

**Deep links**
- Format: `keleustes://fleet/<nodeId>[/<tab>][?item=…]`.
- `parse_link` already yields `(Screen::Fleet, Some("<nodeId>/<tab>"))` (`kls-app/src/desktop.rs:85-104`).
- New `Fleet::focus(&mut self, id)` splits node and tab. An unknown tab falls back to `overview`.
- `fleet.rs` is a shared contract file, so this needs the coordinator. The view itself goes in a new file, `kls-ui/src/screens/host.rs`.

**Tab slugs:** `overview`, `products`, `repos`, `agents`, `schedules`, `library`, `config`, `sessions`, `provision`, `diagnostics`, `audit`.

**Repo rows** link to `keleustes://repos/<owner>/<name>` (`Repos::focus`, `screens/repos.rs:114`).

**Session rows** link to `keleustes://sessions/<id>`.

**Audit tab** opens `keleustes://audit?node=<id>`, which needs a `node` query parameter added to `query_focus`.

**Offline host:** the view shows the last inventory with a stale banner. Every mutating button is disabled with the hover text `Offline`.

**Capability gating:** an action the host does not list in `Capabilities.host_actions` is hidden. If it is listed but the host gate level (§6.1) is too low, it shows as disabled with `LOCK_SIMPLE` and the hover text `Host gate: read`.

### 3.2 Frame

```
┌ ⟵ Fleet ─────────────────────────────────────────────────────────────────┐
│ [HARD_DRIVES] build-01   ● Online  linux/amd64  belai 0.42.1 ↑0.43.0     │
│ gate: provision  [LOCK_SIMPLE]shell off   rc ✓ gh ✓ terminal ✓   2s ago  │
│ [ARROW_CLOCKWISE] Refresh  [PLAY] Dispatch  [PENCIL] Rename  [PROHIBIT]  │
├──────────────────────────────────────────────────────────────────────────┤
│ Overview  Products  Repos  Agents  Schedules  Library  Config  Sessions  │
│ Provision  Diagnostics  Audit                                            │
└──────────────────────────────────────────────────────────────────────────┘
```

- `Dispatch` opens the existing dispatch dialog with this node preselected.
- A factory stop banner appears above the tabs (`POWER` Factory stopped: changes blocked).
- An A2A blackout banner appears above the tabs (`BROADCAST` A2A blackout).

### 3.3 Overview

```
┌ Health ──────────────┐ ┌ Products ──────────────────┐ ┌ Load ─────────────┐
│ ✓ link   12ms        │ │ [TERMINAL] CLI     ✓ 4.24.37│ │ sessions 2 / 3    │
│ ✓ preflight 9/9      │ │ [+PACKAGE] Pkg FW  ✓ npm pypi│ │ workers  1 / 4    │
│ ! cert 21d           │ │ [SHIELD] AI FW     ! 1 warn │ │ jobs     0 / 4    │
│ ✓ gh  ✓ git  ✓ bwrap │ │ [+RADAR] Sensor    ✗ absent │ │ disk  sessions 1.2G│
└──────────────────────┘ └────────────────────────────┘ └───────────────────┘
┌ Needs you ───────────────────────────────────────────────────────────────┐
│ [HAND_PALM] sensor enroll needs root   [COPY] Copy command  [CHECK] Done │
│ [WARNING]   belai update available     [ARROW_RIGHT] Provision           │
└──────────────────────────────────────────────────────────────────────────┘
```

- The Load tile comes from WP-B `HostStatus`. Until WP-B lands it shows `—`.

### 3.4 Products

```
Product        State        Version   Latest   Auth / mode        Actions
[TERMINAL] CLI ● active     4.24.37   4.24.40  org a1b2… keyring  [↑ Upgrade] [KEY Sign in] [⋯]
[PACKAGE] Pkg  ● configured —         —        npm ✓ pypi ✓ go ✗  [SLIDERS Ecosystems] [⋯]
[SHIELD] AI FW ● warn       —         —        claude ✓ codex ✗   [SLIDERS Clients] [⋯]
[RADAR] Sensor ○ absent     —         0.9.3    —                  [DOWNLOAD Install] (linux)
                                                    [ARROW_SQUARE_OUT] Console
```

- Expanding a row shows its checks (name, ok/warn/error, a hint of 160 characters or fewer), its paths, its units, `probed 30s ago` and `[ARROW_CLOCKWISE] Probe`.
- **Ecosystems dialog:** checkboxes from the host's ecosystem list, `[EYE] Preview` (dry run, shows file diffs), then `[CHECK] Apply`. Disabling runs `uninstall --except`, and `--purge` is an extra checkbox.
- **Clients dialog:** the same flow for AI firewall clients (scope user or project, per client id).
- **Sensor on non-Linux:** `unsupported`, with no actions.
- **Sensor install** is a two-step hand-off (§5.4).
- `⋯` menu: `Uninstall`, `Sign out` (CLI), `Copy diagnostics`.

### 3.5 Repos

```
[MAGNIFYING_GLASS] filter   [+ Trust dir]  [CLOUD_ARROW_DOWN Clone repo]
Repo                 Path                         Branch  State   Trusted  Agents  Scans        
acme/api ↗           ~/src/api                    main    ✓ clean  ✓        6       sca sast secr
acme/web ↗           ~/src/web                    feat/x  ! dirty  ✓        2       sca
(no remote)          ~/scratch                    —       —        ✗        —       —     [Trust]
```

- `↗` opens `keleustes://repos/acme/api`. The owner and name come from the parsed remote; a repo with no remote has no link.
- Clicking a row opens its Agents & detections pane below (§3.6) with the same repo selected.
- **Row menu:** `Untrust`, `Drop stale worktrees`, `Open in Sessions` (filter by `cwd`), `Dispatch here`.

### 3.6 Agents & detections

```
Repo: [acme/api ▾]   Global: [✓] agents.enabled   max workers [4]
┌ Built-in agents ───────────────────────────── [FUNNEL] all | on | off ┐
│ [✓] belai:vulnetix-scanner   schedule 0 3 * * *   last ✓ 6h   [PLAY] │
│ [✓] belai:deps-javascript    on card              —           [PLAY] │
│ [ ] belai:pr-reviewer        on PR                —                  │
│ …                                                                    │
└──────────────────────────────────────────────────────────────────────┘
┌ Detections ──────────────────────────────────────────────────────────┐
│ build  cargo, npm      lint  clippy, eslint     test  cargo test      │
│ Scans [✓] sca [✓] sast [✓] secrets [ ] iac [ ] containers  [✓] dep watch│
│ Sweep [✓] include in vulnetix sweep                                   │
│ Findings ↗ (Repos → acme/api)                                         │
└──────────────────────────────────────────────────────────────────────┘
                               [EYE] Preview changes   [CHECK] Apply
```

- `PLAY` dispatches the worker now (`Dispatch{kind:"worker", agent, cwd}`).
- The built-in list comes from `builtin-agents.md` P1 (catalogue) and the host's advertised profiles.

### 3.7 Schedules (WP-G)

```
[+ New]  Profile/crew        Cron         TZ    Dir         Enabled  Last    
         belai:vulnetix-...  0 3 * * *    UTC   ~/src/api   [✓]      ✓ 6h   [PLAY Run now] [PENCIL] [TRASH]
         crew:maintenance    0 */6 * * *  UTC   ~/src/web   [ ]      ✗ 2d   …
```

- Local host schedules and factory schedules that target this node are listed together, each with a source tag (`host` or `factory`).
- `Run now` is a dispatch with `trigger.source="schedule"`. It is refused when the factory is stopped.

### 3.8 Library

```
Kind     Name                 Host hash   Factory   State       Actions
profile  belai:repo-onboarder 3fa1…       3fa1…     ✓ current
skill    release-notes        —           9b02…     install     [DOWNLOAD]
hook     pre-commit-lint      77c0…       —         host only   [UPLOAD Backup]
mcp      clef                 builtin     —         host only
plugin   my-plugin 1.2        —           —         read only
```

- These rows come from WP-E. Plugins are read only (row 39).

### 3.9 Config

```
Scope [Global ▾ | Project: acme/api]        sha 4c1e…   [ARROW_CLOCKWISE]
Section     Key                         Value             Remote
Model       model                       claude-…          [edit]
            effort                      high              [edit]
Safety      sandbox.mode                auto              [tighten ▾]
            guardrails                  on                [tighten]
            ask_permission              on                [tighten]
Workers     agents.max_workers          4                 [edit]
Sync        sync.remote_prompts         off               [tighten]
rc gates    --web-controls              on                LOCK host-only
Keys        providers.anthropic         ✓ key present     LOCK vault
                           [EYE] Diff   [CHECK] Save (if sha 4c1e…)
```

- Values never include secrets. A key shows as `✓ present` or `✗`.
- `Raw` (read-only JSON, redacted) is available behind `CODE`.

### 3.10 Sessions

- The Sessions list filtered by `node_id`, linking to `keleustes://sessions/<id>`.
- The live workers and crew runs table comes from WP-B, with pause, resume and stop from WP-D.
- `[TRASH] Prune ended` runs a `session_prune` action.

### 3.11 Provision

```
Plan [Baseline dev host ▾]  [+ New plan]  [COPY Duplicate]
 #  Step                                   Class      Preview      
 1  CLI install (brew)                     provision  brew install vulnetix/tap/vulnetix
 2  CLI sign in (vault: vulnetix/acme)     provision  auth login --noninteractive --store keyring
 3  Pkg FW enable npm, pypi                provision  ~/.npmrc +2 lines, pip.conf +3 lines
 4  AI FW clients: claude-code (user)      provision  ~/.claude/settings.json env +2
 5  Trust ~/src/api                        config     projects.json
 6  Clone acme/web → ~/src/web             provision  git clone --depth 50
 7  Library: profiles set "core"           config     3 install, 1 current
 8  Config: agents.max_workers=4           config     settings.json 1 key
            [EYE] Preview on host     [CHECK] Run (type host name to confirm)
History
 ✓ 2026-11-20 14:02  Baseline dev host   by owner (fleet.host/provision)  8/8   [EYE]
 ✗ 2026-11-19 09:11  Sensor enroll       by owner  1/2 needs root          [EYE]
```

- While a plan runs, each step shows a progress bar and a scrubbed log of 200 lines or fewer.
- `Cancel` stops after the current step.
- `Run` stays disabled until a preview has run on the host and its `preview_sha256` is current.

### 3.12 Diagnostics

```
Preflight  [ARROW_CLOCKWISE] Run      ✓ vulnetix  ✓ credentials  ✓ keleustes  ! guardrails off in ~/x
Link       rtt 12ms  resume_after 4412  cert expires 21d  clock skew +0.2s
Disk       sessions 1.2G  worktrees 3.4G  logs 80M       [TRASH Prune sessions] [TRASH Drop worktrees]
Tools      gh 2.62 ✓ auth  git 2.47  bwrap 0.11  rg 14.1  llama-server —  docker —
Log        rc.log  [lines 200 ▾] [ARROW_CLOCKWISE]  (scrubbed, view only)
           [COPY Copy bundle]  → JSON: inventory + preflight + product checks (no secrets)
```

---

## 4. Protocol (additive; `proto/keleustes/node/v1/node.proto`)

**Field numbers.** belai-counterparts reserves `NodeFrame.body` 13-14,
`ServerFrame.body` 19, `HostAdvert` 10, `Capabilities` 6-9 and `Dispatch`
16-44. Host management therefore uses a disjoint block, **30 and up**. The
coordinator owns the file.

```proto
// Host management. Inventory and progress are node → server unary calls
// (mTLS identity = node id). Actions are replayable ServerFrames, acked by
// the node only after it has filed the first HostActionUpdate.
service Host {
  // Full snapshot: on link, on change (debounced 5 s), and at most every 60 s.
  rpc Report(HostInventory) returns (Ok);
  // Progress and results of a HostAction, in seq order per action.
  rpc Progress(HostActionUpdate) returns (Ok);
}

message ServerFrame {
  oneof body {
    // ... existing 10..18, WP-J 19
    HostAction host_action = 30;
  }
}

message Capabilities {
  // ... existing 1..5, WP-D 6..9
  // HostAction kinds this belai executes (closed list, §6.1).
  repeated string host_actions = 30;
  // Host gate: "off", "read", "config", "provision". From the rc flag
  // --keleustes-manage, never from settings.
  string manage_level = 31;
  // --keleustes-shell given (HM-15).
  bool manage_shell = 32;
}

message HostInventory {
  int64 at_ms = 1;
  BelaiFacts belai = 2;
  repeated ProductStatus products = 3;
  repeated HostRepo repos = 4;
  repeated ToolFact tools = 5;
  repeated Check preflight = 6;
  HostLimits limits = 7;
  RcGates gates = 8;
  DiskUsage disk = 9;
  // sha256 of settings.json, for config_set if_match.
  string settings_sha256 = 10;
  // Names only. Never env, args or values.
  repeated string plugins = 11;
  repeated McpServerFact mcp = 12;
  repeated ProviderFact providers = 13;
  SandboxFact sandbox = 14;
}

message BelaiFacts {
  string version = 1;
  string install_method = 2;   // brew, scoop, winget, go, direct
  string latest = 3;           // from selfupdate cache, may be empty
  bool update_available = 4;
  string root = 5;             // $BELAI_HOME, home-relative ("~/.vulnetix/belai")
}

message ProductStatus {
  // "cli", "package_firewall", "ai_firewall", "sensor".
  string product = 1;
  // "absent", "installed", "configured", "active", "degraded", "error",
  // "unsupported".
  string state = 2;
  string version = 3;
  string latest = 4;
  string path = 5;             // home-relative where possible
  string install_method = 6;
  bool authenticated = 7;
  string org_id = 8;           // identifier only
  string auth_source = 9;      // "env", "project", "home", "keyring"
  bool running = 10;           // sensor service active
  bool enabled = 11;           // sensor unit enabled
  string mode = 12;            // sensor: monitor/guard/enforce
  repeated Toggle parts = 13;  // pkg FW ecosystems, AI FW clients
  repeated Check checks = 14;
  int64 probed_at_ms = 15;
  repeated string degraded = 16; // probe steps that failed
  repeated string units = 17;    // "vulnetix-sensor.service: active/enabled"
}

message Toggle {
  string id = 1;        // "npm", "pypi", "claude-code"
  string label = 2;
  string scope = 3;     // "user", "project"
  bool on = 4;
  string path = 5;      // config file it lives in
  string state = 6;     // AI FW client state as reported
}

message Check {
  string id = 1;
  string status = 2;    // "ok", "warn", "error", "skip"
  string hint = 3;      // ≤160 chars, harness-composed, no tool output
}

message HostRepo {
  string path = 1;                // home-relative
  string remote = 2;              // credentials stripped
  string forge_host = 3;
  string owner = 4;
  string name = 5;
  string branch = 6;
  GitFacts git = 7;               // WP-A message
  bool trusted = 8;
  bool registered = 9;
  int64 last_used_ms = 10;
  RepoDetections detections = 11;
  // Effective per-repo agent switches (HM-6) and prefs (ProjectPrefs keys).
  repeated Toggle agents = 12;
  map<string, string> prefs = 13;
}

message RepoDetections {
  repeated string build = 1;     // builddetect
  repeated string lint = 2;      // lintdetect
  repeated string test = 3;      // testdetect
  repeated string languages = 4;
  repeated string scans = 5;     // vulnetix subcommands enabled for this repo
  bool dep_watch = 6;
  bool in_sweep = 7;
  int64 last_scan_ms = 8;
}

message ToolFact { string name = 1; string version = 2; bool ok = 3; string note = 4; }
message HostLimits {
  uint32 max_sessions = 1; uint32 max_workers = 2; uint32 reserved_workers = 3;
  uint32 max_jobs = 4; uint32 idle_s = 5;
}
message RcGates {
  bool web_controls = 1; bool web_shell = 2; bool web_project_settings = 3;
  bool allow_guardrails_off = 4; bool no_clone = 5; bool allow_private_cidr = 6;
}
message DiskUsage { int64 sessions = 1; int64 worktrees = 2; int64 logs = 3; int64 free = 4; }
message McpServerFact { string name = 1; string transport = 2; bool builtin = 3; repeated string keys_bound = 4; }
message ProviderFact { string provider = 1; bool key_present = 2; string source = 3; }
message SandboxFact { string mode = 1; string network = 2; string backend = 3; }

message HostAction {
  string id = 1;
  // Closed list, §6.1. Unknown kinds are refused.
  string kind = 2;
  // Parameters as flat typed keys checked per kind on the host. No argv,
  // no paths outside allowed roots, no free text that reaches a model.
  map<string, string> params = 3;
  bool dry_run = 4;
  // For a non-dry-run: sha256 of the preview the person confirmed. belai
  // recomputes the plan and refuses if it differs ("preview stale").
  string preview_sha256 = 5;
  // config_set: the settings_sha256 the edit was made against.
  string if_match = 6;
  // product_auth only: one-shot, memory-only, env of the child process.
  VaultGrant vault = 7;
  Origin origin = 8;
  int64 deadline_ms = 9;
  // A plan run groups steps; belai runs one action at a time per host.
  string plan_run_id = 10;
  uint32 step = 11;
}

message Origin {
  string actor = 1;        // "owner" (desktop owner); later person ids
  string surface = 2;      // "fleet.host/products", "plan:<id>", "workflow:<id>"
  string run_id = 3;
  string workflow_id = 4;
  string plan_id = 5;
  string reason = 6;       // typed by the person, ≤200 chars, audit only
}

message HostActionUpdate {
  string action_id = 1;
  uint32 seq = 2;
  // "accepted", "refused", "running", "needs_human", "done", "failed",
  // "cancelled".
  string state = 3;
  string step = 4;
  uint32 pct = 5;
  // Scrubbed (vaultenv), ≤200 lines/update, ≤64 KiB total per action.
  repeated string lines = 6;
  // dry_run: the plan (argv shown to the person, file diffs) and its hash.
  string preview_json = 7;
  string preview_sha256 = 8;
  // Result of read kinds (config_get, log_tail, preflight) as JSON.
  string result_json = 9;
  string error = 10;
  // needs_human: exact command for a person to run on the host, and why.
  string handoff_command = 11;
  string handoff_reason = 12;   // closed list: "needs_root", "needs_browser"
}
```

Version skew: an old belai never lists `host_actions`, so the Host view shows
inventory from `HostAdvert` only and every action is hidden.

---

## 5. Host-side behaviour (belai)

### 5.1 New code

- **`internal/hostmgmt/`** (new):
  - `inventory.go` collects `HostInventory`.
  - `actions.go` holds the closed kind table: kind → class, param validator, planner, executor.
  - `recipes.go` holds the product recipes as fixed argv. Paths below are relative to `belai/`.
- **`internal/products/`** (new) has one prober per product:
  - `cli.go` wraps `vulnetixcli.Probe`.
  - `pkgfw.go` reads the files in the `cli/pkg/packagefirewall/detect.go` list until the CLI has JSON status (HM-2b), then calls `vulnetix package-firewall status -o json`.
  - `aifw.go` runs `vulnetix ai-firewall status -o json`.
  - `sensor.go` runs `vulnetix-sensor --version` and `vulnetix-sensor status` (exit 3 means inactive), reads `health.json` and `update.json`, and runs `systemctl is-active` and `is-enabled`. Each probe is capped at 10 s, and probe results are cached for 60 s.
- **Factorylink wiring:**
  - `factorylink/host.go` gets `Host.Report`.
  - `factorylink/rc.go` gets `RCHooks.HostAction` and routes `ServerFrame.host_action` the same way as `Dispatch` (`rc.go:65-77`).
  - `rc/factory.go:107-125` (`factoryAdvert`) fills `host_actions` and `manage_level`.
- **rc flags** (`cmd/belai/rccmd.go:120-134`):
  - `--keleustes-manage=off|read|config|provision` (default `read`)
  - `--keleustes-shell` (default off)
  - Both are recorded in `rc/rc.json`. No setting and no remote action can change them.
- **Execution:**
  - Actions run one at a time per host, in a queue that survives a restart: `rc/hostactions/<id>.json`, so a replayed frame is de-duplicated by id.
  - The ack is sent after the first `HostActionUpdate` is filed.
  - Every refusal is an update with `state=refused`, so it is also audited.

### 5.2 Config writes (`config_get`, `config_set`)

- **`config_get`:**
  - Returns the merged view as JSON for the global scope or a project path.
  - Strips `providers.*.api_key*`, MCP `env` values, credentials and tokens. A secret key comes back as `{"present":true}`.
- **`config_set`:**
  - Takes `params{key=value}` for keys in a new `config.RemoteKeys` table (sibling of `ProjectPrefs`). The table gives each key a type, a range and a mode: `set` or `tighten`.
  - The edit must match `if_match`. On a mismatch belai refuses with "settings changed on host".
  - belai writes with `SaveGlobal` (`settings.go:2487`), keeping a backup at `settings.json.bak.<action-id>`.
- **Never remote:**
  - anything `dangerously*`
  - `hooks.allowed_programs`
  - `mcp.servers` (WP-E only)
  - `providers.*.base_url`, `api_key_env` and `host`, because they could route keys elsewhere
  - `forges`, `permissions.allow`, `classifier.*`
  - `update_check`
  - all rc gates
- **Tighten-only keys** are `sandbox.mode`, `guardrails`, `ask_permission`, `read_only`, `sync.remote_*`, `sync.hooks` and `a2a.*`. Loosening one is refused unless the host runs with `--web-allow-guardrails-off`. `a2a.*` stays tighten-only even then.
- **Project prefs** go through `rc/prefs.go` and `ProjectPrefs` unchanged, and still need `--web-project-settings`. If WP-K lands first, `project_prefs_set` calls the same function. Do not build two paths.

### 5.3 Products: allowlisted recipes

Each recipe is a fixed program plus an argv template whose parameters are
validated enums, so no shell is involved. Every recipe has a dry run that returns
`preview_json` (argv and file diffs) and its sha256.

| Kind / product | Recipe |
|---|---|
| `product_install cli` | `brew install vulnetix/tap/vulnetix` or `scoop install vulnetix`, chosen by host (`vulnetixcli.PlanInstall`). Direct install: belai downloads the release asset and `checksums.txt` from the pinned GitHub release URL, verifies the sha256, and writes the binary to `~/.local/bin/vulnetix` (never `/usr/local` without root). `install.sh` is never piped to a shell. Provenance is the URL plus sha256. |
| `product_upgrade cli` | `vulnetix update`; brew and scoop upgrade by install method |
| `product_auth cli` | `vulnetix auth login --noninteractive --store keyring`, with `VULNETIX_API_KEY` and `VULNETIX_ORG_ID` from `HostAction.vault`, set in the child's environment only (never argv). The grant is dropped on exit. Fallback: `needs_browser` hand-off to `vulnetix auth login`. |
| `product_uninstall cli` | `vulnetix auth logout` (optional), then the package manager's uninstall, or removal of the direct-installed binary |
| `product_configure package_firewall` | `vulnetix package-firewall <eco>` for each `on`. `vulnetix package-firewall uninstall --except <on…>`. Optional `--purge` and `--remove-credentials`. The dry run uses the CLI's `--dry-run` plus a belai file diff. |
| `product_configure ai_firewall` | `vulnetix ai-firewall install --client <id> --scope <user\|project>` and `uninstall --client <id>`. The exact client flags must be checked against `cli/cmd/ai_firewall.go` (open item O1). A project scope is allowed only for a trusted repo path. |
| `product_install sensor` | Linux only. belai can do the download and verify step to a staging path. The root steps (`install`, `enroll`, unit enable) are a `needs_root` hand-off (§5.4). |
| `product_upgrade sensor` | The sensor updates itself. The action only runs `vulnetix-sensor guard-check` (no root) and reports. If `autoUpdate=false`, it hands off (`needs_root`). |
| `product_uninstall sensor` | `needs_root` hand-off: `sudo vulnetix-sensor uninstall`. A locked profile (the probe's `protection` check) refuses without the one-time code the console makes for that device. The host screen asks for it, and `code=XXXX-XXXX-XXXX` adds `--code` to the hand-off; the plan's params record only that a code was given. |
| `product_probe <p>` | read: rerun the prober and Report |

### 5.4 Root and browser hand-offs

belai never runs `sudo`. When a step needs root, it returns
`state=needs_human`, `handoff_reason=needs_root`, and the exact command. For
example: `sudo vulnetix-sensor enroll --from-cli-login --profile <id>`, where
the profile is a validated identifier.

Keleustes shows the command with `COPY` and offers `CHECK Done`, which
re-probes the host. The action settles `done` only when the probe confirms the
expected state. If it does not, the action settles `failed`.

Opt-in later (O2): a sudoers rule that allows only the exact command, documented and never written by belai.

### 5.5 belai self-upgrade and restart (`belai_upgrade`, `rc_restart`)

- **Upgrade method:** `brew upgrade belai`, `scoop update belai`, or `go install …@v<ver>` (pinned version). A direct install downloads the release asset with checksum, replaces the binary atomically, and keeps `belai.prev`.
- **Restart:** the daemon re-execs itself. Before that it drains: new dispatches are refused with "restarting", and it waits until running sessions and workers reach 0 or `params.force_after_s` passes.
- **Force:** the force path needs its own confirmation in Keleustes.
- **Version check:** after the restart, the new link's `Hello.belai_version` must equal the target, or the action is `failed`. A direct install then rolls back to `belai.prev` (supervised by the old binary's exit hook).

### 5.6 Repos (`repo_trust`, `repo_untrust`, `repo_clone`, `worktree_drop`)

- **Trust:** a path is trusted only under `workspace_dirs`, and only for an existing git repository. `trustgate.Grant` (`trustgate.go:74`) does the write.
- **Clone:**
  - Needs a `forge_host`/`owner`/`name` triple whose forge host the host's `forges` settings or `gh` already know. That gives an https or ssh URL with no credentials in it.
  - The target is `<workspace_dir>/<name>`. `--depth` is optional.
  - Clone is refused when the host runs with `--no-clone`.
  - The clone runs through the existing `rc/clone.go`.

### 5.7 Per-repo built-in agents and detections (new in belai)

belai has no per-repo switch today. Only `agents.enabled` exists, and the project layer can only tighten it (`settings.go:2535-2564`).

New `ProjectPrefs` wildcard keys:
- `agents.builtin.<profile>`: `on`/`off`, default inherit
- `vulnetix.scans`: a subset of the CLI's scanner subcommands
- `vulnetix.dep_watch`
- `vulnetix.sweep`

**Enforcement**
- `rc/workers.go:32` (`startWorkers`) and the scheduler refuse a built-in profile that is `off` for the target cwd, with the reason `disabled for repo`.
- `projectregistry/sweep.go:34` skips repos with `sweep=off`.

**Reporting:** `HostRepo.detections` is filled from `builddetect`, `lintdetect` and `testdetect`, run lazily and cached by HEAD commit.

### 5.8 Diagnostics (`preflight`, `log_tail`, `session_prune`)

- **`preflight`** returns `rc.Preflight` (`rc/preflight.go:118`) as `Check[]`.
- **`log_tail`** returns up to 1000 lines of `rc/rc.log`, scrubbed by `vaultenv`, as `result_json`. The UI only displays it, and it is never sent to a model.
- **`session_prune`** applies `session_retention_days` now and reports the bytes freed.

---

## 6. Keleustes side

### 6.1 Action classes and approvals

| Class | Kinds | Host gate (`manage_level`) | Keleustes confirmation |
|---|---|---|---|
| read | `inventory_refresh`, `product_probe`, `config_get`, `preflight`, `log_tail` | ≥ read | none; audited |
| config | `config_set`, `project_prefs_set`, `repo_trust`, `repo_untrust`, `worktree_drop`, `session_prune`, `agents_set` (HM-6) | ≥ config | dialog with the preview diff and a `Save` click |
| provision | `product_install`, `product_upgrade`, `product_uninstall`, `product_configure`, `product_auth`, `belai_upgrade`, `rc_restart`, `repo_clone` | provision | host-side dry run, then a preview, then the host name typed to confirm; `preview_sha256` must match |
| shell | `shell_exec` (HM-15) | provision + `--keleustes-shell` | typed confirmation per command; argv shown in full; 60 s cap; output ≤ 64 KiB; off by default |
| dispatch | existing session/worker/crew | rc running | existing |

**Library installs** (WP-E) map to config, except hooks and MCP servers, which map to provision with a per-item confirmation (WP-E risk note).

**The two gates are joined with AND.** The host gate is the final gate, and Keleustes policy only adds to it (cross-cutting risk in `belai-counterparts.md`).

### 6.2 Classifier kinds (belai side, no new kind needed)

| Inbound payload | Treatment on the node |
|---|---|
| `HostAction.params` | Typed validators per kind: enums, ints, paths resolved and kept under allowed roots, identifiers matched against `^[A-Za-z0-9._:/@-]{1,128}$`. No param is model-facing. |
| `config_set` values | `config.RemoteKeys` type check, the same as `ProjectPrefs` |
| `project_prefs_set` | `config.ProjectPrefs` (unchanged) |
| `Origin.reason` | Audit only, capped, never shown to a model (`KindAudit`-style: stored, not rendered to models) |
| Library docs (WP-E) | `libstore.UntrustedText` plus the kind's validator (`KindSkill` and the others) |
| Dispatch prompt from the Host view | existing `CleanPrompt` plus classifier |

**Outbound to Keleustes:** inventory strings are capped, and paths are made home-relative. Check hints and lines are harness-composed or `vaultenv`-scrubbed. Nothing in `HostInventory` or `HostActionUpdate` carries a secret value. A test asserts this with a canary secret.

### 6.3 Kill switch and A2A, per class

| Factory state | read | config | provision / shell | Plans in flight |
|---|---|---|---|---|
| running | allowed | allowed | allowed | run |
| **stopped** | allowed, audited with `control:"stopped"` (diagnosis during an incident) | refused + event + audit | refused + event + audit; belai refuses too (`control.json`) | stop before the next step; the current step is cancelled if its recipe allows it (downloads), otherwise it settles |
| A2A blackout | allowed | refused for `a2a.*`, `sync.remote_*` and agent library installs; others allowed | allowed unless the step installs agents or MCP servers | as left |
| `Fleet.Request` (WP-I) of any `host_*` kind | refused | refused | refused | n/a |

Decision O3: if the owner wants reads blocked under stop as well, it is one
flag in `kls-core::host::policy`. Each refusal is audited either way.

When a future engine `provision` step (HM-9b) lands, it follows the
engine's kill-switch path (`kls-engine/src/exec.rs:643,654`) and pause/stop
modes (`sf-parity.md`, "Kill switch, holds, windows").

### 6.4 Events and audit

**Events** (`kls-core/src/host.rs`, new file):
- `host.inventory` (on change only)
- `host.action.requested`, `host.action.refused`, `host.action.queued`, `host.action.accepted`, `host.action.needs_human`, `host.action.finished` (`data.state` is done, failed or cancelled)
- `host.plan.started`, `host.plan.finished`

**Ledger** (`kls-core/src/audit.rs:142`): `ledger_kind_for_event` gains these mappings.
- `host.action.requested|refused|queued|finished|needs_human` → `("host_action", "user")` when the origin is a person
- `host.action.accepted` and host-side refusals → `("host_action", "host")`
- `host.plan.*` → `("host_plan", "user")`
- `PREFIXES` gains `"host."`

**Entry contents:** `{actionId, node, kind, class, origin, paramsDigest, previewSha256, outcome, reason}`.
- Params are stored as a digest plus non-secret values.
- The vault grant appears as its grant id only (the existing `vault.granted` entry covers the secret access).

### 6.5 Store (migration `0120_host_management.sql`, sqlite + postgres)

```sql
CREATE TABLE host_inventory (node_id TEXT PRIMARY KEY, at BIGINT NOT NULL, json TEXT NOT NULL);
CREATE TABLE host_actions (
  id TEXT PRIMARY KEY, node_id TEXT NOT NULL, kind TEXT NOT NULL, class TEXT NOT NULL,
  params_json TEXT NOT NULL, dry_run BOOLEAN NOT NULL, preview_sha256 TEXT NOT NULL DEFAULT '',
  origin_json TEXT NOT NULL, plan_run_id TEXT NOT NULL DEFAULT '', step INTEGER NOT NULL DEFAULT 0,
  state TEXT NOT NULL, requested_at BIGINT NOT NULL, delivered_at BIGINT, finished_at BIGINT,
  error TEXT NOT NULL DEFAULT '', result_json TEXT NOT NULL DEFAULT '',
  handoff_json TEXT NOT NULL DEFAULT '');
CREATE INDEX host_actions_node ON host_actions(node_id, requested_at);
CREATE TABLE host_action_lines (action_id TEXT NOT NULL, seq INTEGER NOT NULL, at BIGINT NOT NULL,
  line TEXT NOT NULL, PRIMARY KEY (action_id, seq));
CREATE TABLE host_plans (id TEXT PRIMARY KEY, name TEXT NOT NULL, steps_json TEXT NOT NULL,
  version BIGINT NOT NULL, created_at BIGINT NOT NULL, updated_at BIGINT NOT NULL);
CREATE TABLE host_plan_runs (id TEXT PRIMARY KEY, plan_id TEXT NOT NULL, plan_version BIGINT NOT NULL,
  node_id TEXT NOT NULL, origin_json TEXT NOT NULL, state TEXT NOT NULL, step INTEGER NOT NULL,
  started_at BIGINT NOT NULL, finished_at BIGINT);
```

- The new code goes in `kls-store/src/host.rs`, a new owned file.
- Retention: action lines are kept 30 days (a sweeper next to the teleport one). Action rows are kept forever, as the audit index.

### 6.6 kls-node

- **`kls-node/src/host.rs`** (new owned file):
  - `Host.Report` upserts `host_inventory` and emits `host.inventory` when the snapshot hash changes.
  - `Host.Progress` checks that the action is addressed to this node, enforces monotonic seq and appends to the store.
- **`gateway.rs`:** `send_host_action(node, HostAction)` is the durable outbox, the same as dispatch (`gateway.rs:38` delivery sweeper).
- **`server.rs`:** router wiring only (coordinator).

### 6.7 kls-core

**`kls-core/src/host.rs`** (new):
- `request_action(node, kind, params, dry_run, origin) -> ActionId` checks, in this order:
  1. kill switch
  2. A2A
  3. node online
  4. the kind is listed in `host_actions`
  5. `manage_level` is high enough
  6. per-class policy
  7. `preview_sha256` is current

  It then audits and queues.
- `confirm(action_id_of_preview, typed_name)` creates the real action.
- Plans: `run_plan(plan, node)` runs the steps in sequence and re-checks the kill switch before each one.
- Grants: `vault.grant_for_purpose("vulnetix_auth", node)` needs a new vault secret kind `vulnetix` (`api_key` + `org_id`), with a 5 min TTL.
- `NodeGateway` (shared) gains `host_action(node, HostAction) -> Result<String>`. This needs the coordinator.

### 6.8 kls-ui

- `screens/host.rs` (new) holds the tabs in §3. The tab state goes in `Fleet`.
- `fleet.rs` (coordinator) adds row click, `focus` and the `CARET_RIGHT` button.
- `loader.rs`/`model.rs` (coordinator) load `host_inventory` and `host_actions` for the open node. The view reloads on `host.*` events.
- The icon constants in §3 go in `icons.rs`.

---

## 7. Work packages

`dep:` lists hard dependencies. `soft:` means the package works without it and gets better once it lands.

| WP | Scope | Repos | dep | soft |
|---|---|---|---|---|
| **HM-1** Inventory | proto `Host.Report`, `HostInventory` (minus products), `Capabilities.host_actions/manage_level`; belai `hostmgmt/inventory.go`, `--keleustes-manage` flag; kls-node `host.rs`; kls-store `host_inventory`; kls-core event | proto, belai, kls-node, kls-store, kls-core | none | WP-B (load), WP-E (library inventory) |
| **HM-2** Product probes | `internal/products/*` for the 4 products; `ProductStatus` | belai | HM-1 | none |
| **HM-2b** CLI JSON status | `vulnetix package-firewall status -o json`, `vulnetix-sensor status -o json`, `vulnetix auth status -o json`; fix the brew test string "Vulnesis CLI" vs code "Vulnetix CLI" (`homebrew-tap/Formula/vulnetix.rb:35`) | cli, homebrew-tap | none | none |
| **HM-3** Host view (read) | routing, frame, Overview, Products, Repos (read only), Config (read), Diagnostics (read); deep links and tests | kls-ui, kls-app | HM-1 (build against fake) | HM-2 |
| **HM-4** Action pipeline | `HostAction` frame, `Host.Progress`, store tables, kls-core policy (kill switch, A2A, gate, classes), audit kinds, confirm/preview, outbox and replay; belai queue, de-duplication and refusals; read kinds `inventory_refresh`, `product_probe`, `preflight` | all | HM-1 | none |
| **HM-5** Config and trust | `config.RemoteKeys`, `config_get/set` with `if_match`, tighten-only rules, `project_prefs_set`, `repo_trust/untrust`; UI Config tab edits | belai, kls-core, kls-ui | HM-4 | **WP-K** (share `rc/prefs.go`; whichever lands second reuses it) |
| **HM-6** Per-repo agents and detections | new `ProjectPrefs` keys, enforcement in `startWorkers`/scheduler/sweep, `RepoDetections`; Agents tab | belai, kls-ui | HM-5 | builtin-agents **P1** (catalogue), **WP-D** (worker dispatch fields) |
| **HM-7** Product provisioning | recipes §5.3, dry-run previews, hand-offs §5.4, `product_auth` with the vault `vulnetix` kind; Products tab actions | belai, kls-core vault, kls-ui | HM-2, HM-4 | HM-2b |
| **HM-8** belai upgrade and restart | §5.5 | belai, kls-ui | HM-4 | WP-B (drain uses live counts) |
| **HM-9** Provision plans | plans, plan runs, preview-all, typed confirm, history; Provision tab | kls-store, kls-core, kls-ui | HM-5, HM-7 | HM-8, HM-11 |
| **HM-9b** Engine `provision` step | workflow node that runs a plan on a node | kls-engine | HM-9 | sf-parity kill-switch pause/stop modes |
| **HM-10** Schedules tab | host plus factory schedules per node, run now, enable, edit | kls-ui | **WP-G** | **WP-C** (scheduler runs in factory mode) |
| **HM-11** Library tab | host inventory against factory, install/backup, diverged | kls-ui | **WP-E** | **WP-F** (MCP secrets) |
| **HM-12** Sessions and workforce tab | per-node sessions, live workers, pause/resume/stop | kls-ui | **WP-A**, **WP-B**, **WP-D** | none |
| **HM-13** Diagnostics actions | `log_tail`, `session_prune`, `worktree_drop`, disk, link facts, copy bundle | belai, kls-ui | HM-4 | none |
| **HM-14** Needs-you and notifications | hand-offs and update-available in `needs_you` and the tray | kls-core, kls-app | HM-7 | **WP-J** |
| **HM-15** Shell (opt-in) | `shell_exec` behind `--keleustes-shell` and the provision gate; full argv audit | belai, kls-core, kls-ui | HM-4 | none; lowest priority |
| **HM-16** E2E suite | §8.2 | belai e2e, justfile | HM-4 + whichever WPs it covers | none |

**Order:**
1. HM-1, HM-2 and HM-2b (in parallel)
2. HM-3 and HM-4
3. HM-5, HM-7 and HM-13
4. HM-6, HM-8 and HM-9
5. HM-10, HM-11 and HM-12 as their belai-counterparts packages land
6. HM-14
7. HM-15 last

Packages that can start now, independent of belai-counterparts: HM-1 to HM-5, HM-7, HM-8 and HM-13.

---

## 8. Tests

### 8.1 Automated, no network (every PR)

**Fake belai host (Rust, `kls-node/src/tests.rs` harness):** a test node joins, links, reports a canned `HostInventory`, and answers `HostAction` with scripted updates.
- Inventory is stored and the event fires only on change.
- Progress for an action addressed to another node gets `PermissionDenied`.
- A replayed `HostAction` after reconnect is delivered once and de-duplicated.
- A non-monotonic seq is refused.

**kls-core, per class (rules 1-4):**
- An action is refused under `stopped` with an event and an audit entry. A read is allowed under `stopped` and its audit entry has `control:"stopped"`.
- A `config_set` of `a2a.*` is refused under blackout.
- `Fleet.Request` of any `host_*` kind is refused (with WP-I).
- Origin is stored and shown in the audit `subject`.
- A preview hash mismatch is refused.
- A kind not in `host_actions` is refused before queueing.
- `manage_level=read` refuses config.
- A plan stops between steps when the kill switch flips.

**belai Go (`factorylink/fake_test.go`, `hostmgmt/*_test.go`):**
- Gating and flags:
  - An unknown kind is refused.
  - Params outside allowed roots are refused (path traversal, symlink escape).
  - `--keleustes-manage=off` refuses everything.
  - Refused while `control.json` is stopped.
- Config writes:
  - `RemoteKeys` tighten-only rules hold.
  - Stale `if_match` is refused.
  - `providers.*.base_url` is refused.
- Secrets and grants:
  - A secret canary in settings, environment and credentials never appears in `HostInventory`, `config_get` or `log_tail`.
  - The `product_auth` grant reaches the child's environment only, and is gone after exit.
- Recipes and probes:
  - Each recipe's dry run is deterministic: same inputs, same `preview_sha256`.
  - The product probers run against fixture binaries (shell stubs that print the real output formats from §1, including exit 3 for sensor status).
- Per-repo enforcement:
  - `agents.builtin.X=off` refuses `startWorkers` for that cwd.
- Upgrade and restart:
  - `rc_restart` drains, then re-execs.
  - A version mismatch after restart fails the action.

**kls-ui:**
- `parse_link("keleustes://fleet/n1/products")` → `(Fleet, "n1/products")`.
- `Fleet::focus` opens the tab.
- A repo row link builds `keleustes://repos/acme/api`.
- Gated actions are hidden or disabled per capability.

### 8.2 E2E with real binaries (`just e2e-host`, nightly, containers)

This extends `justfile:31-38`. It runs `belai/e2e/keleustes_test.go`-style tests with `KELEUSTES_BIN` against a headless Keleustes.

1. **Inventory:** `belai rc --keleustes-manage=provision` joins. The test asserts that the inventory shows belai's version and the tools.
2. **CLI:** a real `vulnetix` from the pinned release runs in a temp `HOME`.
   - The probe shows `installed` and `authenticated=false`.
   - `product_auth` with a test org key from CI secrets gives `authenticated=true`. A no-secret variant asserts the `needs_browser` hand-off.
3. **Package firewall:** `product_configure` for npm and pypi in the temp `HOME`. The test asserts the diffs in `~/.npmrc` and `pip.conf` and the probe state. Disabling one restores the file.
4. **AI firewall:** client claude-code in the temp `HOME`. The test asserts that `ai-firewall status -o json` shows the client `on`.
5. **Sensor:** the test runs in a systemd container (privileged, nightly only).
   - Install returns `needs_root`.
   - The test runs the hand-off command as root.
   - `Done` re-probes and shows `active`, then uninstall runs.
6. **Config:** `config_set agents.max_workers=2`. `settings.json` changes, and a stale `if_match` is refused.
7. **Repos:** clone a local bare repo exposed as a forge fixture, then trust it. The inventory shows it with owner and name.
8. **belai upgrade:** direct install from a local release fixture with `checksums.txt`. The link comes back with the new version. A bad checksum is refused.
9. **Rules:** with the kill switch on, every mutating kind is refused on both sides, and the audit chain verifies with all `host.action.*` entries.

### 8.3 Manual (release checklist)

- On macOS (brew) and Windows (scoop), install and upgrade the CLI and belai. The sensor shows `unsupported`.
- Run a full Baseline dev host plan on a fresh VM. Check that the history and origin show in Audit.

---

## 9. Open items

- **O1:** Check the exact `vulnetix ai-firewall install/uninstall` client and scope flags, and `package-firewall --dry-run` coverage per ecosystem, in `cli/cmd/ai_firewall.go` and `package_firewall.go`.
- **O2:** Sensor root steps. Decide whether hand-off only is enough, or whether a documented exact-command sudoers rule is wanted.
- **O3:** Reads while the factory is stopped. The default is allowed and audited. A single flag can block them.
- **O4:** Package-firewall and AI-firewall entitlement (`/billing/addons`). Keleustes holds no Vulnetix session, so it shows the host CLI's answer. Decide whether that counts as a status the host may report.
- **O5:** The brand string. The brew test expects "Vulnesis CLI" but the code prints "Vulnetix CLI" (`homebrew-tap/Formula/vulnetix.rb:35` vs `cli/cmd/version.go:29-51`). This is in HM-2b.
- **O6:** Migration number `0120`. The coordinator should confirm it does not collide with the belai-counterparts migrations (`0101_node_sessions.sql` exists).

## Status re-check (2026-11-22)

Code read in both repos on 2026-11-22. Paths are relative to the keleustes repo root; `belai/`, `cli/` and `homebrew-tap/` prefix the sibling repos. The wire moved to its own `proto/keleustes/node/v1/host.proto` (see `host-proto-request.md`), so rows that named `node.proto` fields are judged against it.

| item | priority | status | evidence (file:line) | what's left |
|---|---|---|---|---|
| HM-1 Inventory | P0 | done | proto/keleustes/node/v1/host.proto:42; belai/internal/hostmgmt/inventory.go:44; crates/kls-node/src/host.rs:39; crates/kls-core/src/hosts.rs:1229 | — |
| HM-2 Product probes | P0 | done | belai/internal/products/products.go:156; belai/internal/products/sensor.go:53; belai/internal/products/pkgfw.go:14 | — |
| HM-2b CLI JSON status | P2 | partial | cli/cmd/package_firewall_status.go:54; cli/cmd/auth_status_json.go:10; cli/cmd/vulnetix-sensor/provision_cmds.go:444 | brand string fix (O5) |
| HM-3 Host view (read) | P0 | done | crates/kls-ui/src/screens/host.rs:29,185; crates/kls-ui/src/fleet.rs:79 | — |
| HM-4 Action pipeline | P0 | done | crates/kls-core/src/hosts.rs:858,1080,1170; crates/kls-node/src/host.rs:115; belai/internal/hostmgmt/queue.go:23 | — |
| HM-5 Config and trust | P1 | done | belai/internal/config/remotekeys.go:84; belai/internal/hostmgmt/configkinds.go:122,176; crates/kls-ui/src/screens/host.rs:2402,2423,1488; crates/kls-ui/src/screens/host.rs:4120 (test) | — (project scope view + project_prefs_set edits; Trust dir for a typed path) |
| HM-6 Per-repo agents, detections | P1 | done | belai/internal/config/projectprefs_repo.go:145,155; belai/internal/rc/workers.go:88; belai/internal/hostmgmt/repos.go:220; crates/kls-ui/src/screens/host.rs:1364 | — |
| HM-7 Product provisioning | P1 | done | belai/internal/hostmgmt/recipes.go:102,126,262,298; crates/kls-core/src/hosts.rs:1120; crates/kls-ui/src/screens/host.rs:1012,1062 | — |
| HM-8 belai upgrade, restart | P1 | done | belai/internal/hostmgmt/upgrade.go:30-49; belai/internal/hostmgmt/restart.go:12; belai/internal/rc/hostmgmt.go:107; crates/kls-core/src/hosts.rs:1256 | crash before hostmgmt starts needs a supervisor (B5 note) |
| HM-9 Provision plans | P1 | done | crates/kls-core/src/hosts.rs:1602,242,1312; crates/kls-ui/src/screens/host.rs:3113,3241; crates/kls-core/src/hosts/tests.rs:1012; crates/kls-ui/src/screens/host.rs:4140 (test) | — (per-step bars, follow-the-step log ≤200 lines, lines scrubbed on store) |
| HM-9b Engine provision step | P2 | done | crates/kls-engine/src/exec.rs:2393,3288,3411; crates/kls-engine/src/validate.rs:620; crates/kls-engine/src/exec/tests/parity.rs:918 | — |
| HM-10 Schedules tab | P2 | done | crates/kls-ui/src/screens/host.rs:1589; crates/kls-core/src/hosts.rs:2068; crates/kls-store/migrations/sqlite/0113_host_schedules.sql:3 | — |
| HM-11 Library tab | P2 | done | crates/kls-ui/src/screens/host.rs:1816; crates/kls-core/src/hosts.rs:2131,2285 | — |
| HM-12 Sessions, workforce tab | P2 | done | crates/kls-ui/src/screens/host.rs:2212,2265 | — |
| HM-13 Diagnostics actions | P1 | done | belai/internal/hostmgmt/diag.go:50,114,167; crates/kls-ui/src/screens/host.rs:3365,3375,3394,3434; crates/kls-core/src/hosts.rs:208,230,1357; crates/kls-node/src/host.rs:82,89; crates/kls-node/src/host/tests.rs:167 | — (rtt is an upper bound: fastest action round trip; skew includes latency; cert from the TLS handshake) |
| HM-14 Needs-you, notifications | P2 | done | crates/kls-core/src/needs.rs:26,82,527; crates/kls-app/src/desktop.rs:279; crates/kls-core/src/hosts.rs:1484 | — |
| HM-15 Shell (opt-in) | P2 | done | belai/internal/hostmgmt/shell.go:18; crates/kls-core/src/hosts.rs:910; crates/kls-ui/src/screens/host.rs:2858 | — |
| HM-16 E2E suite | P1 | done | belai/e2e/keleustes_host_test.go:248,267,298,336,367,392,412,451,479; belai/justfile:191; crates/kls-app/src/host_cmd.rs:15 | steps 1-9 run with real binaries (`just e2e-host` passed 2026-11-22). Step 5 runs only as root in a systemd container (`KELEUSTES_E2E_SENSOR=1`); steps 3-4 assert the signed-out refusal unless `VULNETIX_E2E_ORG_KEY`/`_ORG_ID` are set |
| §3.1 Entry and routing | P0 | done | crates/kls-ui/src/fleet.rs:252,279; crates/kls-ui/src/lib.rs:434; crates/kls-app/src/desktop.rs:710-731; crates/kls-ui/src/screens/host.rs:124,3250 | — |
| §3.2 Frame | P2 | done | crates/kls-ui/src/screens/host.rs:857,583,688,724; crates/kls-core/src/hosts.rs:1619,1672; crates/kls-core/src/hosts/tests.rs:1097,1143 | — (Dispatch is a host-view dialog through the gateway, not the Sessions dialog: sessions.rs has no node preselect) |
| §3.3 Overview | P2 | done | crates/kls-ui/src/screens/host.rs:662,764,937 | — |
| §3.4 Products | P2 | partial | crates/kls-ui/src/screens/host.rs:821,886,929,958,967 | `Sign out` (CLI) in the row menu |
| §3.5 Repos | P2 | done | crates/kls-ui/src/screens/host.rs:1488,1573,1583,1591,3902 | — (Sessions here filters the host Sessions tab by cwd) |
| §3.6 Agents & detections | P1 | done | crates/kls-ui/src/screens/host.rs:1400,1470,1485-1515,1566 | — (applies per toggle, no batch Preview/Apply) |
| §3.7 Schedules | P2 | done | crates/kls-ui/src/screens/host.rs:1742,1773 | — |
| §3.8 Library | P2 | done | crates/kls-ui/src/screens/host.rs:1816,1913 | — |
| §3.9 Config | P2 | done | crates/kls-ui/src/screens/host.rs:2402,2423,3824 | — |
| §3.10 Sessions | P2 | done | crates/kls-ui/src/screens/host.rs:2217,2265 | — |
| §3.11 Provision | P1 | done | crates/kls-ui/src/screens/host.rs:3113,3241 | — |
| §3.12 Diagnostics | P2 | done | crates/kls-ui/src/screens/host.rs:3365,3375,3394,3434 | — |
| §3 icons | P2 | done | crates/kls-ui/src/icons.rs:137-143 | — (RADAR is crosshair: the bundled Phosphor has no radar glyph) |
| §4 `Host` service Report, Actions, Progress | P0 | done | proto/keleustes/node/v1/host.proto:25-33; crates/kls-node/src/server.rs:289 | — |
| §4 `ServerFrame.host_action = 30` | P2 | n-a | proto/keleustes/node/v1/host.proto:29 | replaced by the `Host.Actions` stream |
| §4 `Capabilities.host_actions/manage_level/manage_shell` | P0 | done | proto/keleustes/node/v1/host.proto:71 | — (moved to `HostInventory.manage`) |
| §4 `HostInventory` and sub-messages | P0 | done | proto/keleustes/node/v1/host.proto:42-258 | — |
| §4 `HostAction`, `Origin`, `HostActionUpdate` | P0 | done | proto/keleustes/node/v1/host.proto:260,286,298 | — |
| §5.1 hostmgmt, products, wiring, rc flags, queue | P0 | done | belai/cmd/belai/rccmd.go:134-135; belai/cmd/belai/rchost.go:62; belai/internal/rc/record.go:31; belai/internal/hostmgmt/queue.go:23; belai/internal/hostmgmt/actions.go:73-102 | — |
| §5.2 config_get, config_set | P0 | done | belai/internal/hostmgmt/configkinds.go:122,176,207; belai/internal/config/remotekeys.go:103,117,135 | — |
| §5.3 product recipes | P1 | done | belai/internal/hostmgmt/recipes.go:102,157,189,220,262,291 | — |
| §5.4 root and browser hand-offs | P1 | done | belai/internal/hostmgmt/recipes.go:126,298; crates/kls-core/src/hosts.rs:1314,1370 | — |
| §5.5 belai_upgrade, rc_restart | P1 | done | belai/internal/hostmgmt/upgrade.go:48-49,108; belai/internal/hostmgmt/restart.go:12-30; belai/internal/rc/hostmgmt.go:107 | — |
| §5.6 repo_trust, untrust, clone, worktree_drop | P1 | done | belai/internal/hostmgmt/repos.go:78,101,123,158; belai/internal/rc/clone.go:236,314; belai/internal/rc/hostmgmt.go:86; belai/internal/hostmgmt/clone_test.go; belai/internal/rc/clone_depth_test.go | — (target `<first workspace_dir>/<name>`, falls back to `<worktrees>/repos/…` with a plan note when no workspace_dirs; `depth` applied; an occupied target refused) |
| §5.7 per-repo agents, detections | P1 | done | belai/internal/config/projectprefs_repo.go:41-58,145,155; belai/internal/rc/workers.go:88; belai/internal/rc/scheduler.go:257; belai/internal/projectregistry/sweep.go:120 | — |
| §5.8 preflight, log_tail, session_prune | P1 | done | belai/internal/hostmgmt/diag.go:50,114,125,167,182 | — |
| §6.1 classes and approvals | P0 | done | crates/kls-core/src/hosts.rs:77,139,940,1017 | — |
| §6.2 classifier, param validators | P0 | done | belai/internal/hostmgmt/params.go; belai/internal/hostmgmt/hostmgmt_test.go:389,442 | — |
| §6.3 kill switch and A2A per class | P0 | done | crates/kls-core/src/hosts.rs:866-890,654,1080; crates/kls-node/src/fleet.rs:151,166 | — |
| §6.4 events and audit | P0 | done | crates/kls-core/src/hosts.rs:30-53,724,1622 | — (ledger written directly, not via `ledger_kind_for_event`) |
| §6.5 store migration and retention | P0 | done | crates/kls-store/migrations/sqlite/0105_host_management.sql:3-68; crates/kls-store/migrations/postgres/0105_host_management.sql:3; crates/kls-store/src/host.rs:197-631; crates/kls-core/src/sweeps.rs:121 | — |
| §6.6 kls-node host.rs | P0 | done | crates/kls-node/src/host.rs:39,62,85,115,152 | — |
| §6.6 `NodeGateway::host_action` outbox | P2 | n-a | crates/kls-node/src/host.rs:115 | replaced by the `Host.Actions` replay |
| §6.7 kls-core request_action, confirm, plans, vault grant | P0 | done | crates/kls-core/src/hosts.rs:858,1017,1703,1120 | — |
| §6.8 kls-ui wiring | P0 | done | crates/kls-ui/src/fleet.rs:79; crates/kls-ui/src/lib.rs:434; crates/kls-ui/src/screens/host.rs:339 | — |
| R4 audit deep link `?node=` | P2 | done | crates/kls-ui/src/screens/audit.rs:210; crates/kls-app/src/desktop.rs:673 | — |
| B3 `keleustes host` CLI | P1 | done | crates/kls-app/src/host_cmd.rs:15-40; crates/kls-app/src/main.rs:512 | — |
| §8.1 fake host (kls-node) | P0 | done | crates/kls-node/src/host/tests.rs:150,167,245,287 | — |
| §8.1 kls-core rules 1-4 | P0 | done | crates/kls-core/src/hosts/tests.rs:139,166,245,270,295,343,378,393,541 | — |
| §8.1 belai Go | P0 | done | belai/internal/hostmgmt/hostmgmt_test.go:263-784; belai/internal/hostmgmt/grpc_test.go:85; belai/internal/products/products_test.go:105; belai/internal/rc/hostmgmt_test.go:39 | — |
| §8.1 kls-ui | P1 | done | crates/kls-ui/src/screens/host.rs:3179,3213,3250; crates/kls-app/src/desktop.rs:710-731 | — |
| §8.2 E2E real binaries | P1 | done | belai/e2e/keleustes_host_test.go:248-530; belai/internal/hostmgmt/manager.go:256,259,448 (loopback-only release fixture); belai/cmd/belai/rccmd.go:200 (re-exec keeps its own rc record) | sensor step is nightly/root only; firewall steps need a test org key for the configured path |
| §8.3 manual checklist | P2 | n-a | — | release-time manual run |
| O1 AI FW / pkg FW flags | P1 | done | cli/cmd/ai_firewall.go:400,418,843; belai/internal/hostmgmt/recipes.go:262-268 | — |
| O2 sensor sudoers | P2 | partial | belai/internal/hostmgmt/recipes.go:115,126 | decide hand-off only vs documented sudoers rule |
| O3 reads while stopped | P2 | done | crates/kls-core/src/hosts.rs:871-880; crates/kls-core/src/hosts/tests.rs:166 | — |
| O4 entitlement | P2 | done | belai/internal/products/pkgfw.go:24,60 | — |
| O5 brand string | P2 | missing | homebrew-tap/Formula/vulnetix.rb:35; cli/cmd/version.go:28 | align brew test and version output |
| O6 migration number | P2 | done | crates/kls-store/migrations/sqlite/0105_host_management.sql | — (0105, not 0120) |

Counts: done 59, partial 3, missing 1, n-a 3 (66 rows; P4 update 2026-11-22).
