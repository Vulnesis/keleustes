//! The pure halves of the `github` outputs (web factory_engine_node_issues.go,
//! factory_engine_review.go, factory_engine_approve.go): what a CLI scan
//! found, which findings an issue holds, the issue and review bodies, the
//! verdict a reviewer's notes give, and the readiness rules an approval
//! must pass. No I/O here; `forge.rs` does the forge calls.

use kls_github::forge::api::InlineComment;
use kls_store::engine::RunStep;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

// ---- findings ----------------------------------------------------------------

/// One finding reduced to identifiers (the CLI result grammar).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Finding {
    pub id: String,
    pub kind: String,
    pub path: String,
    pub severity: String,
    #[serde(rename = "ref")]
    pub reference: String,
}

/// What the run's scans found, merged (counts add, the highest exit code
/// stands, any not-evaluable job marks the whole).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Findings {
    pub job: String,
    pub exit_code: i64,
    pub critical: i64,
    pub high: i64,
    pub medium: i64,
    pub low: i64,
    pub items: Vec<Finding>,
    pub not_evaluable: bool,
}

impl Findings {
    pub fn total(&self) -> i64 {
        self.critical + self.high + self.medium + self.low
    }
}

fn text(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// The scan steps that finished before `before_seq` (the latest attempt of
/// each node), merged. `None` when no scan left a result.
pub fn findings(steps: &[RunStep], before_seq: i64) -> Option<Findings> {
    let mut latest: BTreeMap<&str, &RunStep> = BTreeMap::new();
    for s in steps
        .iter()
        .filter(|s| s.node_type == "scan" && s.seq < before_seq)
    {
        match latest.get(s.node_id.as_str()) {
            Some(c) if c.seq > s.seq => {}
            _ => {
                latest.insert(&s.node_id, s);
            }
        }
    }
    let mut picked: Vec<&RunStep> = latest
        .into_values()
        .filter(|s| s.status == "succeeded")
        .collect();
    picked.sort_by_key(|s| s.seq);
    let mut out = Findings::default();
    let mut any = false;
    for s in picked {
        let d: Value = serde_json::from_str(&s.details_json).unwrap_or(Value::Null);
        let r = &d["result"];
        if !r.is_object() {
            continue;
        }
        let job = text(r, "job");
        if !any {
            out.job = job;
        } else if !job.is_empty() && !out.job.split('+').any(|j| j == job) {
            out.job = format!("{}+{job}", out.job);
        }
        any = true;
        let n = |k: &str| r["counts"][k].as_i64().unwrap_or(0).max(0);
        out.critical += n("critical");
        out.high += n("high");
        out.medium += n("medium");
        out.low += n("low");
        out.exit_code = out.exit_code.max(r["exitCode"].as_i64().unwrap_or(0));
        out.not_evaluable |= r["notEvaluable"].as_bool().unwrap_or(false)
            || d["notEvaluable"].as_bool().unwrap_or(false)
            || r["exitCode"].as_i64() == Some(3);
        for f in r["findings"].as_array().into_iter().flatten().take(500) {
            let id = text(f, "id");
            if id.is_empty() {
                continue;
            }
            out.items.push(Finding {
                id,
                kind: text(f, "kind"),
                path: text(f, "path"),
                severity: text(f, "severity").to_lowercase(),
                reference: text(f, "ref"),
            });
        }
    }
    any.then_some(out)
}

/// Severity order; info and unknown rank below low.
pub fn sev_rank(s: &str) -> i32 {
    match s {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

/// Whether `sev` meets `min` (`any` meets everything).
pub fn sev_at_least(sev: &str, min: &str) -> bool {
    min == "any" || (sev_rank(sev) >= sev_rank(min) && sev_rank(sev) > 0)
}

/// `ref` with anything outside `[A-Za-z0-9._-]` folded to `-`.
pub fn safe_ref(r: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in r.chars() {
        if c.is_ascii_alphanumeric() || "._-".contains(c) {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let s = out.trim_matches(|c| c == '-' || c == '.').to_string();
    if s.is_empty() { "state".into() } else { s }
}

/// The findings one issue holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Group {
    pub key: String,
    pub items: Vec<Finding>,
}

/// `maxIssues` (1 to 10), else 5 for `group: id` and 1 for `group: run`.
pub fn max_issues(cfg: &Value) -> usize {
    match cfg.get("maxIssues").and_then(Value::as_i64) {
        Some(n) if (1..=10).contains(&n) => n as usize,
        _ if cfg.get("group").and_then(Value::as_str) == Some("id") => 5,
        _ => 1,
    }
}

/// Selects the findings to file (severity floor, kinds) and groups them,
/// ordered by severity then id so the same scan gives the same issues.
/// Returns the groups, the findings kept and the groups left out by the cap.
pub fn select(
    items: &[Finding],
    min: &str,
    group: &str,
    kinds: &[String],
    skip: &[String],
    limit: usize,
) -> (Vec<Group>, usize, usize) {
    let mut keep: Vec<Finding> = items
        .iter()
        .filter(|f| kinds.is_empty() || kinds.contains(&f.kind))
        .filter(|f| !skip.contains(&f.kind) && sev_at_least(&f.severity, min))
        .cloned()
        .collect();
    keep.sort_by(|a, b| {
        sev_rank(&b.severity)
            .cmp(&sev_rank(&a.severity))
            .then_with(|| a.id.cmp(&b.id))
    });
    let total = keep.len();
    if total == 0 {
        return (Vec::new(), 0, 0);
    }
    if group != "id" {
        return (
            vec![Group {
                key: "all".into(),
                items: keep,
            }],
            total,
            0,
        );
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut omitted = BTreeSet::new();
    for f in keep {
        let k = safe_ref(&f.id);
        if let Some(g) = groups.iter_mut().find(|g| g.key == k) {
            g.items.push(f);
        } else if groups.len() >= limit {
            omitted.insert(k);
        } else {
            groups.push(Group {
                key: k,
                items: vec![f],
            });
        }
    }
    (groups, total, omitted.len())
}

/// The comment that finds an issue again on the next run.
pub fn issue_marker(workflow: &str, group: &str) -> String {
    format!("<!-- keleustes:findings {workflow} {group} -->")
}

fn code(s: &str) -> String {
    if s.is_empty() {
        "-".into()
    } else {
        format!("`{}`", s.replace('`', "'"))
    }
}

/// Rows an issue body lists; the rest are in the run's evidence.
pub const ISSUE_ROWS: usize = 50;

/// The issue body: identifiers and counts only, each id in a code span so
/// an id that looks like a mention pings nobody.
pub fn issue_body(workflow: &str, run: &str, min: &str, g: &Group, f: &Findings) -> String {
    let mut b = format!(
        "Filed by the Software Factory workflow `{workflow}` (run `{run}`). The workflow keeps this issue current; edits are overwritten.\n\n"
    );
    b.push_str(&format!(
        "CLI job: `{}`. Findings in this issue: {} (at or above {min}). The scan found {}: {} critical, {} high, {} medium, {} low.\n\n",
        f.job,
        g.items.len(),
        f.total(),
        f.critical,
        f.high,
        f.medium,
        f.low
    ));
    b.push_str("| Severity | Id | Kind | Path | Ref |\n|---|---|---|---|---|\n");
    for it in g.items.iter().take(ISSUE_ROWS) {
        b.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            it.severity,
            code(&it.id),
            code(&it.kind),
            code(&it.path),
            code(&it.reference)
        ));
    }
    if g.items.len() > ISSUE_ROWS {
        b.push_str(&format!(
            "\n{} more are in the run's evidence.\n",
            g.items.len() - ISSUE_ROWS
        ));
    }
    b.push('\n');
    b.push_str(&issue_marker(workflow, &g.key));
    b.push('\n');
    b
}

/// The package-firewall command lines a profile's package managers call for.
pub fn firewall_commands(package_managers: &[String]) -> Vec<String> {
    let eco = |pm: &str| -> Option<&'static str> {
        Some(match pm {
            "npm" | "pnpm" | "yarn" | "bun" => "npm",
            "pip" | "poetry" | "uv" | "pipenv" => "pypi",
            "go" => "go",
            "maven" | "gradle" => "maven",
            "cargo" => "cargo",
            "bundler" => "gem",
            "composer" => "composer",
            "nuget" => "nuget",
            _ => return None,
        })
    };
    let set: BTreeSet<String> = package_managers
        .iter()
        .filter_map(|pm| eco(&pm.to_lowercase()))
        .map(|e| format!("vulnetix package-firewall {e}"))
        .collect();
    set.into_iter().collect()
}

pub fn firewall_body(workflow: &str, run: &str, cmds: &[String]) -> String {
    let mut b = format!(
        "Filed by the Software Factory workflow `{workflow}` (run `{run}`). The workflow keeps this issue current; edits are overwritten.\n\n"
    );
    b.push_str("The package managers this repository uses can install through the Vulnetix Package Firewall. Run the line for each one on every developer machine and build host that installs this repository's dependencies:\n\n");
    for c in cmds {
        b.push_str(&format!("- `{c}`\n"));
    }
    b.push_str("\nThe factory changes no machine; this issue is the whole recommendation.\n\n");
    b.push_str(&issue_marker(workflow, "all"));
    b.push('\n');
    b
}

/// The check's verdict: neutral when the CLI could not evaluate its input
/// (exit 3), failure on exit 1 or a finding at or above `min`, else success.
/// Returns (conclusion, headline, hits).
pub fn check_conclusion(f: &Findings, min: &str) -> (&'static str, String, usize) {
    let hits = f
        .items
        .iter()
        .filter(|i| sev_at_least(&i.severity, min))
        .count();
    if f.not_evaluable || f.exit_code == 3 {
        return (
            "neutral",
            "The CLI could not evaluate this change".into(),
            hits,
        );
    }
    if f.exit_code == 1 || hits > 0 {
        return (
            "failure",
            format!("{hits} finding(s) at or above {min}"),
            hits,
        );
    }
    ("success", format!("No findings at or above {min}"), hits)
}

// ---- review verdicts ---------------------------------------------------------

pub const MAX_FINDINGS: usize = 20;
pub const MAX_INLINE: usize = 20;
const MAX_FINDING_TEXT: usize = 500;

/// A reviewer's finding: a location when the note names `FILE:LINE`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct NoteFinding {
    pub path: String,
    pub line: i64,
    pub text: String,
}

/// What the reviewer's notes say: `verdict:` approve | comment |
/// request_changes, and `finding:` lines.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Verdict {
    pub verdict: String,
    pub findings: Vec<NoteFinding>,
}

