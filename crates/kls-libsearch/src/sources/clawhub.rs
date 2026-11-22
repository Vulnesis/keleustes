//! S3: ClawHub (OpenClaw skills). Remote: queried live with
//! `nonSuspiciousOnly=true`. Always owner-qualified: a bare slug can answer
//! 409 `AMBIGUOUS_SKILL_SLUG`, resolved from `matches[]`. Items come as the
//! version ZIP (`/download?slug=&version=`), read within caps. ClawHub's
//! `isSuspicious` and moderation become `Flag::Upstream` (shown, never
//! trusted as safe).

use super::seal;
use crate::fetch::{Ctx, urlencode};
use crate::git::{self, Limits};
use crate::licence::Licence;
use crate::scan::Flag;
use crate::source::{
    Ecosystem, Fetched, Hit, ItemType, Locator, Mode, Query, Source, Trust, Upstream,
};
use anyhow::{Result, anyhow, bail};
use bytes::Bytes;
use serde_json::Value;
use std::io::Read;

pub const ID: &str = "clawhub";
pub const API: &str = "https://clawhub.ai/api/v1";
/// A version ZIP may be at most this large.
const ZIP_MAX: u64 = 8 * 1024 * 1024;

pub struct ClawHub;

fn s<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for k in path {
        cur = cur.get(*k)?;
    }
    cur.as_str()
}

fn hit_of(r: &Value) -> Option<Hit> {
    let reference = s(r, &["install", "reference"]).map(str::to_string);
    let (owner, slug) = match reference.as_deref().and_then(|x| x.split_once('/')) {
        Some((o, sl)) => (o.to_string(), sl.to_string()),
        None => (
            s(r, &["ownerHandle"])
                .or_else(|| s(r, &["native", "ownerHandle"]))?
                .to_string(),
            s(r, &["slug"])?.to_string(),
        ),
    };
    let skill = r.get("native").and_then(|n| n.get("skill"));
    let mut flags = vec![];
    if skill
        .and_then(|k| k.get("isSuspicious"))
        .and_then(|x| x.as_bool())
        == Some(true)
    {
        flags.push(Flag::Upstream("clawhub: suspicious".into()));
    }
    if let Some(v) = s(r, &["trust", "clawHubVerdict"]) {
        flags.push(Flag::Upstream(format!("clawhub: {v}")));
    }
    let tags = skill
        .and_then(|k| k.get("topics"))
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(Hit {
        source: ID.into(),
        locator: Locator::ClawHub {
            owner,
            slug: slug.clone(),
            version: None,
        },
        item_type: ItemType::Skill,
        ecosystem: Ecosystem::OpenClaw,
        name: slug.clone(),
        title: s(r, &["displayName"]).unwrap_or(&slug).to_string(),
        description: s(r, &["summary"]).unwrap_or_default().to_string(),
        tags,
        licence: Licence::unknown(),
        trust: if r.get("official").and_then(|x| x.as_bool()) == Some(true) {
            Trust::Trusted
        } else {
            Trust::Community
        },
        popularity: r.get("downloads").and_then(|x| x.as_u64()),
        updated_at: r.get("updatedAt").and_then(|x| x.as_i64()),
        flags,
    })
}

/// `?owner=` for a skill, resolving a 409 ambiguity: the given owner when
/// it is among the matches, else an error listing them.
pub async fn detail(cx: &Ctx, owner: &str, slug: &str) -> Result<(String, Value)> {
    // Owner-qualified whenever the owner is known; a bare slug may be
    // ambiguous.
    let url = if owner.is_empty() {
        format!("{API}/skills/{}", urlencode(slug))
    } else {
        format!(
            "{API}/skills/{}?owner={}",
            urlencode(slug),
            urlencode(owner)
        )
    };
    let r = cx
        .get_req(&url, "application/json", 4 * 1024 * 1024)
        .await?;
    if r.status == 409 {
        let v: Value = r.json()?;
        let refs: Vec<String> = v
            .get("matches")
            .and_then(|m| m.as_array())
            .into_iter()
            .flatten()
            .filter_map(|m| {
                m.get("ownerHandle")
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
            })
            .collect();
        // A bare slug with one owner resolves to it; more need a choice.
        let pick = (owner.is_empty() && refs.len() == 1).then(|| refs[0].clone());
        if let Some(o) = pick {
            let url = format!("{API}/skills/{}?owner={}", urlencode(slug), urlencode(&o));
            return Ok((o, cx.get_json(&url, 4 * 1024 * 1024).await?));
        }
        bail!(
            "{slug} is ambiguous on ClawHub: pick one of {}",
            refs.join(", ")
        );
    }
    if !r.ok() {
        bail!("{url} answered {}", r.status);
    }
    Ok((owner.to_string(), r.json()?))
}

