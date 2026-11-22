//! Curated GitHub repos (G1–G14, M1–M4) and the person's taps (Gx),
//! crawled without credentials: `ls-refs` over smart HTTP gives the commit;
//! an unchanged commit stops there; otherwise ONE codeload tarball at that
//! commit, filtered to the catalogue globs. Repos marked `huge` are never
//! tarballed; their items come raw by path (S2 entries, or `gh` trees).
//!
//! [`fetch_github`] is the shared item fetch every GitHub-backed source
//! uses: pinned by commit, files rooted at the item, exact URLs recorded.

use super::{describe, gh_code, marketplace, seal, tag};
use crate::catalogue::{self, CuratedRepo, PathRule, Tap};
use crate::fetch::Ctx;
use crate::git::{self, Limits, Tree};
use crate::licence::Licence;
use crate::scan::Flag;
use crate::source::{
    Ecosystem, Fetched, Hit, ItemType, Locator, Mode, Refresh, Source, Trust, Upstream,
};
use anyhow::{Result, bail};
use bytes::Bytes;
use std::collections::BTreeMap;

/// One crawled repo source.
pub struct GitHubRepo {
    id: String,
    label: String,
    owner: String,
    repo: String,
    git_ref: Option<String>,
    rules: Vec<PathRule>,
    keep: Vec<String>,
    marketplaces: Vec<String>,
    huge: bool,
    licence: String,
    trust: Trust,
    stars: u64,
    discovery: Option<String>,
    types: Vec<ItemType>,
    ecos: Vec<Ecosystem>,
    /// Tap globs without a type: each kept file is typed by the detector.
    untyped: bool,
}

impl GitHubRepo {
    pub fn curated(c: CuratedRepo) -> Self {
        let mut types: Vec<ItemType> = c.paths.iter().map(|p| p.item_type).collect();
        let mut ecos: Vec<Ecosystem> = c.paths.iter().map(|p| p.ecosystem).collect();
        if !c.marketplaces.is_empty() {
            types.extend([ItemType::Skill]);
        }
        if c.discovery.is_some() {
            types.push(ItemType::Skill);
            ecos.push(Ecosystem::Generic);
        }
        if c.huge {
            types.push(ItemType::Skill);
            ecos.push(if c.repo == "hermes-agent" {
                Ecosystem::Hermes
            } else {
                Ecosystem::OpenClaw
            });
        }
        types.sort();
        types.dedup();
        ecos.sort();
        ecos.dedup();
        Self {
            id: c.id(),
            label: format!("{}/{}", c.owner, c.repo),
            owner: c.owner,
            repo: c.repo,
            git_ref: c.git_ref,
            rules: c.paths,
            keep: c.keep,
            marketplaces: c.marketplaces,
            huge: c.huge,
            licence: c.licence,
            trust: c.trust,
            stars: c.stars,
            discovery: c.discovery,
            types,
            ecos,
            untyped: false,
        }
    }