fn strip_bullet(line: &str) -> &str {
    let l = line.trim_start();
    l.strip_prefix("- ")
        .or_else(|| l.strip_prefix("* "))
        .or_else(|| l.strip_prefix('-'))
        .or_else(|| l.strip_prefix('*'))
        .unwrap_or(l)
        .trim_start()
}

/// `key: value` (key case-insensitive), the value trimmed.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let l = strip_bullet(line);
    let (k, v) = l.split_once(':')?;
    k.trim().eq_ignore_ascii_case(key).then(|| v.trim())
}

fn norm_verdict(s: &str) -> Option<&'static str> {
    let s = s.trim().to_lowercase().replace(['-', ' '], "_");
    match s.as_str() {
        "approve" => Some("approve"),
        "comment" => Some("comment"),
        "request_changes" => Some("request_changes"),
        _ => None,
    }
}

/// Control characters to spaces, an `@` defused (zero-width space), at
/// most 500 characters.
fn clean_text(s: &str) -> String {
    let mut out = String::new();
    for (n, c) in s.chars().enumerate() {
        if n >= MAX_FINDING_TEXT {
            out.push_str("...");
            break;
        }
        out.push(if c.is_control() { ' ' } else { c });
        if c == '@' {
            out.push('\u{200b}');
        }
    }
    out.trim().to_string()
}

