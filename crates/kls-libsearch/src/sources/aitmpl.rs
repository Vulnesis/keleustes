//! S6: aitmpl / claude-code-templates (`www.aitmpl.com/components.json`).
//! Indexed. Content is fetched raw from `davila7/claude-code-templates` at
//! a resolved commit (`cli-tool/components/<type>s/<path>`), never from the
//! branch. aitmpl's `skillspector` verdict other than SAFE is a
//! `Flag::Upstream` (shown, never trusted as safe).

use super::github_repo::{GhItem, fetch_github, latest_commit};
use super::hint_licence;
use crate::fetch::{Ctx, INDEX_MAX};
use crate::scan::Flag;
use crate::source::{
    Ecosystem, Fetched, Hit, ItemType, Locator, Mode, Refresh, Source, Trust, Upstream,
};
use anyhow::{Result, bail};
use serde_json::Value;

pub const ID: &str = "aitmpl";
pub const URL: &str = "https://www.aitmpl.com/components.json";
pub const OWNER: &str = "davila7";
pub const REPO: &str = "claude-code-templates";

pub struct Aitmpl;

fn hits_of(v: &Value) -> Vec<Hit> {
    let mut out = vec![];
    for (key, t, dir) in [
        ("agents", ItemType::Agent, "agents"),
        ("commands", ItemType::Command, "commands"),
        ("hooks", ItemType::Hook, "hooks"),
        ("mcps", ItemType::Mcp, "mcps"),
        ("skills", ItemType::Skill, "skills"),
    ] {
        for c in v.get(key).and_then(|x| x.as_array()).into_iter().flatten() {
            let s = |k: &str| c.get(k).and_then(|x| x.as_str()).unwrap_or_default();
            let path = s("path");
            let name = s("name");
            if path.is_empty() || name.is_empty() || crate::git::clean_path(path).is_err() {
                continue;
            }
            let mut flags = vec![];
            if let Some(sp) = c.get("skillspector") {
                let rec = sp
                    .get("recommendation")
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                if !rec.is_empty() && rec != "SAFE" {
                    let sev = sp.get("severity").and_then(|x| x.as_str()).unwrap_or("");
                    flags.push(Flag::Upstream(
                        format!("skillspector: {rec} {sev}").trim().to_string(),
                    ));
                }
            }
            let lic = s("license");
            out.push(Hit {
                source: ID.into(),
                locator: Locator::GitHub {
                    owner: OWNER.into(),
                    repo: REPO.into(),
                    git_ref: None,
                    path: format!("cli-tool/components/{dir}/{path}"),
                },
                item_type: t,
                ecosystem: Ecosystem::ClaudeCode,
                name: name.into(),
                title: name.into(),
                description: s("description").chars().take(300).collect(),
                tags: [s("category")]
                    .into_iter()
                    .filter(|x| !x.is_empty())
                    .map(str::to_string)
                    .collect(),
                // The repo is MIT when an entry names none.
                licence: hint_licence(Some(if lic.is_empty() { "MIT" } else { lic }), "index"),
                trust: Trust::Community,
                popularity: c.get("downloads").and_then(|x| x.as_u64()),
                updated_at: None,
                flags,
            });
        }
    }
    out
}

#[async_trait::async_trait]
impl Source for Aitmpl {
    fn id(&self) -> &str {
        ID
    }
    fn label(&self) -> &str {
        "aitmpl"
    }
    fn types(&self) -> &[ItemType] {
        &[
            ItemType::Agent,
            ItemType::Skill,
            ItemType::Command,
            ItemType::Hook,
            ItemType::Mcp,
        ]
    }
    fn ecosystems(&self) -> &[Ecosystem] {
        &[Ecosystem::ClaudeCode]
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
        Ok(Refresh {
            hits: hits_of(&r.json()?),
            ..Default::default()
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
            bail!("not an aitmpl item");
        };
        let mut f = fetch_github(
            cx,
            GhItem {
                owner,
                repo,
                git_ref: git_ref.as_deref(),
                path,
                item_type: hit.item_type,
                raw_only: true,
                licence_hint: Some(hit.licence.spdx.as_str()),
                name_hint: &hit.name,
                plugin: false,
            },
        )
        .await?;
        f.flags.extend(hit.flags.iter().cloned());
        Ok(f)
    }

    async fn latest(&self, cx: &Ctx, _loc: &Locator) -> Result<Option<Upstream>> {
        latest_commit(cx, OWNER, REPO, None).await
    }
}
