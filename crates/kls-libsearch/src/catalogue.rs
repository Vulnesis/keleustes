//! Curated sources (`catalogue.json`, shipped with the release), the
//! person's taps (`owner/repo[@ref]` plus globs) and the generic
//! harness-path detector (§4.3), built from a vendored copy of belai's
//! `harnesses.json` project dirs (`harness_paths.json`).

use crate::source::{Ecosystem, ItemType, Trust};
use anyhow::{Result, bail};
use serde::Deserialize;
use std::sync::OnceLock;

static CATALOGUE: &str = include_str!("catalogue.json");
static HARNESS_PATHS: &str = include_str!("harness_paths.json");

/// One glob that yields items of one type and dialect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathRule {
    pub glob: String,
    pub item_type: ItemType,
    pub ecosystem: Ecosystem,
    /// File names never items (`README.md`).
    pub exclude: Vec<String>,
}

/// A curated GitHub repo (G1–G14, M1–M4).
#[derive(Clone, Debug, PartialEq)]
pub struct CuratedRepo {
    /// `G1`, `M2`... from the spec's catalogue.
    pub key: String,
    pub owner: String,
    pub repo: String,
    pub git_ref: Option<String>,
    /// SPDX id of the repo root licence, or empty.
    pub licence: String,
    pub trust: Trust,
    pub stars: u64,
    /// Never tarballed: fetched raw by path (hermes-agent, openclaw).
    pub huge: bool,
    pub paths: Vec<PathRule>,
    /// Extra globs kept with the items (hook scripts, Codex metadata).
    pub keep: Vec<String>,
    /// Plugin marketplace files in the repo.
    pub marketplaces: Vec<String>,
    /// `readme`: a discovery list (links to repos).
    pub discovery: Option<String>,
}

impl CuratedRepo {
    pub fn id(&self) -> String {
        format!("gh:{}/{}", self.owner, self.repo)
    }
}

#[derive(Deserialize)]
struct RawCatalogue {
    repos: Vec<RawRepo>,
}

#[derive(Deserialize)]
struct RawRepo {
    key: String,
    owner: String,
    repo: String,
    #[serde(default)]
    git_ref: Option<String>,
    #[serde(default)]
    licence: String,
    trust: String,
    #[serde(default)]
    stars: u64,
    #[serde(default)]
    huge: bool,
    #[serde(default)]
    paths: Vec<RawRule>,
    #[serde(default)]
    keep: Vec<String>,
    #[serde(default)]
    marketplaces: Vec<String>,
    #[serde(default)]
    discovery: Option<String>,
}

#[derive(Deserialize)]
struct RawRule {
    glob: String,
    #[serde(rename = "type")]
    item_type: String,
    eco: String,
    #[serde(default)]
    exclude: Vec<String>,
}

/// The curated repos (parsed once).
pub fn curated() -> Vec<CuratedRepo> {
    static C: OnceLock<Vec<CuratedRepo>> = OnceLock::new();
    C.get_or_init(|| {
        let raw: RawCatalogue = serde_json::from_str(CATALOGUE).expect("catalogue.json parses");
        raw.repos
            .into_iter()
            .map(|r| CuratedRepo {
                key: r.key,
                owner: r.owner,
                repo: r.repo,
                git_ref: r.git_ref,
                licence: r.licence,
                trust: Trust::parse(&r.trust),
                stars: r.stars,
                huge: r.huge,
                paths: r
                    .paths
                    .into_iter()
                    .map(|p| PathRule {
                        item_type: ItemType::parse(&p.item_type).expect("catalogue type"),
                        ecosystem: Ecosystem::parse(&p.eco).expect("catalogue eco"),
                        glob: p.glob,
                        exclude: p.exclude,
                    })
                    .collect(),
                keep: r.keep,
                marketplaces: r.marketplaces,
                discovery: r.discovery,
            })
            .collect()
    })
    .clone()
}

// ---- taps -------------------------------------------------------------------------

/// A repo the person added: `owner/repo[@ref]` plus optional globs (no
/// globs: the harness-path detector finds items).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct Tap {
    pub owner: String,
    pub repo: String,
    pub git_ref: Option<String>,
    pub globs: Vec<String>,
    pub item_type: Option<ItemType>,
    pub ecosystem: Option<Ecosystem>,
}

