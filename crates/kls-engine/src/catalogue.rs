//! The builtin belai agent profiles (58) and crews (6), ported verbatim from
//! belai `internal/agentprofile/builtin/*.json` and `crews/*.json` (belai
//! d5b79c2, 2026-11-20), plus their avatars.
//!
//! [`ProfileSpec`] is the typed shape of a belai profile. The agent builder
//! edits it (mode, autonomy, guardrails, handoffs, workspace, budget,
//! memory, knowledge) and stores it as the profile's `meta_json`, which is
//! the format a node reads; unknown keys ride along in `extra`.
//!
//! Keleustes runs builtins by filing a routing card a worker claims
//! ([`crate::route`]); it never copies their prompts to run them.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Where the bundled copy came from (provenance of the catalogue).
pub const SOURCE: &str = "belai internal/agentprofile @ d5b79c2 (2026-11-20)";

const PROFILES_JSON: &str = include_str!("builtins/profiles.json");
const CREWS_JSON: &str = include_str!("builtins/crews.json");
const AVATARS_JSON: &str = include_str!("builtins/avatars.json");

fn is_false(b: &bool) -> bool {
    !*b
}
fn is_zero(n: &i64) -> bool {
    *n == 0
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Personality {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub report_style: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vocabulary: Vec<String>,
}

/// A card move on success or failure.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CardMove {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub list: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drop_labels: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CliSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub write: bool,
}

/// The claim and handoff rules of a worker (belai `KanbanSpec`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KanbanSpec {
    /// The lists it claims from (`backlog`, `review`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lists: Vec<String>,
    /// Every one of these labels must be on a card it claims.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_success: Option<CardMove>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_failure: Option<CardMove>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handoff_to: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handoff_labels: Vec<String>,
    /// The `checkpoint:<reason>` a handoff holds for a person.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub handoff_checkpoint: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_attempts: i64,
    /// A Go duration (`30m`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lease: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliSpec>,
    /// gates, security, quality, lint, conflicts ...
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSpec {
    /// `worktree`, `none`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub isolation: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only: bool,
    /// `agent`, `none`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub publish: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd_per_item: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalize_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_passes_per_item: Option<i64>,
    /// A Go duration (`2h`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub max_wall_per_item: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MemorySpec {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

/// A belai agent profile, typed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileSpec {
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub display_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub identity: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub system_prompt: String,
    /// `worker`, `single`, `goal`, `monitor`, `schedule`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode: String,
    /// `autonomous`, `supervised`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub autonomy: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_iterations: i64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub reflection: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub schedule: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub monitor_condition: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// `low`, `medium`, `high`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub effort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardrails: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_permission: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub palette: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub avatar_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<Personality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kanban: Option<KanbanSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemorySpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<BudgetSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge: Option<KnowledgeSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// Anything else belai knows (facts, ...).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

pub const PROFILE_MODES: [&str; 5] = ["worker", "single", "goal", "monitor", "schedule"];
pub const AUTONOMY: [&str; 2] = ["autonomous", "supervised"];
pub const EFFORT: [&str; 3] = ["low", "medium", "high"];
pub const ISOLATION: [&str; 2] = ["worktree", "none"];
pub const CLAIM_LISTS: [&str; 4] = ["backlog", "in_progress", "review", "done"];

impl ProfileSpec {
    /// Parses a profile's `meta_json` (belai profile JSON); `{}` or junk
    /// gives the default.
    pub fn parse(json: &str) -> ProfileSpec {
        serde_json::from_str(json).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// A worker (claims cards from the board).
    pub fn is_worker(&self) -> bool {
        self.mode == "worker" && self.kanban.is_some()
    }

    /// Runs on its own on the node and is never dispatched.
    pub fn is_background(&self) -> bool {
        !self.is_worker()
    }

    pub fn read_only(&self) -> bool {
        self.workspace.as_ref().is_some_and(|w| w.read_only)
    }

    /// The `checkpoint:<reason>` its handoff holds for, if any.
    pub fn checkpoint(&self) -> Option<&str> {
        self.kanban
            .as_ref()
            .map(|k| k.handoff_checkpoint.as_str())
            .filter(|s| !s.is_empty())
    }

    /// Problems a save must refuse (belai's own rules, the ones Keleustes can check).
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.mode.is_empty() && !PROFILE_MODES.contains(&self.mode.as_str()) {
            out.push(format!("unknown mode {:?}", self.mode));
        }
        if !self.autonomy.is_empty() && !AUTONOMY.contains(&self.autonomy.as_str()) {
            out.push(format!("unknown autonomy {:?}", self.autonomy));
        }
        if !self.effort.is_empty() && !EFFORT.contains(&self.effort.as_str()) {
            out.push(format!("unknown effort {:?}", self.effort));
        }
        if !(0..=1000).contains(&self.max_iterations) {
            out.push("max iterations is 0 to 1000".into());
        }
        if self.mode == "worker" {
            match &self.kanban {
                None => out.push("a worker needs claim lists".into()),
                Some(k) => {
                    if k.lists.is_empty() {
                        out.push("a worker claims from at least one list".into());
                    }
                    for l in &k.lists {
                        if !CLAIM_LISTS.contains(&l.as_str()) {
                            out.push(format!("unknown list {l:?}"));
                        }
                    }
                    if k.labels.is_empty() && k.cli.is_none() {
                        out.push("a worker claims by at least one label".into());
                    }
                    if !(0..=10).contains(&k.max_attempts) {
                        out.push("max attempts is 0 to 10".into());
                    }
                }
            }
        }
        if self.mode == "schedule" && self.schedule.trim().is_empty() {
            out.push("a scheduled agent needs a schedule".into());
        }
        if self.mode == "monitor" && self.monitor_condition.trim().is_empty() {
            out.push("a monitor needs a condition".into());
        }
        if let Some(b) = &self.budget
            && let Some(f) = b.finalize_at
            && !(0.0..=1.0).contains(&f)
        {
            out.push("finalize at is a share from 0 to 1".into());
        }
        out
    }
}

/// A crew member (belai `crews/*.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CrewMemberSpec {
    pub profile: String,
    #[serde(default = "one")]
    pub replicas: i64,
}

