//! Trigger conditions and safeguards for event, webhook and label triggers
//! (vdb-site docs/factory.md 12.6-12.8, adapted to the poll mirror).
//!
//! A trigger node's `conditions` object narrows which GitHub events start
//! it: actions, base/head branches, changed paths (and ignored paths), labels
//! (any / all), author type and allow/deny lists, drafts, origin (same repo
//! or fork) and review state. Safeguards then hold back what passed:
//!
//! - **own branch**: a pull request whose head starts with the factory's
//!   `forge.branch_prefix` (its own work) never starts a run;
//! - **fork needs a person**: a fork pull request starts only a workflow
//!   with a `human` step;
//! - **debounce**: a second fire on the same subject inside
//!   `debounceSeconds` is coalesced into the first;
//! - **runs per hour**: at most `maxRunsPerHour` (default 60) starts per
//!   trigger in any hour;
//! - **fan-out**: one event starts at most `factory.fanoutCap` (6)
//!   triggers, highest `priority` (1 high .. 5 low, default 3) first.

use kls_store::Event;
use kls_store::engine::TriggerFire;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_MAX_RUNS_PER_HOUR: i64 = 60;
pub const DEFAULT_FANOUT_CAP: usize = 6;
pub const DEFAULT_PRIORITY: i64 = 3;
pub const MAX_DEBOUNCE_SECONDS: i64 = 86_400;
pub const DRAFTS: [&str; 3] = ["include", "exclude", "only"];
pub const ORIGINS: [&str; 3] = ["any", "same_repo", "fork"];
pub const AUTHOR_TYPES: [&str; 3] = ["any", "human", "bot"];
/// Who may start it by authoring the event: `team` (OWNER, MEMBER,
/// COLLABORATOR), `outsider` (anyone else, or unknown) or `any`.
pub const SENDERS: [&str; 3] = ["any", "team", "outsider"];
/// The marker the factory writes into what it posts (`forge.rs`).
pub const FACTORY_MARKER: &str = "<!-- keleustes:";
/// Logins that are always the factory itself.
pub const FACTORY_LOGINS: [&str; 2] = ["keleustes[bot]", "belai[bot]"];

/// A trigger node's `conditions` (all optional; empty lists match anything).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Conditions {
    /// Webhook actions (`opened`, `synchronize`, ...).
    pub actions: Vec<String>,
    /// Glob patterns (`main`, `release/*`).
    pub base_branches: Vec<String>,
    pub head_branches: Vec<String>,
    /// Changed paths: at least one must match (globs, `**` allowed).
    pub paths: Vec<String>,
    /// Changed paths that do not count.
    pub ignore_paths: Vec<String>,
    pub labels_any: Vec<String>,
    pub labels_all: Vec<String>,
    /// `any`, `human`, `bot`.
    pub author_type: String,
    pub authors_allow: Vec<String>,
    pub authors_deny: Vec<String>,
    /// `include` (default), `exclude`, `only`.
    pub drafts: String,
    /// `any` (default), `same_repo`, `fork`.
    pub origin: String,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`.
    pub review_states: Vec<String>,
    /// `any` (default), `team`, `outsider` (by the sender's association;
    /// unknown counts as an outsider).
    pub senders: String,
    /// The repository profile the trigger needs (defaults catalogue).
    pub profile: Option<ProfileMatch>,
}

impl Conditions {
    pub fn from_config(config: &Value) -> Conditions {
        config
            .get("conditions")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self == &Conditions::default()
    }

    /// Words for the node's sub-line.
    pub fn summary(&self) -> String {
        let mut p = Vec::new();
        if !self.actions.is_empty() {
            p.push(self.actions.join("|"));
        }
        if !self.base_branches.is_empty() {
            p.push(format!("base {}", self.base_branches.join(",")));
        }
        if !self.paths.is_empty() {
            p.push(format!("paths {}", self.paths.join(",")));
        }
        if !self.labels_any.is_empty() || !self.labels_all.is_empty() {
            p.push("labels".into());
        }
        match self.drafts.as_str() {
            "exclude" => p.push("no drafts".into()),
            "only" => p.push("drafts only".into()),
            _ => {}
        }
        if !self.origin.is_empty() && self.origin != "any" {
            p.push(self.origin.replace('_', " "));
        }
        p.join(" · ")
    }
}

/// What an event says about its subject.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    pub repo: String,
    pub number: Option<i64>,
    /// `owner/repo#7`, a card id, or the event subject.
    pub subject: String,
    pub base: String,
    pub head: String,
    pub author: String,
    pub draft: Option<bool>,
    pub fork: Option<bool>,
    pub labels: Vec<String>,
    /// `None` when the event did not carry the changed files.
    pub paths: Option<Vec<String>>,
    pub review: String,
    pub labels_removed: Vec<String>,
    /// The repository's default branch (`$default` in branch globs).
    pub default_branch: String,
    /// The repository profile the repo scout stored, if any.
    pub profile: Option<RepoProfile>,
    /// Who added the labels (`labeledBy`) and whether it was the factory.
    pub labeled_by: String,
    pub labeled_by_association: String,
    /// Label -> who added it (the forge's `labeledBy` map).
    pub labeled_map: std::collections::BTreeMap<String, String>,
    pub from_factory: bool,
    /// GitHub `author_association` of the sender (OWNER, MEMBER, ...).
    pub association: String,
    /// A `/sf <name>` command in a comment, as `sf:<name>`.
    pub command: Option<String>,
    /// The commit the event is about (`headSha`, `after`, `sha`).
    pub head_sha: String,
    /// The body or comment carries the factory's own marker.
    pub factory_marker: bool,
}

