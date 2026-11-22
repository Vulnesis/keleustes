//! Source adapters (WP2–WP4, WP8): one module per public place items are
//! published, plus the shared GitHub item fetch and small parsing helpers.
//!
//! Every request goes through [`Ctx`] so the kill-switch gate, the
//! deadline and the host budgets apply.

pub mod aitmpl;
pub mod awesome_csv;
pub mod clawhub;
pub mod cline_mcp;
pub mod gemini_gallery;
pub mod gh_code;
pub mod github_repo;
pub mod hermes_index;
pub mod lobehub;
pub mod marketplace;
pub mod mcp_registry;
pub mod pkg_facts;
pub mod skills_sh;
pub mod well_known;

use crate::catalogue::{self, Tap};
use crate::licence::Licence;
use crate::source::{Fetched, Hit, Source, Upstream};
use bytes::Bytes;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Which sources are on, the person's taps and well-known sites, and the
/// optional boosts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    /// Sources turned off (by id).
    pub off: BTreeSet<String>,
    /// Opt-in sources turned on (Hermes index).
    pub on: BTreeSet<String>,
    pub taps: Vec<Tap>,
    /// `https://<site>/.well-known/skills/index.json` URLs.
    pub well_known: Vec<String>,
    /// A signed-in local `gh` (enables `gh-code`).
    pub gh: bool,
    /// The skills.sh legacy search (fragile, feature-flagged).
    pub skills_sh: bool,
}

/// Every known source: the registries and indexes, the curated repos, the
/// person's taps and well-known sites. On or off; see [`is_on`].
pub fn all(cfg: &Config) -> Vec<Arc<dyn Source>> {
    let mut out: Vec<Arc<dyn Source>> = vec![
        Arc::new(mcp_registry::McpRegistry),
        Arc::new(clawhub::ClawHub),
        Arc::new(skills_sh::SkillsSh),
        Arc::new(hermes_index::HermesIndex),
        Arc::new(gemini_gallery::GeminiGallery),
        Arc::new(aitmpl::Aitmpl),
        Arc::new(lobehub::LobeHub),
        Arc::new(cline_mcp::ClineMcp),
        Arc::new(awesome_csv::AwesomeCsv),
        Arc::new(gh_code::GhCode),
    ];
    let mut ids: BTreeSet<String> = BTreeSet::new();
    for c in catalogue::curated() {
        ids.insert(c.id());
        out.push(Arc::new(github_repo::GitHubRepo::curated(c)));
    }
    for t in &cfg.taps {
        if ids.insert(t.id()) {
            out.push(Arc::new(github_repo::GitHubRepo::tap(t.clone())));
        }
    }
    for u in &cfg.well_known {
        if let Ok(w) = well_known::WellKnown::new(u)
            && ids.insert(w.id().to_string())
        {
            out.push(Arc::new(w));
        }
    }
    out
}

/// Whether a source is on: its default unless toggled; `gh` sources need
/// `cfg.gh`, skills.sh needs `cfg.skills_sh`.
pub fn is_on(s: &dyn Source, cfg: &Config) -> bool {
    if s.needs_gh() && !cfg.gh {
        return false;
    }
    if cfg.off.contains(s.id()) {
        return false;
    }
    if s.id() == skills_sh::ID {
        return cfg.skills_sh;
    }
    s.default_on() || cfg.on.contains(s.id())
}

/// The source that serves a hit.
pub fn find(all: &[Arc<dyn Source>], hit_source: &str) -> Option<Arc<dyn Source>> {
    all.iter().find(|s| s.owns(hit_source)).cloned()
}

// ---- shared helpers ------------------------------------------------------------

/// A licence from index metadata (`mit`, `MIT`), unknown when empty.
pub(crate) fn hint_licence(hint: Option<&str>, from: &str) -> Licence {
    match hint.map(str::trim).filter(|s| !s.is_empty()) {
        Some(h) => Licence::from_id(h, from),
        None => Licence::unknown(),
    }
}

/// Finishes a Fetched: the source-file hash.
pub(crate) fn seal(mut f: Fetched) -> Fetched {
    f.upstream.sha256 = f.files_sha256();
    f
}

/// RFC 3339 (`2026-11-20T16:05:37.129Z`) to Unix ms; `None` if unparsable.
pub fn rfc3339_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.split_once(['T', ' ']).unwrap_or((s, "00:00:00Z"));
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let m: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    // Offset.
    let (clock, off) = if let Some(c) = time.strip_suffix('Z').or_else(|| time.strip_suffix('z')) {
        (c, 0i64)
    } else if let Some(i) = time.rfind(['+', '-']) {
        let (c, o) = time.split_at(i);
        let sign = if o.starts_with('-') { -1 } else { 1 };
        let mut p = o[1..].split(':');
        let oh: i64 = p.next()?.parse().ok()?;
        let om: i64 = p.next().unwrap_or("0").parse().ok()?;
        (c, sign * (oh * 3600 + om * 60))
    } else {
        (time, 0)
    };
    let mut t = clock.split(':');
    let hh: i64 = t.next()?.parse().ok()?;
    let mm: i64 = t.next().unwrap_or("0").parse().ok()?;
    let sec = t.next().unwrap_or("0");
    let (ss, frac) = sec.split_once('.').unwrap_or((sec, ""));
    let ss: i64 = ss.parse().ok()?;
    let ms: i64 = format!("{:0<3}", &frac[..frac.len().min(3)])
        .parse()
        .unwrap_or(0);
    // Days from civil (Howard Hinnant).
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + hh * 3600 + mm * 60 + ss - off) * 1000) + ms)
}