    /// A tap: its globs (typed by the tap or the detector), else the
    /// harness-path detector over the whole repo; marketplaces always.
    pub fn tap(t: Tap) -> Self {
        let rules: Vec<PathRule> = if t.globs.is_empty() {
            catalogue::harness_rules()
        } else {
            t.globs
                .iter()
                .map(|g| PathRule {
                    glob: g.clone(),
                    // Typed later per file when the tap gives no type.
                    item_type: t.item_type.unwrap_or(ItemType::Skill),
                    ecosystem: t.ecosystem.unwrap_or(Ecosystem::Generic),
                    exclude: vec!["README.md".into()],
                })
                .collect()
        };
        let typed = t.item_type.is_some();
        Self {
            id: t.id(),
            label: t.spec(),
            owner: t.owner.clone(),
            repo: t.repo.clone(),
            git_ref: t.git_ref.clone(),
            keep: vec![],
            marketplaces: catalogue::MARKETPLACE_FILES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            huge: false,
            licence: String::new(),
            trust: Trust::Community,
            stars: 0,
            discovery: None,
            types: t
                .item_type
                .map(|x| vec![x])
                .unwrap_or_else(|| ItemType::ALL.to_vec()),
            ecos: t
                .ecosystem
                .map(|x| vec![x])
                .unwrap_or_else(|| Ecosystem::ALL.to_vec()),
            untyped: !typed && !t.globs.is_empty(),
            rules,
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }

    fn keep_set(&self) -> Result<globset::GlobSet> {
        let mut pats: Vec<&str> = self.rules.iter().map(|r| r.glob.as_str()).collect();
        pats.extend(self.keep.iter().map(String::as_str));
        pats.extend(self.marketplaces.iter().map(String::as_str));
        if self.discovery.as_deref() == Some("readme") {
            pats.push("README.md");
        }
        git::globs(&pats)
    }

    /// The type and dialect of a kept file, if it is an item.
    fn classify(&self, path: &str) -> Option<(ItemType, Ecosystem)> {
        if self.untyped {
            let set = git::globs(
                &self
                    .rules
                    .iter()
                    .map(|r| r.glob.as_str())
                    .collect::<Vec<_>>(),
            )
            .ok()?;
            if !set.is_match(path) {
                return None;
            }
            return catalogue::detect(path, &catalogue::harness_rules()).or_else(|| {
                path.ends_with("SKILL.md")
                    .then_some((ItemType::Skill, Ecosystem::AgentSkills))
            });
        }
        catalogue::detect(path, &self.rules)
    }

    /// Items in a crawled tree.
    fn hits_from_tree(&self, tree: &Tree, sha: &str) -> Vec<Hit> {
        let licence = super::hint_licence(Some(&self.licence), "catalogue");
        let mut out = Vec::new();
        for (path, data) in &tree.files {
            if git::always_keep(path) && !path.ends_with("SKILL.md") && !path.ends_with(".md") {
                // Licence and manifest files are not items themselves
                // (marketplace.json is parsed below).
                if !(path.ends_with(".mcp.json") || path.ends_with("gemini-extension.json")) {
                    continue;
                }
            }
            let Some((t, eco)) = self.classify(path) else {
                continue;
            };
            for (name, entry) in entries(path, t, data, &self.repo) {
                let (title_name, desc) = describe(path, data);
                let mut tags = vec![];
                if let Some(e) = &entry {
                    tags.push(format!("entry:{e}"));
                }
                if let Some(p) = plugin_of(path) {
                    tags.push(format!("plugin:{p}"));
                }
                let lic = item_licence(tree, path, data).unwrap_or_else(|| licence.clone());
                out.push(Hit {
                    source: self.id.clone(),
                    locator: Locator::GitHub {
                        owner: self.owner.clone(),
                        repo: self.repo.clone(),
                        git_ref: Some(sha.to_string()),
                        path: item_locator_path(path, t),
                    },
                    item_type: t,
                    ecosystem: eco,
                    title: title_name
                        .filter(|_| entry.is_none())
                        .unwrap_or_else(|| name.clone()),
                    name,
                    description: if entry.is_some() { String::new() } else { desc },
                    tags,
                    licence: lic,
                    trust: self.trust,
                    popularity: (self.stars > 0).then_some(self.stars),
                    updated_at: None,
                    flags: vec![],
                });
            }
        }
        for m in &self.marketplaces {
            if let Some(data) = tree.files.get(m) {
                out.extend(marketplace::hits(
                    &self.id,
                    &self.owner,
                    &self.repo,
                    sha,
                    m,
                    data,
                    self.trust,
                    &licence,
                ));
            }
        }
        if self.discovery.as_deref() == Some("readme")
            && let Some(data) = tree.files.get("README.md")
        {
            out.extend(super::awesome_csv::readme_hits(
                &self.id,
                std::str::from_utf8(data).unwrap_or_default(),
            ));
        }
        out
    }
}

/// The plugin a component path belongs to (`plugins/x/agents/a.md` → x).
fn plugin_of(path: &str) -> Option<&str> {
    let mut it = path.split('/');
    match it.next()? {
        "plugins" | "external_plugins" => it.next(),
        _ => None,
    }
}

/// The licence an item carries itself: front matter `license`, else a
/// `LICENSE*` next to it.
fn item_licence(tree: &Tree, path: &str, data: &[u8]) -> Option<Licence> {
    if path.ends_with(".md")
        && let Ok(t) = std::str::from_utf8(data)
        && let Some(l) = super::front_matter(t).get("license")
    {
        return Some(Licence::from_id(
            l.split_whitespace()
                .next()
                .unwrap_or(l)
                .trim_end_matches('.'),
            "frontmatter",
        ));
    }
    let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    tree.files.iter().find_map(|(p, b)| {
        let (pd, name) = p.rsplit_once('/').unwrap_or(("", p));
        let up = name.to_ascii_uppercase();
        (pd == dir && (up.starts_with("LICENSE") || up.starts_with("LICENCE")))
            .then(|| crate::licence::detect_text(&String::from_utf8_lossy(b)))
            .flatten()
            .map(|id| Licence::from_id(&id, "LICENSE"))
    })
}

/// The locator path of an item: a skill's dir, else the file.
pub fn item_locator_path(path: &str, t: ItemType) -> String {
    if t == ItemType::Skill && path.ends_with("/SKILL.md") {
        path.trim_end_matches("/SKILL.md").to_string()
    } else if t == ItemType::Skill && path == "SKILL.md" {
        String::new()
    } else {
        path.to_string()
    }
}

fn stem(name: &str) -> &str {
    for ext in [
        ".agent.md",
        ".instructions.md",
        ".prompt.md",
        ".clinerules.md",
        ".md",
        ".mdc",
        ".toml",
        ".json",
    ] {
        if let Some(s) = name.strip_suffix(ext) {
            return s;
        }
    }
    name.trim_start_matches('.')
}

/// The item name(s) a file yields, with the entry key for multi-entry
/// files (`.mcp.json` servers, `.roomodes` modes).
pub fn entries(path: &str, t: ItemType, data: &[u8], repo: &str) -> Vec<(String, Option<String>)> {
    let parts: Vec<&str> = path.split('/').collect();
    let file = *parts.last().unwrap_or(&path);
    let parent = |n: usize| parts.len().checked_sub(n + 1).map(|i| parts[i]);
    match t {
        ItemType::Skill => {
            let n = parent(1).unwrap_or(repo);
            vec![(n.to_string(), None)]
        }
        ItemType::Mcp => {
            let v: serde_json::Value = serde_json::from_slice(data).unwrap_or_default();
            let servers = v.get("mcpServers").and_then(|s| s.as_object());
            match servers {
                Some(m) if !m.is_empty() => {
                    m.keys().map(|k| (k.clone(), Some(k.clone()))).collect()
                }
                _ => vec![],
            }
        }
        ItemType::Agent if file == ".roomodes" => {
            let v: serde_json::Value = serde_json::from_slice(data)
                .ok()
                .or_else(|| serde_norway::from_slice(data).ok())
                .unwrap_or_default();
            v.get("customModes")
                .and_then(|m| m.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m.get("slug").and_then(|s| s.as_str()))
                        .map(|s| (s.to_string(), Some(s.to_string())))
                        .collect()
                })
                .unwrap_or_default()
        }
        ItemType::Hook => {
            let n = match parent(1) {
                Some("hooks") | Some(".claude") | Some(".github") | Some(".codex")
                | Some(".cursor") => parent(2).unwrap_or(repo),
                Some(p) => p,
                None => repo,
            };
            vec![(n.to_string(), None)]
        }
        ItemType::Command => {
            // Name from the path below the commands dir: a/b.md -> a:b.
            let base = parts
                .iter()
                .rposition(|p| matches!(*p, "commands" | "command" | "workflows" | "prompts"))
                .map(|i| i + 1)
                .unwrap_or(parts.len() - 1);
            let rel: Vec<&str> = parts[base..].to_vec();
            let mut segs: Vec<String> = rel.iter().map(|s| s.to_string()).collect();
            if let Some(l) = segs.last_mut() {
                *l = stem(l).to_string();
            }
            vec![(segs.join(":"), None)]
        }
        ItemType::Prompt | ItemType::Agent => {
            let s = stem(file);
            let n = if file.starts_with('.') && !file.ends_with(".md") {
                // `.cursorrules`, `.clinerules`: the folder names it.
                parent(1).unwrap_or(repo).to_string()
            } else if matches!(
                file,
                "AGENTS.md" | "CLAUDE.md" | "GEMINI.md" | "copilot-instructions.md"
            ) {
                format!("{repo}-{}", s.to_ascii_lowercase())
            } else {
                s.to_string()
            };
            vec![(n, None)]
        }
    }
}