fn strs(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn s(data: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| data.get(*k).and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

impl Facts {
    pub fn from_event(ev: &Event, data: &Value) -> Facts {
        let files = ["files", "changedFiles", "paths"]
            .iter()
            .find_map(|k| data.get(*k).filter(|v| v.is_array()));
        Facts {
            repo: s(data, &["repo"]),
            number: data.get("number").and_then(Value::as_i64),
            subject: ev.subject.clone(),
            base: s(data, &["base", "baseRef"]),
            head: s(data, &["head", "headRef", "branch", "headBranch"]),
            head_sha: s(data, &["headSha", "after", "sha"]),
            factory_marker: ["body", "comment"]
                .iter()
                .any(|k| s(data, &[k]).contains(FACTORY_MARKER)),
            author: s(data, &["author", "sender"]),
            draft: data.get("draft").and_then(Value::as_bool),
            fork: data.get("fork").and_then(Value::as_bool),
            labels: strs(data.get("labels")),
            paths: files.map(|v| strs(Some(v))),
            review: s(data, &["review", "reviewState"]),
            labels_removed: strs(data.get("labelsRemoved")),
            default_branch: s(data, &["defaultBranch"]),
            profile: None,
            labeled_by: s(data, &["labeledBy"]),
            labeled_by_association: s(data, &["labeledByAssociation"]),
            from_factory: data
                .get("fromFactory")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            association: s(
                data,
                &["commentAssociation", "authorAssociation", "association"],
            ),
            command: command_of(data),
            labeled_map: data
                .get("labeledBy")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.to_lowercase(), v.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    pub fn is_bot(&self) -> bool {
        let a = self.author.to_ascii_lowercase();
        a.ends_with("[bot]") || a.ends_with("-bot") || a == "dependabot" || a == "renovate"
    }
}

/// What a repository is built with: from its file paths (kls-github
/// `profile::refresh`) and the repo scout's `repo_profile` output.
pub use kls_core::repo_profile::RepoProfile;

/// A trigger's profile condition: every given list needs one overlap;
/// `scouted` needs any profile. Fails closed on an unscouted repository.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProfileMatch {
    pub scouted: bool,
    pub signals: Vec<String>,
    pub package_managers: Vec<String>,
    pub languages: Vec<String>,
}

impl ProfileMatch {
    pub fn matches(&self, p: &RepoProfile) -> bool {
        let any = |want: &[String], have: &[String]| {
            want.is_empty()
                || want
                    .iter()
                    .any(|w| have.iter().any(|h| h.eq_ignore_ascii_case(w)))
        };
        any(&self.signals, &p.signals)
            && any(&self.package_managers, &p.package_managers)
            && any(&self.languages, &p.languages)
    }
}

/// `/sf fix-tests` (first line of a comment) as `sf:fix-tests`.
pub fn sf_command(body: &str) -> Option<String> {
    let line = body.trim_start().lines().next()?.trim();
    let name = line.strip_prefix("/sf ")?.split_whitespace().next()?;
    let ok = !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| format!("sf:{}", name.to_lowercase()))
}

/// The `/sf` command of a `github.issue.comment` event (`command`, or parsed
/// from `body` / `comment`), as `sf:<name>`.
pub fn command_of(data: &Value) -> Option<String> {
    if let Some(c) = data
        .get("command")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
    {
        let c = c.to_lowercase();
        return Some(if c.starts_with("sf:") {
            c
        } else {
            format!("sf:{c}")
        });
    }
    let body = data
        .get("body")
        .or_else(|| data.get("comment"))
        .and_then(Value::as_str)
        .unwrap_or("");
    body.lines().find_map(sf_command)
}

/// Who may start work with a `/sf` comment or a label.
pub fn trusted_association(a: &str) -> bool {
    matches!(
        a.to_ascii_uppercase().as_str(),
        "OWNER" | "MEMBER" | "COLLABORATOR"
    )
}

/// `*` matches within a path segment, `**` across segments; case-insensitive.
pub fn glob(pattern: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => {
                let rest = p[2..].strip_prefix(b"/").unwrap_or(&p[2..]);
                (0..=t.len()).any(|i| go(rest, &t[i..])) || go(&p[2..], t)
            }
            Some(b'*') => (0..=t.len())
                .take_while(|i| *i == 0 || t[*i - 1] != b'/')
                .any(|i| go(&p[1..], &t[i..])),
            Some(b'?') => !t.is_empty() && t[0] != b'/' && go(&p[1..], &t[1..]),
            Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
        }
    }
    let p = pattern.trim().to_ascii_lowercase();
    let t = text.trim().to_ascii_lowercase();
    go(p.as_bytes(), t.as_bytes())
}