/// `path:line[: ]text`.
fn location(s: &str) -> Option<(String, i64, String)> {
    let (path, rest) = s.split_once(':')?;
    if path.is_empty() || path.starts_with(':') || path.chars().any(char::is_whitespace) {
        return None;
    }
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 7 {
        return None;
    }
    let after = &rest[digits.len()..];
    let text = match after.chars().next() {
        Some(':') | Some(' ') | Some('\t') => after[1..].trim_start(),
        None => "",
        _ => return None,
    };
    let line: i64 = digits.parse().ok()?;
    (line > 0).then(|| {
        (
            path.trim_start_matches("./").to_string(),
            line,
            text.to_string(),
        )
    })
}

/// The newest note with a `verdict:` line, and every `finding:` from it on.
pub fn parse_verdict(notes: &[String]) -> Option<Verdict> {
    let mut start = None;
    'outer: for (i, note) in notes.iter().enumerate().rev() {
        for line in note.lines() {
            if let Some(v) = field(line, "verdict").and_then(norm_verdict) {
                start = Some((i, v));
                break 'outer;
            }
        }
    }
    let (start, verdict) = start?;
    let mut out = Verdict {
        verdict: verdict.into(),
        findings: Vec::new(),
    };
    for note in &notes[start..] {
        for line in note.lines() {
            let Some(raw) = field(line, "finding") else {
                continue;
            };
            if out.findings.len() >= MAX_FINDINGS {
                continue;
            }
            let mut f = NoteFinding {
                text: clean_text(raw),
                ..Default::default()
            };
            if let Some((path, n, text)) = location(raw) {
                f = NoteFinding {
                    path,
                    line: n,
                    text: clean_text(&text),
                };
            }
            if !f.text.is_empty() {
                out.findings.push(f);
            }
        }
    }
    Some(out)
}

