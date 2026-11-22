//! S2: the Hermes skills index (`skills-index.json`, ~45 MB raw, 104k
//! rows, mostly ClawHub mirrors). Opt-in. Parsed element by element (no
//! document tree), keeping the GitHub-backed rows (official, github,
//! skills.sh); ClawHub rows are left to S3, which knows their owners.
//! Items are fetched raw by path at a resolved commit (hermes-agent is far
//! too large to tarball).

use super::github_repo::{GhItem, fetch_github, latest_commit};
use crate::fetch::{Ctx, INDEX_MAX};
use crate::licence::Licence;
use crate::source::{
    Ecosystem, Fetched, Hit, ItemType, Locator, Mode, Refresh, Source, Trust, Upstream,
};
use anyhow::{Result, bail};
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt;

pub const ID: &str = "hermes-index";
pub const URL: &str = "https://nousresearch.github.io/hermes-agent/docs/api/skills-index.json";

pub struct HermesIndex;

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    trust_level: String,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    resolved_github_id: Option<String>,
}

fn clean(p: &str) -> Option<String> {
    crate::git::clean_path(p.trim_matches('/')).ok()
}

fn hit_of(e: Entry) -> Option<Hit> {
    let (owner, repo, path, trust) = match e.source.as_str() {
        "official" => {
            let (o, r) = e.repo.split_once('/')?;
            (
                o.to_string(),
                r.to_string(),
                clean(&e.path)?,
                Trust::Official,
            )
        }
        "github" => {
            let (o, r) = e.repo.split_once('/')?;
            let t = if e.trust_level == "trusted" {
                Trust::Trusted
            } else {
                Trust::Community
            };
            (o.to_string(), r.to_string(), clean(&e.path)?, t)
        }
        "skills.sh" | "skills-sh" => {
            let id = e.resolved_github_id.as_deref()?;
            let mut it = id.splitn(3, '/');
            let (o, r, p) = (it.next()?, it.next()?, it.next()?);
            (o.to_string(), r.to_string(), clean(p)?, Trust::Community)
        }
        _ => return None,
    };
    let hermes = repo == "hermes-agent";
    Some(Hit {
        source: ID.into(),
        locator: Locator::GitHub {
            owner,
            repo,
            git_ref: None,
            path,
        },
        item_type: ItemType::Skill,
        ecosystem: if hermes {
            Ecosystem::Hermes
        } else {
            Ecosystem::AgentSkills
        },
        title: e.name.clone(),
        name: e.name,
        description: e.description,
        tags: e.tags.into_iter().take(12).collect(),
        licence: if hermes {
            Licence::from_id("MIT", "index")
        } else {
            Licence::unknown()
        },
        trust,
        popularity: None,
        updated_at: None,
        flags: vec![],
    })
}

/// The index body, read one skill at a time.
pub struct Parsed {
    pub hits: Vec<Hit>,
    pub generated_at: Option<String>,
    pub rows: u64,
}

struct Skills<'a> {
    hits: &'a mut Vec<Hit>,
    rows: &'a mut u64,
}

impl<'de> DeserializeSeed<'de> for Skills<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Skills<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list of skills")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(e) = seq.next_element::<Entry>()? {
            *self.rows += 1;
            if let Some(h) = hit_of(e) {
                self.hits.push(h);
            }
        }
        Ok(())
    }
}

struct Top;

impl<'de> Visitor<'de> for Top {
    type Value = Parsed;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("the skills index object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Parsed, A::Error> {
        let mut hits = Vec::new();
        let mut rows = 0;
        let mut generated_at = None;
        while let Some(k) = map.next_key::<String>()? {
            match k.as_str() {
                "skills" => map.next_value_seed(Skills {
                    hits: &mut hits,
                    rows: &mut rows,
                })?,
                "generated_at" => generated_at = map.next_value::<Option<String>>()?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Parsed {
            hits,
            generated_at,
            rows,
        })
    }
}

/// Parses an index body element by element.
pub fn parse(body: &[u8]) -> Result<Parsed> {
    let mut de = serde_json::Deserializer::from_slice(body);
    Ok(de.deserialize_map(Top)?)
}

#[async_trait::async_trait]
impl Source for HermesIndex {
    fn id(&self) -> &str {
        ID
    }
    fn label(&self) -> &str {
        "Hermes index"
    }
    fn types(&self) -> &[ItemType] {
        &[ItemType::Skill]
    }
    fn ecosystems(&self) -> &[Ecosystem] {
        &[Ecosystem::Hermes, Ecosystem::AgentSkills]
    }
    fn mode(&self) -> Mode {
        Mode::Indexed
    }
    fn default_on(&self) -> bool {
        false
    }

    async fn refresh(&self, cx: &Ctx, cursor: Option<&str>) -> Result<Refresh> {
        let r = cx.get(URL, INDEX_MAX).await?;
        let p = parse(&r.body)?;
        if r.not_modified || (cursor.is_some() && p.generated_at.as_deref() == cursor) {
            return Ok(Refresh {
                cursor: p.generated_at,
                not_modified: true,
                ..Default::default()
            });
        }
        Ok(Refresh {
            hits: p.hits,
            cursor: p.generated_at,
            not_modified: false,
            partial: false,
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
            bail!("not a Hermes index item");
        };
        fetch_github(
            cx,
            GhItem {
                owner,
                repo,
                git_ref: git_ref.as_deref(),
                path,
                item_type: ItemType::Skill,
                raw_only: true,
                licence_hint: (repo == "hermes-agent").then_some("MIT"),
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
