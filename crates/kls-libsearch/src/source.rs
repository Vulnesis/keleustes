//! What a source is and what it returns: [`Source`], [`Query`], [`Hit`],
//! [`Locator`], [`Fetched`].

use crate::fetch::Ctx;
use crate::licence::{Licence, LicencePolicy};
use crate::scan::Flag;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The library types search covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemType {
    Agent,
    Skill,
    Command,
    Hook,
    Mcp,
    Prompt,
}

impl ItemType {
    pub const ALL: [ItemType; 6] = [
        ItemType::Agent,
        ItemType::Skill,
        ItemType::Command,
        ItemType::Hook,
        ItemType::Mcp,
        ItemType::Prompt,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ItemType::Agent => "agent",
            ItemType::Skill => "skill",
            ItemType::Command => "command",
            ItemType::Hook => "hook",
            ItemType::Mcp => "mcp",
            ItemType::Prompt => "prompt",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }

    /// The `library_items.kind` it imports into (`None` for agents, which
    /// go to `agent_profiles`).
    pub fn library_kind(self) -> Option<&'static str> {
        match self {
            ItemType::Agent => None,
            t => Some(t.as_str()),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ItemType::Agent => "Agent",
            ItemType::Skill => "Skill",
            ItemType::Command => "Command",
            ItemType::Hook => "Hook",
            ItemType::Mcp => "MCP",
            ItemType::Prompt => "Prompt",
        }
    }
}

/// Where a definition comes from (its dialect).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Ecosystem {
    ClaudeCode,
    Codex,
    Hermes,
    OpenClaw,
    GeminiCli,
    Cursor,
    Cline,
    Roo,
    Copilot,
    OpenCode,
    Windsurf,
    McpRegistry,
    LobeHub,
    AgentSkills,
    Generic,
}

impl Ecosystem {
    pub const ALL: [Ecosystem; 15] = [
        Ecosystem::ClaudeCode,
        Ecosystem::Codex,
        Ecosystem::Hermes,
        Ecosystem::OpenClaw,
        Ecosystem::GeminiCli,
        Ecosystem::Cursor,
        Ecosystem::Cline,
        Ecosystem::Roo,
        Ecosystem::Copilot,
        Ecosystem::OpenCode,
        Ecosystem::Windsurf,
        Ecosystem::McpRegistry,
        Ecosystem::LobeHub,
        Ecosystem::AgentSkills,
        Ecosystem::Generic,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Ecosystem::ClaudeCode => "claude-code",
            Ecosystem::Codex => "codex",
            Ecosystem::Hermes => "hermes",
            Ecosystem::OpenClaw => "openclaw",
            Ecosystem::GeminiCli => "gemini-cli",
            Ecosystem::Cursor => "cursor",
            Ecosystem::Cline => "cline",
            Ecosystem::Roo => "roo",
            Ecosystem::Copilot => "copilot",
            Ecosystem::OpenCode => "opencode",
            Ecosystem::Windsurf => "windsurf",
            Ecosystem::McpRegistry => "mcp-registry",
            Ecosystem::LobeHub => "lobehub",
            Ecosystem::AgentSkills => "agent-skills",
            Ecosystem::Generic => "generic",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Ecosystem::ClaudeCode => "Claude Code",
            Ecosystem::Codex => "Codex",
            Ecosystem::Hermes => "Hermes",
            Ecosystem::OpenClaw => "OpenClaw",
            Ecosystem::GeminiCli => "Gemini CLI",
            Ecosystem::Cursor => "Cursor",
            Ecosystem::Cline => "Cline",
            Ecosystem::Roo => "Roo",
            Ecosystem::Copilot => "Copilot",
            Ecosystem::OpenCode => "opencode",
            Ecosystem::Windsurf => "Windsurf",
            Ecosystem::McpRegistry => "MCP",
            Ecosystem::LobeHub => "LobeHub",
            Ecosystem::AgentSkills => "Agent Skills",
            Ecosystem::Generic => "Generic",
        }
    }
}

/// Official = the vendor's own repo or registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Trust {
    Official,
    Trusted,
    Community,
}

impl Trust {
    pub const ALL: [Trust; 3] = [Trust::Official, Trust::Trusted, Trust::Community];