/// New-side line numbers a unified patch shows (added or context).
pub fn patch_lines(patch: &str) -> BTreeSet<i64> {
    let mut out = BTreeSet::new();
    let mut line = 0i64;
    let mut in_hunk = false;
    for l in patch.lines() {
        if let Some(rest) = l.strip_prefix("@@ ") {
            // @@ -a,b +c,d @@
            if let Some(plus) = rest.split_whitespace().find(|p| p.starts_with('+')) {
                line = plus[1..]
                    .split(',')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                in_hunk = true;
            }
            continue;
        }
        if !in_hunk || l.is_empty() {
            continue;
        }
        match l.as_bytes()[0] {
            b'+' | b' ' => {
                out.insert(line);
                line += 1;
            }
            _ => {}
        }
    }
    out
}

/// Marks a review the factory posted: run, node, head and verdict.
pub fn review_marker(run: &str, node: &str, sha: &str, verdict: &str) -> String {
    format!("<!-- keleustes:review run={run} node={node} sha={sha} verdict={verdict} -->")
}

/// `(run, node, sha, verdict)` of a review marker in `body`.
pub fn read_review_marker(body: &str) -> Option<(String, String, String, String)> {
    let start = body.find("<!-- keleustes:review ")?;
    let rest = &body[start + "<!-- keleustes:review ".len()..];
    let end = rest.find("-->")?;
    let mut kv: BTreeMap<&str, &str> = BTreeMap::new();
    for part in rest[..end].split_whitespace() {
        if let Some((k, v)) = part.split_once('=') {
            kv.insert(k, v);
        }
    }
    Some((
        kv.get("run")?.to_string(),
        kv.get("node")?.to_string(),
        kv.get("sha")?.to_lowercase(),
        kv.get("verdict").unwrap_or(&"comment").to_string(),
    ))
}

/// The review to post: the event (never APPROVE), inline comments on diff
/// lines, the rest listed in the body.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ReviewOut {
    pub event: String,
    pub body: String,
    pub comments: Vec<InlineComment>,
}

/// `diff`: path -> the new-side lines its patch shows (absent when unknown).
pub fn build_review(
    v: &Verdict,
    diff: &BTreeMap<String, BTreeSet<i64>>,
    marker: &str,
) -> ReviewOut {
    let event = if v.verdict == "request_changes" {
        "REQUEST_CHANGES"
    } else {
        "COMMENT"
    };
    let mut comments = Vec::new();
    let mut general = Vec::new();
    for f in &v.findings {
        if f.line > 0
            && diff.get(&f.path).is_some_and(|l| l.contains(&f.line))
            && comments.len() < MAX_INLINE
        {
            comments.push(InlineComment {
                path: f.path.clone(),
                line: f.line,
                body: f.text.clone(),
            });
            continue;
        }
        if f.line > 0 {
            general.push(format!("- `{}:{}`: {}", f.path, f.line, f.text));
        } else {
            general.push(format!("- {}", f.text));
        }
    }
    let mut b = String::from("Review by the Software Factory.\n\n");
    b.push_str(match v.verdict.as_str() {
        "request_changes" => "The reviewer asks for changes.",
        "approve" => "The reviewer found nothing to change.",
        _ => "The reviewer has comments.",
    });
    if !comments.is_empty() {
        b.push_str(&format!(
            " {} finding(s) are on the lines they concern.",
            comments.len()
        ));
    }
    if !general.is_empty() {
        b.push_str("\n\nFindings:\n");
        b.push_str(&general.join("\n"));
    }
    b.push_str("\n\nThis is a recommendation. A person decides whether to merge.\n\n");
    b.push_str(marker);
    ReviewOut {
        event: event.into(),
        body: b,
        comments,
    }
}

