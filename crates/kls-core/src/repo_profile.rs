//! What a repository is built with (web `factory_repo_profile.go`,
//! `docs/factory.md` 2.8.3): package managers, test runners, CI, artifact
//! types, languages and signals, worked out from file paths.
//!
//! Keleustes keeps no file mirror, so the paths come from one GitHub tree
//! listing (kls-github `profile::refresh`, through the budgeted, cached
//! client). The cheap signals come from paths on every refresh; the deep
//! ones (`crypto`, `ai-sdk`, ...) only from the repo scout, and count for
//! [`DEEP_SIGNAL_TTL_MS`] after it ran. Everything here is pure.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// The closed set of signals, in the order the docs list them.
pub const SIGNALS: [&str; 17] = [
    "dockerfile",
    "compose",
    "helm",
    "k8s",
    "terraform",
    "cloudformation",
    "github-workflows",
    "openapi",
    "crypto",
    "ai-sdk",
    "agent-config",
    "sigstore",
    "release-artifacts",
    "vulnetix-config",
    "monorepo",
    "installed-deps",
    "license-file",
];

/// Signals file paths cannot show; only the repo scout reports them.
pub const DEEP_SIGNALS: [&str; 5] = [
    "crypto",
    "ai-sdk",
    "agent-config",
    "installed-deps",
    "release-artifacts",
];

/// How long after a scout its deep signals count.
pub const DEEP_SIGNAL_TTL_MS: i64 = 30 * 86_400_000;

/// A repository's profile, kept in settings under [`RepoProfile::key`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RepoProfile {
    pub signals: Vec<String>,
    pub package_managers: Vec<String>,
    pub languages: Vec<String>,
    /// When the repo scout last reported (unix ms, 0 = never).
    pub scouted_at: i64,
    pub test_runners: Vec<String>,
    pub ci: Vec<String>,
    pub artifact_types: Vec<String>,
    /// When the paths were last read (unix ms, 0 = never).
    pub refreshed_at: i64,
    /// Paths the last refresh read, and whether the listing was cut.
    pub files: usize,
    pub truncated: bool,
}