fn one() -> i64 {
    1
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CrewSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub one_per_repo: bool,
    #[serde(default)]
    pub members: Vec<CrewMemberSpec>,
}

impl CrewSpec {
    /// The member that takes a new card (the crew's first stage).
    pub fn entry(&self) -> Option<&str> {
        self.members.first().map(|m| m.profile.as_str())
    }
}

/// Every builtin profile, by name.
pub fn profiles() -> &'static [ProfileSpec] {
    static P: OnceLock<Vec<ProfileSpec>> = OnceLock::new();
    P.get_or_init(|| serde_json::from_str(PROFILES_JSON).expect("bundled profiles parse"))
}

pub fn crews() -> &'static [CrewSpec] {
    static C: OnceLock<Vec<CrewSpec>> = OnceLock::new();
    C.get_or_init(|| serde_json::from_str(CREWS_JSON).expect("bundled crews parse"))
}

/// Builtin avatars by `avatar_id` (SVG text).
pub fn avatars() -> &'static BTreeMap<String, String> {
    static A: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    A.get_or_init(|| serde_json::from_str(AVATARS_JSON).expect("bundled avatars parse"))
}

/// `belai:x` or bare `x`.
fn full(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("belai:{name}")
    }
}

pub fn profile(name: &str) -> Option<&'static ProfileSpec> {
    let n = full(name);
    profiles().iter().find(|p| p.name == n)
}

pub fn crew(name: &str) -> Option<&'static CrewSpec> {
    let n = full(name);
    crews().iter().find(|c| c.name == n)
}

/// The members of crew `name` with each member's own model (empty when it
/// sets none): a crew from the workforce (`defined`, by name or id) first,
/// else the builtin crew. Dispatch and the crew inspector both read this.
pub fn crew_members(defined: &[kls_store::vault::Crew], name: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> =
        match defined.iter().find(|c| c.name == name || c.id == name) {
            Some(c) => c
                .members
                .iter()
                .map(|m| (m.agent.clone(), m.model.trim().to_string()))
                .collect(),
            None => crew(name)
                .map(|c| {
                    c.members
                        .iter()
                        .map(|m| (m.profile.clone(), String::new()))
                        .collect()
                })
                .unwrap_or_default(),
        };
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

pub fn avatar_svg(p: &ProfileSpec) -> Option<&'static str> {
    avatars().get(&p.avatar_id).map(String::as_str)
}

