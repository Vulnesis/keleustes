//! S1: the official MCP Registry (`registry.modelcontextprotocol.io`).
//! Indexed (cursor pages, then `updated_since` deltas) and remote
//! (`search=`). Items are fetched at an exact version.

use super::{ms_rfc3339, pkg_facts, rfc3339_ms, single};
use crate::fetch::{Ctx, INDEX_MAX, urlencode};
use crate::licence::Licence;
use crate::scan::Flag;
use crate::source::{
    Ecosystem, Fetched, Hit, ItemType, Locator, Mode, Query, Refresh, Source, Trust, Upstream,
};
use anyhow::{Result, bail};
use bytes::Bytes;
use serde_json::Value;

pub const ID: &str = "mcp-registry";
pub const BASE: &str = "https://registry.modelcontextprotocol.io/v0.1";
const PAGE: usize = 100;
/// Pages per refresh call; the rest continue on the next call.
const MAX_PAGES: usize = 60;

pub struct McpRegistry;

/// The URL of one server version.
pub fn version_url(name: &str, version: &str) -> String {
    format!(
        "{BASE}/servers/{}/versions/{}",
        urlencode(name),
        urlencode(version)
    )
}

fn hit_of(entry: &Value) -> Option<Hit> {
    let s = entry.get("server")?;
    let name = s.get("name")?.as_str()?.to_string();
    let version = s
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("latest");
    let meta = entry
        .get("_meta")
        .and_then(|m| m.get("io.modelcontextprotocol.registry/official"));
    let status = meta
        .and_then(|m| m.get("status"))
        .and_then(|x| x.as_str())
        .unwrap_or("active");
    if status == "deleted" {
        return None;
    }
    let mut flags = vec![];
    if status == "deprecated" {
        flags.push(Flag::Upstream("registry: deprecated".into()));
    }
    let short = name.rsplit('/').next().unwrap_or(&name).to_string();
    let title = s
        .get("title")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| short.clone());
    let trust = if name.starts_with("io.modelcontextprotocol/") {
        Trust::Official
    } else {
        Trust::Community
    };
    let mut tags: Vec<String> = vec![name.clone()];
    for p in s
        .get("packages")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
    {
        if let Some(t) = p.get("registryType").and_then(|x| x.as_str()) {
            tags.push(t.to_string());
        }
    }
    if s.get("remotes")
        .and_then(|r| r.as_array())
        .is_some_and(|r| !r.is_empty())
    {
        tags.push("remote".into());
    }
    Some(Hit {
        source: ID.into(),
        locator: Locator::Mcp {
            name: name.clone(),
            version: version.to_string(),
        },
        item_type: ItemType::Mcp,
        ecosystem: Ecosystem::McpRegistry,
        name: short,
        title,
        description: s
            .get("description")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        tags,
        licence: Licence::unknown(),
        trust,
        popularity: None,
        updated_at: meta
            .and_then(|m| m.get("updatedAt"))
            .and_then(|x| x.as_str())
            .and_then(rfc3339_ms),
        flags,
    })
}

/// One page: hits, the next cursor, the newest `updatedAt` seen.
fn page(v: &Value) -> (Vec<Hit>, Option<String>, Option<i64>) {
    let mut newest = None;
    let hits: Vec<Hit> = v
        .get("servers")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let h = hit_of(e)?;
            newest = newest.max(h.updated_at);
            Some(h)
        })
        .collect();
    let next = v
        .get("metadata")
        .and_then(|m| m.get("nextCursor"))
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    (hits, next, newest)
}

/// Cursor grammar: `since:<rfc3339>` (a finished crawl), or
/// `page:<registry cursor>|since:<rfc3339>` (a crawl in progress).
fn parse_cursor(c: Option<&str>) -> (Option<String>, Option<String>) {
    let mut page = None;
    let mut since = None;
    for part in c.unwrap_or_default().split('|') {
        if let Some(p) = part.strip_prefix("page:") {
            page = Some(p.to_string());
        } else if let Some(s) = part.strip_prefix("since:") {
            since = Some(s.to_string());
        }
    }
    (page, since)
}

#[async_trait::async_trait]
impl Source for McpRegistry {
    fn id(&self) -> &str {
        ID
    }
    fn label(&self) -> &str {
        "MCP registry"
    }
    fn types(&self) -> &[ItemType] {
        &[ItemType::Mcp]
    }
    fn ecosystems(&self) -> &[Ecosystem] {
        &[Ecosystem::McpRegistry]
    }
    fn mode(&self) -> Mode {
        Mode::Both
    }

