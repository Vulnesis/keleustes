//! WP8: the optional `gh` boost. With a signed-in local `gh` (passed in as
//! `Ctx::gh`, budgeted by kls-forge), GitHub code search finds items in
//! any public repo (`filename:SKILL.md`, `path:.claude/agents`, ...; 10/min),
//! `git/trees?recursive=1` lists dirs of huge repos, and the licence API
//! fills an unknown licence. Without `gh` this source is off and the
//! helpers answer errors; nothing else depends on it.

use super::github_repo::{GhItem, fetch_github, item_locator_path, latest_commit};
use crate::catalogue;
use crate::fetch::{Class, Ctx, urlencode};
use crate::licence::Licence;
use crate::source::{
    Ecosystem, Fetched, Hit, ItemType, Locator, Mode, Query, Source, Trust, Upstream,
};
use anyhow::{Result, anyhow, bail};
use serde_json::Value;

pub const ID: &str = "gh-code";

pub struct GhCode;

/// The code-search qualifiers per type.
pub fn qualifiers(t: ItemType) -> &'static [&'static str] {
    match t {
        ItemType::Skill => &["filename:SKILL.md"],
        ItemType::Agent => &["path:.claude/agents"],
        ItemType::Command => &["path:.claude/commands"],
        ItemType::Hook => &["filename:hooks.json"],
        ItemType::Mcp => &["filename:.mcp.json"],
        ItemType::Prompt => &["filename:AGENTS.md"],
    }
}

async fn gh_json(cx: &Ctx, path: &str) -> Result<Value> {
    cx.check().await?;
    let gh = cx
        .gh
        .as_ref()
        .ok_or_else(|| anyhow!("gh is not signed in"))?;
    let class = cx.class;
    gh.get_json(path, class).await
}

/// Paths under `dir` at `sha` (blobs only), through `gh`.
pub async fn tree(cx: &Ctx, owner: &str, repo: &str, sha: &str, dir: &str) -> Result<Vec<String>> {
    let v = gh_json(
        cx,
        &format!("repos/{owner}/{repo}/git/trees/{sha}?recursive=1"),
    )
    .await?;
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{}/", dir.trim_matches('/'))
    };
    Ok(v.get("tree")
        .and_then(|t| t.as_array())
        .into_iter()
        .flatten()
        .filter(|e| e.get("type").and_then(|x| x.as_str()) == Some("blob"))
        .filter_map(|e| e.get("path").and_then(|x| x.as_str()))
        .filter(|p| p.starts_with(&prefix))
        .map(str::to_string)
        .collect())
}

/// The repo licence (SPDX id) through `gh`.
pub async fn licence(cx: &Ctx, owner: &str, repo: &str) -> Result<Option<String>> {
    let v = gh_json(cx, &format!("repos/{owner}/{repo}/license")).await?;
    Ok(v.get("license")
        .and_then(|l| l.get("spdx_id"))
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string))
}

fn hit_of(item: &Value, t: ItemType) -> Option<Hit> {
    let path = item.get("path")?.as_str()?;
    let full = item.get("repository")?.get("full_name")?.as_str()?;
    let (owner, repo) = full.split_once('/')?;
    crate::git::clean_path(path).ok()?;
    let eco = catalogue::detect(path, &catalogue::harness_rules())
        .map(|x| x.1)
        .unwrap_or(Ecosystem::Generic);
    let name = super::github_repo::entries(path, t, b"", repo)
        .into_iter()
        .next()
        .map(|x| x.0)
        .unwrap_or_else(|| repo.to_string());
    Some(Hit {
        source: ID.into(),
        locator: Locator::GitHub {
            owner: owner.into(),
            repo: repo.into(),
            git_ref: None,
            path: item_locator_path(path, t),
        },
        item_type: t,
        ecosystem: eco,
        title: name.clone(),
        name,
        description: format!("{full}: {path}"),
        tags: vec![],
        licence: Licence::unknown(),
        trust: Trust::Community,
        popularity: None,
        updated_at: None,
        flags: vec![],
    })
}

#[async_trait::async_trait]
impl Source for GhCode {
    fn id(&self) -> &str {
        ID
    }
    fn label(&self) -> &str {
        "GitHub code search"
    }
    fn types(&self) -> &[ItemType] {
        &ItemType::ALL
    }
    fn ecosystems(&self) -> &[Ecosystem] {
        &[
            Ecosystem::ClaudeCode,
            Ecosystem::AgentSkills,
            Ecosystem::Generic,
        ]
    }
    fn mode(&self) -> Mode {
        Mode::Remote
    }
    fn needs_gh(&self) -> bool {
        true
    }

    async fn search(&self, cx: &Ctx, q: &Query) -> Result<Vec<Hit>> {
        if cx.gh.is_none() || q.text.trim().is_empty() {
            return Ok(vec![]);
        }
        let types: Vec<ItemType> = if q.types.is_empty() {
            vec![ItemType::Skill]
        } else {
            q.types.clone()
        };
        let mut out = vec![];
        // At most two searches per query: the limit is 10/min.
        for t in types.into_iter().take(2) {
            for qual in qualifiers(t) {
                let query = format!("{} {qual}", q.text.trim());
                let v = gh_json(
                    cx,
                    &format!(
                        "search/code?q={}&per_page={}",
                        urlencode(&query),
                        q.limit.clamp(1, 30)
                    ),
                )
                .await?;
                out.extend(
                    v.get("items")
                        .and_then(|i| i.as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|i| hit_of(i, t)),
                );
            }
        }
        Ok(out)
    }

    async fn fetch(&self, cx: &Ctx, hit: &Hit) -> Result<Fetched> {
        let Locator::GitHub {
            owner,
            repo,
            git_ref,
            path,
        } = &hit.locator
        else {
            bail!("not a GitHub item");
        };
        let lic = if cx.gh.is_some() {
            licence(cx, owner, repo).await.ok().flatten()
        } else {
            None
        };
        fetch_github(
            cx,
            GhItem {
                owner,
                repo,
                git_ref: git_ref.as_deref(),
                path,
                item_type: hit.item_type,
                raw_only: false,
                licence_hint: lic.as_deref(),
                name_hint: &hit.name,
                plugin: false,
            },
        )
        .await
    }

    async fn latest(&self, cx: &Ctx, loc: &Locator) -> Result<Option<Upstream>> {
        match loc {
            Locator::GitHub { owner, repo, .. } => latest_commit(cx, owner, repo, None).await,
            _ => Ok(None),
        }
    }
}

/// The class to use for a gh call made on a person's behalf.
pub fn class_for(user: bool) -> Class {
    if user { Class::User } else { Class::Background }
}
