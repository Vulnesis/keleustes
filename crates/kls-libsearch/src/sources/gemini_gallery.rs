//! S5: the Gemini CLI extension gallery (`geminicli.com/extensions.json`,
//! ~2.2k extensions). Indexed as a catalogue: one hit per item type an
//! extension declares (`hasSkills`, `hasMCP`, `hasHooks`,
//! `hasCustomCommands`, `hasContext`); the extension repo is crawled on
//! demand when a hit is previewed.

use super::github_repo::{GhItem, fetch_github, item_locator_path, latest_commit};
use super::{github_repo_of, hint_licence, rfc3339_ms};
use crate::fetch::{Ctx, INDEX_MAX};
use crate::git::{self, Limits};
use crate::source::{
    Ecosystem, Fetched, Hit, ItemType, Locator, Mode, Refresh, Source, Trust, Upstream,
};
use anyhow::{Result, bail};
use serde_json::Value;

pub const ID: &str = "gemini-gallery";
pub const URL: &str = "https://geminicli.com/extensions.json";

pub struct GeminiGallery;

/// The extension-relative paths that hold items of a type.
pub fn globs_for(t: ItemType) -> &'static [&'static str] {
    match t {
        ItemType::Skill => &["skills/*/SKILL.md"],
        ItemType::Command => &["commands/**/*.toml"],
        ItemType::Hook => &["hooks/hooks.json"],
        ItemType::Mcp => &["gemini-extension.json"],
        ItemType::Prompt => &["GEMINI.md", "AGENTS.md"],
        ItemType::Agent => &["agents/*.md"],
    }
}

fn hits_of(e: &Value) -> Vec<Hit> {
    let s = |k: &str| e.get(k).and_then(|x| x.as_str()).unwrap_or_default();
    let b = |k: &str| e.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    let Some((owner, repo)) = github_repo_of(s("url")) else {
        return vec![];
    };
    let name = if s("extensionName").is_empty() {
        repo.clone()
    } else {
        s("extensionName").to_string()
    };
    let mut out = vec![];
    for (flag, t) in [
        ("hasSkills", ItemType::Skill),
        ("hasMCP", ItemType::Mcp),
        ("hasHooks", ItemType::Hook),
        ("hasCustomCommands", ItemType::Command),
        ("hasContext", ItemType::Prompt),
    ] {
        if !b(flag) {
            continue;
        }
        out.push(Hit {
            source: ID.into(),
            locator: Locator::GitHub {
                owner: owner.clone(),
                repo: repo.clone(),
                git_ref: None,
                path: String::new(),
            },
            item_type: t,
            ecosystem: Ecosystem::GeminiCli,
            name: name.clone(),
            title: s("fullName").to_string(),
            description: if s("extensionDescription").is_empty() {
                s("repoDescription").to_string()
            } else {
                s("extensionDescription").to_string()
            },
            tags: vec![format!("version:{}", s("extensionVersion"))],
            licence: hint_licence(Some(s("licenseKey")), "index"),
            trust: if b("isGoogleOwned") {
                Trust::Official
            } else {
                Trust::Community
            },
            popularity: e.get("stars").and_then(|x| x.as_u64()),
            updated_at: rfc3339_ms(s("lastUpdated")),
            flags: vec![],
        });
    }
    out
}

/// Crawls a repo at HEAD for the first item of `t` under `globs` (the one
/// named `prefer` when there is one), then fetches it pinned.
pub async fn crawl_pick(
    cx: &Ctx,
    owner: &str,
    repo: &str,
    t: ItemType,
    globs: &[&str],
    prefer: &str,
    licence_hint: Option<&str>,
) -> Result<Fetched> {
    let sha = super::github_repo::resolve(cx, owner, repo, None).await?;
    let set = git::globs(globs)?;
    let tree = cx
        .tarball(owner, repo, &sha, &set, Limits::default())
        .await?;
    let mut items: Vec<&String> = tree
        .files
        .keys()
        .filter(|p| set.is_match(p.as_str()))
        .collect();
    items.sort_by_key(|p| (!p.contains(prefer), p.len()));
    let Some(path) = items.first() else {
        bail!("{owner}/{repo}@{}: no {} found", &sha[..7], t.as_str());
    };
    let lp = item_locator_path(path, t);
    let name = super::github_repo::entries(path, t, &tree.files[*path], repo)
        .into_iter()
        .next()
        .map(|x| x.0)
        .unwrap_or_else(|| prefer.to_string());
    fetch_github(
        cx,
        GhItem {
            owner,
            repo,
            git_ref: Some(&sha),
            path: &lp,
            item_type: t,
            raw_only: false,
            licence_hint,
            name_hint: &name,
            plugin: false,
        },
    )
    .await
}

#[async_trait::async_trait]
impl Source for GeminiGallery {
    fn id(&self) -> &str {
        ID
    }
    fn label(&self) -> &str {
        "Gemini gallery"
    }
    fn types(&self) -> &[ItemType] {
        &[
            ItemType::Skill,
            ItemType::Command,
            ItemType::Hook,
            ItemType::Mcp,
            ItemType::Prompt,
        ]
    }
    fn ecosystems(&self) -> &[Ecosystem] {
        &[Ecosystem::GeminiCli]
    }
    fn mode(&self) -> Mode {
        Mode::Indexed
    }

    async fn refresh(&self, cx: &Ctx, _cursor: Option<&str>) -> Result<Refresh> {
        let r = cx.get(URL, INDEX_MAX).await?;
        if r.not_modified {
            return Ok(Refresh {
                not_modified: true,
                ..Default::default()
            });
        }
        let v: Vec<Value> = r.json()?;
        Ok(Refresh {
            hits: v.iter().flat_map(hits_of).collect(),
            ..Default::default()
        })
    }

    async fn fetch(&self, cx: &Ctx, hit: &Hit) -> Result<Fetched> {
        let Locator::GitHub { owner, repo, .. } = &hit.locator else {
            bail!("not a gallery item");
        };
        let lic = (!hit.licence.spdx.is_empty()).then_some(hit.licence.spdx.as_str());
        crawl_pick(
            cx,
            owner,
            repo,
            hit.item_type,
            globs_for(hit.item_type),
            &hit.name,
            lic,
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