fn valid_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl Tap {
    /// `owner/repo` or `owner/repo@ref` (a branch, tag or 40-hex commit);
    /// `https://github.com/owner/repo` also works.
    pub fn parse(spec: &str, globs: &[String]) -> Result<Self> {
        let s = spec.trim();
        let s = s
            .strip_prefix("https://github.com/")
            .unwrap_or(s)
            .trim_end_matches('/');
        let (path, git_ref) = match s.split_once('@') {
            Some((p, r)) => (p, Some(r.trim().to_string()).filter(|r| !r.is_empty())),
            None => (s, None),
        };
        let Some((owner, repo)) = path.split_once('/') else {
            bail!("a tap is owner/repo[@ref]");
        };
        let repo = repo.trim_end_matches(".git");
        if !valid_part(owner) || !valid_part(repo) || repo.contains('/') {
            bail!("a tap is owner/repo[@ref] (letters, digits, - _ .)");
        }
        if let Some(r) = &git_ref
            && (r.contains("..")
                || r.starts_with('/')
                || r.chars().any(|c| c.is_whitespace() || c == '\\'))
        {
            bail!("bad ref {r:?}");
        }
        let globs: Vec<String> = globs
            .iter()
            .map(|g| g.trim().to_string())
            .filter(|g| !g.is_empty())
            .collect();
        for g in &globs {
            if g.starts_with('/') || g.contains("..") {
                bail!("bad glob {g:?}");
            }
            crate::git::globs(&[g.as_str()])?;
        }
        Ok(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            git_ref,
            globs,
            item_type: None,
            ecosystem: None,
        })
    }

    pub fn id(&self) -> String {
        format!("gh:{}/{}", self.owner, self.repo)
    }

    pub fn spec(&self) -> String {
        match &self.git_ref {
            Some(r) => format!("{}/{}@{r}", self.owner, self.repo),
            None => format!("{}/{}", self.owner, self.repo),
        }
    }
}

// ---- the harness-path detector ---------------------------------------------------------

#[derive(Deserialize)]
struct HarnessFile {
    harnesses: Vec<Harness>,
}

#[derive(Deserialize)]
struct Harness {
    id: String,
    dirs: std::collections::BTreeMap<String, HarnessDir>,
}

#[derive(Deserialize)]
struct HarnessDir {
    project: Vec<String>,
    shape: String,
    #[serde(default)]
    files: Vec<String>,
}

/// The ecosystem of a belai harness id.
pub fn harness_ecosystem(id: &str) -> Ecosystem {
    match id {
        "claude-code" => Ecosystem::ClaudeCode,
        "codex" => Ecosystem::Codex,
        "gemini-cli" => Ecosystem::GeminiCli,
        "cursor" => Ecosystem::Cursor,
        "github-copilot" => Ecosystem::Copilot,
        "cline" => Ecosystem::Cline,
        "roo" => Ecosystem::Roo,
        "opencode" => Ecosystem::OpenCode,
        "windsurf" => Ecosystem::Windsurf,
        "openclaw" => Ecosystem::OpenClaw,
        _ => Ecosystem::Generic,
    }
}

