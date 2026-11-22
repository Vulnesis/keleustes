//! S8: the Cline MCP marketplace (`api.cline.bot/v1/mcp/marketplace`).
//! Discovery only: entries carry a GitHub URL and no server config. A
//! preview fetches the repo README at a resolved commit, so the review
//! shows the setup text ("README setup", drafted by hand).

use super::github_repo::latest_commit;
use super::{github_repo_of, rfc3339_ms, seal};
use crate::fetch::{Ctx, INDEX_MAX};
use crate::licence::Licence;
use crate::source::{
    Ecosystem, Fetched, Hit, ItemType, Locator, Mode, Refresh, Source, Trust, Upstream,
};
use anyhow::{Result, bail};
use bytes::Bytes;
use serde_json::Value;

pub const ID: &str = "cline-mcp";
pub const URL: &str = "https://api.cline.bot/v1/mcp/marketplace";
/// The file a discovery-only Fetched carries besides the README.
pub const ENTRY_FILE: &str = "cline-marketplace.json";

pub struct ClineMcp;

fn hit_of(e: &Value) -> Option<Hit> {
    let s = |k: &str| e.get(k).and_then(|x| x.as_str()).unwrap_or_default();
    let (owner, repo) = github_repo_of(s("githubUrl"))?;
    Some(Hit {
        source: ID.into(),
        locator: Locator::GitHub {
            owner,
            repo: repo.clone(),
            git_ref: None,
            path: String::new(),
        },
        item_type: ItemType::Mcp,
        ecosystem: Ecosystem::Cline,
        name: repo,
        title: s("name").into(),
        description: s("description").into(),
        tags: e
            .get("tags")
            .and_then(|t| t.as_array())
            .map(|t| {
                t.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        licence: Licence::unknown(),
        trust: Trust::Community,
        popularity: e.get("downloadCount").and_then(|x| x.as_u64()),
        updated_at: rfc3339_ms(s("updatedAt")),
        flags: vec![],
    })
}

#[async_trait::async_trait]
impl Source for ClineMcp {
    fn id(&self) -> &str {
        ID
    }
    fn label(&self) -> &str {
        "Cline MCP"
    }
    fn types(&self) -> &[ItemType] {
        &[ItemType::Mcp]
    }
    fn ecosystems(&self) -> &[Ecosystem] {
        &[Ecosystem::Cline]
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
            hits: v.iter().filter_map(hit_of).collect(),
            ..Default::default()
        })
    }

    async fn fetch(&self, cx: &Ctx, hit: &Hit) -> Result<Fetched> {
        let Locator::GitHub { owner, repo, .. } = &hit.locator else {
            bail!("not a Cline marketplace item");
        };
        let sha = super::github_repo::resolve(cx, owner, repo, None).await?;
        let mut f = Fetched {
            name_hint: hit.name.clone(),
            ..Default::default()
        };
        f.upstream.commit = Some(sha.clone());
        f.upstream.fetched_at = cx.now_ms;
        for p in ["README.md", "readme.md", "README"] {
            let url = Ctx::raw_url(owner, repo, &sha, p);
            let r = cx
                .get_req(&url, "text/plain", crate::fetch::FILE_MAX)
                .await?;
            if r.ok() {
                f.files.insert("README.md".into(), r.body);
                f.file_urls.insert("README.md".into(), url.clone());
                f.upstream.urls.push(url);
                break;
            }
        }
        let entry = serde_json::json!({
            "discoveryOnly": true,
            "name": hit.title,
            "githubUrl": format!("https://github.com/{owner}/{repo}"),
            "note": "README setup: no server config is published",
        });
        f.files.insert(
            ENTRY_FILE.into(),
            Bytes::from(serde_json::to_vec_pretty(&entry)?),
        );
        f.primary = Some(ENTRY_FILE.into());
        Ok(seal(f))
    }

    async fn latest(&self, cx: &Ctx, loc: &Locator) -> Result<Option<Upstream>> {
        match loc {
            Locator::GitHub { owner, repo, .. } => latest_commit(cx, owner, repo, None).await,
            _ => Ok(None),
        }
    }
}
