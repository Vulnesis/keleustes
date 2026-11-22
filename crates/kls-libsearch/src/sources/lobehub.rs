//! S7: LobeHub agents (`chat-agents.lobehub.com`). Indexed from
//! `index.json`; an item is its `{identifier}.json` (system role in
//! `config.systemRole`). The repo (`lobehub/lobe-chat-agents`) is MIT.

use super::{hint_licence, rfc3339_ms, single};
use crate::fetch::{Ctx, FILE_MAX, INDEX_MAX, urlencode};
use crate::source::{
    Ecosystem, Fetched, Hit, ItemType, Locator, Mode, Refresh, Source, Trust, Upstream,
};
use anyhow::{Result, bail};
use serde_json::Value;

pub const ID: &str = "lobehub";
pub const BASE: &str = "https://chat-agents.lobehub.com";

pub struct LobeHub;

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !id.contains("..")
}

pub fn detail_url(id: &str) -> String {
    format!("{BASE}/{}.json", urlencode(id))
}

fn hit_of(a: &Value) -> Option<Hit> {
    let id = a.get("identifier")?.as_str()?;
    if !valid_id(id) {
        return None;
    }
    let meta = a.get("meta").cloned().unwrap_or_default();
    let m = |k: &str| meta.get(k).and_then(|x| x.as_str()).unwrap_or_default();
    Some(Hit {
        source: ID.into(),
        locator: Locator::LobeHub { id: id.into() },
        item_type: ItemType::Agent,
        ecosystem: Ecosystem::LobeHub,
        name: id.into(),
        title: m("title").into(),
        description: m("description").into(),
        tags: meta
            .get("tags")
            .and_then(|t| t.as_array())
            .map(|t| {
                t.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        licence: hint_licence(Some("MIT"), "index"),
        trust: Trust::Community,
        popularity: None,
        updated_at: a
            .get("createdAt")
            .and_then(|x| x.as_str())
            .and_then(|d| rfc3339_ms(&format!("{d}T00:00:00Z"))),
        flags: vec![],
    })
}

#[async_trait::async_trait]
impl Source for LobeHub {
    fn id(&self) -> &str {
        ID
    }
    fn label(&self) -> &str {
        "LobeHub"
    }
    fn types(&self) -> &[ItemType] {
        &[ItemType::Agent]
    }
    fn ecosystems(&self) -> &[Ecosystem] {
        &[Ecosystem::LobeHub]
    }
    fn mode(&self) -> Mode {
        Mode::Indexed
    }

    async fn refresh(&self, cx: &Ctx, _cursor: Option<&str>) -> Result<Refresh> {
        let r = cx.get(&format!("{BASE}/index.json"), INDEX_MAX).await?;
        if r.not_modified {
            return Ok(Refresh {
                not_modified: true,
                ..Default::default()
            });
        }
        let v: Value = r.json()?;
        Ok(Refresh {
            hits: v
                .get("agents")
                .and_then(|a| a.as_array())
                .into_iter()
                .flatten()
                .filter_map(hit_of)
                .collect(),
            ..Default::default()
        })
    }

    async fn fetch(&self, cx: &Ctx, hit: &Hit) -> Result<Fetched> {
        let Locator::LobeHub { id } = &hit.locator else {
            bail!("not a LobeHub item");
        };
        if !valid_id(id) {
            bail!("bad LobeHub id");
        }
        let url = detail_url(id);
        let r = cx.get(&url, FILE_MAX).await?;
        let v: Value = r.json()?;
        if v.get("identifier").and_then(|x| x.as_str()) != Some(id.as_str()) {
            bail!("{url}: another agent answered");
        }
        let version = v
            .get("createdAt")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        let mut f = single(&format!("{id}.json"), r.body, &url, version, cx.now_ms);
        f.name_hint = id.clone();
        f.licence_hint = Some("MIT".into());
        Ok(f)
    }

    async fn latest(&self, cx: &Ctx, loc: &Locator) -> Result<Option<Upstream>> {
        let Locator::LobeHub { id } = loc else {
            return Ok(None);
        };
        let url = detail_url(id);
        let r = cx.get(&url, FILE_MAX).await?;
        let mut files = std::collections::BTreeMap::new();
        files.insert(format!("{id}.json"), r.body);
        Ok(Some(Upstream {
            sha256: crate::source::files_sha256(&files),
            urls: vec![url],
            fetched_at: cx.now_ms,
            ..Default::default()
        }))
    }
}