/// Builtin workers a step may dispatch, sorted.
pub fn worker_names() -> Vec<&'static str> {
    profiles()
        .iter()
        .filter(|p| p.is_worker())
        .map(|p| p.name.as_str())
        .collect()
}

/// Builtin profiles that run in the background (never dispatched).
pub fn background_names() -> Vec<&'static str> {
    profiles()
        .iter()
        .filter(|p| p.is_background())
        .map(|p| p.name.as_str())
        .collect()
}

pub fn crew_names() -> Vec<&'static str> {
    crews().iter().map(|c| c.name.as_str()).collect()
}

/// Whether a Vulnetix CLI job writes (runs on `belai:cli-runner-write`).
pub fn cli_write_job(job: &str) -> bool {
    profile("belai:cli-runner-write")
        .and_then(|p| p.kanban.as_ref())
        .and_then(|k| k.cli.as_ref())
        .is_some_and(|c| c.jobs.iter().any(|j| j == job))
}

/// Every Vulnetix CLI job a builtin runner claims.
pub fn cli_jobs() -> Vec<&'static str> {
    ["belai:cli-runner", "belai:cli-runner-write"]
        .iter()
        .filter_map(|n| profile(n))
        .filter_map(|p| p.kanban.as_ref()?.cli.as_ref())
        .flat_map(|c| c.jobs.iter().map(String::as_str))
        .collect()
}

/// Most replicas one crew member may run.
pub const MAX_REPLICAS: i64 = 8;