    pub fn as_str(self) -> &'static str {
        match self {
            Trust::Official => "official",
            Trust::Trusted => "trusted",
            Trust::Community => "community",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "official" => Trust::Official,
            "trusted" => Trust::Trusted,
            _ => Trust::Community,
        }
    }

    /// The ranking boost.
    pub fn boost(self) -> f64 {
        match self {
            Trust::Official => 2.0,
            Trust::Trusted => 1.0,
            Trust::Community => 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// Crawled into the local index.
    Indexed,
    /// Queried live and merged in.
    Remote,
    Both,
}

impl Mode {
    pub fn indexed(self) -> bool {
        matches!(self, Mode::Indexed | Mode::Both)
    }
    pub fn remote(self) -> bool {
        matches!(self, Mode::Remote | Mode::Both)
    }
    pub fn label(self) -> &'static str {
        match self {
            Mode::Indexed => "I",
            Mode::Remote => "R",
            Mode::Both => "I+R",
        }
    }
}

/// Where one item lives upstream.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Locator {
    /// `path` is the item root (a SKILL.md dir, a plugin dir or one file).
    /// `git_ref` is a commit SHA when known (pinned), else a branch/tag to
    /// resolve before fetching.
    GitHub {
        owner: String,
        repo: String,
        git_ref: Option<String>,
        path: String,
    },
    ClawHub {
        owner: String,
        slug: String,
        version: Option<String>,
    },
    Mcp {
        name: String,
        version: String,
    },
    LobeHub {
        id: String,
    },
    /// A single file at a URL (well-known, aitmpl raw).
    Url {
        url: String,
    },
}

impl Locator {
    /// A short label for a list row (`owner/repo`, `clawhub`, the URL host).
    pub fn short(&self) -> String {
        match self {
            Locator::GitHub { owner, repo, .. } => format!("{owner}/{repo}"),
            Locator::ClawHub { owner, slug, .. } => format!("{owner}/{slug}"),
            Locator::Mcp { name, .. } => name.clone(),
            Locator::LobeHub { id } => id.clone(),
            Locator::Url { url } => url
                .split("://")
                .nth(1)
                .and_then(|r| r.split('/').next())
                .unwrap_or(url)
                .to_string(),
        }
    }

    /// A page a person can open for it.
    pub fn web_url(&self) -> String {
        match self {
            Locator::GitHub {
                owner,
                repo,
                git_ref,
                path,
            } => {
                let r = git_ref.as_deref().unwrap_or("HEAD");
                if path.is_empty() {
                    format!("https://github.com/{owner}/{repo}")
                } else {
                    format!("https://github.com/{owner}/{repo}/tree/{r}/{path}")
                }
            }
            Locator::ClawHub { owner, slug, .. } => format!("https://clawhub.ai/{owner}/{slug}"),
            Locator::Mcp { name, .. } => format!(
                "https://registry.modelcontextprotocol.io/v0.1/servers/{}/versions/latest",
                crate::fetch::urlencode(name)
            ),
            Locator::LobeHub { id } => format!("https://lobehub.com/assistants/{id}"),
            Locator::Url { url } => url.clone(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// The upstream identity of fetched content.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upstream {
    /// 40-hex commit, when git-backed.
    pub commit: Option<String>,
    /// Registry / semver / ClawHub version.
    pub version: Option<String>,
    /// sha256 over the fetched source files (sorted `path \0 bytes`).
    pub sha256: String,
    /// The exact immutable URLs fetched (provenance).
    pub urls: Vec<String>,
    pub fetched_at: i64,
}

/// One search result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    /// Source id: "mcp-registry", "clawhub", "gh:anthropics/skills".
    pub source: String,
    pub locator: Locator,
    pub item_type: ItemType,
    pub ecosystem: Ecosystem,
    pub name: String,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub licence: Licence,
    pub trust: Trust,
    /// Stars / installs / downloads.
    pub popularity: Option<u64>,
    pub updated_at: Option<i64>,
    /// Upstream signals (isSuspicious, skillspector).
    pub flags: Vec<Flag>,
}

impl Hit {
    /// The index row id: `source|locator_json|type`.
    pub fn id(&self) -> String {
        format!(
            "{}|{}|{}",
            self.source,
            self.locator.to_json(),
            self.item_type.as_str()
        )
    }
}

/// A search.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub text: String,
    /// Empty: every type.
    pub types: Vec<ItemType>,
    /// Empty: every ecosystem.
    pub ecosystems: Vec<Ecosystem>,
    /// Empty: every source.
    pub sources: Vec<String>,
    /// Empty: every trust level.
    pub trust: Vec<Trust>,
    pub licence: LicencePolicy,
    pub hide_flagged: bool,
    pub limit: usize,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            text: String::new(),
            types: vec![],
            ecosystems: vec![],
            sources: vec![],
            trust: vec![],
            licence: LicencePolicy::Any,
            hide_flagged: false,
            limit: 200,
        }
    }
}