/// Reads a skill ZIP within the tarball caps: regular files only, clean
/// relative paths, no credential files.
pub fn read_zip(data: &[u8], lim: Limits) -> Result<std::collections::BTreeMap<String, Bytes>> {
    let mut z =
        zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| anyhow!("not a ZIP: {e}"))?;
    let mut out = std::collections::BTreeMap::new();
    let mut kept = 0u64;
    let mut expanded = 0u64;
    for i in 0..z.len() {
        let mut e = z.by_index(i)?;
        if e.is_dir() {
            continue;
        }
        if e.is_symlink() {
            continue;
        }
        let Ok(path) = git::clean_path(e.name()) else {
            continue;
        };
        let name = path.rsplit('/').next().unwrap_or(&path);
        if git::credential_name(name) || path.split('/').count() > lim.depth + 1 {
            continue;
        }
        if e.size() > lim.file_bytes || out.len() >= lim.files {
            continue;
        }
        let mut buf = Vec::new();
        (&mut e).take(lim.file_bytes + 1).read_to_end(&mut buf)?;
        expanded += buf.len() as u64;
        if buf.len() as u64 > lim.file_bytes || expanded > lim.expanded_bytes {
            continue;
        }
        if kept + buf.len() as u64 > lim.kept_bytes {
            break;
        }
        kept += buf.len() as u64;
        out.insert(path, Bytes::from(buf));
    }
    Ok(out)
}

#[async_trait::async_trait]
impl Source for ClawHub {
    fn id(&self) -> &str {
        ID
    }
    fn label(&self) -> &str {
        "ClawHub"
    }
    fn types(&self) -> &[ItemType] {
        &[ItemType::Skill]
    }
    fn ecosystems(&self) -> &[Ecosystem] {
        &[Ecosystem::OpenClaw]
    }
    fn mode(&self) -> Mode {
        Mode::Remote
    }

    async fn search(&self, cx: &Ctx, q: &Query) -> Result<Vec<Hit>> {
        if q.text.trim().is_empty() {
            return Ok(vec![]);
        }
        let url = format!(
            "{API}/search?q={}&limit={}&nonSuspiciousOnly=true",
            urlencode(q.text.trim()),
            q.limit.clamp(1, 50)
        );
        let v: Value = cx.get_json(&url, 4 * 1024 * 1024).await?;
        Ok(v.get("results")
            .and_then(|r| r.as_array())
            .into_iter()
            .flatten()
            .filter_map(hit_of)
            .collect())
    }

    async fn fetch(&self, cx: &Ctx, hit: &Hit) -> Result<Fetched> {
        let Locator::ClawHub {
            owner,
            slug,
            version,
        } = &hit.locator
        else {
            bail!("not a ClawHub item");
        };
        let (owner, d) = detail(cx, owner, slug).await?;
        let version = match version {
            Some(v) => v.clone(),
            None => d
                .get("latestVersion")
                .and_then(|l| l.get("version"))
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow!("{owner}/{slug}: no version"))?
                .to_string(),
        };
        let mut flags = vec![];
        if let Some(m) = d.get("moderation").filter(|m| !m.is_null()) {
            let why = m
                .get("verdict")
                .or_else(|| m.get("status"))
                .and_then(|x| x.as_str())
                .unwrap_or("moderated");
            flags.push(Flag::Upstream(format!("clawhub moderation: {why}")));
        }
        flags.extend(
            hit.flags
                .iter()
                .filter(|f| matches!(f, Flag::Upstream(_)))
                .cloned(),
        );
        let url = format!(
            "{API}/download?slug={}&version={}&owner={}",
            urlencode(slug),
            urlencode(&version),
            urlencode(&owner)
        );
        let r = cx.get(&url, ZIP_MAX).await?;
        let files = read_zip(&r.body, Limits::default())?;
        if !files.contains_key("SKILL.md") {
            bail!("{owner}/{slug}@{version}: the download has no SKILL.md");
        }
        let licence_files = files
            .iter()
            .filter(|(p, _)| {
                let u = p.to_ascii_uppercase();
                !p.contains('/') && (u.starts_with("LICENSE") || u.starts_with("LICENCE"))
            })
            .map(|(p, b)| (p.clone(), b.clone()))
            .collect();
        let file_urls = files
            .keys()
            .map(|p| (p.clone(), format!("{url}#{p}")))
            .collect();
        Ok(seal(Fetched {
            files,
            upstream: Upstream {
                commit: None,
                version: Some(version),
                sha256: String::new(),
                urls: vec![url],
                fetched_at: cx.now_ms,
            },
            licence_files,
            licence_hint: d
                .get("latestVersion")
                .and_then(|l| l.get("license"))
                .and_then(|x| x.as_str())
                .map(str::to_string),
            flags,
            file_urls,
            name_hint: slug.clone(),
            primary: Some("SKILL.md".into()),
        }))
    }

    async fn latest(&self, cx: &Ctx, loc: &Locator) -> Result<Option<Upstream>> {
        let Locator::ClawHub { owner, slug, .. } = loc else {
            return Ok(None);
        };
        let (_, d) = detail(cx, owner, slug).await?;
        Ok(d.get("latestVersion")
            .and_then(|l| l.get("version"))
            .and_then(|x| x.as_str())
            .map(|v| Upstream {
                version: Some(v.to_string()),
                fetched_at: cx.now_ms,
                ..Default::default()
            }))
    }
}
