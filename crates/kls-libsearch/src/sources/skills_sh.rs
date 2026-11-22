//! S4: skills.sh legacy search (`/api/search`, undocumented, Algolia
//! behind it; the documented `/api/v1` needs a token). Feature-flagged:
//! drop it if it starts answering 401. Content comes from GitHub raw at a
//! resolved commit, from `skills/`, `.agents/skills/` or `.claude/skills/`.

use super::github_repo::latest_commit;
use super::seal;
use crate::fetch::{Ctx, urlencode};
use crate::licence::Licence;
use crate::source::{
    Ecosystem, Fetched, Hit, ItemType, Locator, Mode, Query, Source, Trust, Upstream,
};
use anyhow::{Result, bail};
use serde_json::Value;

pub const ID: &str = "skills-sh";

pub struct SkillsSh;

/// Where a skill dir may live in its repo, tried in order.
pub const SKILL_DIRS: [&str; 4] = ["skills/", ".agents/skills/", ".claude/skills/", ""];

#[async_trait::async_trait]
impl Source for SkillsSh {
    fn id(&self) -> &str {
        ID
    }
    fn label(&self) -> &str {
        "skills.sh"
    }
    fn types(&self) -> &[ItemType] {
        &[ItemType::Skill]
    }
    fn ecosystems(&self) -> &[Ecosystem] {
        &[Ecosystem::AgentSkills]
    }
    fn mode(&self) -> Mode {
        Mode::Remote
    }
    fn default_on(&self) -> bool {
        false
    }

    async fn search(&self, cx: &Ctx, q: &Query) -> Result<Vec<Hit>> {
        if q.text.trim().is_empty() {
            return Ok(vec![]);
        }
        let url = format!(
            "https://skills.sh/api/search?q={}&limit={}",
            urlencode(q.text.trim()),
            q.limit.clamp(1, 50)
        );
        let r = cx
            .get_req(&url, "application/json", 4 * 1024 * 1024)
            .await?;
        if r.status == 401 {
            bail!("skills.sh now needs a token (401): turn this source off");
        }
        if !r.ok() {
            bail!("{url} answered {}", r.status);
        }
        let v: Value = r.json()?;
        Ok(v.get("skills")
            .and_then(|s| s.as_array())
            .into_iter()
            .flatten()
            .filter_map(|s| {
                let src = s.get("source")?.as_str()?;
                let (owner, repo) = src.split_once('/')?;
                let id = s.get("skillId")?.as_str()?;
                if id.contains("..") || id.starts_with('/') {
                    return None;
                }
                let name = s.get("name").and_then(|x| x.as_str()).unwrap_or(id);
                Some(Hit {
                    source: ID.into(),
                    locator: Locator::GitHub {
                        owner: owner.into(),
                        repo: repo.into(),
                        git_ref: None,
                        path: id.into(),
                    },
                    item_type: ItemType::Skill,
                    ecosystem: Ecosystem::AgentSkills,
                    name: name.into(),
                    title: name.into(),
                    description: format!("{src} on skills.sh"),
                    tags: vec![],
                    licence: Licence::unknown(),
                    trust: Trust::Community,
                    popularity: s.get("installs").and_then(|x| x.as_u64()),
                    updated_at: None,
                    flags: vec![],
                })
            })
            .collect())
    }

    async fn fetch(&self, cx: &Ctx, hit: &Hit) -> Result<Fetched> {
        let Locator::GitHub {
            owner,
            repo,
            git_ref,
            path,
        } = &hit.locator
        else {
            bail!("not a skills.sh item");
        };
        let sha = super::github_repo::resolve(cx, owner, repo, git_ref.as_deref()).await?;
        for dir in SKILL_DIRS {
            let p = format!("{dir}{path}/SKILL.md");
            let url = Ctx::raw_url(owner, repo, &sha, &p);
            let r = cx
                .get_req(&url, "text/plain", crate::fetch::FILE_MAX)
                .await?;
            if r.status == 404 {
                continue;
            }
            if !r.ok() {
                bail!("{url} answered {}", r.status);
            }
            let mut f = Fetched {
                name_hint: hit.name.clone(),
                primary: Some("SKILL.md".into()),
                ..Default::default()
            };
            f.files.insert("SKILL.md".into(), r.body);
            f.file_urls.insert("SKILL.md".into(), url.clone());
            f.upstream = Upstream {
                commit: Some(sha),
                urls: vec![url],
                fetched_at: cx.now_ms,
                ..Default::default()
            };
            return Ok(seal(f));
        }
        bail!("{owner}/{repo}: no SKILL.md for {path}")
    }

    async fn latest(&self, cx: &Ctx, loc: &Locator) -> Result<Option<Upstream>> {
        match loc {
            Locator::GitHub { owner, repo, .. } => latest_commit(cx, owner, repo, None).await,
            _ => Ok(None),
        }
    }
}
