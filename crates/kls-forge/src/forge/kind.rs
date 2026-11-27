//! Forge kinds, their dialects and CLIs, and the connection model.
//!
//! A [`Connection`] is one signed-in place Keleustes works on. It holds no
//! secret: the credential stays in the forge CLI's own store. Repository and
//! org keys stay `owner/name` and `acme` on github.com, and carry the host
//! elsewhere (`codeberg.org/ana/app`, `codeberg.org/ana`).

use kls_store::github::{ForgeConnectionRow, GITHUB_COM};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    Github,
    Ghes,
    Gitlab,
    Gitea,
    Forgejo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Dialect {
    GitHub,
    GitLab,
    Gitea,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliKind {
    Gh,
    Glab,
    Tea,
}

impl CliKind {
    pub fn name(self) -> &'static str {
        match self {
            CliKind::Gh => "gh",
            CliKind::Glab => "glab",
            CliKind::Tea => "tea",
        }
    }

    /// The environment variable that points tests (or a person) at another binary.
    pub fn env_var(self) -> &'static str {
        match self {
            CliKind::Gh => "KELEUSTES_GH",
            CliKind::Glab => "KELEUSTES_GLAB",
            CliKind::Tea => "KELEUSTES_TEA",
        }
    }

    /// The program to run: `$KELEUSTES_<CLI>` or the plain name.
    pub fn program(self) -> String {
        std::env::var(self.env_var()).unwrap_or_else(|_| self.name().into())
    }

    /// No prompts, no colour, no pager, no update check.
    pub fn env(self) -> &'static [(&'static str, &'static str)] {
        match self {
            CliKind::Gh => &[
                ("GH_PROMPT_DISABLED", "1"),
                ("GH_NO_UPDATE_NOTIFIER", "1"),
                ("NO_COLOR", "1"),
                ("GH_PAGER", ""),
            ],
            CliKind::Glab => &[
                ("GLAB_NO_PROMPT", "1"),
                ("GLAB_CHECK_UPDATE", "false"),
                ("NO_COLOR", "1"),
                ("PAGER", ""),
            ],
            CliKind::Tea => &[("NO_COLOR", "1"), ("PAGER", "")],
        }
    }

    /// Install commands per platform (label, command).
    pub fn install(self) -> &'static [(&'static str, &'static str)] {
        match self {
            CliKind::Gh => &[
                ("Arch", "sudo pacman -S github-cli"),
                ("macOS", "brew install gh"),
                ("Windows", "winget install --id GitHub.cli"),
            ],
            CliKind::Glab => &[
                ("Arch", "sudo pacman -S glab"),
                ("macOS", "brew install glab"),
                ("Windows", "winget install glab.glab"),
            ],
            CliKind::Tea => &[
                ("Arch", "paru -S tea   # AUR"),
                ("macOS", "brew install tea"),
                ("Go", "go install code.gitea.io/tea@latest"),
            ],
        }
    }

    pub fn docs_url(self) -> &'static str {
        match self {
            CliKind::Gh => "https://cli.github.com",
            CliKind::Glab => "https://gitlab.com/gitlab-org/cli/-/releases",
            CliKind::Tea => "https://dl.gitea.com/tea",
        }
    }
}

impl ForgeKind {
    pub const ALL: [ForgeKind; 5] = [
        ForgeKind::Github,
        ForgeKind::Ghes,
        ForgeKind::Gitlab,
        ForgeKind::Gitea,
        ForgeKind::Forgejo,
    ];

    pub fn id(self) -> &'static str {
        match self {
            ForgeKind::Github => "github",
            ForgeKind::Ghes => "ghes",
            ForgeKind::Gitlab => "gitlab",
            ForgeKind::Gitea => "gitea",
            ForgeKind::Forgejo => "forgejo",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|k| k.id() == s.trim().to_ascii_lowercase())
            .or(match s.trim().to_ascii_lowercase().as_str() {
                "codeberg" => Some(ForgeKind::Forgejo),
                "github-enterprise" | "enterprise" => Some(ForgeKind::Ghes),
                _ => None,
            })
    }

    pub fn dialect(self) -> Dialect {
        match self {
            ForgeKind::Github | ForgeKind::Ghes => Dialect::GitHub,
            ForgeKind::Gitlab => Dialect::GitLab,
            ForgeKind::Gitea | ForgeKind::Forgejo => Dialect::Gitea,
        }
    }

    pub fn cli(self) -> CliKind {
        match self.dialect() {
            Dialect::GitHub => CliKind::Gh,
            Dialect::GitLab => CliKind::Glab,
            Dialect::Gitea => CliKind::Tea,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ForgeKind::Github => "GitHub",
            ForgeKind::Ghes => "GitHub Enterprise",
            ForgeKind::Gitlab => "GitLab",
            ForgeKind::Gitea => "Gitea",
            ForgeKind::Forgejo => "Forgejo",
        }
    }

    pub fn pr_noun(self) -> &'static str {
        match self {
            ForgeKind::Gitlab => "merge request",
            _ => "pull request",
        }
    }

    /// The host a new connection of this kind starts with.
    pub fn default_host(self) -> Option<&'static str> {
        match self {
            ForgeKind::Github => Some("github.com"),
            ForgeKind::Gitlab => Some("gitlab.com"),
            ForgeKind::Gitea => Some("gitea.com"),
            ForgeKind::Ghes | ForgeKind::Forgejo => None,
        }
    }

    /// REST root, for links and the unauthenticated probe only.
    pub fn api_base(self, host: &str) -> String {
        match self {
            ForgeKind::Github if host == "github.com" => "https://api.github.com".into(),
            ForgeKind::Github | ForgeKind::Ghes => format!("https://{host}/api/v3"),
            ForgeKind::Gitlab => format!("https://{host}/api/v4"),
            ForgeKind::Gitea | ForgeKind::Forgejo => format!("https://{host}/api/v1"),
        }
    }
}

