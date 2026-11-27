//! The session fact index: what each session touched, the session-level
//! counterpart of the engine's per-run facts (`RunDetail::facts`).
//!
//! Kinds: `repo` (owner/name), `pr` (owner/name#N), `card` (key), `agent`,
//! `model` (provider/model), `provider`, `mode`, `effort`, `belai` (the
//! host's belai version), `node`, `run`, `vuln` (CVE-…, GHSA-…), `file`,
//! `command` (a shell command's first line), `domain` (a fetched URL's
//! host), `mcp` (an MCP server), `skill` and `subagent`. They come from the
//! session row, its dispatch and host, its transcript (text and tool
//! metadata) and its host audit lines (`repo.commit`, `repo.publish`,
//! `a2a.*`). Each value keeps when it was first and last seen, so the
//! "ran on" kinds ([`TIMELINE_KINDS`]) read as a timeline.
//!
//! The fold is incremental: a cursor per session remembers the last
//! transcript entry and audit line read, so [`fold_session`] runs on every
//! `session.*` event cheaply. [`session_facts`], [`sessions_with`] and
//! [`fact_values`] are the reader API for the Sessions and Audit screens.

use crate::Core;
use anyhow::Result;
use kls_store::audit::SessionFact;
use serde_json::Value;
use std::collections::BTreeSet;

pub use kls_store::audit::SessionFact as Fact;

/// The fact kinds, in display order.
pub const KINDS: [&str; 18] = [
    "repo", "pr", "card", "agent", "model", "provider", "mode", "effort", "belai", "node", "run",
    "vuln", "file", "command", "domain", "mcp", "skill", "subagent",
];

/// What a session ran on: one value at a time, so a change is a timeline.
pub const TIMELINE_KINDS: [&str; 7] = [
    "model", "provider", "mode", "effort", "agent", "belai", "node",
];

/// What a session touched or contacted.
pub const TOUCHED_KINDS: [&str; 10] = [
    "repo", "pr", "vuln", "card", "domain", "file", "command", "mcp", "skill", "subagent",
];

const ENTRY_PAGE: i64 = 500;
/// Most values of one transcript kind (files, commands, ...) a fold keeps.
const FILE_MAX: usize = 200;
const COMMAND_MAX: usize = 160;

/// `owner/name` when `s` looks like one.
fn repo_slug(s: &str) -> Option<String> {
    let s = s.trim().trim_end_matches(".git").trim_matches('/');
    let mut parts = s.split('/');
    let (o, n) = (parts.next()?, parts.next()?);
    let ok = |x: &str| {
        !x.is_empty()
            && x.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    (parts.next().is_none() && ok(o) && ok(n)).then(|| format!("{o}/{n}"))
}

/// Facts a run of text names: pull request URLs (and their repos),
/// CVE and GHSA ids, and `K-xxxxxx` card keys.
pub fn scan_text(text: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || "()[]<>\"'`,;".contains(c)) {
        let w = word.trim_end_matches(['.', ':', '!', '?']);
        if let Some(i) = w.find("/pull/") {
            // https://<host>/<owner>/<name>/pull/<n>
            let before = &w[..i];
            let tail: String = w[i + 6..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            let segs: Vec<&str> = before.rsplitn(3, '/').collect();
            if segs.len() >= 2
                && !tail.is_empty()
                && let Some(repo) = repo_slug(&format!("{}/{}", segs[1], segs[0]))
            {
                out.push(("pr", format!("{repo}#{tail}")));
                out.push(("repo", repo));
            }
            continue;
        }
        let up = w.to_ascii_uppercase();
        if let Some(rest) = up.strip_prefix("CVE-") {
            let mut it = rest.splitn(2, '-');
            if let (Some(y), Some(n)) = (it.next(), it.next())
                && y.len() == 4
                && y.chars().all(|c| c.is_ascii_digit())
                && n.len() >= 4
                && n.chars().all(|c| c.is_ascii_digit())
            {
                out.push(("vuln", up.clone()));
            }
        } else if up.starts_with("GHSA-") && up.len() == 19 && up.matches('-').count() == 3 {
            out.push(("vuln", up.clone()));
        } else if let Some(k) = w.strip_prefix("K-")
            && (4..=12).contains(&k.len())
            && k.chars().all(|c| c.is_ascii_alphanumeric())
        {
            out.push(("card", w.to_string()));
        }
    }
    out
}

/// The host of a URL (`https://api.github.com/x` -> `api.github.com`).
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host
        .rsplit('@')
        .next()?
        .split(':')
        .next()?
        .to_ascii_lowercase();
    (!host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-.".contains(c)))
    .then_some(host)
}

/// Facts a tool entry's metadata names: files it read or wrote, the shell
/// command it ran, the host it fetched, the MCP server, skill or subagent
/// it called.
fn scan_meta(meta: &Value) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let input = meta.get("input").unwrap_or(meta);
    fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
        v.get(k).and_then(Value::as_str).unwrap_or("").trim()
    }
    for k in ["file_path", "path", "filePath", "notebook_path"] {
        if let Some(p) = input.get(k).and_then(Value::as_str)
            && !p.is_empty()
            && p.len() <= 512
        {
            out.push(("file", p.to_string()));
        }
    }
    let tool = ["name", "tool", "toolName"]
        .iter()
        .map(|k| str_of(meta, k))
        .find(|t| !t.is_empty())
        .unwrap_or("");
    let cmd = str_of(input, "command");
    if !cmd.is_empty() {
        let line: String = cmd
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(COMMAND_MAX)
            .collect();
        // A line that may carry a credential is not indexed.
        let up = line.to_ascii_uppercase();
        if !["TOKEN", "SECRET", "PASSWORD", "API_KEY", "AUTHORIZATION"]
            .iter()
            .any(|w| up.contains(w))
        {
            out.push(("command", line));
        }
    }
    if let Some(h) = url_host(str_of(input, "url")) {
        out.push(("domain", h));
    }
    if let Some(server) = tool
        .strip_prefix("mcp__")
        .and_then(|t| t.split("__").next())
        .filter(|s| !s.is_empty())
    {
        out.push(("mcp", server.to_string()));
    }
    match tool {
        "Skill" => {
            let s = [str_of(input, "skill"), str_of(input, "name")]
                .into_iter()
                .find(|s| !s.is_empty());
            if let Some(s) = s {
                out.push(("skill", s.to_string()));
            }
        }
        "Task" | "Agent" => {
            let a = str_of(input, "subagent_type");
            if !a.is_empty() {
                out.push(("subagent", a.to_string()));
            }
        }
        _ => {}
    }
    out
}

