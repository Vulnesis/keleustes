//! G10: `hesreallyhim/awesome-claude-code`'s resource table (CSV) and G9
//! style README lists: discovery. Each row linking a GitHub repo becomes a
//! candidate hit; previewing one crawls that repo with the harness-path
//! detector for the row's type. Adding it as a tap keeps it indexed.

use super::gemini_gallery::crawl_pick;
use super::github_repo::latest_commit;
use super::{github_repo_of, seal};
use crate::catalogue;
use crate::fetch::Ctx;
use crate::licence::Licence;
use crate::source::{
    Ecosystem, Fetched, Hit, ItemType, Locator, Mode, Refresh, Source, Trust, Upstream,
};
use anyhow::{Result, bail};

pub const ID: &str = "awesome-claude-code";
pub const OWNER: &str = "hesreallyhim";
pub const REPO: &str = "awesome-claude-code";
pub const FILE: &str = "THE_RESOURCES_TABLE_NEW.csv";

pub struct AwesomeCsv;

/// RFC 4180 rows (quoted fields, doubled quotes, newlines in quotes).
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut q = false;
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match (q, c) {
            (true, '"') if it.peek() == Some(&'"') => {
                field.push('"');
                it.next();
            }
            (true, '"') => q = false,
            (true, c) => field.push(c),
            (false, '"') => q = true,
            (false, ',') => row.push(std::mem::take(&mut field)),
            (false, '\r') => {}
            (false, '\n') => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            (false, c) => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// The library type a resource category suggests.
pub fn type_of(category: &str, sub: &str) -> ItemType {
    let c = format!("{category} {sub}").to_ascii_lowercase();
    if c.contains("hook") {
        ItemType::Hook
    } else if c.contains("slash") || c.contains("command") {
        ItemType::Command
    } else if c.contains("agent") && !c.contains("skill") {
        ItemType::Agent
    } else if c.contains("claude.md") || c.contains("rules") {
        ItemType::Prompt
    } else if c.contains("mcp") {
        ItemType::Mcp
    } else {
        ItemType::Skill
    }
}

fn candidate(
    source: &str,
    owner: String,
    repo: String,
    t: ItemType,
    title: &str,
    desc: &str,
) -> Hit {
    Hit {
        source: source.into(),
        locator: Locator::GitHub {
            owner,
            repo: repo.clone(),
            git_ref: None,
            path: String::new(),
        },
        item_type: t,
        ecosystem: Ecosystem::ClaudeCode,
        name: repo,
        title: title.into(),
        description: desc.chars().take(300).collect(),
        tags: vec!["discovery".into()],
        licence: Licence::unknown(),
        trust: Trust::Community,
        popularity: None,
        updated_at: None,
        flags: vec![],
    }
}

/// Candidate hits from the CSV.
pub fn csv_hits(text: &str) -> Vec<Hit> {
    let rows = parse_csv(text);
    let Some(head) = rows.first() else {
        return vec![];
    };
    let col = |n: &str| head.iter().position(|h| h.trim() == n);
    let (Some(name), Some(cat), Some(link)) = (col("Display Name"), col("Category"), col("Link"))
    else {
        return vec![];
    };
    let sub = col("Sub-Category");
    let desc = col("Description");
    let active = col("Active");
    let stale = col("Stale");
    let mut out = vec![];
    let mut seen = std::collections::BTreeSet::new();
    for r in rows.iter().skip(1) {
        let g = |i: Option<usize>| i.and_then(|i| r.get(i)).map(String::as_str).unwrap_or("");
        if g(active).eq_ignore_ascii_case("false") || g(stale).eq_ignore_ascii_case("true") {
            continue;
        }
        let Some((o, rp)) = github_repo_of(g(Some(link))) else {
            continue;
        };
        if !seen.insert(format!("{o}/{rp}")) {
            continue;
        }
        let t = type_of(g(Some(cat)), g(sub));
        out.push(candidate(ID, o, rp, t, g(Some(name)), g(desc)));
    }
    out
}

/// Candidate hits from a README's GitHub links (G9).
pub fn readme_hits(source: &str, text: &str) -> Vec<Hit> {
    let mut out = vec![];
    let mut seen = std::collections::BTreeSet::new();
    for (i, _) in text.match_indices("https://github.com/") {
        let rest = &text[i..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '"' | '\'' | '>' | '<'))
            .unwrap_or(rest.len());
        let link = &rest[..end];
        let Some((o, r)) = github_repo_of(link) else {
            continue;
        };
        if o.eq_ignore_ascii_case("voltagent") || !seen.insert(format!("{o}/{r}")) {
            continue;
        }
        out.push(candidate(source, o, r.clone(), ItemType::Skill, &r, ""));
        if out.len() >= 2000 {
            break;
        }
    }
    out
}

/// The detector globs for one type (for an on-demand crawl).
pub fn detector_globs(t: ItemType) -> Vec<String> {
    catalogue::harness_rules()
        .into_iter()
        .filter(|r| r.item_type == t)
        .map(|r| r.glob)
        .collect()
}

/// Previews a discovery hit: the first item of its type in the repo.
pub async fn fetch_candidate(cx: &Ctx, hit: &Hit) -> Result<Fetched> {
    let Locator::GitHub { owner, repo, .. } = &hit.locator else {
        bail!("not a GitHub candidate");
    };
    let globs = detector_globs(hit.item_type);
    let refs: Vec<&str> = globs.iter().map(String::as_str).collect();
    let f = crawl_pick(cx, owner, repo, hit.item_type, &refs, "", None).await?;
    Ok(seal(f))
}

#[async_trait::async_trait]
impl Source for AwesomeCsv {
    fn id(&self) -> &str {
        ID
    }
    fn label(&self) -> &str {
        "awesome-claude-code"
    }
    fn types(&self) -> &[ItemType] {
        &[
            ItemType::Skill,
            ItemType::Agent,
            ItemType::Command,
            ItemType::Hook,
            ItemType::Mcp,
            ItemType::Prompt,
        ]
    }
    fn ecosystems(&self) -> &[Ecosystem] {
        &[Ecosystem::ClaudeCode]
    }
    fn mode(&self) -> Mode {
        Mode::Indexed
    }

    async fn refresh(&self, cx: &Ctx, cursor: Option<&str>) -> Result<Refresh> {
        let sha = super::github_repo::resolve(cx, OWNER, REPO, None).await?;
        if cursor == Some(sha.as_str()) {
            return Ok(Refresh {
                cursor: Some(sha),
                not_modified: true,
                ..Default::default()
            });
        }
        let (_, body) = cx.raw(OWNER, REPO, &sha, FILE).await?;
        Ok(Refresh {
            hits: csv_hits(&String::from_utf8_lossy(&body)),
            cursor: Some(sha),
            ..Default::default()
        })
    }

    async fn fetch(&self, cx: &Ctx, hit: &Hit) -> Result<Fetched> {
        fetch_candidate(cx, hit).await
    }

    async fn latest(&self, cx: &Ctx, loc: &Locator) -> Result<Option<Upstream>> {
        match loc {
            Locator::GitHub { owner, repo, .. } => latest_commit(cx, owner, repo, None).await,
            _ => Ok(None),
        }
    }
}