/// Problems with a crew (website `validateCrew` / `claimConflicts`): two
/// members that claim the same cards, a handoff label no member claims, a
/// replica count out of range, an unknown or background member.
/// `resolve` finds a member's profile (saved profiles first, then builtins).
pub fn crew_problems(
    members: &[(String, i64)],
    resolve: &dyn Fn(&str) -> Option<ProfileSpec>,
) -> Vec<String> {
    let mut out = Vec::new();
    if members.is_empty() {
        out.push("a crew needs at least one member".into());
    }
    let mut specs: Vec<(String, KanbanSpec)> = Vec::new();
    for (name, count) in members {
        if !(1..=MAX_REPLICAS).contains(count) {
            out.push(format!("{name}: 1 to {MAX_REPLICAS} replicas"));
        }
        match resolve(name) {
            None => out.push(format!("{name}: no such agent")),
            Some(p) if !p.is_worker() => out.push(format!("{name}: not a worker")),
            Some(p) => specs.push((name.clone(), p.kanban.unwrap_or_default())),
        }
    }
    let claims = |k: &KanbanSpec, list: &str, labels: &[String]| {
        k.lists.iter().any(|l| l == list)
            && k.labels
                .iter()
                .all(|need| labels.iter().any(|l| l.eq_ignore_ascii_case(need)))
    };
    for (i, (a, ka)) in specs.iter().enumerate() {
        for (b, kb) in specs.iter().skip(i + 1) {
            if a != b
                && ka.lists.iter().any(|l| kb.lists.contains(l))
                && !ka.labels.is_empty()
                && ka.labels == kb.labels
            {
                out.push(format!("{a} and {b} claim the same cards"));
            }
        }
        // A handoff (or success move) must land where a member claims it.
        let mut hand: Vec<(String, Vec<String>)> = Vec::new();
        if !ka.handoff_labels.is_empty() {
            hand.push(("backlog".into(), ka.handoff_labels.clone()));
        }
        if let Some(m) = &ka.on_success
            && !m.labels.is_empty()
            && m.list != "done"
        {
            hand.push((m.list.clone(), m.labels.clone()));
        }
        for (list, labels) in hand {
            let taken = specs.iter().any(|(_, k)| claims(k, &list, &labels));
            if !taken && !labels.iter().all(|l| l == "ticket" || l == "reviewed") {
                out.push(format!(
                    "{a} hands off {} to {list}, which no member claims",
                    labels.join(", ")
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crew_members_prefer_the_workforce_crew() {
        use kls_store::vault::{Crew, CrewMember};
        let m = |agent: &str, model: &str| CrewMember {
            agent: agent.into(),
            role: String::new(),
            count: 1,
            model: model.into(),
        };
        let defined = vec![Crew {
            id: "c1".into(),
            name: "mine".into(),
            members: vec![m("a", " p/x "), m("a", ""), m("b", "")],
            ..Default::default()
        }];
        let want = vec![
            ("a".to_string(), "p/x".to_string()),
            ("b".into(), "".into()),
        ];
        assert_eq!(crew_members(&defined, "mine"), want);
        assert_eq!(crew_members(&defined, "c1"), want);
        let builtin = crew_members(&defined, "belai:feature");
        assert_eq!(builtin.len(), 3);
        assert!(builtin.iter().all(|(_, m)| m.is_empty()));
        assert!(crew_members(&[], "nope").is_empty());
    }

    #[test]
    fn catalogue_matches_belai() {
        assert_eq!(profiles().len(), 58);
        assert_eq!(crews().len(), 6);
        assert_eq!(worker_names().len(), 40);
        assert_eq!(background_names().len(), 18);
        assert!(crew("maintenance").is_some());
        assert!(profile("belai:deps-javascript").unwrap().is_background());
        assert!(profile("deps-npm").is_none());
        let b = profile("builder").unwrap();
        let k = b.kanban.as_ref().unwrap();
        assert_eq!(k.lists, ["backlog"]);
        assert_eq!(k.labels, ["build"]);
        assert_eq!(k.on_success.as_ref().unwrap().list, "review");
        assert!(k.extra.contains_key("gates"));
        assert_eq!(
            profile("planner").unwrap().checkpoint(),
            Some("plan_approval")
        );
        assert!(profile("planner").unwrap().read_only());
        // Every crew member is a builtin worker, and every avatar resolves.
        for c in crews() {
            for m in &c.members {
                assert!(profile(&m.profile).unwrap().is_worker(), "{}", m.profile);
            }
        }
        for p in profiles().iter().filter(|p| !p.avatar_id.is_empty()) {
            assert!(avatar_svg(p).is_some(), "{}", p.name);
        }
    }

    #[test]
    fn builtin_crews_validate_and_conflicts_show() {
        let r = |n: &str| profile(n).cloned();
        for c in crews() {
            let m: Vec<(String, i64)> = c
                .members
                .iter()
                .map(|m| (m.profile.clone(), m.replicas))
                .collect();
            assert!(
                crew_problems(&m, &r).is_empty(),
                "{}: {:?}",
                c.name,
                crew_problems(&m, &r)
            );
        }
        let bad = vec![
            ("belai:builder".to_string(), 1),
            ("belai:builder".to_string(), 0),
            ("belai:deps-go".to_string(), 1),
            ("belai:nobody".to_string(), 1),
        ];
        let p = crew_problems(&bad, &r);
        assert!(p.iter().any(|x| x.contains("replicas")));
        assert!(p.iter().any(|x| x.contains("not a worker")));
        assert!(p.iter().any(|x| x.contains("no such agent")));
        // The planner hands `build` to the backlog: alone it has no taker.
        let p = crew_problems(&[("belai:planner".into(), 1)], &r);
        assert!(p.iter().any(|x| x.contains("no member claims")), "{p:?}");
        assert!(crew_problems(&[], &r).len() == 1);
    }

    #[test]
    fn every_builtin_passes_its_own_checks_and_round_trips() {
        for p in profiles() {
            assert!(p.problems().is_empty(), "{}: {:?}", p.name, p.problems());
            let back = ProfileSpec::parse(&p.to_json());
            assert_eq!(&back, p, "{}", p.name);
        }
        let bad = ProfileSpec {
            mode: "worker".into(),
            autonomy: "yolo".into(),
            ..Default::default()
        };
        assert_eq!(bad.problems().len(), 2);
    }
}