/// Facts a host audit line names.
fn scan_line(kind: &str, json: &str) -> Vec<(&'static str, String)> {
    let v: Value = serde_json::from_str(json).unwrap_or_default();
    let d = v.get("data").unwrap_or(&v);
    let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let mut out = Vec::new();
    match kind {
        "repo.commit" | "repo.publish" => {
            if let Some(r) = repo_slug(&s("repo")) {
                out.push(("repo", r.clone()));
                let n = d
                    .get("pr")
                    .or_else(|| d.get("number"))
                    .and_then(|x| x.as_i64().or_else(|| x.as_str()?.parse().ok()));
                if let Some(n) = n {
                    out.push(("pr", format!("{r}#{n}")));
                }
            }
            out.extend(scan_text(&s("url")));
        }
        k if k.starts_with("a2a.") => {
            for a in ["from", "to"] {
                let x = s(a);
                if !x.is_empty() {
                    out.push(("agent", x));
                }
            }
        }
        k if k.starts_with("finding.") || k == "vex.written" => {
            out.extend(scan_text(&format!("{} {}", s("id"), s("vuln"))));
        }
        _ => {}
    }
    out
}

/// Folds what is new in session `id` into the index. Returns how many
/// facts it recorded (repeats included).
pub async fn fold_session(core: &Core, id: &str) -> Result<usize> {
    let s = core.store();
    let Some(sess) = s.session(id).await? else {
        return Ok(0);
    };
    let (mut entry_idx, mut line_idx) = s.session_fact_cursor(id).await?;
    let mut facts: Vec<(String, String, i64)> = Vec::new();
    let at = sess.seen_at.max(sess.started_at);
    let mut add = |kind: &str, value: String, at: i64| {
        let v = value.trim().to_string();
        if !v.is_empty() {
            facts.push((kind.to_string(), v, at));
        }
    };
    if let Some(r) = repo_slug(&sess.repo) {
        add("repo", r, at);
    }
    add("agent", sess.agent.clone(), at);
    add("provider", sess.provider.clone(), at);
    add("mode", sess.mode.clone(), at);
    // The effort the dispatch asked for, else the agent profile's.
    let mut effort = String::new();
    if !sess.dispatch_id.is_empty()
        && let Some(d) = s.dispatch(&sess.dispatch_id).await?
    {
        effort = serde_json::from_str::<Value>(&d.spec_json)
            .ok()
            .and_then(|v| v.get("effort").and_then(Value::as_str).map(String::from))
            .unwrap_or_default();
    }
    if effort.is_empty()
        && !sess.agent.is_empty()
        && let Some(a) = s.agent_profile_by_name(&sess.agent).await?
    {
        effort = serde_json::from_str::<Value>(&a.meta_json)
            .ok()
            .and_then(|v| v.get("effort").and_then(Value::as_str).map(String::from))
            .unwrap_or_default();
    }
    add("effort", effort, at);
    if let Some(n) = s.node(&sess.node_id).await? {
        add("belai", n.host.belai_version, at);
    }
    add("node", sess.node_id.clone(), at);
    add("card", sess.card_id.clone(), at);
    add("run", sess.run_id.clone(), at);
    if !sess.model.is_empty() {
        add(
            "model",
            format!("{}/{}", sess.provider, sess.model)
                .trim_start_matches('/')
                .to_string(),
            at,
        );
    }
    let mut seen: BTreeSet<(&str, String)> = BTreeSet::new();
    loop {
        let page = s.entries(id, entry_idx + 1, ENTRY_PAGE).await?;
        for e in &page {
            for (k, v) in scan_text(&e.text) {
                add(k, v, e.at);
            }
            let meta: Value = serde_json::from_str(&e.meta_json).unwrap_or_default();
            for (k, v) in scan_meta(&meta) {
                if seen.iter().filter(|(x, _)| *x == k).count() < FILE_MAX
                    && seen.insert((k, v.clone()))
                {
                    add(k, v, e.at);
                }
            }
            entry_idx = entry_idx.max(e.idx);
        }
        if (page.len() as i64) < ENTRY_PAGE {
            break;
        }
    }
    loop {
        let page = s
            .audit_lines(&sess.node_id, id, line_idx + 1, ENTRY_PAGE)
            .await?;
        for l in &page {
            for (k, v) in scan_line(&l.kind, &l.json) {
                add(k, v, l.at);
            }
            line_idx = line_idx.max(l.idx);
        }
        if (page.len() as i64) < ENTRY_PAGE {
            break;
        }
    }
    // The session row's own facts are refreshed each time; count them once.
    facts.sort();
    facts.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    let n = facts.len();
    s.session_facts_add(id, &facts, entry_idx, line_idx).await?;
    Ok(n)
}