    async fn refresh(&self, cx: &Ctx, cursor: Option<&str>) -> Result<Refresh> {
        let (mut next, since) = parse_cursor(cursor);
        let delta = since.is_some() && next.is_none();
        let mut hits = Vec::new();
        let mut newest: Option<i64> = since.as_deref().and_then(rfc3339_ms);
        let mut all_cached = true;
        for _ in 0..MAX_PAGES {
            let mut url = format!("{BASE}/servers?version=latest&limit={PAGE}");
            if let Some(c) = &next {
                url.push_str(&format!("&cursor={}", urlencode(c)));
            }
            if delta && let Some(s) = &since {
                url.push_str(&format!("&updated_since={}", urlencode(s)));
            }
            let r = cx.get(&url, INDEX_MAX).await?;
            all_cached &= r.not_modified;
            let v: Value = r.json()?;
            let (h, n, nw) = page(&v);
            hits.extend(h);
            newest = newest.max(nw);
            next = n;
            if next.is_none() {
                break;
            }
        }
        let since_s = newest.map(ms_rfc3339).or(since);
        let cursor = match (&next, &since_s) {
            (Some(p), Some(s)) => Some(format!("page:{p}|since:{s}")),
            (Some(p), None) => Some(format!("page:{p}")),
            (None, Some(s)) => Some(format!("since:{s}")),
            (None, None) => None,
        };
        Ok(Refresh {
            not_modified: all_cached && hits.is_empty(),
            hits,
            cursor,
            // A delta or an unfinished crawl never retires rows.
            partial: delta || next.is_some(),
        })
    }

    async fn search(&self, cx: &Ctx, q: &Query) -> Result<Vec<Hit>> {
        if q.text.trim().is_empty() {
            return Ok(vec![]);
        }
        let url = format!(
            "{BASE}/servers?search={}&version=latest&limit={}",
            urlencode(q.text.trim()),
            q.limit.clamp(1, 50)
        );
        let v: Value = cx.get_json(&url, INDEX_MAX).await?;
        Ok(page(&v).0)
    }

    async fn fetch(&self, cx: &Ctx, hit: &Hit) -> Result<Fetched> {
        let Locator::Mcp { name, version } = &hit.locator else {
            bail!("not a registry item");
        };
        let version = if version.is_empty() || version == "latest" {
            match self.latest(cx, &hit.locator).await? {
                Some(u) => u.version.unwrap_or_default(),
                None => bail!("{name}: no latest version"),
            }
        } else {
            version.clone()
        };
        let url = version_url(name, &version);
        let r = cx.get(&url, crate::fetch::FILE_MAX).await?;
        let v: Value = r.json()?;
        let server = v.get("server").cloned().unwrap_or(v);
        if server.get("name").and_then(|x| x.as_str()) != Some(name.as_str()) {
            bail!("{url}: the registry answered another server");
        }
        let got = server
            .get("version")
            .and_then(|x| x.as_str())
            .unwrap_or_default();
        if got != version {
            bail!("{url}: asked for {version}, got {got}");
        }
        let body = Bytes::from(serde_json::to_vec_pretty(&server)?);
        let mut f = single("server.json", body, &url, Some(version), cx.now_ms);
        f.name_hint = name.clone();
        // Package facts for the preview (licence, latest): best effort.
        if let Some(p) = server
            .get("packages")
            .and_then(|p| p.as_array())
            .and_then(|a| a.first())
        {
            let id = p
                .get("identifier")
                .and_then(|x| x.as_str())
                .unwrap_or_default();
            let facts = match p.get("registryType").and_then(|x| x.as_str()) {
                Some("npm") => pkg_facts::npm(cx, id).await.ok(),
                Some("pypi") => pkg_facts::pypi(cx, id).await.ok(),
                _ => None,
            };
            if let Some(fc) = facts {
                f.licence_hint = fc.licence.clone();
                f.upstream.urls.push(fc.url.clone());
            }
        }
        Ok(f)
    }

    async fn latest(&self, cx: &Ctx, loc: &Locator) -> Result<Option<Upstream>> {
        let Locator::Mcp { name, .. } = loc else {
            return Ok(None);
        };
        let url = version_url(name, "latest");
        let v: Value = cx.get_json(&url, crate::fetch::FILE_MAX).await?;
        let s = v.get("server").unwrap_or(&v);
        Ok(s.get("version")
            .and_then(|x| x.as_str())
            .map(|ver| Upstream {
                version: Some(ver.to_string()),
                urls: vec![version_url(name, ver)],
                fetched_at: cx.now_ms,
                ..Default::default()
            }))
    }
}