#[async_trait::async_trait]
impl Source for GitHubRepo {
    fn id(&self) -> &str {
        &self.id
    }
    fn label(&self) -> &str {
        &self.label
    }
    fn types(&self) -> &[ItemType] {
        &self.types
    }
    fn ecosystems(&self) -> &[Ecosystem] {
        &self.ecos
    }
    fn mode(&self) -> Mode {
        Mode::Indexed
    }

    async fn refresh(&self, cx: &Ctx, cursor: Option<&str>) -> Result<Refresh> {
        if self.huge {
            // Never tarballed; items come from the Hermes index / raw.
            return Ok(Refresh {
                not_modified: true,
                ..Default::default()
            });
        }
        let refs = cx.ls_refs(&self.owner, &self.repo).await?;
        let Some(sha) = git::pick_ref(&refs, self.git_ref.as_deref()) else {
            bail!(
                "{}: no ref {}",
                self.label,
                self.git_ref.as_deref().unwrap_or("HEAD")
            );
        };
        if cursor == Some(sha.as_str()) {
            return Ok(Refresh {
                cursor: Some(sha),
                not_modified: true,
                ..Default::default()
            });
        }
        let keep = self.keep_set()?;
        let tree = cx
            .tarball(&self.owner, &self.repo, &sha, &keep, Limits::default())
            .await?;
        Ok(Refresh {
            hits: self.hits_from_tree(&tree, &sha),
            cursor: Some(sha),
            not_modified: false,
            partial: tree.truncated,
        })
    }

