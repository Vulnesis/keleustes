# Gap: every forge, not only github.com

Last updated: 2026-11-21. Status: design. No code is changed by this file.

Keleustes reaches GitHub only through `gh` (`crates/kls-github`): on this host
when `gh auth status --hostname github.com` succeeds (`LocalGh`), otherwise on
a connected belai node that reports `gh_authenticated` (`NodeGh`, a `github`
job). Keleustes holds no forge token. The web Software Factory
(`vdb-site/docs/forge-connections.md`, `api/internal/forge`,
`api/internal/forgecap`) supports five kinds: `github`, `ghes`, `gitea`,
`forgejo` (Codeberg included) and `gitlab`. This doc says how Keleustes reaches
parity for every kind: sign-in (browser where the CLI can do it, a guided token
otherwise) and the GitHub feature set (repos, pulls, checks, reviews, issues,
labels, conflict commit, polling), always through the forge's own CLI.

The rule stays: **credentials live in the forge CLI's own store, never in
Keleustes.** Keleustes runs the CLI, hands it a token on stdin when the
person pastes one, and forgets it.

---

## 1. What exists today (and what is GitHub-only)

| Piece | File | GitHub-only because |
|---|---|---|
| `GitHub` trait (`route()`, `gh(args, stdin)`) | `kls-github/src/lib.rs` | names `gh`, one host |
| `LocalGh`, `LocalAuth` | `local.rs` | `--hostname github.com` hard-coded via `login::HOST` |
| `WebLogin`, `login_with_token`, `ALL_SCOPES`, `token_url` | `login.rs` | gh flags, github.com device URL, classic scopes |
| `Selector` | `select.rs` | one cached route per process; node filter `gh_authenticated` |
| `NodeGh` | `node.rs` | job kind `github`, `JobNeeds { gh: true }` |
| `Client` | `client.rs` | `gh api --include`, `x-ratelimit-*`, `gh api rate_limit` |
| `http::parse_include` | `http.rs` | reusable as is (glab prints the same shape) |
| `Budget` | `budget.rs` | keyed by route only |
| `sync`, `parse`, `conflicts`, `labels`, `poller` | | GitHub REST paths and shapes; conflict commit through `git/trees`, `git/commits` with two parents |
| `GitHubForge` (engine `Forge` impl) | `kls-engine/src/forge.rs` | one `Service`, GitHub paths |
| Store | `migrations/*/0006_github.sql` | keys `owner/name`, `login`; quota/ETag keyed by route |
| Node capability | `proto/keleustes/node/v1/node.proto` `Capabilities.gh_authenticated` | one bool, github.com only (belai `factorylink/host.go` runs `gh auth status --hostname github.com`) |
| Node job | belai `docs/keleustes.md` "`github`" | runs `gh` only, allowlisted subcommands |
| UI | `kls-ui/src/screens/repos.rs`, `gh_signin.rs` | one route tile, one sign-in dialog for github.com |

Existing bug found while reading (independent of this gap): the engine's
`check_run` action (`kls-engine/src/forge.rs`) POSTs
`/repos/{repo}/check-runs` with the `gh` user token. GitHub only lets a GitHub
App create check runs; a user or OAuth token gets 403. Map it to a commit
status (`POST /repos/{repo}/statuses/{sha}`, `context = name`) on every kind,
which is also what Gitea, Forgejo and GitLab need. See WP-7.

### What belai already has

- `belai/internal/forge` (TUI runs panel, fleet publish) knows GitHub (`gh`),
  GitLab (`glab mr view|create|list|note|close`, `glab ci get -F json`) and
  Gitea/Forgejo (direct REST with a token from `BELAI_FORGE_TOKEN_<HOST>`,
  `GITEA_TOKEN`, `FORGEJO_TOKEN`, because "tea has no checks and no per-branch
  pull request view"). `hosts.go` maps hosts to kinds (`codeberg.org` is
  Forgejo, configurable `SetHosts`). `auth.go RemoteAuth` builds per-host git
  credential helpers.
- What belai does **not** have: a capability advertising forge logins other
  than `gh_authenticated`, or a job that runs `glab`/`tea`. Both are needed
  for the node fallback (WP-9).

---

## 2. Per-forge CLI table

One CLI per dialect. Gitea and Forgejo (and Codeberg) share `tea`: it is the
only Gitea-family CLI with an API passthrough (`tea api`). `fj`
(forgejo-cli) and `berg` (codeberg-cli) are not used: neither has a raw API
command, and each keeps its own token store that `tea` cannot read, so a
sign-in through them would not give Keleustes a route.

Feature-detect every optional flag by running `<cli> <cmd> --help` once per
CLI version (cached by `<cli> --version`), never by version number alone.

### 2.1 Summary