fn any_glob(pats: &[String], text: &str) -> bool {
    pats.iter().any(|p| glob(p, text))
}

/// Branch globs with `$default` standing for the repository's default branch.
fn branch_match(pats: &[String], branch: &str, default: &str) -> bool {
    pats.iter().any(|p| {
        if p == "$default" {
            !default.is_empty() && branch == default
        } else {
            glob(p, branch)
        }
    })
}

fn has_label(labels: &[String], want: &str) -> bool {
    labels.iter().any(|l| l.eq_ignore_ascii_case(want.trim()))
}

/// `Err(reason)` when `c` refuses the event; `names` are its webhook names
/// (`pull_request.opened`, ...).
pub fn check(c: &Conditions, names: &[String], f: &Facts) -> Result<(), String> {
    if !c.actions.is_empty() {
        let acts: Vec<&str> = names
            .iter()
            .filter_map(|n| n.split_once('.').map(|(_, a)| a))
            .collect();
        if !c
            .actions
            .iter()
            .any(|a| acts.iter().any(|x| x.eq_ignore_ascii_case(a.trim())))
        {
            return Err("action".into());
        }
    }
    if !c.base_branches.is_empty()
        && (f.base.is_empty() || !branch_match(&c.base_branches, &f.base, &f.default_branch))
    {
        return Err("base_branch".into());
    }
    if !c.head_branches.is_empty()
        && (f.head.is_empty() || !branch_match(&c.head_branches, &f.head, &f.default_branch))
    {
        return Err("head_branch".into());
    }
    if !c.paths.is_empty() || !c.ignore_paths.is_empty() {
        let Some(paths) = &f.paths else {
            // Fail closed: a path filter with no file list does not fire.
            return Err("paths_unknown".into());
        };
        let counted: Vec<&String> = paths
            .iter()
            .filter(|p| !any_glob(&c.ignore_paths, p))
            .collect();
        if counted.is_empty() {
            return Err("ignored_paths".into());
        }
        if !c.paths.is_empty() && !counted.iter().any(|p| any_glob(&c.paths, p)) {
            return Err("paths".into());
        }
    }
    if !c.labels_any.is_empty() && !c.labels_any.iter().any(|l| has_label(&f.labels, l)) {
        return Err("labels_any".into());
    }
    if !c.labels_all.iter().all(|l| has_label(&f.labels, l)) {
        return Err("labels_all".into());
    }
    match c.author_type.as_str() {
        "human" if f.is_bot() => return Err("author_type".into()),
        "bot" if !f.is_bot() => return Err("author_type".into()),
        _ => {}
    }
    if c.authors_deny
        .iter()
        .any(|a| a.eq_ignore_ascii_case(&f.author))
    {
        return Err("author_denied".into());
    }
    if !c.authors_allow.is_empty()
        && !c
            .authors_allow
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&f.author))
    {
        return Err("author_not_allowed".into());
    }
    match (c.drafts.as_str(), f.draft) {
        ("exclude", Some(true)) => return Err("draft".into()),
        ("only", d) if d != Some(true) => return Err("not_draft".into()),
        _ => {}
    }
    match c.origin.as_str() {
        // Unknown origin fails closed.
        "same_repo" if f.number.is_some() && f.fork != Some(false) => {
            return Err("origin".into());
        }
        "fork" if f.fork != Some(true) => return Err("origin".into()),
        _ => {}
    }
    match c.senders.as_str() {
        // Unknown association is an outsider (fails closed for `team`).
        "team" if !trusted_association(&f.association) => return Err("sender".into()),
        "outsider" if trusted_association(&f.association) => return Err("sender".into()),
        _ => {}
    }
    if let Some(m) = &c.profile {
        match &f.profile {
            None => return Err("profile_unknown".into()),
            Some(p) if !m.matches(p) => return Err("profile".into()),
            _ => {}
        }
    }
    if !c.review_states.is_empty()
        && !c
            .review_states
            .iter()
            .any(|r| r.eq_ignore_ascii_case(&f.review))
    {
        return Err("review_state".into());
    }
    Ok(())
}