    async fn fetch(&self, cx: &Ctx, hit: &Hit) -> Result<Fetched> {
        let Locator::GitHub {
            owner,
            repo,
            git_ref,
            path,
        } = &hit.locator
        else {
            bail!("{} serves GitHub items only", self.id);
        };
        if let Some(why) = tag(hit, "refused") {
            return Ok(marketplace::refused_fetched(hit, why, cx.now_ms));
        }
        if hit.tags.iter().any(|t| t == "discovery") {
            return super::awesome_csv::fetch_candidate(cx, hit).await;
        }
        let own = owner == &self.owner && repo == &self.repo;
        let mut f = fetch_github(
            cx,
            GhItem {
                owner,
                repo,
                git_ref: git_ref.as_deref(),
                path,
                item_type: hit.item_type,
                raw_only: own && self.huge,
                licence_hint: if own {
                    Some(self.licence.as_str())
                } else {
                    None
                },
                name_hint: &hit.name,
                plugin: tag(hit, "plugin").is_some() && !own,
            },
        )
        .await?;
        // A marketplace entry pinned to `sha` with a `ref`: the ref must
        // still name that commit.
        if let (Some(r), Some(pinned)) = (tag(hit, "ref"), git_ref.as_deref())
            && git::is_sha(pinned)
        {
            match resolve(cx, owner, repo, Some(r)).await {
                Ok(now) if now != pinned => f.flags.push(Flag::Refused(format!(
                    "upstream ref {r} is now {} but the marketplace pins {}",
                    &now[..7],
                    &pinned[..7]
                ))),
                Ok(_) => {}
                Err(e) => f
                    .flags
                    .push(Flag::Refused(format!("cannot check ref {r}: {e}"))),
            }
        }
        Ok(f)
    }

    async fn latest(&self, cx: &Ctx, loc: &Locator) -> Result<Option<Upstream>> {
        let Locator::GitHub { owner, repo, .. } = loc else {
            return Ok(None);
        };
        let want = if owner == &self.owner && repo == &self.repo {
            self.git_ref.as_deref()
        } else {
            None
        };
        latest_commit(cx, owner, repo, want).await
    }
}