/// Folds every session (startup backfill).
pub async fn fold_all(core: &Core) -> Result<usize> {
    let mut n = 0;
    for sess in core
        .store()
        .sessions(&kls_store::sessions::SessionQuery {
            limit: 5000,
            ..Default::default()
        })
        .await?
    {
        n += fold_session(core, &sess.id).await?;
    }
    Ok(n)
}

/// A session's facts, by kind then value.
pub async fn session_facts(core: &Core, id: &str) -> Result<Vec<SessionFact>> {
    core.store().session_facts(id).await
}

/// Sessions that touched all of `facts` ((kind, value)), newest first.
pub async fn sessions_with(
    core: &Core,
    facts: &[(String, String)],
    limit: i64,
) -> Result<Vec<String>> {
    core.store().sessions_with_facts(facts, limit).await
}

/// The most common values of a kind across sessions, for filter chips.
pub async fn fact_values(core: &Core, kind: &str, limit: i64) -> Result<Vec<(String, i64)>> {
    core.store().session_fact_values(kind, limit).await
}

/// Parses `kind:value` filter words (`repo:acme/api pr:acme/api#4`);
/// other words are ignored.
pub fn parse_filters(s: &str) -> Vec<(String, String)> {
    s.split_whitespace()
        .filter_map(|w| {
            let (k, v) = w.split_once(':')?;
            (KINDS.contains(&k) && !v.is_empty()).then(|| (k.to_string(), v.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_text_meta_and_lines() {
        let t = scan_text(
            "Opened https://github.com/acme/api/pull/42. Fixes CVE-2024-12345 and GHSA-abcd-efgh-ijkl for K-7f21ab",
        );
        assert!(t.contains(&("pr", "acme/api#42".into())));
        assert!(t.contains(&("repo", "acme/api".into())));
        assert!(t.contains(&("vuln", "CVE-2024-12345".into())));
        assert!(t.contains(&("vuln", "GHSA-ABCD-EFGH-IJKL".into())));
        assert!(t.contains(&("card", "K-7f21ab".into())));
        assert!(scan_text("CVE-24-1 K- plain words").is_empty());
        let m = scan_meta(&serde_json::json!({"input": {"file_path": "src/main.rs"}}));
        assert_eq!(m, vec![("file", "src/main.rs".to_string())]);
        let m = scan_meta(
            &serde_json::json!({"tool": "Bash", "input": {"command": "cargo test -p x\nsecond line"}}),
        );
        assert_eq!(m, vec![("command", "cargo test -p x".to_string())]);
        assert!(
            scan_meta(
                &serde_json::json!({"tool": "Bash", "input": {"command": "export GH_TOKEN=x"}})
            )
            .is_empty(),
            "a line that may carry a credential is not indexed"
        );
        let m = scan_meta(
            &serde_json::json!({"tool": "WebFetch", "input": {"url": "https://user@Docs.RS:443/x?y"}}),
        );
        assert_eq!(m, vec![("domain", "docs.rs".to_string())]);
        let m = scan_meta(&serde_json::json!({"name": "mcp__github__get_issue", "input": {}}));
        assert_eq!(m, vec![("mcp", "github".to_string())]);
        let m = scan_meta(&serde_json::json!({"tool": "Skill", "input": {"skill": "pdf"}}));
        assert_eq!(m, vec![("skill", "pdf".to_string())]);
        let m =
            scan_meta(&serde_json::json!({"tool": "Task", "input": {"subagent_type": "Explore"}}));
        assert_eq!(m, vec![("subagent", "Explore".to_string())]);
        assert!(url_host("not a url").is_none());
        for k in TIMELINE_KINDS.iter().chain(TOUCHED_KINDS.iter()) {
            assert!(KINDS.contains(k), "{k}");
        }
        let l = scan_line("repo.publish", r#"{"data":{"repo":"acme/api","pr":7}}"#);
        assert!(l.contains(&("pr", "acme/api#7".into())));
        assert_eq!(
            scan_line("a2a.sent", r#"{"data":{"from":"qa","to":"builder"}}"#).len(),
            2
        );
        assert_eq!(
            parse_filters("repo:acme/api junk pr:acme/api#4 bogus:x"),
            vec![
                ("repo".into(), "acme/api".into()),
                ("pr".into(), "acme/api#4".into())
            ]
        );
    }

    #[tokio::test]
    async fn folds_incrementally_and_filters() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        let s = c.store();
        let host = kls_store::HostInfo {
            belai_version: "1.4.0".into(),
            ..Default::default()
        };
        s.insert_node("n1", "box", &host, "serial", "tok")
            .await
            .unwrap();
        let sess = kls_store::sessions::Session {
            id: "s1".into(),
            node_id: "n1".into(),
            repo: "acme/api".into(),
            provider: "anthropic".into(),
            model: "sonnet".into(),
            mode: "plan".into(),
            agent: "builder".into(),
            started_at: 1,
            ..Default::default()
        };
        s.upsert_session(&sess).await.unwrap();
        s.append_entries(
            "s1",
            &[kls_store::sessions::Entry {
                idx: 0,
                role: "assistant".into(),
                text: "PR https://github.com/acme/api/pull/9 fixes CVE-2025-00001".into(),
                at: 5,
                meta_json: "{}".into(),
            }],
        )
        .await
        .unwrap();
        assert!(fold_session(&c, "s1").await.unwrap() > 0);
        let f = session_facts(&c, "s1").await.unwrap();
        let has = |k: &str, v: &str| f.iter().any(|x| x.kind == k && x.value == v);
        assert!(
            has("repo", "acme/api") && has("pr", "acme/api#9") && has("vuln", "CVE-2025-00001")
        );
        assert!(has("model", "anthropic/sonnet") && has("agent", "builder") && has("node", "n1"));
        assert!(has("provider", "anthropic") && has("mode", "plan") && has("belai", "1.4.0"));
        // A model change is a second value with its own first and last seen.
        let mut later = sess.clone();
        later.model = "opus".into();
        later.seen_at = 50;
        s.upsert_session(&later).await.unwrap();
        fold_session(&c, "s1").await.unwrap();
        let f = session_facts(&c, "s1").await.unwrap();
        let models: Vec<&str> = f
            .iter()
            .filter(|x| x.kind == "model")
            .map(|x| x.value.as_str())
            .collect();
        assert_eq!(models.len(), 2, "{models:?}");
        // The cursor moved: folding again reads no new entries.
        assert_eq!(s.session_fact_cursor("s1").await.unwrap().0, 0);
        fold_session(&c, "s1").await.unwrap();
        let pr = session_facts(&c, "s1")
            .await
            .unwrap()
            .into_iter()
            .find(|x| x.kind == "pr")
            .unwrap();
        assert_eq!(pr.hits, 1, "the transcript is not read twice");
        let hits = sessions_with(&c, &parse_filters("repo:ACME/api vuln:cve-2025-00001"), 10)
            .await
            .unwrap();
        assert_eq!(hits, vec!["s1".to_string()]);
        assert!(
            sessions_with(&c, &parse_filters("repo:other/x"), 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fact_values(&c, "repo", 5).await.unwrap(),
            vec![("acme/api".to_string(), 1)]
        );
        s.session_facts_delete("s1").await.unwrap();
        assert!(session_facts(&c, "s1").await.unwrap().is_empty());
    }
}