| | github.com | GHES | GitLab (gitlab.com, self-managed) | Gitea | Forgejo / Codeberg |
|---|---|---|---|---|---|
| CLI | `gh` | `gh` | `glab` | `tea` | `tea` |
| Install | `brew install gh` · `winget install --id GitHub.cli` · `pacman -S github-cli` · apt/dnf repo at cli.github.com | same | `brew install glab` · `winget install glab.glab` · `pacman -S glab` · releases at gitlab.com/gitlab-org/cli/-/releases | `brew install tea` · `go install code.gitea.io/tea@latest` · binaries at dl.gitea.com/tea | same as Gitea |
| Minimum | 2.40 (`--active`; older falls back, as today) | same | a glab whose `auth login --help` lists `--stdin`; `--device` optional | a tea whose `--help` lists `api` and `login add --oauth` | same |
| Browser sign-in | device code: `gh auth login --hostname github.com --web --git-protocol <p> --skip-ssh-key --scopes <all>` | device code: same with `--hostname <ghes>` | **device** (GitLab ≥ 17.9): `glab auth login --hostname <h> --device --git-protocol <p>`; **web** (OAuth + loopback): `glab auth login --hostname <h> --web --git-protocol <p>` | OAuth + PKCE + loopback: `tea login add --name <h> --url https://<h> --oauth` | same; Codeberg needs the pre-registered tea app enabled (verify, §8.4) |
| Browser output to parse | stderr: `! One-time code (XXXX-XXXX) copied to clipboard` / `First copy your one-time code: XXXX-XXXX`, then `https://<host>/login/device` (parsers exist: `login::device_code`, `device_url`) | same, URL `https://<ghes>/login/device` | device: a one-time code and a verification URL (wording not documented; parse with the generic code and URL finders, pin with a recorded fixture); web: the authorize URL; glab opens the browser itself | stdout: `Please authorize the application by visiting this URL in your browser:` then the URL; `Opening browser for authorization...`; success `Login as <user> on <url> successful. Added this login as <name>`; failure `Error: Redirect URL not registered in Gitea` | same |
| Browser timeout | 15 min (device code) | 15 min | device: server's `expires_in`; web: glab's | **60 s** (tea's `authTimeout`); the dialog shows a countdown and Retry | same |
| Self-managed precondition | none | device flow for the GitHub CLI OAuth app on the instance; older GHES falls back to the browser flow inside gh | web on self-managed: an OAuth application on the instance with glab's redirect URI and `glab config set client_id <id> --host <h>` before login; device needs GitLab 17.9 | `[oauth2] DEFAULT_APPLICATIONS` includes `tea` (Gitea's default; pre-registered client id `d57cb8c4-630c-4168-8324-ec79935e18d4`, redirect `http://127.0.0.1` any port) | same setting on Forgejo |
| Token sign-in | `gh auth login --hostname <h> --git-protocol <p> --skip-ssh-key --with-token` (stdin) | same | `glab auth login --hostname <h> --git-protocol <p> --stdin` (stdin); add `--api-host`, `--api-protocol` only when they differ | `tea login add --name <h> --url https://<h> --token <t>` (argv; no stdin flag documented; see §2.6) | same |
| Token page (guided walk-through) | `https://github.com/settings/tokens/new?description=Keleustes&scopes=<all>` (exists: `login::token_url`) | `https://<ghes>/settings/tokens/new?description=Keleustes&scopes=<all-ghes>` | `https://<h>/-/user_settings/personal_access_tokens?name=Keleustes&scopes=<all-gl>` (prefill since 15.5; older `/-/profile/personal_access_tokens`) | `https://<h>/user/settings/applications` (no prefill; the dialog lists what to tick) | same |
| Status (signed in?) | `gh auth status --hostname <h> --active` exit 0; text: `Logged in to <h> account <login>`, `- Token scopes: '…'` | same | `glab auth status --hostname <h>` exit code, then `glab api --hostname <h> user` → `username` (authoritative; status output is not documented, so it is only a hint). `-F json` when `--help` lists it | `tea login list -o json` → the entry whose `url` host is `<h>` (its `name`, `user`); then `tea api -l <name> /user` exit 0 and `login` | same |
| Scopes check | `- Token scopes:` line (exists: `local::missing_scopes`); fine-grained reports none | same | PAT: `glab api --hostname <h> personal_access_tokens/self` → `scopes`, `expires_at`; OAuth (glab's app): fixed, treated as full; 401/404 on `/self` means OAuth | no read API: probe `GET /user`, `GET /repos/search?limit=1`, `GET /notifications?limit=1`, `GET /user/orgs`; a 403 body naming `required scope(s)` names the missing one | same |
| API passthrough | `gh api --hostname <h> -i -X M -H … --input -` (stdout: status line, headers, body; exists) | same | `glab api --hostname <h> -i -X M -H … --input -` (stdout: same shape as gh, `http::parse_include` reads it); path relative to `/api/v4` | `tea api -l <name> -i -X M -H 'k:v' -d @-` (body on stdout, **status and headers on stderr**); path relative to `/api/v1` | same |
| Pagination | `Link: rel="next"` (exists, `MAX_PAGES` 50 × 100) | same | `Link` and `x-next-page`; `per_page` ≤ 100; do not use `--paginate` (one conditional request per page instead) | `Link`, `X-Total-Count`; `limit` ≤ server `MAX_RESPONSE_ITEMS` (default 50) | same |
| Conditional GET (ETag / 304) | yes, free when 304 | yes | ETag on some endpoints only: send `If-None-Match` when stored, treat 304 as a bonus; rely on `updated_after` | no useful ETags on JSON lists; rely on `since` and `sort=recentupdate` | same |
| Rate limits | 5,000 req/h per user token; `x-ratelimit-{limit,remaining,used,reset,resource}`; secondary 403/429 with `retry-after`; `gh api rate_limit` free | off by default; when on, GitHub's headers; `rate_limit` answers 404 "Rate limiting is not enabled" → budget unknown | gitlab.com: authenticated API 2,000 req/min per user; `RateLimit-{Limit,Remaining,Reset,Observed}`, 429 + `Retry-After`; self-managed off by default | none sent: count calls per hour against `forge.calls_per_hour` (default 5000), `budget_known = false` | Codeberg: proxy 429 with `Retry-After`; otherwise as Gitea |
| "All scopes" to request | `login::ALL_SCOPES` (20 classic scopes, exists) | `ALL_SCOPES` minus github.com-only `copilot`, `codespace` (GitHub ignores unknown scopes in the OAuth request, but the token page refuses them; keep a GHES list) | `api read_api read_user read_repository write_repository read_registry write_registry create_runner manage_runner k8s_proxy ai_features self_rotate` (+ `sudo admin_mode` only for admins; the walk-through shows them unticked). `api` alone covers everything Keleustes calls | `write:activitypub write:issue write:misc write:notification write:organization write:package write:repository write:user` (+ `write:admin` for admins). tea's OAuth asks `admin,user,issue,misc,notification,organization,package,repository` | same |
| Git credential for pushes | `gh auth git-credential` | same | `glab auth git-credential` (and `--setup-git-credential-helper` at login) | `tea login helper` (`tea login add --helper` registers it) | same |
| Token expiry to show | classic PATs optional; OAuth none | same | PAT expiry required (≤ 365 d by default): show `expires_at` from `/personal_access_tokens/self` and warn at 14 d | optional | optional |

### 2.2 gh (github.com, GHES)

Today's code with the host as a parameter:

- `login::HOST`, `DEVICE_URL` become per connection: `https://<host>/login/device`.
- `git_protocol(program)` becomes `gh config get git_protocol --host <host>`.
- `ALL_SCOPES` is per kind (`scopes(kind)`), GHES drops `copilot` and
  `codespace`.
- `gh auth status --hostname <ghes> --active`, `gh api --hostname <ghes>`.
  `GH_HOST` is not used: every argv names `--hostname`, so two connections
  never cross.
- `rate_limit`: a 404 with "Rate limiting is not enabled" sets
  `budget_known = false`, no error.

### 2.3 glab (GitLab)

- Sign-in order offered: **Device** when the host answers GitLab ≥ 17.9 and
  `glab auth login --help` lists `--device`; **Browser** (`--web`) on
  gitlab.com always, on self-managed only when `glab config get client_id
  --host <h>` is set; **Token** always.
- `--web` opens the browser itself (glab runs a loopback callback). Keleustes
  passes `BROWSER` through unchanged for glab (unlike gh, where the dialog
  opens the URL), and still parses any `https://` line so the dialog can show
  an Open button if the opener fails.
- Version probe without a token: `GET https://<h>/api/v4/version` needs auth
  on most instances, so the probe is best effort: try `--device`, and on a
  "device flow not supported/404" error switch the dialog to Browser or Token.
- Always pass `--git-protocol` so glab never prompts. Run with
  `GLAB_NO_PROMPT=1`, `NO_COLOR=1`, `GLAB_CHECK_UPDATE=false`, stdin piped.
- Project ids in paths are the URL-encoded full path:
  `projects/group%2Fsub%2Fname/merge_requests`. GitLab owners may be nested
  groups, so `valid_repo` gets a GitLab form (`a/b/c/name`, each segment
  checked).

### 2.4 tea (Gitea, Forgejo, Codeberg)

- One tea login per connection, named `<host>` (so `-l <host>` is stable). If
  a login for that URL exists under another name, use its name (from
  `tea login list -o json`) and do not add a second.
- `--oauth` runs PKCE with a loopback server on a random port and opens the
  browser; the printed URL is captured for an Open button. tea waits 60 s;
  the dialog says so with a countdown and offers Retry.
- `Error: Redirect URL not registered` → the instance disabled tea's
  pre-registered app: switch to Token with that reason shown.
- `tea api -i` writes status and headers to **stderr**: the transport joins
  `stderr` (head) + `\n\n` + `stdout` (body) and reuses `http::parse_include`.
- Forgejo is told apart by `/api/v1/version` answering `<v>+gitea-<compat>`
  (as `forge.IsForgejo`); Codeberg is Forgejo with a preset host.

### 2.5 Why not fj / berg

| CLI | Auth | API passthrough | Verdict |
|---|---|---|---|
| `fj` (forgejo-cli) | `fj auth login` (OAuth + PKCE, loopback `127.0.0.1:26218`, built-in client ids for codeberg.org and ~16 hosts), `fj auth add-key <user> [key]`, `fj auth list` (`user@host`) | none | no route possible |
| `berg` (codeberg-cli) | `berg auth login` (token, `BERG_BASE_URL` for other hosts) | none | no route possible |

If tea's pre-registered OAuth app is disabled on a Forgejo host but `fj`
has a client id for it, the dialog still uses tea with a token: Keleustes
never moves a token between CLIs.

### 2.6 Token on argv (tea)

`tea login add --token` takes the token as an argument, so it is visible in
the process list for the life of that one process (under a second). Before
shipping, check whether the installed tea reads `GITEA_SERVER_TOKEN` or a
stdin form for `login add` (feature-detect from `--help`); use that when
present. Otherwise: argv, no shell, process killed on drop, never logged,
never in an error string (errors are built from tea's last stderr line with
the token redacted by exact match).

---

## 3. Architecture

### 3.1 Model

```rust
// kls-github/src/forge/kind.rs  (crate renamed kls-forge in WP-11)

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind { Github, Ghes, Gitlab, Gitea, Forgejo }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect { GitHub, GitLab, Gitea }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CliKind { Gh, Glab, Tea }

impl ForgeKind {
    pub fn dialect(self) -> Dialect;
    pub fn cli(self) -> CliKind;              // Github|Ghes→Gh, Gitlab→Glab, Gitea|Forgejo→Tea
    pub fn label(self) -> &'static str;       // "GitHub", "GitHub Enterprise", …
    pub fn pr_noun(self) -> &'static str;     // "pull request" | "merge request"
    pub fn default_host(self) -> Option<&'static str>; // github.com, gitlab.com, None…
    pub fn api_base(self, host: &str) -> String;       // for links and the probe only
}

/// One signed-in place Keleustes works on. Holds no secret.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,          // "github.com" for the default github.com row, else uuid v7
    pub kind: ForgeKind,
    pub host: String,        // lower-case, IDNA, no scheme, may carry :port
    pub label: String,       // "Codeberg", "work GHES"
    pub login: String,       // the account last seen signed in
    pub cli_name: String,    // tea login name, "" for gh/glab
    pub git_protocol: String,
    pub route_pref: RoutePref, // Auto | LocalOnly | Node(id)
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Connection {
    /// The key stored where `owner/name` was: unchanged for github.com,
    /// `host/owner/name` (lower-cased) elsewhere.
    pub fn repo_key(&self, forge_full_name: &str) -> String;
    pub fn forge_name<'a>(&self, repo_key: &'a str) -> &'a str; // strips host/
    pub fn org_key(&self, account: &str) -> String;           // "acme" | "codeberg.org/acme"
}
```

The github.com connection is created by the migration with id `github.com`,
so every existing row, event subject and setting keeps its meaning (the web
SF's "no GitHub row changes meaning" rule).

### 3.2 Traits

```rust
// What one CLI invocation printed (GhOutput renamed; alias kept).
pub type CliOutput = GhOutput;

/// A way to run a forge CLI for one connection: here or on a node.
/// Replaces `GitHub`. `LocalCli`, `NodeCli` and the fake implement it.
#[async_trait]
pub trait ForgeCli: Send + Sync {
    fn route(&self) -> Route;
    fn cli(&self) -> CliKind;
    async fn run(&self, args: &[String], stdin: Option<&str>) -> Result<CliOutput>;
}

/// The CLI's sign-in and status, per CLI kind. Stateless; built per call.
#[async_trait]
pub trait CliAuth: Send + Sync {
    fn cli(&self) -> CliKind;
    /// Installed? Version and detected flags (`--device`, `api`, `--oauth`, …).
    async fn probe(&self, exec: &dyn Exec) -> CliProbe;
    /// Signed in to `conn.host`, as whom, with which scopes missing.
    async fn status(&self, exec: &dyn Exec, conn: &Connection) -> AuthState;
    /// Sign-in methods this CLI offers for this host, best first.
    fn methods(&self, probe: &CliProbe, conn: &Connection) -> Vec<SignInMethod>;
    /// argv and environment for an interactive method.
    fn interactive(&self, m: SignInMethod, conn: &Connection) -> Spawn;
    /// argv, environment and how the token is passed.
    fn token(&self, conn: &Connection) -> (Spawn, TokenInput);
    /// The token page and the scopes to tick there.
    fn token_page(&self, conn: &Connection) -> TokenPage;
    /// Reads one output line of an interactive sign-in.
    fn progress(&self, line: &str) -> Option<Progress>;
    /// Git credential helper for pushes (`!gh auth git-credential`, …).
    fn git_helper(&self, conn: &Connection) -> String;
}

pub enum SignInMethod { DeviceCode, BrowserLoopback, Token }
pub enum TokenInput { Stdin, Arg(&'static str), Env(&'static str) }
pub struct TokenPage { pub url: String, pub scopes: Vec<&'static str>, pub prefilled: bool, pub expiry_required: bool }
pub enum Progress { Code { code: String, url: String }, OpenUrl(String), PressEnter, Done(String /*login*/), Failed(String) }

pub enum AuthState {
    SignedIn { login: String, missing: Vec<String>, expires_at: Option<i64> },
    SignedOut(String),
    Missing(String),     // CLI not installed
    TooOld(String),      // installed, lacks a needed command (e.g. `tea api`)
}

/// REST through the CLI, one dialect-neutral request/response.
#[async_trait]
pub trait Transport: Send + Sync {
    /// `path` is relative to the API root (`/repos/..`, `projects/..`).
    async fn request(&self, method: &str, path: &str, body: Option<&Value>,
                     headers: &[(&str, &str)]) -> Result<http::Raw>;
}
// GhTransport  : gh   api --hostname H -i -X M [-H ..] [--input -]   (stdout)
// GlabTransport: glab api --hostname H -i -X M [-H ..] [--input -]   (stdout)
// TeaTransport : tea  api -l NAME   -i -X M [-H ..] [-d @-]          (stderr head + stdout body)
```

`Client` (today `client.rs`) becomes generic over `Transport` and keeps what
it does: budget guard, ETag store, call counting, pagination by `Link`
(plus `x-next-page` for GitLab), quota observation. Quota header readers are
per dialect:

```rust
pub trait RateHeaders { fn observe(&self, raw: &Raw, now: i64) -> Option<Quota>; fn backoff_until(&self, raw: &Raw, now: i64) -> Option<i64>; }
// GitHub: x-ratelimit-*;  GitLab: RateLimit-* + Retry-After;  Gitea: none → counted window
```

The dialect layer turns forge JSON into the shapes the store already holds,
so `sync`, the poller, the engine and the UI keep one code path:

```rust
/// What the factory needs from a forge, normalised. Mirrors
/// vdb-site `forge.Client` (api/internal/forge/client.go).
#[async_trait]
pub trait ForgeApi: Send + Sync {
    fn conn(&self) -> &Connection;
    async fn viewer(&self, caller: &str) -> Result<Viewer>;
    async fn accounts(&self, caller: &str) -> Result<Vec<GhOrg>>;            // orgs / groups + self
    async fn repos(&self, account: &GhOrg, caller: &str) -> Result<Vec<GhRepo>>;
    async fn repo(&self, full: &str, caller: &str) -> Result<GhRepo>;
    async fn open_pulls(&self, repo: &str, caller: &str) -> Result<Paged<GhPull>>;
    async fn pull(&self, repo: &str, n: i64, caller: &str) -> Result<PullRead>; // + checks, reviews, association
    async fn checks(&self, repo: &str, sha: &str, caller: &str) -> Result<ChecksSummary>;
    async fn issues_since(&self, repo: &str, since: i64, caller: &str) -> Result<Vec<GhIssue>>;
    async fn labels(&self, repo: &str, caller: &str) -> Result<Vec<GhLabel>>;
    async fn ensure_label(&self, repo: &str, name: &str, color: &str, desc: &str, caller: &str) -> Result<bool>;
    async fn create_pull(&self, repo: &str, p: &PullIn, caller: &str) -> Result<(GhPull, bool /*existing*/)>;
    async fn set_draft(&self, repo: &str, n: i64, draft: bool, caller: &str) -> Result<()>;
    async fn pull_action(&self, repo: &str, n: i64, a: &PullAction, caller: &str) -> Result<()>; // ready, update branch, reviewers, merge
    async fn comment(&self, repo: &str, n: i64, body: &str, caller: &str) -> Result<()>;
    async fn submit_review(&self, repo: &str, r: &ReviewIn, caller: &str) -> Result<Value>;
    async fn add_labels(&self, repo: &str, n: i64, labels: &[String], caller: &str) -> Result<()>;
    async fn create_issue(&self, repo: &str, i: &IssueIn, caller: &str) -> Result<(i64, String)>;
    async fn set_status(&self, repo: &str, sha: &str, s: &StatusIn, caller: &str) -> Result<Value>;
    async fn commits(&self, repo: &str, n: i64, caller: &str) -> Result<Vec<Value>>;   // delivery capture
    // Conflict plan (read side; write side is git, §3.6)
    async fn compare(&self, repo: &str, base: &str, head: &str, caller: &str) -> Result<Compare>; // merge base + files
    async fn file_at(&self, repo: &str, path: &str, sha: &str, caller: &str) -> Result<Option<Vec<u8>>>;
    async fn permission(&self, repo: &str, login: &str, caller: &str) -> Result<String>;
    fn supports(&self, cap: Cap) -> Support;   // from the capability table, §5
}
// GitHubApi (today's sync.rs/conflicts.rs paths), GitLabApi, GiteaApi
```

`sync.rs` keeps every store write and event; its REST calls move behind
`ForgeApi`. `parse.rs` splits into `parse/github.rs`, `parse/gitlab.rs`,
`parse/gitea.rs`, each producing `GhPull`, `GhIssue`, … (struct names stay;
renaming them is churn with no gain).

### 3.3 Route selection per connection

```rust
pub struct Selector {
    core: Core,
    exec: Arc<dyn Exec>,
    cached: Mutex<HashMap<String /*conn id*/, (RouteStatus, Arc<dyn ForgeCli>)>>,
}
impl Selector {
    pub async fn select(&self, conn: &Connection) -> (RouteStatus, Option<Arc<dyn ForgeCli>>);
    pub fn invalidate(&self, conn_id: Option<&str>);
}
```

Order for a connection (unchanged in spirit):

1. `route_pref = Node(id)`: that node, if online and it advertises a login for
   `(kind, host)`.
2. Local: `CliAuth::status(conn)` is `SignedIn` → `LocalCli { program, conn }`.
3. Node: the first non-revoked online node (by name, then id) whose
   `Capabilities.forges` holds `{cli, host, signed_in: true}` →
   `NodeCli { node, conn }`. For `github.com` the old `gh_authenticated` bool
   still counts, so belai builds from before WP-9 keep working.
4. Unavailable with the reasons of 2 and 3.

`ROUTE_TTL_MS` stays 60 s per connection. `Route::key()` becomes
`<conn id>|local` and `<conn id>|node:<id>`; for github.com the old keys
`local`, `node:<id>` are kept (migration keeps the stored quota rows valid).
The `github.route` event keeps its kind; its subject becomes the connection
id and its data gains `connection`, `kind`, `host`.

Program names: `$KELEUSTES_GH`, `$KELEUSTES_GLAB`, `$KELEUSTES_TEA`
(tests point them at fakes). Env per CLI (no prompts, no colour, no pager,
no update check): gh as today; glab `GLAB_NO_PROMPT=1 GLAB_CHECK_UPDATE=false
NO_COLOR=1 PAGER=`; tea `NO_COLOR=1 PAGER=`.

### 3.4 Node fallback through belai

Wire change (coordinator-owned `node.proto`, WP-9):

```proto
message Capabilities {
  bool gh_authenticated = 1;           // kept: github.com via gh
  ...
  repeated ForgeLogin forges = 6;
}
message ForgeLogin {
  string cli = 1;        // "gh" | "glab" | "tea"
  string host = 2;       // lower-case
  string kind = 3;       // "github" | "ghes" | "gitlab" | "gitea" | "forgejo" | "" (unknown)
  string login = 4;
  bool signed_in = 5;
}
```

belai side (`factorylink/host.go`): every minute (`ghAuthTTL`), list logins:
`gh auth status` (all hosts, parse `Logged in to <host> account <login>`),
`glab auth status --all` (exit code per host, `-F json` when available),
`tea login list -o json`. Hosts whose kind belai already knows
(`forge.HostKind`) carry it.

New job kind `forge` (belai `docs/keleustes.md` "Jobs"):

```json
{"cli": "glab", "host": "gitlab.example.com", "args": ["api", "..."], "stdin": "..." }
```

- Refused unless the host's advert lists `{cli, host, signed_in}` (checked
  again for the job).
- `gh`: the existing `github` allowlist, plus `--hostname <host>` forced.
- `glab`: `args[0] == "api"` only; `--hostname <host>` forced; refuses
  `--form`, `--input <file>` other than `-`, `-F @file`, and any path
  argument (same path rules as `github`).
- `tea`: `args[0] == "api"` only; `-l <tea login for host>` forced (belai
  resolves the name, Keleustes never names it); `-d`/`-F` take only `@-`;
  `-o` refused.
- Answer: `{"stdout","stderr","exit","truncated"}`, as `github`.

`JobNeeds` (kls-core, coordinator) gains `forge: Option<(CliKind, String)>`;
`kls-node/src/gateway.rs` routes on it. Keleustes sends `github` jobs for
github.com until the node advertises `forges`, then `forge` jobs for all.

Conflict commits on a node need a push (§3.6): a second job kind
`forge_push` `{host, repo_url, bundle_b64, ref, expect_sha}`; belai already
pushes with per-host credential helpers (`forge.RemoteAuth`). Until it
exists, conflict commits off GitHub need the local route.

### 3.5 Store schema (migration `0009_forges.sql`, both dialects)

```sql
CREATE TABLE forge_connections (
    id           TEXT PRIMARY KEY,          -- 'github.com' or uuid v7
    kind         TEXT NOT NULL,             -- github|ghes|gitlab|gitea|forgejo
    host         TEXT NOT NULL,
    label        TEXT NOT NULL DEFAULT '',
    login        TEXT NOT NULL DEFAULT '',
    cli_name     TEXT NOT NULL DEFAULT '',  -- tea login name
    git_protocol TEXT NOT NULL DEFAULT 'https',
    route_pref   TEXT NOT NULL DEFAULT 'auto',
    enabled      BIGINT NOT NULL DEFAULT 1,
    -- Last AuthState and CliProbe as JSON, for the UI before a recheck.
    status_json  TEXT NOT NULL DEFAULT '{}',
    created_at   BIGINT NOT NULL,
    updated_at   BIGINT NOT NULL
);
CREATE UNIQUE INDEX forge_connections_host ON forge_connections (kind, host);
INSERT INTO forge_connections (id, kind, host, created_at, updated_at)
    VALUES ('github.com', 'github', 'github.com', 0, 0);

ALTER TABLE gh_orgs   ADD COLUMN connection TEXT NOT NULL DEFAULT 'github.com';
ALTER TABLE gh_orgs   ADD COLUMN forge_login TEXT NOT NULL DEFAULT '';   -- 'acme' when login = 'codeberg.org/acme'
ALTER TABLE gh_repos  ADD COLUMN connection TEXT NOT NULL DEFAULT 'github.com';
ALTER TABLE gh_repos  ADD COLUMN forge_full_name TEXT NOT NULL DEFAULT ''; -- '' means = full_name
ALTER TABLE gh_repos  ADD COLUMN forge_id BIGINT;                         -- GitLab project id
CREATE INDEX gh_repos_connection ON gh_repos (connection);
ALTER TABLE gh_quota  ADD COLUMN budget_known BIGINT NOT NULL DEFAULT 1;
ALTER TABLE gh_resolutions ADD COLUMN connection TEXT NOT NULL DEFAULT 'github.com';
-- signing gains 'unsigned_push' (built locally, pushed over git); verified read back.
```

- Keys: `gh_repos.full_name`, `gh_pulls.repo`, `gh_issues.repo`,
  `gh_labels.repo`, `delivery_*.repo` hold the repo key
  (`host/owner/name` off github.com). `gh_orgs.login` holds the org key.
  Code never parses the host out of a key; it looks up `gh_repos.connection`.
- `gh_etags.cache_key` and `gh_quota.route`/`gh_calls.route` already carry the
  route key, which now starts with the connection id off github.com.
- Delivery keys `pr:<repo key>#<n>` stay valid (GitLab `n` is the iid).
- Settings: `github.poll_interval_s`, `github.poll_enabled` apply to every
  connection; add `forge.calls_per_hour` (default 5000) for counted budgets.
  Per-connection overrides live in `forge_connections` later if needed.

### 3.6 Conflict commit off GitHub

Only GitHub's API makes a two-parent commit. Gitea's `contents` and GitLab's
`repository/commits` make single-parent commits, which leave the pull
request conflicted (the merge base does not move). So, like the web SF
(`forge.BuildTree` + `GitRemote.Push`), Keleustes builds the merge commit
with git and pushes it, with the CLI as the credential helper:

1. Read side unchanged in shape: `compare` (GitLab `repository/compare`
   answers the merge base; Gitea 1.22 does not, so walk both histories as
   `giteaClient.MergeBase` does, ≤ 1000 commits, refuse criss-cross),
   `file_at`, diff3 (`merge3.rs`).
2. Write: in a temp bare repo, `git fetch --depth=<n> <url> <head_sha>
   <base_sha>`, `git hash-object -w`, `git mktree`/`read-tree` +
   `update-index` + `write-tree`, `git commit-tree -p head -p base`, then
   `git push <url> <sha>:refs/heads/<head_ref> --force-with-lease=refs/heads/<head_ref>:<head_sha>`
   (a moved branch is refused, never overwritten).
3. Credentials: `git -c credential.https://<host>.helper= -c
   credential.https://<host>.helper='!<cli> auth git-credential'` (glab,
   gh) or `'!tea login helper'` (tea). Keleustes sees no token.
4. `gh_resolutions.signing = 'unsigned_push'`; GitHub keeps the API path and
   `github_signs`.

### 3.7 Engine

`GitHubForge` becomes `PlaneForge { service }`: `target()` resolves the
run's repo key to a connection, gets its `ForgeApi`, and every action calls
the trait. Before an action it checks `supports(cap)`; `Unavailable` fails
the step with `forge_capability_unavailable: <cap> on <kind>: <note>` (the web
SF's wording). `check_run` writes a commit status on every kind (see §1).
`webhook_events` is unchanged: the poller emits the same `github.*` events
with `connection` in the data, so triggers match on every forge.

---

## 4. Feature mapping per forge

Paths are relative to the CLI's API root. `{p}` is the GitLab project path
URL-encoded; `{r}` is `owner/name`.

| SF feature | GitHub / GHES (`gh api`) | GitLab (`glab api`) | Gitea / Forgejo (`tea api`) |
|---|---|---|---|
| Viewer | `/user` | `user` | `/user` |
| Accounts | `/user/orgs` | `groups?min_access_level=30&all_available=false` | `/user/orgs` |
| Repos of account | `/orgs/{o}/repos`, `/user/repos?affiliation=owner` | `groups/{g}/projects?include_subgroups=true`, `users/{id}/projects`, `projects?membership=true` | `/orgs/{o}/repos`, `/user/repos` |
| Repo | `/repos/{r}` | `projects/{p}` (`default_branch`, `permissions`) | `/repos/{r}` (`permissions`) |
| Open pulls | `/repos/{r}/pulls?state=open` | `projects/{p}/merge_requests?state=opened&updated_after=` | `/repos/{r}/pulls?state=open&sort=recentupdate` |
| Pull | `/repos/{r}/pulls/{n}` | `projects/{p}/merge_requests/{iid}` (`detailed_merge_status`, `draft`, `diff_refs`) | `/repos/{r}/pulls/{n}` (`mergeable`) |
| Checks | `/commits/{sha}/check-runs` (+ statuses) | `projects/{p}/repository/commits/{sha}/statuses`, pipeline of the MR | `/repos/{r}/commits/{sha}/status` |
| Reviews / decision | `/pulls/{n}/reviews` | `merge_requests/{iid}/approvals` (+ notes) | `/pulls/{n}/reviews`, latest non-stale per reviewer |
| Submit review | `POST /pulls/{n}/reviews` | approve: `POST .../approve`; comment: `POST .../notes`; request changes: note + `POST .../unapprove`; inline: `.../discussions` with position | `POST /pulls/{n}/reviews` |
| Draft / ready | `gh pr ready <n> [--undo] -R <repo>` (today; on GHES `-R <host>/<owner>/<name>`, the only non-`api` gh call besides sign-in) | `Draft: ` title prefix via `PUT merge_requests/{iid}` | `WIP: ` title prefix via `PATCH /pulls/{n}` |
| Update branch | `PUT /pulls/{n}/update-branch` | `PUT merge_requests/{iid}/rebase` | `POST /pulls/{n}/update?style=merge` |
| Request reviewers | `POST /pulls/{n}/requested_reviewers` (users, teams) | `PUT merge_requests/{iid}` `reviewer_ids` (users only; ids via `users?username=`) | `POST /pulls/{n}/requested_reviewers` |
| Merge | `PUT /pulls/{n}/merge` | `PUT merge_requests/{iid}/merge` (`squash`) | `POST /pulls/{n}/merge` (`Do`: merge/squash/rebase) |
| Create pull | `POST /repos/{r}/pulls` | `POST projects/{p}/merge_requests` (find existing by `source_branch`) | `POST /repos/{r}/pulls` |
| Issues since | `/issues?state=all&since=&sort=updated` (skip PRs) | `projects/{p}/issues?updated_after=&order_by=updated_at&sort=asc` | `/repos/{r}/issues?type=issues&state=all&since=` |
| Comment | `POST /issues/{n}/comments` | `POST issues/{iid}/notes` or `merge_requests/{iid}/notes` | `POST /issues/{n}/comments` |
| Labels list / create | `/labels`, `POST /labels` | `projects/{p}/labels`, `POST` (color `#rrggbb`) | `/labels?limit=50`, `POST` (color `#rrggbb`) |
| Add label | `POST /issues/{n}/labels` (names) | `PUT issues/{iid}` `add_labels` (names) | `POST /issues/{n}/labels` (**ids**: resolve, create missing) |
| Create issue | `POST /issues` | `POST projects/{p}/issues` | `POST /repos/{r}/issues` (label ids) |
| Status (was check_run) | `POST /statuses/{sha}` | `POST projects/{p}/statuses/{sha}` (`state`: success/failed/running) | `POST /statuses/{sha}` |
| Commits of pull | `/pulls/{n}/commits` | `merge_requests/{iid}/commits` | `/pulls/{n}/commits` |
| Compare / merge base | `/compare/{a}...{b}` | `repository/compare?from=&to=&straight=false` | `/compare/{a}...{b}`; merge base by history walk on 1.22 |
| File at sha | `/contents/{path}?ref=` | `repository/files/{path}/raw?ref=` | `/raw/{path}?ref=` or `/contents/{path}?ref=` |
| Conflict commit | git data API, two parents | git push (§3.6) | git push (§3.6) |
| Permission / association | payload `author_association` | `projects/{p}/members/all/{uid}` access level | `/collaborators/{login}/permission`; 403 → teams (as `permissionFromTeams`) |
| Rate budget | headers + `rate_limit` | `RateLimit-*` headers | counted window |
| Polling (no webhooks) | ETag 304s make idle polls free | `updated_after` cursor per repo | `since` cursor per repo (issues); pulls by `recentupdate` until older than cursor |

Polling: the poller gets one cursor per `(connection, repo, list)` in
`gh_etags` (key `cursor|<route>|<path>`, body = last `updated_at`). Webhooks
stay out of scope: a desktop app has no public endpoint, as today.

---

## 5. Capability matrix (from forgecap, desktop view)

Port `forgecap.Catalogue()` as a static Rust table (`forge/cap.rs`) with the
same ids and notes; pin it to a copy of
`vdb-site/api/internal/forgecap/testdata/forge-capabilities.json` in
`kls-github/testdata/` with a test that fails when they differ (same
`FORGECAP_UPDATE` refresh). Desktop column adds the Keleustes mechanism.

| id | github | ghes | gitea | forgejo | gitlab | Keleustes note |
|---|---|---|---|---|---|---|
| connect | native | emulated | emulated | emulated | emulated | the CLI's own sign-in (gh / glab / tea) |
| webhooks | native | native | emulated | emulated | emulated | **not used on desktop: polling on every kind** |
| repos | native | native | emulated | emulated | emulated | |
| pull_requests | native | native | native | native | emulated (MR iid) | |
| draft | native | native | emulated `WIP:` | emulated `WIP:` | emulated `Draft:` | |
| checks | native | native | emulated statuses | emulated statuses | emulated statuses | gate reads statuses on demand |
| merge_state | native | native | emulated | emulated | native | |
| review_decision | native | native | emulated | emulated | emulated | |
| reviews | native | native | native | native | emulated | |
| team_reviewers | native | native | native | native | unavailable | refuse with note |
| update_branch | native | native | emulated merge | emulated merge | emulated rebase | |
| signed_commits | native | native | emulated | emulated | emulated | desktop: unsigned push off GitHub (no KMS) |
| issues | native | native | emulated (label ids) | emulated | native | |
| close_reason | native | native | unavailable | unavailable | unavailable | board-only |
| rate_limit | native | native | emulated (counted) | emulated | native | |
| bot_identity | native | native | emulated | emulated | emulated | desktop: the signed-in person, no bot |
| security_alerts | native | native | unavailable | unavailable | unavailable | |
| merge_queue | native | native | unavailable | unavailable | unavailable | |
| app_capabilities | native | native | unavailable | unavailable | unavailable | |
| remote_clone | native | native | native | native | native | belai node |
| manifest_curator, pix_sandbox | | | | | | web only; not in Keleustes |

Events (`forgecap.Events()`) matter only for the trigger vocabulary: the
poller synthesises `pull_request.*`, `issues.*`, `pull_request_review`,
`check_run.completed` from state changes on every kind, so the event
matrix collapses to "polled" for all five.

---

## 6. Sign-in UX

Minimal prose, an icon and a functional label on every control. Icons are
Phosphor (`kls-ui/src/icons.rs`); add `GITLAB_LOGO`, `BUILDINGS`, `COFFEE`
(tea), `GIT_FORK` (Forgejo), `MOUNTAINS` (Codeberg), `TIMER`.

### 6.1 Repos screen: connections strip (replaces the single route tile)

```
┌ ⎇ Repositories ─────────────────────── [⟳ Refresh orgs] [⤓ Poll] [🔌 Recheck] ┐
│ ◉ github.com   ana · via local gh · 4,812/5,000            [⋯]               │
│ ⛰ codeberg.org ana · via node xeon · 212 calls/h           [⋯]               │
│ 🦊 gitlab.acme ✗ signed out                     [→ Sign in] [⋯]               │
│                                                          [＋ Connect forge]   │
└───────────────────────────────────────────────────────────────────────────────┘
```

`[⋯]` menu: `⟳ Recheck`, `→ Sign in again`, `🔑 Scopes`, `⏻ Disable`,
`🗑 Remove`. Repos table gains a host chip on rows off github.com.

### 6.2 Connect: pick forge and host

```
┌ ＋ Connect forge ─────────────────────────────────────── ✕ ┐
│ [◉ GitHub] [🏢 GitHub Enterprise] [🦊 GitLab]               │
│ [☕ Gitea] [⑂ Forgejo] [⛰ Codeberg]                          │
│                                                             │
│ Host  [ gitlab.acme.dev                        ] [🔍 Check] │
│ ✓ GitLab 18.4 · ✓ glab 1.62 · device ✓ · browser ✗          │
│                                             [→ Continue]    │
└─────────────────────────────────────────────────────────────┘
```

Host is prefilled and read-only for GitHub (`github.com`), Codeberg
(`codeberg.org`), and prefilled editable for GitLab (`gitlab.com`) and Gitea
(`gitea.com`). Check runs the unauthenticated probe and `CliAuth::probe`.

CLI missing:

```
│ ✗ glab not installed                                         │
│ $ brew install glab                          [⧉ Copy] [↗ Docs] │
│                                              [⟳ Check again] │
```

### 6.3 Sign in (one dialog, tabs only for the methods that CLI offers)

Device code (gh on github.com/GHES, glab --device):

```
┌ 🦊 Sign in to gitlab.acme.dev ─────────────────────────── ✕ ┐
│ [⌨ glab] [gitlab.acme.dev] [🔑 api + 11 scopes]              │
│ [📱 Device] [🌐 Browser] [🔑 Token]                           │
│                                                              │
│ ONE-TIME CODE                                                │
│ ABCD-1234   [⧉]                                              │
│ [↗ https://gitlab.acme.dev/oauth/device]                     │
│ ◌ Waiting for approval  ⏱ 14:32            [✕ Cancel]       │
└──────────────────────────────────────────────────────────────┘
```

Browser loopback (glab --web, tea --oauth):

```
│ [🌐 Browser]                                                  │
│ [🌐 Start]                                                    │
│ → ◌ Approve in browser  ⏱ 0:48     [↗ Open again] [✕ Cancel] │
│ ✗ Redirect URL not registered      [🔑 Use token]  [⟳ Retry]  │
```

Token walk-through (every CLI):

```
│ [🔑 Token]                                                    │
│ 1 [↗ Open token page]          (prefilled: name, scopes)     │
│ 2 Tick                          ☐ write:repository ☐ write:issue │
│                                 ☐ write:organization ☐ write:user … [⧉ Copy list] │
│ 3 Expiry  365 days (required)                                 │
│ 4 [ •••••••••••••••••••• ]                       [✓ Sign in] │
│ ✓ ana · 12/12 scopes · expires 2027-11-20                     │
```

Step 2 shows only when the page cannot prefill (Gitea, Forgejo) or a scope is
missing after sign-in (`AuthState::SignedIn { missing }`), then with
`[→ Sign in again]`. Done: `✓ ana` and `[✓ Done]`.

Per forge default tab: github.com Device · GHES Device · GitLab.com Device
(Browser if `--device` absent) · GitLab self-managed Device if ≥ 17.9, else
Token (Browser only when a client id is configured) · Gitea/Forgejo/Codeberg
Browser (tea OAuth), Token on redirect error.

### 6.4 Headless / CLI

`keleustes forge login --kind gitlab --host gitlab.acme.dev [--device | --web | --with-token < token.txt]`
and `keleustes forge status [--host H]` (exit 0 when signed in, one line per
connection: `host\tlogin\troute\tmissing`). `keleustes github login` stays as
an alias for `--kind github --host github.com`.

---

## 7. Independent work packages

Ordered by dependency; packages on the same level run in parallel.
Owners per `docs/WORKSTREAMS.md` (M4 owns kls-github, its store module and
migrations; coordinator owns proto, kls-core lib/gateway, kls-store lib).

| WP | Scope | Depends on | Owner |
|---|---|---|---|
| **WP-1 Model & store** | `ForgeKind`, `Connection`, repo/org key helpers, `0009_forges.sql` (both dialects), store fns `connections()`, `upsert_connection`, `repo_connection(key)`; tests on SQLite/Postgres | — | M4 |
| **WP-2 CLI transport** | `ForgeCli` (rename of `GitHub`), `LocalCli` per `CliKind`, env per CLI, `Transport` impls Gh/Glab/Tea (tea stderr-head join), `RateHeaders` per dialect, `Client` generic, counted budget (`budget_known`) | WP-1 | M4 |
| **WP-3 Auth adapters** | `CliAuth` for gh (host-param of today's code), glab, tea: probe, status parsing, methods, progress parsers, token pages and scope lists, git helper; `WebLogin` generalised to `InteractiveLogin` driven by `Progress` | WP-1 | M4 |
| **WP-4 GitLab dialect** | `GitLabApi` + `parse/gitlab.rs`: everything in §4 | WP-2 | M4 |
| **WP-5 Gitea dialect** | `GiteaApi` + `parse/gitea.rs` (Gitea and Forgejo), label ids, history-walk merge base, permission-from-teams | WP-2 | M4 |
| **WP-6 Selector & poller per connection** | per-connection routes and cache, events with `connection`, poller loop over enabled connections with cursors, quota view per connection | WP-2, WP-3 | M4 |
| **WP-7 Engine** | `PlaneForge`, capability refusals, `check_run` → commit status (fixes the 403 on GitHub too, can ship first on its own) | WP-4, WP-5 (status fix: none) | M3 + M4 |
| **WP-8 Conflict commit over git** | temp bare repo build + `--force-with-lease` push with CLI credential helper; `unsigned_push`; UI note on the conflicts screen | WP-3 | M4 |
| **WP-9 belai + proto** | `Capabilities.forges`, `ForgeLogin`; belai login listing; `forge` job with glab/tea allowlists; later `forge_push`; `JobNeeds.forge`; gateway routing; `NodeCli` | WP-1 (shapes) | coordinator + belai |
| **WP-10 UI** | connections strip, Connect dialog, generalised sign-in dialog (`gh_signin.rs` → `forge_signin.rs`), host chips, conflicts note; icons | WP-3, WP-6 | M4 (screens) |
| **WP-11 Capability table & docs** | `forge/cap.rs` pinned to forgecap JSON; rename crate `kls-github` → `kls-forge` (re-export shim for one release); this doc → `docs/forges.md` | WP-4, WP-5 | M4 |
| **WP-12 Test harness** | `FakeCli` (gh/glab/tea output shapes), fake binaries for sign-in, docker Gitea/Forgejo live suite, GitLab live suite gating | WP-2, WP-3 (grows with each) | M4 |

---

## 8. End-to-end test plan

### 8.1 Automated, no network (every PR)

- **FakeCli** (extends `fake.rs`): answers `gh api -i`, `glab api -i` (stdout
  head) and `tea api -i` (stderr head) from registered replies, with each
  dialect's rate headers (GitHub `x-ratelimit-*`, GitLab `RateLimit-*` + 429
  `Retry-After`, Gitea none), `Link`/`x-next-page` paging, 304s for GitHub.
  Records calls. One table-driven test per §4 row per dialect, with JSON
  fixtures recorded from real servers (gitlab.com, Gitea 1.22, Forgejo 11,
  GHES if available) under `testdata/<dialect>/`.
- **Fake binaries** (shell scripts, as `login.rs` tests do) for every sign-in:
  gh device (both wordings, exists), glab device (fixture of real output),
  glab `--web` (prints URL, waits, exits 0), tea `--oauth` (prints
  `Please authorize…` + URL, then `Login as … successful`), tea redirect error,
  tea 60 s timeout, token rejected (401 text) per CLI, CLI missing, CLI too
  old (`api` absent from `--help`). Assert stdin carries the token and argv
  never does (except tea per §2.6, asserted redacted in errors).
- **Status parsers**: recorded `gh auth status`, `glab auth status` (text
  and json), `tea login list -o json`, `personal_access_tokens/self`.
- **Selector**: local signed in / out per connection, node advertising
  `forges`, old node with only `gh_authenticated`, pref node offline.
- **Store**: migration on a 0008 database with GitHub rows keeps every key;
  repo key helpers round-trip nested GitLab groups.
- **Conflict push**: a local bare "remote" with `file://` URL; build the merge
  commit, push with lease; a moved branch is refused.
- **Capability table** pinned to the forgecap JSON copy.
- **belai** (Go): allowlist tests for `forge` jobs (glab/tea), login listing
  from fake binaries.

### 8.2 Automated, live (nightly CI, containers)

- **Gitea** (`gitea/gitea:1.22` and latest) and **Forgejo** (`codeberg.org/forgejo/forgejo:11`):
  start, `gitea admin user create --admin`, `gitea admin user
  generate-access-token --scopes all`, seed `acme/app` with `main`/`feature`
  conflicting on `app.txt` (as `live_gitea_test.go`), real `tea login add
  --token`, then run the whole plane: discovery, pulls, statuses as checks,
  reviews, labels by id, issues since, draft/ready, update branch, merge,
  conflict commit pushed through `tea login helper`, stale push refused.
  Gated by `KLS_LIVE_GITEA=1`.
- **tea OAuth** against the same container with a headless browser
  (Playwright: log in, click Authorize on the loopback URL tea printed).
  Optional job; proves the parser and the 60 s path.
- **GitLab**: `gitlab/gitlab-ce` is too heavy per run; a gitlab.com test group
  with a PAT in CI secrets (`KLS_LIVE_GITLAB_HOST`, `KLS_LIVE_GITLAB_TOKEN`),
  `glab auth login --stdin`, same suite. Nightly, rate-aware (2,000/min).
- **github.com**: existing suite with a test account token via `gh auth login
  --with-token`, gated by `KLS_LIVE_GITHUB_TOKEN`.

### 8.3 Manual (a human, once per release, checklist in the release issue)

| Forge | Human steps |
|---|---|
| github.com | Connect → Device: code copied, browser opens `/login/device`, approve; scopes chip says 20/20; Token tab: page prefilled, paste, ✓. Sign out in a terminal (`gh auth logout`) → strip shows signed out within 60 s; node fallback picks a belai node. |
| GHES | On a trial/test instance: Device flow (and the fallback when device flow is off), token page with the GHES scope list, `rate_limit` 404 handled. |
| gitlab.com | Device; Browser (`--web`, glab opens the browser); Token with prefilled page; expiry shown; MR draft/ready, approve, rebase, merge, conflict commit pushed. |
| GitLab self-managed | < 17.9: Device hidden, Token default; with `client_id` set: Browser works; without: Browser hidden. |
| Gitea | tea OAuth approve in < 60 s; let it time out once; Token walk-through (no prefill: tick list). |
| Codeberg | tea OAuth (verifies Codeberg keeps tea's pre-registered app, §8.4); Token fallback; 429 backoff visible in the strip. |
| Forgejo self-hosted | as Gitea; Forgejo detected from `/api/v1/version`. |
| Node route | Signed out locally, belai node signed in to each host: every feature row works via `forge` jobs; a `forge` job for a host the node does not advertise is refused. |

### 8.4 Facts to verify before WP-3 ships (open items)

1. Exact `glab auth login --device` output wording (record a fixture).
2. `glab auth status -F json` field names, and its exit code when signed out.
3. Whether the GitHub CLI OAuth app's device flow is on by default on current
   GHES, and gh's fallback text when it is not.
4. Codeberg and stock Forgejo: is tea's pre-registered OAuth app
   (`DEFAULT_APPLICATIONS`) enabled?
5. Whether `tea login add` can take the token from stdin or an env var
   (§2.6), and whether `tea api` paths include `/api/v1` or are relative.
6. `tea login helper` name and argv in the installed tea (`--help`).

---

## Sources

- glab: `docs/source/auth/login.md`, `auth/status.md`, `api/_index.md` in gitlab.com/gitlab-org/cli
- tea: `docs/CLI.md`, `modules/auth/oauth.go` in gitea.com/gitea/tea
- Gitea pre-registered OAuth apps (#26291, tea #27509), Forgejo OAuth2 provider docs
- forgejo-cli `src/auth.rs` (docs.rs 0.5.0); codeberg-cli (docs.rs 0.5.5)
- gh: cli.github.com/manual/gh_auth_login; GHES "authorizing OAuth apps" (device flow)
- In-tree: `vdb-site/docs/forge-connections.md`, `belai-forge-requests.md`,
  `vdb-site/api/internal/forge/*.go`, `forgecap/forgecap.go`;
  `belai/internal/forge/*.go`, `belai/internal/factorylink/host.go`,
  `belai/docs/keleustes.md`

## Status re-check (2026-11-22)

Read-only pass over the current tree of both repos (files under
`crates/kls-github`, `kls-engine`, `kls-ui` were being edited by another
agent during the pass; line numbers can drift). Paths are relative to the
keleustes root, `belai/` prefix for belai.

| item | priority | status | evidence (file:line) | what's left |
|---|---|---|---|---|
| WP-1 Model & store | P0 | done | crates/kls-github/src/forge/kind.rs:13,221,345; crates/kls-store/src/github.rs:1653-1857 | — |
| WP-2 CLI transport | P0 | done | crates/kls-github/src/forge/cli.rs:10; client.rs:214,500-523,711,754; exec.rs:33 | `Transport`/`RateHeaders` are functions in `Client`, not traits (fine) |
| WP-3 Auth adapters | P0 | done | crates/kls-github/src/forge/auth.rs:127,148,255,434,667; login.rs:109,150,430 | — (tea token now in `GITEA_SERVER_TOKEN`, see §2.6 row) |
| WP-4 GitLab dialect | P1 | done | crates/kls-github/src/forge/gitlab.rs:153-980 | MR list has no `updated_after` (inline review now uses `discussions`, see §4 Submit review) |
| WP-5 Gitea dialect | P1 | done | crates/kls-github/src/forge/gitea.rs:167,596,658,874 | — (criss-cross refusal: see §4 Compare) |
| WP-6 Selector & poller per connection | P0 | done | crates/kls-github/src/select.rs:97,147,208; poller.rs:72,94 | — |
| WP-7 Engine | P1 | done (P6, 2026-11-22) | crates/kls-github/src/forge/kind.rs:473 (`pull_ref_of`: GitHub `/pull/N`, GitLab `/-/merge_requests/N` with nested groups, Gitea `/pulls/N`); crates/kls-engine/src/forge.rs:97 (`pull_number_of` uses it); tests kind.rs:504, kls-engine/src/forge/tests.rs:180; crates/kls-engine/src/ops.rs:1665 (run facts collect pulls through `pull_ref_of`: GitLab `/-/merge_requests/N` and Gitea `/pulls/N` too); test ops.rs:1769 `run_facts_collect_pull_urls_of_every_forge` | `GitHubForge` rename optional (cosmetic) |
| WP-8 Conflict commit over git | P1 | done (P2, 2026-11-22) | crates/kls-github/src/conflicts.rs:71,77 (`forge`, `api_signed`), :260,512; forge/push.rs:132,306; crates/kls-ui/src/screens/conflicts.rs:436 ("Commit and push" off GitHub), :498 ("Unsigned push" chip), :922, :1032 | — |
| WP-9 belai + proto | P0 | done (P2, 2026-11-22) | proto/keleustes/node/v1/node.proto:297,302,482; crates/kls-core/src/gateway.rs:230; crates/kls-node/src/gateway.rs:81,90; belai/internal/factorylink/forgelogins.go:182; belai/internal/forgejob/forge.go:80 | — |
| belai `forge` job (glab/tea api allowlists) | P0 | done (P2, 2026-11-22) | belai/internal/forgejob/forge.go:80 (`ParseForge`), :242 (glab/tea closed flag tables, one API path), :308 (gh: `--hostname` forced after `api`, `GH_HOST` otherwise, other-host `-R` refused), :387 (`Run`: no shell, scrubbed env, host env dropped, tea `-l` from `tea login list`); belai/internal/rc/factoryjobs.go:153, :57 (advertised); tests belai/internal/forgejob/forge_test.go, belai/internal/rc/factoryforgejob_test.go (audit `forge`/`tool`, kill switch); e2e belai/e2e/keleustes_forge_test.go | — |
| NodeCli route gating | P0 | done (P2, 2026-11-22) | crates/kls-github/src/select.rs:248 (`node_reach`: `forge` job only when `job_kinds` lists `forge`, else `github` for github.com); crates/kls-node/src/gateway.rs:90 (`runs_kind`: no job of a kind a node does not list); tests crates/kls-github/src/tests.rs:204, gateway.rs `job_kind_tests` | — |
| `forge_push` job | P1 | done | crates/kls-github/src/node.rs:147; belai/internal/forgejob/push.go:38; belai/internal/rc/factoryjobs.go:128 | — |
| §1 check_run → commit status | P0 | done | crates/kls-engine/src/forge.rs:695; crates/kls-github/src/forge/api.rs:843 | — |
| §3.1 ForgeKind / Dialect / CliKind | P0 | done | crates/kls-github/src/forge/kind.rs:13,22,30,108 | — |
| §3.1 Connection + key helpers | P0 | done | crates/kls-github/src/forge/kind.rs:221,345,354,364,440 | — |
| §3.2 ForgeCli (rename, alias) | P0 | done | crates/kls-github/src/lib.rs:122-145 | — |
| §3.2 CliAuth + AuthState/Progress/TokenPage | P0 | done | crates/kls-github/src/forge/auth.rs:16-147 | — |
| §3.2 ForgeApi + per-dialect impls | P0 | done | crates/kls-github/src/forge/api.rs:160,469,497; gitlab.rs:153; gitea.rs:177 | parse split lives in gitlab.rs/gitea.rs, not `parse/*.rs` (fine) |
| §3.2 RateHeaders + counted budget | P1 | done | crates/kls-github/src/client.rs:124-160,711,754; crates/kls-store/src/github.rs:1847 | — |
| §3.3 route order, TTL, per-conn cache | P0 | done | crates/kls-github/src/select.rs:35,97,147 | — |
| §3.3 route keys per connection | P0 | done | crates/kls-github/src/forge/kind.rs:370 | — |
| §3.3 `github.route` event with connection | P1 | done | crates/kls-github/src/select.rs:131-140 | — |
| §3.3 `$KELEUSTES_*` and env per CLI | P1 | done | crates/kls-github/src/forge/kind.rs:46-77; exec.rs:33-40 | — |
| §3.4 proto `forges` / `ForgeLogin` | P0 | done | proto/keleustes/node/v1/node.proto:297,302 | — |
| §3.4 belai login listing | P0 | done | belai/internal/factorylink/forgelogins.go:182,219; host.go:62 | `glab -F json` not used (text parse) |
| §3.4 `JobNeeds.forge` + gateway routing | P0 | done | crates/kls-core/src/gateway.rs:230; crates/kls-node/src/gateway.rs:81,140 | — |
| §3.5 migration 0009 (both dialects) | P0 | done | crates/kls-store/migrations/sqlite/0009_forges.sql; postgres/0009_forges.sql | — |
| §3.5 `forge.calls_per_hour` | P2 | done | crates/kls-github/src/client.rs:134; crates/kls-core/src/audit/config.rs:459 | — |
| §3.6 merge commit over git, lease, CLI helper | P1 | done | crates/kls-github/src/forge/push.rs:132-306 | — |
| §3.6 `unsigned_push` + resolution connection | P1 | done | crates/kls-github/src/conflicts.rs:402,512 | — |
| §3.7 capability refusals | P1 | done | crates/kls-engine/src/forge.rs:1017,1043; crates/kls-github/src/forge/api.rs:1216 | — |
| §3.7 events carry `connection` | P1 | done | crates/kls-github/src/sync.rs:86,139,312; events.rs | — |
| §2.6 tea token: stdin/env detect, redaction | P1 | done (P2, 2026-11-22) | crates/kls-github/src/forge/auth.rs:99,103,759,849 (`TokenInput::Arg` removed; token in `GITEA_SERVER_TOKEN`); login.rs:430 (`tea login add --help` probe; an old tea is refused before the token goes anywhere); test login.rs `token_reaches_glab_on_stdin_and_tea_errors_are_redacted` (argv has no token) | — |
| §2.3/2.4 server probe (version, Forgejo detect, GitLab ≥17.9) | P2 | done (2026-11-28) | crates/kls-github/src/forge/server.rs:18 (`GITLAB_DEVICE_MIN` 17.9), :55 (`device_flow`), :108 (`parse`: GitLab `/api/v4/version`, Gitea/Forgejo `/api/v1/version` with `+gitea-` → Forgejo, GHES `/api/v3/meta`), :147 (unauthenticated GET, 5 s); crates/kls-github/src/service.rs:302 (`probe` fills `CliProbe.server`), :315 (`server_probe`: through the route when signed in, else one unauthenticated GET; kill switch, audit `server_probe`, stored), :367; crates/kls-github/src/forge/auth.rs:26 (`CliProbe.server`), :571 (glab `--device` hidden on a GitLab older than 17.9, kept when the version is unknown, e.g. a 401); crates/kls-store/migrations/{sqlite,postgres}/0260_forge_server.sql (`server_json`), crates/kls-store/src/github.rs:1733,1746; tests server.rs:185, auth.rs:988 (17.8 → Token only, 17.9 → Device), forge/tests.rs:1387 (HTTP answer, audit, kill switch). Live: `Forgejo 13.0.5 via route` and `Gitea 1.22.6 via route` (live_forge.rs:42) | — |
| §4 Viewer / Accounts / Repos / Repo | P0 | done | gitlab.rs:157-195; gitea.rs:181-222 | — |
| §4 Open pulls / Pull | P0 | done | gitlab.rs:197,211; gitea.rs:224,250 | — |
| §4 Checks (statuses) | P1 | done | gitlab.rs:217; gitea.rs:261; api.rs:548 | — |
| §4 Reviews / decision | P1 | done | gitlab.rs:230; gitea.rs:275 | — |
| §4 Submit review | P1 | done (P2, 2026-11-22) | crates/kls-github/src/forge/gitlab.rs:156 (`diff_refs`), :550 (`review_inline`: `POST …/discussions` with a text position; note fallback without diff refs, on an older head, or on 400/422); gitea.rs:480,504; test forge/tests.rs:1072 (audit, kill switch) | — |
| §4 Draft / ready | P1 | done | api.rs:640; gitlab.rs:319; gitea.rs:353 | — |
| §4 Update branch | P1 | done | gitlab.rs:339; gitea.rs:369 | — |
| §4 Request reviewers | P2 | done | gitlab.rs:357 (teams refused); gitea.rs:387 | — |
| §4 Merge | P1 | done | gitlab.rs:394; gitea.rs:406 | — |
| §4 Create pull | P0 | done | gitlab.rs:447; gitea.rs:444 | — |
| §4 Issues since | P1 | done | gitlab.rs:252; gitea.rs:285 | — |
| §4 Comment | P1 | done | gitlab.rs:477; gitea.rs:462 | — |
| §4 Labels list / create | P1 | done | gitlab.rs:269,279; gitea.rs:302,313 | — |
| §4 Add label (Gitea ids) | P1 | done | gitlab.rs:611; gitea.rs:582-596 | — |
| §4 Create issue | P1 | done | gitlab.rs:634; gitea.rs:615 | — |
| §4 Status | P0 | done | gitlab.rs:655; gitea.rs:635 | — |
| §4 Commits of pull | P2 | done | gitlab.rs:420; gitea.rs:429 | — |
| §4 Compare / merge base | P1 | done (P6, 2026-11-22) | gitlab.rs:767-813; crates/kls-github/src/forge/gitea.rs:171 (`merge_bases`: best common ancestors by each commit's `parents`), :714 (more than one is refused as a criss-cross merge); test gitea.rs:1019 | GitLab takes the server's `repository/merge_base` (git's own pick); detecting a criss-cross there needs the commit graph, not fetched (P2) |
| §4 File at sha | P1 | done | gitlab.rs:733; gitea.rs:701 | — |
| §4 Conflict commit | P1 | done | conflicts.rs:260 | — |
| §4 Permission / association | P2 | done | gitlab.rs:924; gitea.rs:874 | — |
| §4 Rate budget | P1 | done | client.rs:124,711 | — |
| §4 Polling cursors | P2 | done (2026-11-28) | crates/kls-github/src/sync.rs:399 (`PULLS_FULL_EVERY_MS`, a full pass every 30 min), :411 (`sync_pulls`: cursor `cursor\|<conn>\|pulls\|<repo>` = newest `updated_at` seen; closed pulls in a cursored list read once; unlisted pulls left alone unless mergeability or checks are settling), :415 (GitHub keeps full lists: ETag 304s); forge/api.rs:182 (`pulls_since`); gitlab.rs:230 (`state=all&updated_after=…&order_by=updated_at&sort=desc`); gitea.rs:294 (`state=all&sort=recentupdate`, stops at the cursor); client.rs:192 (`get_until`); test forge/tests.rs:1449; exercised live (second `sync_pulls` in live_forge.rs) | — |
| §5 capability table pinned | P1 | done | crates/kls-github/src/forge/cap.rs:36,160,181; testdata/forge-capabilities.json | — |
| §5 events "polled" on every kind | P2 | done | crates/kls-github/src/forge/tests.rs:469 | — |
| §6 icons | P2 | done | crates/kls-ui/src/icons.rs:118-123 | — |
| §6.1 connections strip + menu | P1 | done | crates/kls-ui/src/screens/repos.rs:1751-1910 | github.com keeps its own route tile (strip lists others) |
| §6.1 host chip | P2 | done | crates/kls-ui/src/screens/repos.rs:536 | — |
| §6.2 Connect dialog | P1 | done | crates/kls-ui/src/screens/gh_signin.rs:638-816; server chip :94 (`✓ Forgejo 13.0`, `⚠` when the server is another kind than the one picked, `? Server` when unknown), :776; test :855 | — |
| §6.2 CLI missing: install, Copy, Docs, Check again | P2 | done | gh_signin.rs:241,611-636 | — |
| §6.3 device / browser tabs, countdown, Use token, Retry | P1 | done | gh_signin.rs:307-410 | — |
| §6.3 token walk-through (steps, Copy list, expiry) | P1 | done | gh_signin.rs:444-510 | — |
| §6.3 default tab per forge | P2 | done | crates/kls-github/src/forge/auth.rs:380,554,790 | — |
| §6 "merge request" wording (`pr_noun`) | P2 | done (P2, 2026-11-22) | crates/kls-ui/src/screens/repos.rs:151,1363,1489 (`pr_noun` on the repo detail) | cross-forge totals ("Open PRs", "PRs merged") keep the short form |
| §6.4 headless `forge login/status`, github alias | P1 | done | crates/kls-app/src/forge_cmd.rs:14-182; main.rs:69 | — |
| WP-10 rename `gh_signin.rs` → `forge_signin.rs` | P2 | missing | crates/kls-ui/src/screens/gh_signin.rs | cosmetic |
| WP-11 crate rename `kls-github` → `kls-forge` | P2 | missing | crates/kls-github | rename + re-export shim |
| WP-11 `docs/forges.md` | P2 | done | docs/forges.md | — |
| §8.1 FakeCli gh/glab/tea | P1 | done | crates/kls-github/src/fake.rs:67,223-238 | — |
| §8.1 per-row tests with recorded fixtures | P2 | done (P2, 2026-11-22) | crates/kls-github/testdata/gitlab/{merge_request,discussion,approvals}.json, testdata/gitea/{repo,labels}.json; loaded by `fixture` at forge/tests.rs:61 | more rows can move off inline JSON |
| §8.1 fake sign-in binaries, token never on argv | P1 | done (P6, 2026-11-22) | crates/kls-github/src/login.rs:584-759; :689 `tea_oauth_60s_timeout_fails_with_teas_words` (Keleustes waits past tea's 60 s, tea's timeout is a failure with its words) | — |
| §8.1 status parsers | P1 | done | crates/kls-github/src/forge/auth.rs:939-1082 | — |
| §8.1 selector cases | P1 | done (P6, 2026-11-22) | forge/tests.rs:276,650; tests.rs:106-166, :204 (`github_com_node_without_forge_job_gets_the_github_job`); crates/kls-github/src/tests.rs:204 `preferred_node_offline_falls_back_then_returns` | — |
| §8.1 store migration on 0008 db | P2 | done (2026-11-28) | crates/kls-store/src/github.rs:2352 `upgrade_from_0008_keeps_every_github_row` (`SQLITE_MIGRATIONS.run_to(8)`, GitHub rows in gh_orgs/gh_repos/gh_pulls/gh_quota/gh_resolutions, then `Store::open` runs 0009 onwards: every key and value kept, `connection` = github.com, the github.com connection row, `server_json` = `{}`) | SQLite only (Postgres runs the same SQL files) |
| §8.1 conflict push with lease | P1 | done | crates/kls-github/src/forge/push.rs:398 | — |
| §8.1 rules 1-4 (audit, kill switch) | P0 | done | crates/kls-github/src/forge/tests.rs:345; forge/guard.rs | — |
| §8.1 belai tests (forge job allowlist, login listing) | P1 | done (P2, 2026-11-22) | belai/internal/factorylink/forgelogins_test.go; belai/internal/forgejob/push_test.go; belai/internal/forgejob/forge_test.go (allowlist, forced host, tea login) | — |
| §8.2 live Gitea/Forgejo | P2 | done (live, 2026-11-28) | crates/kls-github/tests/live_forge.rs:17; crates/kls-github/live/forge-live.sh (podman or docker; random password and token per run, passed by env or file, never argv; `KLS_LIVE_TEA_RELEASE` runs a verified tea release in a runner image); justfile:33. Ran `forge-live.sh` with tea 0.16.0 (official release, sha256 9c142c8a…0636 matched checksums.txt) against `codeberg.org/forgejo/forgejo:13` (13.0.5+gitea-1.22.0) and `docker.io/gitea/gitea:1.22` (1.22.6) on 127.0.0.1:3300/3301: `live_gitea_family_plane ... ok` on both (discovery, pulls twice, statuses as checks, labels by id, issues, draft/ready, conflict commit pushed through `tea login helper`, stale push refused) | not wired into nightly CI (needs a runner with a container engine) |
| §8.2 tea OAuth headless browser | P2 | done (live, 2026-11-28) | crates/kls-github/tests/live_forge.rs:210 `live_tea_oauth_headless`: Keleustes' own `WebLogin` (BrowserLoopback) runs the real `tea login add --oauth`; the test plays the browser with reqwest (form login as the e2e user, the authorize URL tea printed, the grant form, the redirect to tea's loopback). `tea OAuth approved headlessly in 501 ms` (Forgejo 13.0.5) and `in 403 ms` (Gitea 1.22.6). Also by hand with an OAuth2 app registered through `POST /api/v1/user/applications/oauth2` and tea `--client-id`/`--redirect-url`: approved; with an unregistered redirect Forgejo answers 400 in the browser and tea only says `Error: authorization failed: authentication timed out after 1m0s` (fixture auth.rs:1189) | — (no Playwright: plain HTTP is enough for Forgejo's form login and grant page) |
| §8.2 GitLab and github.com live | P2 | missing | — | gated suites (`KLS_LIVE_GITLAB_*`, `KLS_LIVE_GITHUB_TOKEN`); need real accounts and tokens |
| §8.4.1 glab `--device` wording fixture | P1 | done | crates/kls-github/src/forge/auth.rs `glab_device_flow_recorded_wording` (glab 1.130.0 against gitlab.com, 2026-11-28: "First copy your one-time code: 4OWWB4CH" / "Then open this URL on any device to authorize: https://gitlab.com/oauth/device"; the code has no dash) | - (`--device` needs GitLab 17.9 or later, per glab's help) |
| §8.4.2 `glab auth status -F json` | P2 | done | glab 1.130.0 has `-F/--output json`. Recorded: `glab auth status` exits 0 even when its API call fails with 401, so the exit code never proves a sign-in; the probe's `glab api user` check is what decides | - (json not needed: the `api user` result decides) |
| §8.4.3 GHES device flow default | P2 | done (fallback recorded, 2026-11-28) | gh 2.102.0 against a stub GHES (nginx with TLS on `localhost`, `/login/device/code` answering 400 `device_flow_disabled`): gh falls back to its loopback web flow and prints only `Open this URL to continue in your web browser: https://<host>/login/oauth/authorize?client_id=…&redirect_uri=http%3A%2F%2Flocalhost%3A<port>%2F…` (cli/oauth treats 401/403/404/422, `device_flow_disabled` and `unauthorized_client` as "unsupported"). crates/kls-github/src/login.rs:86 (`gh_web_fallback_url`), :262 (the window turns that line into a browser step instead of waiting 15 min for a code); test login.rs:652 | whether the GHES "GitHub CLI" app has device flow on by default needs a real instance; Keleustes works either way now |
| §8.4.4 tea OAuth app on Codeberg / Forgejo | P2 | partial (2026-11-28) | stock Forgejo 13.0.5 and Gitea 1.22.6 accept tea's pre-registered client id `d57cb8c4-630c-4168-8324-ec79935e18d4` with a `127.0.0.1:<port>` redirect (live_forge.rs:210, both passed); crates/kls-github/src/forge/auth.rs:886 (http authorize URLs read too) | Codeberg itself not checked (needs a Codeberg account) |
| §8.4.5 tea token stdin/env; `tea api` path root | P1 | done (2026-11-28) | tea 0.16.0 `login add --help`: `--token string, -t string  Access token … [$GITEA_SERVER_TOKEN]`; the live suite signs tea in with the token only in `GITEA_SERVER_TOKEN` (forge-live.sh); `tea api -l localhost:3300 /version` answered `{"version":"13.0.5+gitea-1.22.0"}`, so paths are relative to `/api/v1` (client.rs:262-277, server.rs `route_path`) | — |
| §8.4.6 `tea login helper` argv | P2 | done (2026-11-28) | tea 0.16.0 `login helper --help`: commands `store, erase` (no-op), `setup`, `get` ("Return the stored token for a URL (git credential protocol)"); Keleustes sets `credential.http(s)://<host>.helper=!tea login helper` (auth.rs:898, push.rs:141-152) and git appends `get`. Live: the conflict commit pushed through it on Forgejo 13.0.5 and Gitea 1.22.6 (`signing = unsigned_push`, two parents), a second push refused as moved; belai/internal/forgejob/push.go:89 uses the same helper | — |

Counts: done 86 · partial 1 · missing 3 · n-a 0 (90 rows, recounted after the forges P2 package, 2026-11-28). Missing: the cosmetic renames (WP-10, WP-11, left while other work is in flight) and the GitLab / github.com live suites (need real accounts). Partial: §8.4.4 on Codeberg itself.

### Live check: Gitea and Forgejo in containers (2026-11-28)

`crates/kls-github/live/forge-live.sh` with `KLS_LIVE_TEA_RELEASE` pointing at
the official tea 0.16.0 linux-amd64 release (sha256 checked against the
release's `checksums.txt`), podman 6.1.4 rootless, images
`codeberg.org/forgejo/forgejo:13` (13.0.5+gitea-1.22.0) and
`docker.io/gitea/gitea:1.22` (1.22.6): `live_gitea_family_plane ... ok` and
`live_tea_oauth_headless ... ok` on both (2 passed per image). Each run makes an
admin `e2e` with a random password and an all-scopes token, both only in a
mode-600 temp dir and the child environment, removed with the containers.
Rerun: `KLS_LIVE_TEA_RELEASE=/path/to/tea just forge-live` (or with `tea` on
`PATH`).

### Live check: pr-review post step (2026-11-22)

`pr_review` posted through the real local `gh` on Vulnesis/keleustes#3 (closed
unmerged): one COMMENT review (id 6011433441) with the on-diff finding inline at
`docs/review-probe.md:3`, the off-diff finding in the body, the hidden
`keleustes:review` marker, and no second post on a rerun (`already_reviewed`).
Rerun with `KLS_LIVE_PR=owner/repo#N cargo test -p kls-engine --lib -- --ignored
live_pr_review` (`crates/kls-engine/src/forge/tests.rs`,
`live_pr_review_posts_once_through_real_gh`).