/// The factory's own branch (`forge.branch_prefix`): its pull requests never retrigger it.
pub fn own_branch(prefix: &str, head: &str) -> bool {
    let p = prefix.trim();
    !p.is_empty() && !head.is_empty() && head.starts_with(p)
}

/// Whether the factory itself authored the event (P0 #7): the forge said
/// so (`fromFactory`), the body carries the factory's marker, or the author
/// is one of its identities (`identities`, plus [`FACTORY_LOGINS`]).
pub fn factory_authored(f: &Facts, identities: &[String]) -> bool {
    if f.from_factory || f.factory_marker {
        return true;
    }
    let a = f.author.trim();
    !a.is_empty()
        && (FACTORY_LOGINS.iter().any(|l| l.eq_ignore_ascii_case(a))
            || identities.iter().any(|l| l.trim().eq_ignore_ascii_case(a)))
}

/// The run key of a default's fire: `sys:<key>:<repo>:<subject>[:<sha>]`
/// (`system_key` is `sys:<key>:<repo>`), so a subject at one commit runs a
/// default once.
pub fn run_key(system_key: &str, f: &Facts) -> Option<String> {
    if system_key.is_empty() || f.subject.is_empty() {
        return None;
    }
    let mut k = format!("{system_key}:{}", f.subject.to_lowercase());
    if !f.head_sha.is_empty() {
        k.push(':');
        k.push_str(&f.head_sha);
    }
    Some(k)
}

/// Debounce and hourly cap over a trigger's recent fires (newest first).
/// `Err(reason)` when the fire is held back.
pub fn rate_check(
    recent: &[TriggerFire],
    subject: &str,
    now: i64,
    debounce_seconds: i64,
    max_per_hour: i64,
) -> Result<(), String> {
    let started = |f: &&TriggerFire| f.outcome == "started";
    if debounce_seconds > 0 {
        let window = now - debounce_seconds.min(MAX_DEBOUNCE_SECONDS) * 1000;
        if recent
            .iter()
            .filter(started)
            .any(|f| f.subject == subject && f.at >= window)
        {
            return Err("debounced".into());
        }
    }
    if max_per_hour > 0 {
        let hour = now - 3_600_000;
        let n = recent
            .iter()
            .filter(started)
            .filter(|f| f.at >= hour)
            .count() as i64;
        if n >= max_per_hour {
            return Err("run_cap".into());
        }
    }
    Ok(())
}

/// A trigger's priority (1 high .. 5 low).
pub fn priority(config: &Value) -> i64 {
    config
        .get("priority")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_PRIORITY)
        .clamp(1, 5)
}