fn strs(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn sorted(v: impl IntoIterator<Item = String>) -> Vec<String> {
    v.into_iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Keeps the members of the closed set, sorted and unique.
pub fn clean_signals(v: impl IntoIterator<Item = String>) -> Vec<String> {
    sorted(v)
        .into_iter()
        .filter(|s| SIGNALS.contains(&s.as_str()))
        .collect()
}

fn is_deep(s: &str) -> bool {
    DEEP_SIGNALS.contains(&s)
}

fn yaml_ext(base: &str) -> bool {
    base.ends_with(".yml") || base.ends_with(".yaml")
}

/// What a file's base name says: `(kind, value)`, kind `pm`, `test`, `ci`
/// or `artifact`.
fn by_name(base: &str) -> Option<(&'static str, &'static str)> {
    Some(match base {
        "package-lock.json" | "npm-shrinkwrap.json" => ("pm", "npm"),
        "pnpm-lock.yaml" => ("pm", "pnpm"),
        "yarn.lock" => ("pm", "yarn"),
        "bun.lockb" => ("pm", "bun"),
        "poetry.lock" => ("pm", "poetry"),
        "uv.lock" => ("pm", "uv"),
        "pipfile.lock" => ("pm", "pipenv"),
        "requirements.txt" => ("pm", "pip"),
        "go.mod" => ("pm", "go"),
        "pom.xml" => ("pm", "maven"),
        "build.gradle" | "build.gradle.kts" => ("pm", "gradle"),
        "cargo.toml" => ("pm", "cargo"),
        "gemfile.lock" => ("pm", "bundler"),
        "composer.lock" => ("pm", "composer"),
        "packages.lock.json" => ("pm", "nuget"),
        "package.resolved" => ("pm", "swiftpm"),
        "jest.config.js" | "jest.config.ts" => ("test", "jest"),
        "vitest.config.ts" | "vitest.config.js" => ("test", "vitest"),
        "pytest.ini" | "conftest.py" => ("test", "pytest"),
        "phpunit.xml" => ("test", "phpunit"),
        ".rspec" => ("test", "rspec"),
        ".gitlab-ci.yml" => ("ci", "gitlab ci"),
        "jenkinsfile" => ("ci", "jenkins"),
        "azure-pipelines.yml" => ("ci", "azure pipelines"),
        "dockerfile" | "containerfile" => ("artifact", "container"),
        _ => return None,
    })
}

fn norm(p: &str) -> String {
    p.trim().replace('\\', "/").trim_matches('/').to_string()
}

/// The signals file paths alone show (web `factoryCheapSignals`).
pub fn cheap_signals(paths: &[String]) -> Vec<String> {
    let mut set = BTreeSet::new();
    let mut lock_dirs = BTreeSet::new();
    for p in paths {
        let lp = norm(p).to_lowercase();
        if lp.is_empty() {
            continue;
        }
        let (dir, base) = lp.rsplit_once('/').unwrap_or(("", lp.as_str()));
        let in_dir = |names: &[&str]| dir.split('/').any(|s| names.contains(&s));
        let hit = if base.starts_with("dockerfile") || base.starts_with("containerfile") {
            Some("dockerfile")
        } else if (base.starts_with("docker-compose") || base.starts_with("compose."))
            && yaml_ext(base)
        {
            Some("compose")
        } else if base == "chart.yaml" {
            Some("helm")
        } else if base.ends_with(".tf") {
            Some("terraform")
        } else if (base.starts_with("template.") && (yaml_ext(base) || base.ends_with(".json")))
            || base.ends_with(".template")
        {
            Some("cloudformation")
        } else if base.starts_with("openapi.") || base.starts_with("swagger.") {
            Some("openapi")
        } else if base.ends_with(".sigstore")
            || base.ends_with(".sig")
            || base.starts_with("cosign.")
        {
            Some("sigstore")
        } else if base.starts_with("license") || base.starts_with("copying") {
            Some("license-file")
        } else {
            None
        };
        set.extend(hit);
        if yaml_ext(base) && in_dir(&["k8s", "kubernetes", "manifests"]) {
            set.insert("k8s");
        }
        if lp.starts_with(".github/workflows/") && yaml_ext(base) {
            set.insert("github-workflows");
        }
        if lp.starts_with(".vulnetix/config")
            || (lp.starts_with(".vulnetix/ai-firewall.") && yaml_ext(base))
        {
            set.insert("vulnetix-config");
        }
        if by_name(base).is_some_and(|(k, _)| k == "pm") {
            lock_dirs.insert(dir.to_string());
        }
    }
    if lock_dirs.len() >= 2 {
        set.insert("monorepo");
    }
    set.into_iter().map(str::to_string).collect()
}

/// A profile from a repository's file paths and its language (web
/// `factoryProfileFromFiles`). Pure: the same paths give the same profile.
pub fn from_files(paths: &[String], language: &str) -> RepoProfile {
    let (mut pm, mut test, mut ci, mut art, mut lang) = (
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
    );
    for p in paths {
        let p = norm(p);
        if p.is_empty() {
            continue;
        }
        let base = p.rsplit('/').next().unwrap_or(&p).to_lowercase();
        if let Some((k, v)) = by_name(&base) {
            match k {
                "pm" => pm.insert(v),
                "test" => test.insert(v),
                "ci" => ci.insert(v),
                _ => art.insert(v),
            };
        }
        if base.starts_with("requirements") && base.ends_with(".txt") {
            pm.insert("pip");
        } else if p.starts_with(".github/workflows/") && yaml_ext(&base) {
            ci.insert("github actions");
        } else if p.starts_with(".circleci/") {
            ci.insert("circleci");
        }
        match base.as_str() {
            "go.mod" => {
                test.insert("go test");
                art.insert("go binary");
                lang.insert("go");
            }
            "cargo.toml" => {
                test.insert("cargo test");
                lang.insert("rust");
            }
            "pom.xml" | "build.gradle" | "build.gradle.kts" => {
                test.insert("junit");
                art.insert("jar");
                lang.insert("java");
            }
            "package.json" => {
                art.insert("npm tgz");
                lang.insert("javascript");
            }
            "pyproject.toml" | "setup.py" => {
                art.insert("wheel");
                lang.insert("python");
            }
            "gemfile" => {
                lang.insert("ruby");
            }
            _ => {}
        }
    }
    let mut languages: Vec<String> = lang.into_iter().map(str::to_string).collect();
    languages.push(language.to_string());
    let s = |v: BTreeSet<&str>| v.into_iter().map(str::to_string).collect::<Vec<_>>();
    RepoProfile {
        package_managers: s(pm),
        test_runners: s(test),
        ci: s(ci),
        artifact_types: s(art),
        languages: sorted(languages),
        signals: cheap_signals(paths),
        files: paths.len(),
        ..Default::default()
    }
}

/// Whether a push that changed `paths` touches something the profile reads.
pub fn touches(paths: &[String]) -> bool {
    paths.iter().any(|p| {
        let base = p.rsplit('/').next().unwrap_or(p).to_lowercase();
        !cheap_signals(std::slice::from_ref(p)).is_empty()
            || by_name(&base).is_some()
            || base == "package.json"
            || base == "pyproject.toml"
            || p.starts_with(".github/workflows/")
    })
}

impl RepoProfile {
    /// The settings key a repository's profile is kept under.
    pub fn key(repo: &str) -> String {
        format!("repo.profile:{}", repo.to_lowercase())
    }

    /// Reads a profile out of a scout result (`signals`, `packageManagers`,
    /// `languages`, or nested under `profile`).
    pub fn from_result(v: &Value, now: i64) -> RepoProfile {
        let v = v.get("profile").unwrap_or(v);
        RepoProfile {
            signals: sorted(strs(v.get("signals"))),
            package_managers: sorted(strs(v.get("packageManagers"))),
            languages: sorted(strs(v.get("languages"))),
            scouted_at: now,
            ..Default::default()
        }
    }

    /// Whether the scout at `scouted_at` still counts at `now`.
    pub fn scout_fresh(&self, now: i64) -> bool {
        self.scouted_at > 0 && now - self.scouted_at <= DEEP_SIGNAL_TTL_MS
    }

    /// The signals a reader may use: deep ones drop out once the scout is
    /// older than the TTL (or never ran) while the paths were read.
    pub fn effective_signals(&self, now: i64) -> Vec<String> {
        if self.scout_fresh(now) || self.refreshed_at == 0 {
            return self.signals.clone();
        }
        self.signals
            .iter()
            .filter(|s| !is_deep(s))
            .cloned()
            .collect()
    }

    /// What a path refresh stores over `stored`: the lists just read from
    /// paths; the scout's deep signals and time ride through (web
    /// `factoryMergeSignals`). A stored cheap signal is never kept: a deleted
    /// Dockerfile stops matching at once.
    pub fn after_refresh(stored: Option<&RepoProfile>, mut fresh: RepoProfile, now: i64) -> Self {
        let deep: Vec<String> = stored
            .map(|s| s.signals.iter().filter(|x| is_deep(x)).cloned().collect())
            .unwrap_or_default();
        let mut all = fresh.signals.clone();
        all.extend(deep);
        fresh.signals = clean_signals(all);
        fresh.scouted_at = stored.map(|s| s.scouted_at).unwrap_or(0);
        fresh.refreshed_at = now;
        fresh
    }

    /// What a scout stores over `stored` (web `factoryStoreScout`): the
    /// scout writes the deep signals. Where the paths were read, their cheap
    /// signals and lists stay and the scout's lists are added; otherwise the
    /// scout's whole answer is taken.
    pub fn after_scout(stored: Option<&RepoProfile>, scout: RepoProfile) -> Self {
        let Some(s) = stored.filter(|s| s.refreshed_at > 0) else {
            return scout;
        };
        let mut signals: Vec<String> = s.signals.iter().filter(|x| !is_deep(x)).cloned().collect();
        signals.extend(scout.signals.iter().filter(|x| is_deep(x)).cloned());
        let union = |a: &[String], b: &[String]| sorted(a.iter().chain(b).cloned());
        RepoProfile {
            signals: clean_signals(signals),
            package_managers: union(&s.package_managers, &scout.package_managers),
            languages: union(&s.languages, &scout.languages),
            scouted_at: scout.scouted_at,
            ..s.clone()
        }
    }

    /// The rows a crew's card shows (web `lines`); empty with nothing known.
    pub fn lines(&self) -> String {
        let mut b = String::new();
        for (label, v) in [
            ("Package managers", &self.package_managers),
            ("Test runners", &self.test_runners),
            ("CI", &self.ci),
            ("Artifacts", &self.artifact_types),
            ("Languages", &self.languages),
            ("Signals", &self.signals),
        ] {
            if !v.is_empty() {
                b.push_str(&format!("{label}: {}\n", v.join(", ")));
            }
        }
        if b.is_empty() {
            return b;
        }
        format!("Repository profile (use the variant that matches):\n{b}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn profile_from_paths_matches_the_web_rules() {
        let p = from_files(
            &paths(&[
                "package.json",
                "pnpm-lock.yaml",
                "vitest.config.ts",
                ".github/workflows/ci.yml",
                "Dockerfile",
                "deploy/k8s/app.yaml",
                "infra/main.tf",
                "api/openapi.yaml",
                "LICENSE",
                "svc/go.mod",
                ".vulnetix/config.yaml",
                "charts/app/Chart.yaml",
                "docker-compose.yml",
                "release.sig",
            ]),
            "TypeScript",
        );
        assert_eq!(p.package_managers, vec!["go", "pnpm"]);
        assert_eq!(p.test_runners, vec!["go test", "vitest"]);
        assert_eq!(p.ci, vec!["github actions"]);
        assert_eq!(p.artifact_types, vec!["container", "go binary", "npm tgz"]);
        assert_eq!(p.languages, vec!["go", "javascript", "typescript"]);
        assert_eq!(
            p.signals,
            vec![
                "compose",
                "dockerfile",
                "github-workflows",
                "helm",
                "k8s",
                "license-file",
                "monorepo",
                "openapi",
                "sigstore",
                "terraform",
                "vulnetix-config"
            ]
        );
        assert_eq!(p.files, 14);
        // Same paths, any order: same profile.
        let mut rev = paths(&["b/Cargo.toml", "a/Cargo.toml"]);
        let one = from_files(&rev, "");
        rev.reverse();
        assert_eq!(one, from_files(&rev, ""));
        assert_eq!(one.signals, vec!["monorepo"]);
        assert!(touches(&paths(&["src/Dockerfile.dev"])));
        assert!(touches(&paths(&["poetry.lock"])));
        assert!(!touches(&paths(&["src/main.rs"])));
    }

    #[test]
    fn deep_signals_ride_through_a_refresh_and_expire() {
        let now = 1_000 * DEEP_SIGNAL_TTL_MS;
        let scout = RepoProfile::from_result(
            &serde_json::json!({"signals": ["crypto", "dockerfile"], "packageManagers": ["npm"]}),
            now,
        );
        // Scout first, no paths yet: its whole answer.
        let stored = RepoProfile::after_scout(None, scout.clone());
        assert_eq!(stored.signals, vec!["crypto", "dockerfile"]);
        // A refresh: the Dockerfile is gone, crypto stays.
        let fresh = from_files(&paths(&["package-lock.json"]), "");
        let r = RepoProfile::after_refresh(Some(&stored), fresh, now + 1);
        assert_eq!(r.signals, vec!["crypto"]);
        assert_eq!(r.package_managers, vec!["npm"]);
        assert_eq!(r.scouted_at, now);
        assert_eq!(r.refreshed_at, now + 1);
        assert_eq!(r.effective_signals(now + 2), vec!["crypto"]);
        // Thirty days on, the deep signal no longer counts.
        assert!(r.effective_signals(now + DEEP_SIGNAL_TTL_MS + 2).is_empty());
        // A later scout keeps the path lists, replaces the deep signals.
        let s2 = RepoProfile::from_result(
            &serde_json::json!({"signals": ["ai-sdk", "terraform"], "languages": ["go"]}),
            now + 5,
        );
        let r2 = RepoProfile::after_scout(Some(&r), s2);
        assert_eq!(r2.signals, vec!["ai-sdk"], "cheap signals come from paths");
        assert_eq!(r2.package_managers, vec!["npm"]);
        assert_eq!(r2.languages, vec!["go"]);
        assert_eq!(r2.scouted_at, now + 5);
        assert!(r2.lines().contains("Package managers: npm"));
    }
}