/// Unix ms to RFC 3339 UTC seconds (`2026-11-20T16:05:37Z`).
pub fn ms_rfc3339(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

/// The simple `key: value` front matter of a Markdown file (one level).
pub fn front_matter(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let t = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let Some(rest) = t.strip_prefix("---\n") else {
        return out;
    };
    let Some(end) = rest.find("\n---") else {
        return out;
    };
    for line in rest[..end].lines() {
        if line.starts_with([' ', '\t', '-']) {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().trim_matches(['"', '\'']).to_string();
            if !k.trim().is_empty() && !v.is_empty() && v != "|" && v != ">" {
                out.insert(k.trim().to_string(), v);
            }
        }
    }
    out
}

/// A short description for a file: front matter `description`, a TOML or
/// JSON `description`, else the first heading or line.
pub fn describe(path: &str, data: &[u8]) -> (Option<String>, String) {
    let Ok(text) = std::str::from_utf8(data) else {
        return (None, String::new());
    };
    let cut = |s: &str| s.chars().take(300).collect::<String>();
    if path.ends_with(".json") {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) {
            let name = v.get("name").and_then(|x| x.as_str()).map(str::to_string);
            let d = v
                .get("description")
                .and_then(|x| x.as_str())
                .unwrap_or_default();
            return (name, cut(d));
        }
        return (None, String::new());
    }
    if path.ends_with(".toml") {
        if let Ok(v) = text.parse::<toml::Table>() {
            let d = v
                .get("description")
                .and_then(|x| x.as_str())
                .unwrap_or_default();
            return (None, cut(d));
        }
        return (None, String::new());
    }
    let fm = front_matter(text);
    if let Some(d) = fm.get("description") {
        return (fm.get("name").cloned(), cut(d));
    }
    let body = if text.starts_with("---") {
        text.splitn(3, "\n---").nth(2).unwrap_or(text)
    } else {
        text
    };
    let first = body
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    (fm.get("name").cloned(), cut(first))
}

/// `https://github.com/o/r(.git)(/tree/ref/path)` → (owner, repo).
pub fn github_repo_of(url: &str) -> Option<(String, String)> {
    let rest = url
        .trim()
        .strip_prefix("https://github.com/")
        .or_else(|| url.trim().strip_prefix("http://github.com/"))
        .or_else(|| url.trim().strip_prefix("git@github.com:"))?;
    let mut it = rest.split('/');
    let o = it.next()?.trim();
    let r = it.next()?.trim().trim_end_matches(".git");
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && s != "."
            && s != ".."
    };
    (ok(o) && ok(r)).then(|| (o.to_string(), r.to_string()))
}

/// An upstream with a version and one URL (registries, single files).
pub(crate) fn upstream_v(version: Option<String>, urls: Vec<String>, at: i64) -> Upstream {
    Upstream {
        commit: None,
        version,
        sha256: String::new(),
        urls,
        fetched_at: at,
    }
}

/// One-file Fetched.
pub(crate) fn single(
    path: &str,
    data: Bytes,
    url: &str,
    version: Option<String>,
    at: i64,
) -> Fetched {
    let mut f = Fetched {
        upstream: upstream_v(version, vec![url.to_string()], at),
        primary: Some(path.to_string()),
        ..Default::default()
    };
    f.file_urls.insert(path.to_string(), url.to_string());
    f.files.insert(path.to_string(), data);
    seal(f)
}

/// The sha of a hit's tag `key:value`.
pub(crate) fn tag<'a>(h: &'a Hit, key: &str) -> Option<&'a str> {
    h.tags
        .iter()
        .find_map(|t| t.strip_prefix(key).and_then(|r| r.strip_prefix(':')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        let ms = rfc3339_ms("2026-11-20T16:05:37.129799Z").unwrap();
        assert_eq!(ms_rfc3339(ms), "2026-11-20T16:05:37Z");
        assert_eq!(ms % 1000, 129);
        assert_eq!(rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            rfc3339_ms("2026-10-15T09:00:31+02:00"),
            rfc3339_ms("2026-10-15T07:00:31Z")
        );
        assert!(rfc3339_ms("nope").is_none());
    }

    #[test]
    fn helpers() {
        assert_eq!(
            github_repo_of("https://github.com/42Crunch-AI/claude-plugins.git"),
            Some(("42Crunch-AI".into(), "claude-plugins".into()))
        );
        assert!(github_repo_of("https://gitlab.com/a/b").is_none());
        assert!(github_repo_of("https://github.com/../x").is_none());
        let fm = front_matter(
            "---\nname: pdf\ndescription: \"PDF things\"\nallowed-tools:\n  - Read\n---\nbody",
        );
        assert_eq!(fm.get("description").unwrap(), "PDF things");
        assert_eq!(describe("x.md", b"# Title\n\ntext").1, "Title");
        assert_eq!(
            describe("c.toml", b"description = \"Sync\"\nprompt = \"x\"").1,
            "Sync"
        );
    }

    #[test]
    fn config_switches() {
        let cfg = Config::default();
        let all = all(&cfg);
        let on = |id: &str, cfg: &Config| {
            let s = find(&all, id).unwrap();
            is_on(s.as_ref(), cfg)
        };
        assert!(on("mcp-registry", &cfg));
        assert!(!on("hermes-index", &cfg), "opt-in");
        assert!(!on("gh-code", &cfg), "needs gh");
        assert!(!on("skills-sh", &cfg), "feature flag");
        let mut c2 = cfg.clone();
        c2.on.insert("hermes-index".into());
        c2.off.insert("mcp-registry".into());
        c2.gh = true;
        c2.skills_sh = true;
        assert!(on("hermes-index", &c2) && !on("mcp-registry", &c2));
        assert!(on("gh-code", &c2) && on("skills-sh", &c2));
        assert!(find(&all, "gh:anthropics/skills").is_some());
    }
}