/// Where a connection's calls go.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutePref {
    #[default]
    Auto,
    LocalOnly,
    Node(String),
}

impl RoutePref {
    pub fn parse(s: &str) -> Self {
        match s {
            "local" => RoutePref::LocalOnly,
            s if s.starts_with("node:") => RoutePref::Node(s[5..].to_string()),
            _ => RoutePref::Auto,
        }
    }

    pub fn as_str(&self) -> String {
        match self {
            RoutePref::Auto => "auto".into(),
            RoutePref::LocalOnly => "local".into(),
            RoutePref::Node(id) => format!("node:{id}"),
        }
    }
}

/// One signed-in place Keleustes works on. Holds no secret.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    /// "github.com" for the default row, else uuid v7.
    pub id: String,
    pub kind: ForgeKind,
    /// Lower-case, no scheme, may carry `:port`.
    pub host: String,
    pub label: String,
    pub login: String,
    /// The tea login name ("" for gh and glab).
    pub cli_name: String,
    pub git_protocol: String,
    pub route_pref: RoutePref,
    pub enabled: bool,
    pub origin: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Default for Connection {
    fn default() -> Self {
        Self::github_com()
    }
}

/// `https://Gitlab.Acme.dev/` -> `gitlab.acme.dev`. `None` when it is not a host.
pub fn normalize_host(input: &str) -> Option<String> {
    let s = input.trim();
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let s = s.split(['/', '?', '#']).next()?.to_ascii_lowercase();
    let (name, port) = match s.rsplit_once(':') {
        Some((n, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (n, Some(p)),
        Some(_) => return None,
        None => (s.as_str(), None),
    };
    let ok = !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    ok.then(|| match port {
        Some(p) => format!("{name}:{p}"),
        None => name.to_string(),
    })
}

impl Connection {
    /// The github.com connection the migration creates.
    pub fn github_com() -> Self {
        Self {
            id: GITHUB_COM.into(),
            kind: ForgeKind::Github,
            host: "github.com".into(),
            label: String::new(),
            login: String::new(),
            cli_name: String::new(),
            git_protocol: "https".into(),
            route_pref: RoutePref::Auto,
            enabled: true,
            origin: "migration".into(),
            created_at: 0,
            updated_at: 0,
        }
    }

    /// A new connection of `kind` on `host` (id: uuid v7).
    pub fn new(kind: ForgeKind, host: &str, origin: &str) -> Option<Self> {
        let host = normalize_host(host)?;
        let now = kls_store::now_ms();
        let github_com = kind == ForgeKind::Github && host == "github.com";
        Some(Self {
            id: if github_com {
                GITHUB_COM.into()
            } else {
                uuid::Uuid::now_v7().to_string()
            },
            kind,
            cli_name: if kind.cli() == CliKind::Tea {
                host.clone()
            } else {
                String::new()
            },
            label: if host == "codeberg.org" {
                "Codeberg".into()
            } else {
                String::new()
            },
            host,
            origin: origin.into(),
            created_at: now,
            updated_at: now,
            ..Self::github_com()
        })
    }

    pub fn is_github_com(&self) -> bool {
        self.id == GITHUB_COM
    }

    pub fn cli(&self) -> CliKind {
        self.kind.cli()
    }

    pub fn dialect(&self) -> Dialect {
        self.kind.dialect()
    }

    /// What the UI calls it: the label, else the host.
    pub fn name(&self) -> &str {
        if self.label.is_empty() {
            &self.host
        } else {
            &self.label
        }
    }

    /// The key stored where `owner/name` was: unchanged on github.com,
    /// `host/owner/name` (lower-cased host) elsewhere.
    pub fn repo_key(&self, forge_full_name: &str) -> String {
        if self.is_github_com() {
            forge_full_name.to_string()
        } else {
            format!("{}/{}", self.host, forge_full_name)
        }
    }

    /// The forge's own `owner/name` of a key (strips `host/`).
    pub fn forge_name<'a>(&self, key: &'a str) -> &'a str {
        if self.is_github_com() {
            return key;
        }
        key.strip_prefix(&self.host)
            .and_then(|r| r.strip_prefix('/'))
            .unwrap_or(key)
    }

    /// `acme` on github.com, `codeberg.org/acme` elsewhere.
    pub fn org_key(&self, account: &str) -> String {
        self.repo_key(account)
    }

    /// The key the budget and the ETag cache are kept under for `route`
    /// (unchanged on github.com: `local`, `node:<id>`).
    pub fn route_key(&self, route: &crate::Route) -> String {
        if self.is_github_com() {
            route.key()
        } else {
            format!("{}|{}", self.id, route.key())
        }
    }

    /// The git URL of a repository on this forge.
    /// `$KELEUSTES_FORGE_HTTP_HOSTS` (comma list) names hosts served over
    /// plain http: only the live test containers use it.
    pub fn clone_url(&self, forge_full_name: &str) -> String {
        format!("{}/{}.git", self.web_base(), forge_full_name)
    }

    /// `https://<host>` (`http://` for a host in `$KELEUSTES_FORGE_HTTP_HOSTS`).
    pub fn web_base(&self) -> String {
        let http = std::env::var("KELEUSTES_FORGE_HTTP_HOSTS")
            .map(|v| v.split(',').any(|h| h.trim() == self.host))
            .unwrap_or(false);
        let scheme = if http { "http" } else { "https" };
        format!("{scheme}://{}", self.host)
    }

    /// [`ForgeKind::api_base`] with this connection's scheme.
    pub fn api_base(&self) -> String {
        let https = self.kind.api_base(&self.host);
        match self.web_base().strip_prefix("http://") {
            Some(_) => https.replacen("https://", "http://", 1),
            None => https,
        }
    }

    pub fn from_row(r: &ForgeConnectionRow) -> Option<Self> {
        Some(Self {
            id: r.id.clone(),
            kind: ForgeKind::parse(&r.kind)?,
            host: r.host.clone(),
            label: r.label.clone(),
            login: r.login.clone(),
            cli_name: r.cli_name.clone(),
            git_protocol: if r.git_protocol == "ssh" {
                "ssh".into()
            } else {
                "https".into()
            },
            route_pref: RoutePref::parse(&r.route_pref),
            enabled: r.enabled,
            origin: r.origin.clone(),
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }

    pub fn to_row(&self, status_json: &str) -> ForgeConnectionRow {
        ForgeConnectionRow {
            id: self.id.clone(),
            kind: self.kind.id().into(),
            host: self.host.clone(),
            label: self.label.clone(),
            login: self.login.clone(),
            cli_name: self.cli_name.clone(),
            git_protocol: self.git_protocol.clone(),
            route_pref: self.route_pref.as_str(),
            enabled: self.enabled,
            status_json: status_json.into(),
            origin: self.origin.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

fn segment_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && s != "."
        && s != ".."
}

/// A repository key: `owner/name`, or `host/owner[/sub…]/name` off github.com
/// (GitLab owners may be nested groups, up to 20 segments).
pub fn valid_key(key: &str) -> bool {
    if crate::valid_repo(key) {
        return true;
    }
    let Some((host, rest)) = key.split_once('/') else {
        return false;
    };
    if !host.contains('.') && !host.contains(':') || normalize_host(host).as_deref() != Some(host) {
        return false;
    }
    let segs: Vec<&str> = rest.split('/').collect();
    (2..=20).contains(&segs.len()) && segs.iter().all(|s| segment_ok(s))
}

/// A forge's own `owner/name` with nested groups (`a/b/c`).
pub fn valid_forge_name(full: &str) -> bool {
    let segs: Vec<&str> = full.split('/').collect();
    (2..=20).contains(&segs.len()) && segs.iter().all(|s| segment_ok(s))
}

/// A pull or merge request named by its web URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRef {
    /// Lowercase host, port kept.
    pub host: String,
    /// The forge's own `owner[/sub…]/name`.
    pub repo: String,
    pub number: i64,
}

/// Parses a pull or merge request URL: GitHub `…/owner/name/pull/N`, GitLab
/// `…/group[/sub…]/project/-/merge_requests/N` and Gitea `…/owner/name/pulls/N`.
/// Query, fragment and trailing tabs (`/files`, `/diffs`) are ignored.
pub fn pull_ref_of(url: &str) -> Option<PullRef> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.split(['?', '#']).next()?;
    let (host, path) = rest.split_once('/')?;
    let host = normalize_host(host)?;
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let (repo, num) = if let Some(i) = segs.iter().position(|s| *s == "-") {
        // GitLab: the project path, then `-/merge_requests/N`.
        if segs.get(i + 1) != Some(&"merge_requests") {
            return None;
        }
        (segs[..i].join("/"), *segs.get(i + 2)?)
    } else {
        let i = segs.iter().position(|s| *s == "pull" || *s == "pulls")?;
        if i != 2 {
            return None;
        }
        (segs[..i].join("/"), *segs.get(i + 1)?)
    };
    let number: i64 = num.parse().ok().filter(|n| *n > 0)?;
    valid_forge_name(&repo).then_some(PullRef { host, repo, number })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_urls_of_every_dialect_parse() {
        let r = pull_ref_of("https://github.com/acme/api/pull/7/files?x=1").unwrap();
        assert_eq!(
            (r.host.as_str(), r.repo.as_str(), r.number),
            ("github.com", "acme/api", 7)
        );
        let r = pull_ref_of("https://gitlab.example.com/grp/sub/proj/-/merge_requests/42/diffs#n")
            .unwrap();
        assert_eq!(
            (r.host.as_str(), r.repo.as_str(), r.number),
            ("gitlab.example.com", "grp/sub/proj", 42)
        );
        let r = pull_ref_of("https://codeberg.org/o/n/pulls/3").unwrap();
        assert_eq!((r.repo.as_str(), r.number), ("o/n", 3));
        assert!(pull_ref_of("https://gitlab.com/g/p/-/issues/4").is_none());
        assert!(pull_ref_of("https://gitlab.com/g/p/-/merge_requests/x").is_none());
        assert!(pull_ref_of("https://github.com/acme/api/pull/0").is_none());
        assert!(pull_ref_of("ftp://github.com/acme/api/pull/1").is_none());
        assert!(pull_ref_of("https://github.com/a/b/c/pull/1").is_none());
    }

    #[test]
    fn kinds_map_to_dialects_and_clis() {
        assert_eq!(ForgeKind::Forgejo.cli(), CliKind::Tea);
        assert_eq!(ForgeKind::Ghes.dialect(), Dialect::GitHub);
        assert_eq!(ForgeKind::Gitlab.pr_noun(), "merge request");
        assert_eq!(ForgeKind::parse("codeberg"), Some(ForgeKind::Forgejo));
        assert_eq!(ForgeKind::parse("GitLab"), Some(ForgeKind::Gitlab));
        assert_eq!(
            ForgeKind::Github.api_base("github.com"),
            "https://api.github.com"
        );
        assert_eq!(
            ForgeKind::Gitea.api_base("gitea.com"),
            "https://gitea.com/api/v1"
        );
    }

    #[test]
    fn hosts_normalize() {
        assert_eq!(
            normalize_host("https://GitLab.Acme.dev/x").as_deref(),
            Some("gitlab.acme.dev")
        );
        assert_eq!(
            normalize_host("git.local:3000").as_deref(),
            Some("git.local:3000")
        );
        assert_eq!(normalize_host("bad host"), None);
        assert_eq!(normalize_host("a:b"), None);
        assert_eq!(normalize_host(""), None);
    }

    #[test]
    fn keys_round_trip_and_github_com_is_unchanged() {
        let gh = Connection::github_com();
        assert_eq!(gh.repo_key("acme/api"), "acme/api");
        assert_eq!(gh.forge_name("acme/api"), "acme/api");
        assert_eq!(gh.org_key("acme"), "acme");
        assert_eq!(gh.route_key(&crate::Route::Local), "local");
        let gl = Connection::new(ForgeKind::Gitlab, "GitLab.Acme.dev", "user").unwrap();
        let key = gl.repo_key("grp/sub/app");
        assert_eq!(key, "gitlab.acme.dev/grp/sub/app");
        assert_eq!(gl.forge_name(&key), "grp/sub/app");
        assert!(valid_key(&key));
        assert!(valid_key("acme/api"));
        assert!(!valid_key("nohost/a/b"));
        assert!(gl.route_key(&crate::Route::Local).ends_with("|local"));
        assert!(valid_forge_name("a/b/c"));
        let cb = Connection::new(ForgeKind::Forgejo, "codeberg.org", "user").unwrap();
        assert_eq!(
            (cb.cli_name.as_str(), cb.name()),
            ("codeberg.org", "Codeberg")
        );
        let back = Connection::from_row(&cb.to_row("{}")).unwrap();
        assert_eq!(back, cb);
        assert_eq!(
            Connection::new(ForgeKind::Github, "github.com", "user")
                .unwrap()
                .id,
            GITHUB_COM
        );
        assert_eq!(RoutePref::parse("node:x"), RoutePref::Node("x".into()));
    }
}