pub fn debounce_seconds(config: &Value) -> i64 {
    config
        .get("debounceSeconds")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .clamp(0, MAX_DEBOUNCE_SECONDS)
}

pub fn max_runs_per_hour(config: &Value) -> i64 {
    config
        .get("maxRunsPerHour")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_MAX_RUNS_PER_HOUR)
        .clamp(1, 10_000)
}

/// Problems with a trigger's conditions and safeguards (for validation).
pub fn problems(config: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(v) = config.get("conditions") {
        match serde_json::from_value::<Conditions>(v.clone()) {
            Err(e) => out.push(format!("conditions do not parse: {e}")),
            Ok(c) => {
                if !c.drafts.is_empty() && !DRAFTS.contains(&c.drafts.as_str()) {
                    out.push(format!("drafts is one of {}", DRAFTS.join(", ")));
                }
                if !c.origin.is_empty() && !ORIGINS.contains(&c.origin.as_str()) {
                    out.push(format!("origin is one of {}", ORIGINS.join(", ")));
                }
                if !c.author_type.is_empty() && !AUTHOR_TYPES.contains(&c.author_type.as_str()) {
                    out.push(format!("author type is one of {}", AUTHOR_TYPES.join(", ")));
                }
                if !c.senders.is_empty() && !SENDERS.contains(&c.senders.as_str()) {
                    out.push(format!("senders is one of {}", SENDERS.join(", ")));
                }
            }
        }
    }
    for (k, lo, hi) in [
        ("debounceSeconds", 0, MAX_DEBOUNCE_SECONDS),
        ("maxRunsPerHour", 1, 10_000),
        ("priority", 1, 5),
    ] {
        if let Some(v) = config.get(k)
            && !v.as_i64().is_some_and(|n| (lo..=hi).contains(&n))
        {
            out.push(format!("{k} is {lo} to {hi}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn facts() -> Facts {
        Facts {
            repo: "acme/api".into(),
            number: Some(7),
            subject: "acme/api#7".into(),
            base: "main".into(),
            head: "feature/x".into(),
            author: "ana".into(),
            draft: Some(false),
            fork: Some(false),
            labels: vec!["Bug".into()],
            paths: Some(vec!["src/lib.rs".into(), "docs/a.md".into()]),
            ..Default::default()
        }
    }

    fn names() -> Vec<String> {
        vec!["pull_request.opened".into()]
    }

    #[test]
    fn globs() {
        assert!(glob("src/*.rs", "src/lib.rs"));
        assert!(!glob("src/*.rs", "src/a/lib.rs"));
        assert!(glob("src/**", "src/a/lib.rs"));
        assert!(glob("**/*.md", "docs/a/b.md"));
        assert!(glob("**/*.md", "b.md"));
        assert!(glob("release/*", "release/1.2"));
        assert!(glob("MAIN", "main"));
        assert!(!glob("main", "maint"));
    }

    #[test]
    fn each_condition_refuses_with_its_reason() {
        let f = facts();
        let ok = |c: Conditions| check(&c, &names(), &f);
        assert!(ok(Conditions::default()).is_ok());
        let c = |j: Value| serde_json::from_value::<Conditions>(j).unwrap();
        assert!(ok(c(json!({"actions": ["opened"]}))).is_ok());
        assert_eq!(ok(c(json!({"actions": ["closed"]}))).unwrap_err(), "action");
        assert!(ok(c(json!({"baseBranches": ["main"]}))).is_ok());
        assert_eq!(
            ok(c(json!({"baseBranches": ["release/*"]}))).unwrap_err(),
            "base_branch"
        );
        assert_eq!(
            ok(c(json!({"headBranches": ["fix/*"]}))).unwrap_err(),
            "head_branch"
        );
        assert!(ok(c(json!({"paths": ["src/**"]}))).is_ok());
        assert_eq!(ok(c(json!({"paths": ["web/**"]}))).unwrap_err(), "paths");
        assert_eq!(
            ok(c(json!({"ignorePaths": ["**"]}))).unwrap_err(),
            "ignored_paths"
        );
        assert!(ok(c(json!({"labelsAny": ["bug", "x"]}))).is_ok());
        assert_eq!(
            ok(c(json!({"labelsAll": ["bug", "x"]}))).unwrap_err(),
            "labels_all"
        );
        assert_eq!(
            ok(c(json!({"authorType": "bot"}))).unwrap_err(),
            "author_type"
        );
        assert_eq!(
            ok(c(json!({"authorsDeny": ["ANA"]}))).unwrap_err(),
            "author_denied"
        );
        assert_eq!(
            ok(c(json!({"authorsAllow": ["bo"]}))).unwrap_err(),
            "author_not_allowed"
        );
        assert_eq!(ok(c(json!({"drafts": "only"}))).unwrap_err(), "not_draft");
        assert_eq!(ok(c(json!({"origin": "fork"}))).unwrap_err(), "origin");
        assert!(ok(c(json!({"origin": "same_repo"}))).is_ok());
        assert_eq!(
            ok(c(json!({"reviewStates": ["APPROVED"]}))).unwrap_err(),
            "review_state"
        );

        let mut draft = facts();
        draft.draft = Some(true);
        assert_eq!(
            check(&c(json!({"drafts": "exclude"})), &names(), &draft).unwrap_err(),
            "draft"
        );
        let mut unknown = facts();
        unknown.paths = None;
        assert_eq!(
            check(&c(json!({"paths": ["src/**"]})), &names(), &unknown).unwrap_err(),
            "paths_unknown"
        );
        let mut fork = facts();
        fork.fork = None;
        assert_eq!(
            check(&c(json!({"origin": "same_repo"})), &names(), &fork).unwrap_err(),
            "origin",
            "unknown origin fails closed"
        );
    }

    #[test]
    fn safeguards() {
        assert!(own_branch("belai/K-", "belai/K-7f21ab"));
        assert!(!own_branch("belai/K-", "feature/x"));
        assert!(!own_branch("", "anything"));
        let fire = |subject: &str, at: i64, outcome: &str| TriggerFire {
            schedule_id: "s".into(),
            subject: subject.into(),
            at,
            outcome: outcome.into(),
            ..Default::default()
        };
        let now = 10_000_000;
        let recent = vec![
            fire("a#1", now - 10_000, "started"),
            fire("a#2", now - 5_000, "skipped:debounced"),
        ];
        assert_eq!(
            rate_check(&recent, "a#1", now, 30, 60).unwrap_err(),
            "debounced"
        );
        assert!(rate_check(&recent, "a#1", now, 5, 60).is_ok());
        assert!(
            rate_check(&recent, "a#2", now, 30, 60).is_ok(),
            "skips do not count"
        );
        assert_eq!(
            rate_check(&recent, "a#3", now, 0, 1).unwrap_err(),
            "run_cap"
        );
        let old = vec![fire("a#1", now - 3_700_000, "started")];
        assert!(rate_check(&old, "a#9", now, 0, 1).is_ok());
        assert_eq!(priority(&json!({"priority": 9})), 5);
        assert_eq!(max_runs_per_hour(&json!({})), 60);
        assert!(problems(&json!({"conditions": {"drafts": "maybe"}, "priority": 0})).len() == 2);
        assert!(
            problems(&json!({"conditions": {"paths": ["a"]}, "debounceSeconds": 30})).is_empty()
        );
    }

    #[test]
    fn senders_and_factory_identity() {
        let c = |s: &str| Conditions {
            senders: s.into(),
            ..Default::default()
        };
        let mut f = facts();
        // Unknown association is an outsider.
        assert_eq!(check(&c("team"), &[], &f), Err("sender".into()));
        assert!(check(&c("outsider"), &[], &f).is_ok());
        f.association = "MEMBER".into();
        assert!(check(&c("team"), &[], &f).is_ok());
        assert_eq!(check(&c("outsider"), &[], &f), Err("sender".into()));
        assert!(!problems(&json!({"conditions": {"senders": "nobody"}})).is_empty());

        let mut f = facts();
        assert!(!factory_authored(&f, &[]));
        assert!(factory_authored(&f, &["ANA".into()]));
        f.factory_marker = true;
        assert!(factory_authored(&f, &[]));
        let f = Facts {
            author: "belai[bot]".into(),
            ..Default::default()
        };
        assert!(factory_authored(&f, &[]));
    }
}