// ---- approval readiness ------------------------------------------------------

/// Paths that stop an approval, before `approve.sensitive_paths`.
pub const SENSITIVE_PATHS: &[&str] = &[
    ".github/workflows/**",
    ".github/actions/**",
    ".github/dependabot.yml",
    ".github/dependabot.yaml",
    "**/CODEOWNERS",
    ".circleci/**",
    ".gitlab-ci.yml",
    "**/Jenkinsfile",
    "**/Dockerfile*",
    "**/docker-compose*.y*ml",
    "**/*.tf",
    "**/*.tfvars",
    "terraform/**",
    "**/.env",
    "**/.env.*",
    "**/*.pem",
    "**/*.key",
    ".vulnetix/**",
];

/// The pattern `path` matches, if any.
pub fn sensitive(path: &str, extra: &[String]) -> Option<String> {
    SENSITIVE_PATHS
        .iter()
        .map(|s| s.to_string())
        .chain(extra.iter().cloned())
        .find(|g| crate::triggers::glob(g, path))
}

/// A review as the readiness rules read it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReviewRow {
    pub state: String,
    pub body: String,
    pub commit: String,
    pub author: String,
}

impl ReviewRow {
    pub fn from_json(v: &Value) -> Self {
        Self {
            state: text(v, "state").to_uppercase(),
            body: text(v, "body"),
            commit: text(v, "commit_id").to_lowercase(),
            author: v["user"]["login"].as_str().unwrap_or("").to_lowercase(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadyIn {
    pub state: String,
    pub fork: bool,
    pub draft: bool,
    pub author: String,
    pub head: String,
    /// The head the run was started for ("" when unknown).
    pub run_sha: String,
    pub reviews: Vec<ReviewRow>,
    /// The factory's own login on this forge.
    pub own: String,
    /// A `pr_review` in this run already gave this verdict on `head`.
    pub run_verdict: Option<(String, String)>,
    pub checks_total: i64,
    pub checks_pending: i64,
    pub checks_failed: i64,
    pub mergeable: String,
}

/// Whether an approval may be posted. `terminal`: it never will be for
/// this head; otherwise it waits.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Readiness {
    pub ready: bool,
    pub terminal: bool,
    pub done: bool,
    pub reason: String,
}

pub fn judge(r: &ReadyIn) -> Readiness {
    let head = r.head.to_lowercase();
    let wait = |why: &str| Readiness {
        reason: why.into(),
        ..Default::default()
    };
    let term = |why: &str| Readiness {
        reason: why.into(),
        terminal: true,
        ..Default::default()
    };
    if r.state != "open" {
        return term("pr_closed");
    }
    if r.fork {
        return term("fork");
    }
    if !r.run_sha.is_empty() && !head.is_empty() && !r.run_sha.eq_ignore_ascii_case(&head) {
        return term("stale_sha");
    }
    if r.draft {
        return wait("draft");
    }
    let own = r.own.to_lowercase();
    if !own.is_empty() && r.author.eq_ignore_ascii_case(&own) {
        // No forge lets an account approve its own pull request.
        return term("own_pull_request");
    }
    let mut mine: Option<String> = None;
    let mut latest: BTreeMap<String, &ReviewRow> = BTreeMap::new();
    for rv in &r.reviews {
        if !own.is_empty() && rv.author == own && rv.commit == head {
            if rv.state == "APPROVED" {
                return Readiness {
                    done: true,
                    reason: "already_approved".into(),
                    ..Default::default()
                };
            }
            if let Some((_, _, sha, verdict)) = read_review_marker(&rv.body)
                && sha == head
            {
                mine = Some(verdict);
            }
        }
        if matches!(
            rv.state.as_str(),
            "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED"
        ) {
            latest.insert(rv.author.clone(), rv);
        }
    }
    if mine.is_none()
        && let Some((sha, verdict)) = &r.run_verdict
        && sha.eq_ignore_ascii_case(&head)
    {
        mine = Some(verdict.clone());
    }
    let Some(verdict) = mine else {
        return wait("not_reviewed");
    };
    if verdict != "approve" {
        return term(&format!("review_{verdict}"));
    }
    if latest.values().any(|rv| rv.state == "CHANGES_REQUESTED") {
        return wait("changes_requested");
    }
    if r.checks_total == 0 {
        return wait("no_checks");
    }
    if r.checks_failed > 0 {
        return wait("checks_failed");
    }
    if r.checks_pending > 0 {
        return wait("checks_pending");
    }
    match r.mergeable.as_str() {
        "clean" => {}
        "conflict" => return wait("merge_state_dirty"),
        _ => return wait("merge_state_unknown"),
    }
    Readiness {
        ready: true,
        reason: "ready".into(),
        ..Default::default()
    }
}

/// The JSON a refused or waiting approval reports.
pub fn not_ready_json(r: &Readiness) -> Value {
    json!({ "approved": false, "reason": r.reason, "terminal": r.terminal, "merged": false })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(seq: i64, node: &str, result: Value) -> RunStep {
        RunStep {
            seq,
            node_id: node.into(),
            node_type: "scan".into(),
            status: "succeeded".into(),
            details_json: json!({ "result": result }).to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn findings_merge_and_select() {
        let a = step(
            1,
            "s1",
            json!({"job":"sca","exitCode":1,"counts":{"high":2,"low":1},
                "findings":[{"id":"CVE-2024-0002","kind":"vuln","path":"package.json","severity":"high","ref":"lodash@4"},
                            {"id":"CVE-2024-0001","kind":"vuln","path":"package.json","severity":"high","ref":"x@1"},
                            {"id":"L1","kind":"vuln","path":"a","severity":"low","ref":""}]}),
        );
        let b = step(
            2,
            "s2",
            json!({"job":"sast","exitCode":0,"counts":{"critical":1},
                "findings":[{"id":"rule/sql","kind":"sast","path":"src/db.rs","severity":"critical","ref":""}]}),
        );
        let late = step(9, "s3", json!({"job":"late","counts":{"high":9}}));
        let f = findings(&[a, b, late], 5).unwrap();
        assert_eq!(f.job, "sca+sast");
        assert_eq!((f.critical, f.high, f.low, f.exit_code), (1, 2, 1, 1));
        let (g, total, omitted) = select(&f.items, "high", "run", &[], &[], 1);
        assert_eq!((g.len(), total, omitted), (1, 3, 0));
        assert_eq!(g[0].items[0].id, "rule/sql");
        assert_eq!(g[0].items[1].id, "CVE-2024-0001");
        let (g, _, omitted) = select(&f.items, "any", "id", &[], &["sast".into()], 2);
        assert_eq!(g.len(), 2);
        assert_eq!(omitted, 1);
        let (g, _, _) = select(&f.items, "high", "id", &["sast".into()], &[], 5);
        assert_eq!(g[0].key, "rule-sql");
        assert_eq!(check_conclusion(&f, "critical").0, "failure");
        let clean = Findings {
            exit_code: 3,
            ..Default::default()
        };
        assert_eq!(check_conclusion(&clean, "high").0, "neutral");
        assert_eq!(check_conclusion(&Findings::default(), "high").0, "success");
        assert!(findings(&[], 5).is_none());
    }

    #[test]
    fn issue_body_keeps_its_marker() {
        let f = Findings {
            job: "sca".into(),
            high: 1,
            items: vec![Finding {
                id: "@scope/pkg".into(),
                severity: "high".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let g = Group {
            key: "all".into(),
            items: f.items.clone(),
        };
        let body = issue_body("wf1", "run_1", "high", &g, &f);
        assert!(body.contains("`@scope/pkg`"));
        assert!(body.trim_end().ends_with(&issue_marker("wf1", "all")));
        assert_eq!(
            firewall_commands(&["yarn".into(), "npm".into(), "uv".into(), "swiftpm".into()]),
            vec![
                "vulnetix package-firewall npm".to_string(),
                "vulnetix package-firewall pypi".to_string()
            ]
        );
    }

    /// Golden: verdict notes in, the review the factory posts out.
    #[test]
    fn verdict_notes_golden() {
        let cases: Value =
            serde_json::from_str(include_str!("../../testdata/verdict-notes.json")).unwrap();
        let mut got = Vec::new();
        for c in cases.as_array().unwrap() {
            let notes: Vec<String> = serde_json::from_value(c["notes"].clone()).unwrap();
            let mut diff = BTreeMap::new();
            for (path, patch) in c["patches"].as_object().into_iter().flatten() {
                diff.insert(path.clone(), patch_lines(patch.as_str().unwrap_or("")));
            }
            let v = parse_verdict(&notes);
            let review = v.as_ref().map(|v| {
                build_review(
                    v,
                    &diff,
                    &review_marker("run_1", "n1", "abc1234", &v.verdict),
                )
            });
            got.push(json!({ "name": c["name"], "verdict": v, "review": review }));
        }
        let got = serde_json::to_string_pretty(&got).unwrap() + "\n";
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/verdict-notes.golden.json"
        );
        if std::env::var("UPDATE_GOLDEN").is_ok() {
            std::fs::write(path, &got).unwrap();
        }
        let want = std::fs::read_to_string(path).expect("golden (UPDATE_GOLDEN=1 writes it)");
        assert_eq!(got, want);
    }

    #[test]
    fn markers_round_trip() {
        let m = review_marker("run_1", "n1", "ABC1234", "approve");
        let (run, node, sha, v) = read_review_marker(&format!("text\n{m}")).unwrap();
        assert_eq!(
            (run.as_str(), node.as_str(), sha.as_str(), v.as_str()),
            ("run_1", "n1", "abc1234", "approve")
        );
        assert_eq!(safe_ref("@scope/pkg"), "scope-pkg");
        assert_eq!(
            sensitive(".github/workflows/ci.yml", &[]).as_deref(),
            Some(".github/workflows/**")
        );
        assert_eq!(sensitive("infra/main.tf", &[]).as_deref(), Some("**/*.tf"));
        assert_eq!(
            sensitive("src/lib.rs", &["src/**".into()]).as_deref(),
            Some("src/**")
        );
        assert!(sensitive("src/lib.rs", &[]).is_none());
    }

    fn ready_base() -> ReadyIn {
        ReadyIn {
            state: "open".into(),
            head: "h1".into(),
            author: "ana".into(),
            own: "kel".into(),
            reviews: vec![ReviewRow {
                state: "COMMENTED".into(),
                body: review_marker("run_1", "n1", "h1", "approve"),
                commit: "h1".into(),
                author: "kel".into(),
            }],
            checks_total: 2,
            mergeable: "clean".into(),
            ..Default::default()
        }
    }

    #[test]
    fn readiness_rules() {
        assert!(judge(&ready_base()).ready);
        let mut r = ready_base();
        r.run_sha = "h0".into();
        let j = judge(&r);
        assert!(j.terminal && j.reason == "stale_sha");
        let mut r = ready_base();
        r.checks_failed = 1;
        assert_eq!(judge(&r).reason, "checks_failed");
        let mut r = ready_base();
        r.reviews.clear();
        assert_eq!(judge(&r).reason, "not_reviewed");
        r.run_verdict = Some(("h1".into(), "approve".into()));
        assert!(judge(&r).ready);
        let mut r = ready_base();
        r.author = "KEL".into();
        assert_eq!(judge(&r).reason, "own_pull_request");
        let mut r = ready_base();
        r.reviews.push(ReviewRow {
            state: "CHANGES_REQUESTED".into(),
            author: "bob".into(),
            ..Default::default()
        });
        assert_eq!(judge(&r).reason, "changes_requested");
        let mut r = ready_base();
        r.reviews[0].body = review_marker("run_1", "n1", "h1", "request_changes");
        assert!(judge(&r).terminal);
    }
}