impl Query {
    pub fn text(t: &str) -> Self {
        Self {
            text: t.to_string(),
            ..Default::default()
        }
    }

    /// Whether a hit passes the filters (not the text).
    pub fn admits(&self, h: &Hit) -> bool {
        (self.types.is_empty() || self.types.contains(&h.item_type))
            && (self.ecosystems.is_empty() || self.ecosystems.contains(&h.ecosystem))
            && (self.sources.is_empty() || self.sources.contains(&h.source))
            && (self.trust.is_empty() || self.trust.contains(&h.trust))
            && self.licence.admits(&h.licence)
            && !(self.hide_flagged && !h.flags.is_empty())
    }
}

/// What an indexed refresh returns.
#[derive(Clone, Debug, Default)]
pub struct Refresh {
    pub hits: Vec<Hit>,
    /// Where the next call continues (registry cursor, `updated_since`).
    pub cursor: Option<String>,
    /// Nothing changed upstream (304 or the same commit): keep the rows.
    pub not_modified: bool,
    /// The refresh returned a delta (cursor-based): do not retire rows.
    pub partial: bool,
}

/// The source files of one item, rooted at the item.
#[derive(Clone, Debug, Default)]
pub struct Fetched {
    /// Relative, sanitised paths.
    pub files: BTreeMap<String, Bytes>,
    pub upstream: Upstream,
    /// `LICENSE*` files found up the tree (path, bytes).
    pub licence_files: Vec<(String, Bytes)>,
    /// Licence metadata known from the index / registry (step 3 and 4 of
    /// detection), e.g. `licenseKey`, a marketplace entry's `license`.
    pub licence_hint: Option<String>,
    /// Upstream flags (ClawHub moderation, aitmpl skillspector).
    pub flags: Vec<Flag>,
    /// The file URL each source path came from (provenance `files_json`).
    pub file_urls: BTreeMap<String, String>,
    /// The item's name as the source shows it (the converter slugs it).
    /// For a file holding several entries (`.roomodes`, `.mcp.json`, a
    /// settings `hooks` key) it picks the entry.
    pub name_hint: String,
    /// The main file (`SKILL.md`, `commands/gcs/sync.toml`,
    /// `hooks/hooks.json`, `server.json`), when the source knows it;
    /// converters fall back to known names.
    pub primary: Option<String>,
}

impl Fetched {
    /// sha256 over the files: sorted `path \0 bytes` (each entry followed
    /// by `\0`).
    pub fn files_sha256(&self) -> String {
        files_sha256(&self.files)
    }

    /// One file as UTF-8 text, when it is.
    pub fn text(&self, path: &str) -> Option<&str> {
        self.files
            .get(path)
            .and_then(|b| std::str::from_utf8(b).ok())
    }
}

/// sha256 over sorted `path \0 bytes \0`.
pub fn files_sha256(files: &BTreeMap<String, Bytes>) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for (p, b) in files {
        h.update(p.as_bytes());
        h.update([0u8]);
        h.update(b);
        h.update([0u8]);
    }
    hex::encode(h.finalize())
}

/// One place items are published.
#[async_trait::async_trait]
pub trait Source: Send + Sync {
    fn id(&self) -> &str;
    fn label(&self) -> &str;
    fn types(&self) -> &[ItemType];
    fn ecosystems(&self) -> &[Ecosystem];
    fn mode(&self) -> Mode;
    fn needs_gh(&self) -> bool {
        false
    }
    /// Off unless the person turns it on (Hermes index, skills.sh).
    fn default_on(&self) -> bool {
        true
    }
    /// Indexed sources: crawl into the local index. `cursor` is what the
    /// last call returned.
    async fn refresh(&self, cx: &Ctx, cursor: Option<&str>) -> anyhow::Result<Refresh> {
        let _ = (cx, cursor);
        Ok(Refresh {
            not_modified: true,
            ..Default::default()
        })
    }
    /// Remote sources: live query.
    async fn search(&self, cx: &Ctx, q: &Query) -> anyhow::Result<Vec<Hit>> {
        let _ = (cx, q);
        Ok(vec![])
    }
    /// Whether this source serves `loc` (fetch / latest are routed by it).
    fn owns(&self, hit_source: &str) -> bool {
        hit_source == self.id()
    }
    /// Source files of one item, rooted at the item.
    async fn fetch(&self, cx: &Ctx, hit: &Hit) -> anyhow::Result<Fetched>;
    /// Newest upstream for an imported item; `None` if unknown.
    async fn latest(&self, cx: &Ctx, loc: &Locator) -> anyhow::Result<Option<Upstream>>;
}
