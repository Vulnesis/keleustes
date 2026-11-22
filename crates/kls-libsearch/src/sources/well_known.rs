//! S9: `/.well-known/skills/index.json` on a site the person adds. The
//! host joins the fetch allowlist for this source only when the person
//! adds it. An item is `<base>/<name>/<file>` for each file the index
//! lists (`SKILL.md` first).

use super::seal;
use crate::fetch::{Ctx, FILE_MAX, https_host, urlencode};
use crate::licence::Licence;
use crate::source::{
    Ecosystem, Fetched, Hit, ItemType, Locator, Mode, Refresh, Source, Trust, Upstream,
};
use anyhow::{Result, bail};
use serde_json::Value;

pub struct WellKnown {
    id: String,
    url: String,
    base: String,
    host: String,
}

impl WellKnown {
    /// `url` is the index URL, or the site (`https://x.com/docs`) to
    /// append `/.well-known/skills/index.json` to.
    pub fn new(url: &str) -> Result<Self> {
        let url = url.trim().trim_end_matches('/');
        let url = if url.ends_with("/index.json") {
            url.to_string()
        } else {
            format!("{url}/.well-known/skills/index.json")
        };
        if !url.contains("/.well-known/skills/") {
            bail!("not a well-known skills index: {url}");
        }
        let host = https_host(&url)?;
        let base = url
            .trim_end_matches("index.json")
            .trim_end_matches('/')
            .to_string();
        Ok(Self {
            id: format!("well-known:{host}{}", path_of(&base, &host)),
            url,
            base,
            host,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    fn item_url(&self, name: &str, file: &str) -> String {
        format!("{}/{}/{}", self.base, urlencode(name), file)
    }
}

fn path_of(base: &str, host: &str) -> String {
    base.strip_prefix(&format!("https://{host}"))
        .unwrap_or("")
        .trim_end_matches("/.well-known/skills")
        .to_string()
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !n.contains("..")
}

#[async_trait::async_trait]
impl Source for WellKnown {
    fn id(&self) -> &str {
        &self.id
    }
    fn label(&self) -> &str {
        &self.host
    }
    fn types(&self) -> &[ItemType] {
        &[ItemType::Skill]
    }
    fn ecosystems(&self) -> &[Ecosystem] {
        &[Ecosystem::AgentSkills]
    }
    fn mode(&self) -> Mode {
        Mode::Indexed
    }

    async fn refresh(&self, cx: &Ctx, _cursor: Option<&str>) -> Result<Refresh> {
        cx.http.allow_host(&self.host);
        let r = cx.get(&self.url, 8 * 1024 * 1024).await?;
        if r.not_modified {
            return Ok(Refresh {
                not_modified: true,
                ..Default::default()
            });
        }
        let v: Value = r.json()?;
        let hits = v
            .get("skills")
            .and_then(|s| s.as_array())
            .into_iter()
            .flatten()
            .filter_map(|s| {
                let name = s.get("name")?.as_str()?;
                if !valid_name(name) {
                    return None;
                }
                Some(Hit {
                    source: self.id.clone(),
                    locator: Locator::Url {
                        url: self.item_url(name, "SKILL.md"),
                    },
                    item_type: ItemType::Skill,
                    ecosystem: Ecosystem::AgentSkills,
                    name: name.into(),
                    title: name.into(),
                    description: s
                        .get("description")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .chars()
                        .take(300)
                        .collect(),
                    tags: s
                        .get("files")
                        .and_then(|f| f.as_array())
                        .map(|f| {
                            f.iter()
                                .filter_map(|x| x.as_str())
                                .map(|x| format!("file:{x}"))
                                .collect()
                        })
                        .unwrap_or_default(),
                    licence: Licence::unknown(),
                    trust: Trust::Community,
                    popularity: None,
                    updated_at: None,
                    flags: vec![],
                })
            })
            .collect();
        Ok(Refresh {
            hits,
            ..Default::default()
        })
    }

    async fn fetch(&self, cx: &Ctx, hit: &Hit) -> Result<Fetched> {
        cx.http.allow_host(&self.host);
        let mut files: Vec<String> = hit
            .tags
            .iter()
            .filter_map(|t| t.strip_prefix("file:"))
            .filter_map(|f| crate::git::clean_path(f).ok())
            .take(32)
            .collect();
        if !files.iter().any(|f| f == "SKILL.md") {
            files.insert(0, "SKILL.md".into());
        }
        let mut f = Fetched {
            name_hint: hit.name.clone(),
            primary: Some("SKILL.md".into()),
            ..Default::default()
        };
        for p in files {
            let url = self.item_url(&hit.name, &p);
            if https_host(&url)? != self.host {
                bail!("{url} leaves {}", self.host);
            }
            let r = cx.get(&url, FILE_MAX).await?;
            f.upstream.urls.push(url.clone());
            f.file_urls.insert(p.clone(), url);
            f.files.insert(p, r.body);
        }
        f.upstream.fetched_at = cx.now_ms;
        Ok(seal(f))
    }

    async fn latest(&self, cx: &Ctx, loc: &Locator) -> Result<Option<Upstream>> {
        let Locator::Url { url } = loc else {
            return Ok(None);
        };
        cx.http.allow_host(&self.host);
        let r = cx.get(url, FILE_MAX).await?;
        let mut m = std::collections::BTreeMap::new();
        m.insert("SKILL.md".to_string(), r.body);
        Ok(Some(Upstream {
            sha256: crate::source::files_sha256(&m),
            urls: vec![url.clone()],
            fetched_at: cx.now_ms,
            ..Default::default()
        }))
    }
}
