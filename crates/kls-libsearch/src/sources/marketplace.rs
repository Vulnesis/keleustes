//! Plugin marketplaces (M1–M5): one JSON file in a repo, in three dialects
//! (`.claude-plugin/`, `.agents/plugins/`, `.cursor-plugin/`
//! `marketplace.json`). In-repo plugins (`"./plugins/x"`, `{source:"local"}`)
//! are found by the repo crawl's component globs; this module turns the
//! other entries into hits:
//!
//! - `{source:"url", url, sha}` / `{source:"git-subdir", url, path, ref, sha}`
//!   / `{source:"github", repo, ref?, sha?}` → a plugin hit pinned at `sha`
//!   (a `ref` is re-checked at fetch; a moved ref is refused);
//! - `{source:"command", …}` → a hit carrying `Flag::Refused`: it runs a
//!   program, and is never fetched or run.

use super::{github_repo_of, seal};
use crate::licence::Licence;
use crate::scan::Flag;
use crate::source::{Ecosystem, Fetched, Hit, ItemType, Locator, Trust, Upstream};
use bytes::Bytes;
use serde_json::Value;

pub const REFUSED_COMMAND: &str = "marketplace source runs a program";

/// The dialect of a marketplace file path.
pub fn ecosystem_of(path: &str) -> Ecosystem {
    if path.starts_with(".agents/") {
        Ecosystem::Codex
    } else if path.starts_with(".cursor-plugin/") {
        Ecosystem::Cursor
    } else {
        Ecosystem::ClaudeCode
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or_default()
}

/// Hits for the external and refused entries of one marketplace file.
#[allow(clippy::too_many_arguments)]
pub fn hits(
    source_id: &str,
    owner: &str,
    repo: &str,
    sha: &str,
    file: &str,
    data: &[u8],
    trust: Trust,
    repo_licence: &Licence,
) -> Vec<Hit> {
    let Ok(v) = serde_json::from_slice::<Value>(data) else {
        return vec![];
    };
    let eco = ecosystem_of(file);
    let mut out = Vec::new();
    for p in v
        .get("plugins")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
    {
        let name = s(p, "name").to_string();
        if name.is_empty() {
            continue;
        }
        let licence = match p.get("license").and_then(|x| x.as_str()) {
            Some(l) => Licence::from_id(l, "marketplace"),
            None => Licence::unknown(),
        };
        let base = |locator: Locator, tags: Vec<String>, flags: Vec<Flag>, lic: Licence| Hit {
            source: source_id.to_string(),
            locator,
            item_type: ItemType::Skill,
            ecosystem: eco,
            title: name.clone(),
            name: name.clone(),
            description: s(p, "description").chars().take(300).collect(),
            tags,
            licence: lic,
            trust,
            popularity: None,
            updated_at: None,
            flags,
        };
        let src = p.get("source").cloned().unwrap_or(Value::Null);
        let kind = match &src {
            Value::String(_) => "relative",
            Value::Object(o) => o.get("source").and_then(|x| x.as_str()).unwrap_or(""),
            _ => "",
        };
        match kind {
            "command" => out.push(base(
                Locator::GitHub {
                    owner: owner.into(),
                    repo: repo.into(),
                    git_ref: Some(sha.into()),
                    path: file.into(),
                },
                vec![
                    format!("plugin:{name}"),
                    format!("refused:{REFUSED_COMMAND}"),
                ],
                vec![Flag::Refused(REFUSED_COMMAND.into())],
                licence,
            )),
            "url" | "git-subdir" | "github" => {
                let (o, r) = if kind == "github" {
                    match s(&src, "repo").split_once('/') {
                        Some((o, r)) => (o.to_string(), r.to_string()),
                        None => continue,
                    }
                } else {
                    match github_repo_of(s(&src, "url")) {
                        Some(x) => x,
                        // Only github.com is reachable without credentials.
                        None => continue,
                    }
                };
                let pin = s(&src, "sha");
                let gref = s(&src, "ref");
                let git_ref = if crate::git::is_sha(pin) {
                    Some(pin.to_string())
                } else if !gref.is_empty() {
                    Some(gref.to_string())
                } else {
                    None
                };
                let mut tags = vec![format!("plugin:{name}"), "plugin".to_string()];
                if !gref.is_empty() && crate::git::is_sha(pin) {
                    tags.push(format!("ref:{gref}"));
                }
                let path = s(&src, "path").trim_matches('/').trim_start_matches("./");
                if path.contains("..") {
                    continue;
                }
                out.push(base(
                    Locator::GitHub {
                        owner: o,
                        repo: r,
                        git_ref,
                        path: path.to_string(),
                    },
                    tags,
                    vec![],
                    if licence.known() {
                        licence
                    } else {
                        Licence::unknown()
                    },
                ));
            }
            // In-repo plugins: the crawl's component globs found them.
            _ => {}
        }
    }
    let _ = repo_licence;
    out
}

/// The Fetched for a refused entry: nothing upstream is fetched, the entry
/// is shown, the flag stops the import.
pub fn refused_fetched(hit: &Hit, why: &str, now: i64) -> Fetched {
    let doc = serde_json::json!({
        "name": hit.name, "description": hit.description, "refused": why,
    });
    let mut f = Fetched {
        name_hint: hit.name.clone(),
        primary: Some("marketplace-entry.json".into()),
        flags: vec![Flag::Refused(why.to_string())],
        upstream: Upstream {
            fetched_at: now,
            ..Default::default()
        },
        ..Default::default()
    };
    if let Locator::GitHub { git_ref, .. } = &hit.locator {
        f.upstream.commit = git_ref.clone();
    }
    f.files.insert(
        "marketplace-entry.json".into(),
        Bytes::from(serde_json::to_vec_pretty(&doc).unwrap_or_default()),
    );
    seal(f)
}