/// The detector rules: the vendored harness dirs (specific ecosystems
/// first) and the §4.3 additions. Earlier rules win.
pub fn harness_rules() -> Vec<PathRule> {
    static R: OnceLock<Vec<PathRule>> = OnceLock::new();
    R.get_or_init(|| {
        let mut out: Vec<PathRule> = Vec::new();
        let mut push = |glob: String, t: ItemType, e: Ecosystem, ex: &[&str]| {
            if !out.iter().any(|r| r.glob == glob && r.item_type == t) {
                out.push(PathRule {
                    glob,
                    item_type: t,
                    ecosystem: e,
                    exclude: ex.iter().map(|s| s.to_string()).collect(),
                });
            }
        };
        // §4.3 additions and dialect-specific paths first.
        use Ecosystem as E;
        use ItemType as T;
        for (g, t, e) in [
            (".claude/agents/**/*.md", T::Agent, E::ClaudeCode),
            (".claude/commands/**/*.md", T::Command, E::ClaudeCode),
            (".claude/skills/*/SKILL.md", T::Skill, E::ClaudeCode),
            (".claude/settings.json", T::Hook, E::ClaudeCode),
            (".codex/prompts/*.md", T::Command, E::Codex),
            (".codex/skills/*/SKILL.md", T::Skill, E::Codex),
            (".codex/hooks.json", T::Hook, E::Codex),
            (".agents/skills/*/SKILL.md", T::Skill, E::AgentSkills),
            ("skills/*/SKILL.md", T::Skill, E::AgentSkills),
            (".gemini/commands/**/*.toml", T::Command, E::GeminiCli),
            ("commands/**/*.toml", T::Command, E::GeminiCli),
            ("gemini-extension.json", T::Mcp, E::GeminiCli),
            (".cursor/rules/*.mdc", T::Prompt, E::Cursor),
            (".cursor/commands/*.md", T::Command, E::Cursor),
            (".cursor/hooks.json", T::Hook, E::Cursor),
            (".cursorrules", T::Prompt, E::Cursor),
            (".roo/commands/*.md", T::Command, E::Roo),
            (".roomodes", T::Agent, E::Roo),
            (".roorules", T::Prompt, E::Roo),
            (".clinerules/workflows/*.md", T::Command, E::Cline),
            (".clinerules", T::Prompt, E::Cline),
            (".github/agents/*.agent.md", T::Agent, E::Copilot),
            (".github/prompts/*.prompt.md", T::Command, E::Copilot),
            (
                ".github/instructions/*.instructions.md",
                T::Prompt,
                E::Copilot,
            ),
            (".github/copilot-instructions.md", T::Prompt, E::Copilot),
            (".github/hooks/*/hooks.json", T::Hook, E::Copilot),
            (".opencode/agent/*.md", T::Agent, E::OpenCode),
            (".opencode/command/*.md", T::Command, E::OpenCode),
            (".windsurf/workflows/*.md", T::Command, E::Windsurf),
            (".windsurfrules", T::Prompt, E::Windsurf),
            ("hooks/hooks.json", T::Hook, E::ClaudeCode),
            ("*/hooks/hooks.json", T::Hook, E::ClaudeCode),
            (".mcp.json", T::Mcp, E::ClaudeCode),
            ("*/.mcp.json", T::Mcp, E::ClaudeCode),
            ("CLAUDE.md", T::Prompt, E::ClaudeCode),
            ("GEMINI.md", T::Prompt, E::GeminiCli),
            ("AGENTS.md", T::Prompt, E::Generic),
        ] {
            push(g.into(), t, e, &[]);
        }
        let hf: HarnessFile = serde_json::from_str(HARNESS_PATHS).expect("harness_paths.json");
        let mut hs = hf.harnesses;
        // Specific ecosystems before the generic ones.
        hs.sort_by_key(|h| harness_ecosystem(&h.id) == Ecosystem::Generic);
        for h in &hs {
            let eco = harness_ecosystem(&h.id);
            for (kind, d) in &h.dirs {
                let t = match kind.as_str() {
                    "agent" => T::Agent,
                    "command" | "prompt" => T::Command,
                    "skill" => T::Skill,
                    "hook" => T::Hook,
                    "document" => T::Prompt,
                    _ => continue,
                };
                for p in &d.project {
                    let p = p.trim_end_matches('/');
                    if p.starts_with('~') || p.starts_with('/') || p.contains("..") {
                        continue;
                    }
                    match d.shape.as_str() {
                        "skill-dir" => push(format!("{p}/*/SKILL.md"), t, eco, &[]),
                        "md-file" => push(format!("{p}/**/*.md"), t, eco, &["README.md"]),
                        "json-key" | "json-file" => push(p.to_string(), t, eco, &[]),
                        "doc-file" => {
                            for f in &d.files {
                                let g = if p == "." {
                                    f.clone()
                                } else {
                                    format!("{p}/{f}")
                                };
                                push(g, t, eco, &[]);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        // Plugin marketplaces are handled by the marketplace parser.
        out
    })
    .clone()
}

/// Marketplace files in any repo (M5).
pub const MARKETPLACE_FILES: [&str; 3] = [
    ".claude-plugin/marketplace.json",
    ".agents/plugins/marketplace.json",
    ".cursor-plugin/marketplace.json",
];

/// The first detector rule a repo path matches.
pub fn detect(path: &str, rules: &[PathRule]) -> Option<(ItemType, Ecosystem)> {
    let name = path.rsplit('/').next().unwrap_or(path);
    for r in rules {
        if r.exclude.iter().any(|x| x == name) {
            continue;
        }
        let Ok(g) = globset::GlobBuilder::new(&r.glob)
            .literal_separator(true)
            .build()
        else {
            continue;
        };
        if g.compile_matcher().is_match(path) {
            return Some((r.item_type, r.ecosystem));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// The vendored copy is kept equal to belai's harnesses.json: when
    /// that changes, re-vendor (jq over `dirs.*.project`) and update both
    /// hashes.
    #[test]
    fn harness_paths_pinned() {
        assert_eq!(
            hex::encode(Sha256::digest(HARNESS_PATHS.as_bytes())),
            HARNESS_PATHS_SHA256
        );
        let v: serde_json::Value = serde_json::from_str(HARNESS_PATHS).unwrap();
        assert_eq!(
            v["source_sha256"],
            "ffa77bfc42772c5de0fa4cd648954f393ffa1ddf1af612bebee60459d26bcb72"
        );
    }

    const HARNESS_PATHS_SHA256: &str = include_str!("harness_paths.sha256");

    #[test]
    fn catalogue_parses() {
        let c = curated();
        assert!(c.iter().any(|r| r.id() == "gh:anthropics/skills"));
        let huge: Vec<_> = c
            .iter()
            .filter(|r| r.huge)
            .map(|r| r.repo.as_str())
            .collect();
        assert_eq!(huge, vec!["hermes-agent", "openclaw"]);
        for r in &c {
            for p in &r.paths {
                crate::git::globs(&[p.glob.as_str()]).unwrap();
            }
        }
    }

    #[test]
    fn taps_parse() {
        let t = Tap::parse("obra/superpowers@v1", &[]).unwrap();
        assert_eq!(
            (t.id(), t.git_ref.as_deref()),
            ("gh:obra/superpowers".into(), Some("v1"))
        );
        assert_eq!(
            Tap::parse("https://github.com/a/b.git", &[]).unwrap().repo,
            "b"
        );
        for bad in ["a", "a/b/c", "../x", "a/b@../x", "a b/c"] {
            assert!(Tap::parse(bad, &[]).is_err(), "{bad}");
        }
        assert!(Tap::parse("a/b", &["/etc/*".into()]).is_err());
    }

    #[test]
    fn detector_paths() {
        let r = harness_rules();
        let d = |p: &str| detect(p, &r);
        assert_eq!(
            d(".claude/agents/x/rev.md"),
            Some((ItemType::Agent, Ecosystem::ClaudeCode))
        );
        assert_eq!(
            d(".claude/skills/pdf/SKILL.md"),
            Some((ItemType::Skill, Ecosystem::ClaudeCode))
        );
        assert_eq!(
            d(".gemini/commands/gcs/sync.toml"),
            Some((ItemType::Command, Ecosystem::GeminiCli))
        );
        assert_eq!(
            d(".cursor/rules/a.mdc"),
            Some((ItemType::Prompt, Ecosystem::Cursor))
        );
        assert_eq!(
            d(".github/hooks/x/hooks.json"),
            Some((ItemType::Hook, Ecosystem::Copilot))
        );
        assert_eq!(d(".roomodes"), Some((ItemType::Agent, Ecosystem::Roo)));
        assert_eq!(d("AGENTS.md"), Some((ItemType::Prompt, Ecosystem::Generic)));
        assert_eq!(d(".kiro/steering/README.md").map(|x| x.0), None);
        assert_eq!(d("src/main.rs"), None);
        // Vendored dirs reach harnesses beyond the §4.3 list.
        assert!(r.iter().any(|x| x.glob.starts_with(".adal/skills")));
    }
}

/// The ecosystems that publish items of type `t`, from the harness path
/// rules, the curated repos' rules and the MCP marketplaces. The Discover
/// tab pre-filters its ecosystem list with it when a type pill is picked.
pub fn ecosystems_for(t: ItemType) -> Vec<Ecosystem> {
    let mut out: Vec<Ecosystem> = Vec::new();
    let rules = harness_rules();
    let curated = curated();
    let all = rules
        .iter()
        .chain(curated.iter().flat_map(|c| c.paths.iter()));
    for r in all.filter(|r| r.item_type == t) {
        if !out.contains(&r.ecosystem) {
            out.push(r.ecosystem);
        }
    }
    if t == ItemType::Mcp {
        for e in [Ecosystem::McpRegistry, Ecosystem::Cline, Ecosystem::LobeHub] {
            if !out.contains(&e) {
                out.push(e);
            }
        }
    }
    if !out.contains(&Ecosystem::Generic) {
        out.push(Ecosystem::Generic);
    }
    out
}

#[cfg(test)]
mod ecosystems_for_tests {
    use super::*;

    #[test]
    fn a_type_names_the_ecosystems_that_publish_it() {
        let cmd = ecosystems_for(ItemType::Command);
        assert!(cmd.contains(&Ecosystem::GeminiCli) && cmd.contains(&Ecosystem::Windsurf));
        assert!(!cmd.contains(&Ecosystem::McpRegistry));
        let mcp = ecosystems_for(ItemType::Mcp);
        assert!(mcp.contains(&Ecosystem::McpRegistry) && mcp.contains(&Ecosystem::Cline));
        assert!(!ecosystems_for(ItemType::Prompt).contains(&Ecosystem::McpRegistry));
    }
}