/// The newest commit of a repo (HEAD or `want`).
pub async fn latest_commit(
    cx: &Ctx,
    owner: &str,
    repo: &str,
    want: Option<&str>,
) -> Result<Option<Upstream>> {
    let sha = match resolve(cx, owner, repo, want).await {
        Ok(s) => s,
        Err(e) if e.to_string().contains("no ref") => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(Some(Upstream {
        commit: Some(sha),
        fetched_at: cx.now_ms,
        ..Default::default()
    }))
}

/// The commit a ref names: `ls-refs` over smart HTTP (free), or, when the
/// ref advertisement is too large (repos with huge PR ref lists such as
/// hermes-agent), the public commits feed (`commits/<ref>.atom`, free),
/// then one REST call (`commits/<ref>` as a bare SHA; counted in the
/// anonymous 55/h budget).
pub async fn resolve(cx: &Ctx, owner: &str, repo: &str, want: Option<&str>) -> Result<String> {
    match cx.resolve(owner, repo, want).await {
        Ok(s) => Ok(s),
        Err(e) if e.to_string().contains("byte cap") => {
            let r = want.filter(|w| !w.is_empty()).unwrap_or("HEAD");
            let feed = format!(
                "https://github.com/{owner}/{repo}/commits/{}.atom",
                crate::fetch::urlencode(r)
            );
            if let Ok(resp) = cx.get_req(&feed, "application/atom+xml", 1024 * 1024).await
                && resp.ok()
                && let Some(sha) = first_feed_commit(&resp.body)
            {
                return Ok(sha);
            }
            let url = format!(
                "https://api.github.com/repos/{owner}/{repo}/commits/{}",
                crate::fetch::urlencode(r)
            );
            let resp = cx.get_req(&url, "application/vnd.github.sha", 4096).await?;
            if !resp.ok() {
                bail!("{url} answered {}", resp.status);
            }
            let sha = resp.text()?.trim().to_string();
            if !git::is_sha(&sha) {
                bail!("{url}: not a commit SHA");
            }
            Ok(sha)
        }
        Err(e) => Err(e),
    }
}

/// The newest commit id in a GitHub commits feed
/// (`<id>tag:github.com,2008:Grit::Commit/<sha></id>`).
pub fn first_feed_commit(body: &[u8]) -> Option<String> {
    let t = std::str::from_utf8(body).ok()?;
    let i = t.find("Grit::Commit/")? + "Grit::Commit/".len();
    let sha = t.get(i..i + 40)?;
    git::is_sha(sha).then(|| sha.to_string())
}

/// What [`fetch_github`] fetches.
pub struct GhItem<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    /// A commit SHA (pinned) or a ref to resolve (`None`: HEAD).
    pub git_ref: Option<&'a str>,
    /// A skill's dir, a hook's `hooks.json` / `settings.json`, a file,
    /// or a plugin dir.
    pub path: &'a str,
    pub item_type: ItemType,
    /// Raw by path only (huge repos, single-file indexes).
    pub raw_only: bool,
    pub licence_hint: Option<&'a str>,
    pub name_hint: &'a str,
    /// The path is a whole plugin dir.
    pub plugin: bool,
}

fn parent_dir(p: &str) -> &str {
    p.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

/// The item root and the repo paths to keep, by type.
fn layout(it: &GhItem<'_>) -> (String, Vec<String>, Option<String>) {
    let p = it.path.trim_matches('/');
    let under = |root: &str, g: &str| {
        if root.is_empty() {
            g.to_string()
        } else {
            format!("{root}/{g}")
        }
    };
    if it.plugin {
        return (p.to_string(), vec![under(p, "**")], None);
    }
    match it.item_type {
        ItemType::Skill => {
            let root = if p.ends_with("SKILL.md") {
                parent_dir(p).to_string()
            } else {
                p.to_string()
            };
            let keep = vec![under(&root, "**")];
            (root, keep, Some("SKILL.md".to_string()))
        }
        ItemType::Hook => {
            let file = p;
            let mut root = parent_dir(file).to_string();
            let base = root.rsplit('/').next().unwrap_or(&root).to_string();
            if matches!(base.as_str(), "hooks" | ".claude" | ".codex" | ".cursor") {
                root = parent_dir(&root).to_string();
            }
            let mut keep = vec![file.to_string()];
            for g in [
                "hooks/**",
                "scripts/**",
                ".claude/hooks/**",
                ".codex/hooks/**",
                ".cursor/hooks/**",
            ] {
                keep.push(under(&root, g));
            }
            if !file.ends_with("/hooks/hooks.json") && !file.ends_with(".claude/settings.json") {
                // Copilot `hooks/<n>/hooks.json`: the dir is the bundle.
                keep.push(under(parent_dir(file), "**"));
            }
            let primary = file
                .strip_prefix(&format!("{root}/"))
                .unwrap_or(file)
                .to_string();
            (root, keep, Some(primary))
        }
        _ => {
            let root = parent_dir(p).to_string();
            let name = p.rsplit('/').next().unwrap_or(p).to_string();
            (root, vec![p.to_string()], Some(name))
        }
    }
}

/// Fetches one GitHub item pinned to a commit.
pub async fn fetch_github(cx: &Ctx, it: GhItem<'_>) -> Result<Fetched> {
    let sha = resolve(cx, it.owner, it.repo, it.git_ref).await?;
    let (root, keep, mut primary) = layout(&it);
    let rel = |p: &str| -> Option<String> {
        if root.is_empty() {
            Some(p.to_string())
        } else {
            p.strip_prefix(&format!("{root}/")).map(str::to_string)
        }
    };
    let mut f = Fetched {
        name_hint: it.name_hint.to_string(),
        licence_hint: it
            .licence_hint
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        ..Default::default()
    };
    f.upstream.commit = Some(sha.clone());
    f.upstream.fetched_at = cx.now_ms;
    if it.raw_only {
        // Raw by path: the primary file, or the dir listing through `gh`.
        let mut paths: Vec<String> = vec![];
        if cx.gh.is_some()
            && it.item_type == ItemType::Skill
            && let Ok(list) = gh_code::tree(cx, it.owner, it.repo, &sha, &root).await
        {
            paths = list.into_iter().take(32).collect();
        }
        let main = match &primary {
            Some(p) if !root.is_empty() && it.item_type == ItemType::Skill => format!("{root}/{p}"),
            Some(_) if it.item_type != ItemType::Skill => it.path.trim_matches('/').to_string(),
            Some(p) => p.clone(),
            None => it.path.to_string(),
        };
        if !paths.contains(&main) {
            paths.insert(0, main);
        }
        for p in paths {
            let (url, body) = cx.raw(it.owner, it.repo, &sha, &p).await?;
            if let Some(r) = rel(&p).and_then(|r| git::clean_path(&r).ok()) {
                f.upstream.urls.push(url.clone());
                f.file_urls.insert(r.clone(), url);
                f.files.insert(r, body);
            }
        }
    } else {
        let mut pats: Vec<&str> = keep.iter().map(String::as_str).collect();
        pats.push("LICENSE*");
        pats.push("LICENCE*");
        let set = git::globs(&pats)?;
        let tree = cx
            .tarball(it.owner, it.repo, &sha, &set, Limits::default())
            .await?;
        let keep_set = git::globs(&keep.iter().map(String::as_str).collect::<Vec<_>>())?;
        f.upstream.urls.push(tree.url.clone());
        // Licence files up the tree from the item root.
        let mut ancestors = vec![root.clone()];
        let mut d = root.as_str();
        while !d.is_empty() {
            d = parent_dir(d);
            ancestors.push(d.to_string());
        }
        for (p, b) in &tree.files {
            let (dir, name) = p.rsplit_once('/').unwrap_or(("", p));
            let up = name.to_ascii_uppercase();
            if (up.starts_with("LICENSE") || up.starts_with("LICENCE"))
                && ancestors.iter().any(|a| a == dir)
            {
                f.licence_files.push((p.clone(), b.clone()));
            }
            if !keep_set.is_match(p) {
                continue;
            }
            let Some(r) = rel(p) else { continue };
            f.file_urls
                .insert(r.clone(), Ctx::raw_url(it.owner, it.repo, &sha, p));
            f.files.insert(r, b.clone());
        }
        // Ancestors' LICENSE come first-nearest.
        f.licence_files
            .sort_by_key(|(p, _)| std::cmp::Reverse(p.matches('/').count()));
        if f.files.is_empty() {
            bail!(
                "{}/{}@{}: nothing at {}",
                it.owner,
                it.repo,
                &sha[..7],
                it.path
            );
        }
    }
    if it.plugin {
        primary = f
            .files
            .keys()
            .find(|k| k.starts_with("skills/") && k.ends_with("/SKILL.md"))
            .cloned()
            .or_else(|| f.files.keys().find(|k| k.as_str() == "SKILL.md").cloned());
    }
    f.primary = primary.filter(|p| f.files.contains_key(p));
    Ok(seal(f))
}

/// A Fetched from raw bytes already in hand (tests and indexes).
pub fn from_files(files: BTreeMap<String, Bytes>, primary: &str, upstream: Upstream) -> Fetched {
    seal(Fetched {
        files,
        primary: Some(primary.to_string()),
        upstream,
        ..Default::default()
    })
}
