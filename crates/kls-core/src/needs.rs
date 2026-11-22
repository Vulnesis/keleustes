//! "Needs you": one queue of everything waiting on a person, across areas.
//!
//! - the engine (M3): pending approvals, gates a person passes, steps
//!   waiting for a node, paused runs ([`Core::engine_checkpoints`]);
//! - GitHub (M4): open pull requests awaiting review, and approved ones
//!   ready to merge (managed repositories only);
//! - the board (M2): cards in review or labelled `blocked`;
//! - sessions (M1): open sessions with a pending question;
//! - GitHub: draft pull requests, external issues with no team reply, and
//!   deployments waiting on an environment's reviewers;
//! - the fleet: joined nodes that went offline.
//!
//! The Checkpoints screen and the Cockpit both read [`Core::needs_you`].
//! Screens reload it on the prefixes in [`RELOAD_ON`].

use crate::Core;
use anyhow::Result;
use kls_store::engine::{ACTIVE_RUN_STATUSES, Approval};
use kls_store::sessions::SessionQuery;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Event kind prefixes that change the queue.
pub const RELOAD_ON: [&str; 11] = [
    "run.",
    "host.",
    "approval.",
    "workflow.",
    "card.",
    "session.",
    "github.pull.",
    "github.repo.",
    "github.issue.",
    "github.deployment",
    "node.",
];

/// Deployment-review events (from the GitHub plane): `requested`, then
/// `approved` / `rejected` clear it. Data `{repo, runId, environment, url,
/// requester}`; subject `<repo>:<runId>:<environment>`.
pub const DEPLOY_REVIEW_PREFIX: &str = "github.deployment_review.";
/// A deployment review older than this is not shown (vdb-site: 7 days).
pub const DEPLOY_REVIEW_MAX_AGE_MS: i64 = 7 * 86_400_000;
/// An offline node not seen for this long is not shown.
pub const NODE_OFFLINE_MAX_AGE_MS: i64 = 30 * 86_400_000;
/// `forge.quota.on_exhausted = escalate` (kls_github::budget::kinds::QUOTA_ESCALATED).
pub const QUOTA_ESCALATED: &str = "github.quota.escalated";
/// An escalation without a reset time shows this long.
pub const QUOTA_ESCALATED_MAX_AGE_MS: i64 = 3_600_000;
/// Where a quota checkpoint opens: Budgets > Usage.
pub const QUOTA_USAGE_LINK: &str = "keleustes://budgets/usage";

/// What a checkpoint waits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    /// A pending approval (human step, gate escalation).
    Approval,
    /// A gate a person passes or fails.
    Gate,
    /// A deployment waiting on a person: a deploy gate step of a run, or a
    /// GitHub environment's required review.
    DeployGate,
    /// A step that waits on a node coming online.
    NoNode,
    /// A paused run.
    Paused,
    /// A session asked a question.
    Question,
    /// An open pull request nobody has reviewed yet.
    PullReview,
    /// An approved pull request with green checks, ready to merge.
    PullReady,
    /// An open draft pull request.
    PullDraft,
    /// An open issue from outside the team that nobody on it answered.
    IssueExternal,
    /// A card in review.
    CardReview,
    /// A card on the Blocked list (or labelled `blocked`).
    CardBlocked,
    /// A card held for a person by a `checkpoint:<reason>` label.
    CardHeld,
    /// A worker's claim on a card whose lease ran out.
    ClaimLapsed,
    /// A host step only a person can do (root, a browser sign-in): the
    /// exact command is in `reason`, the Host view link in `url`.
    HostHandoff,
    /// A host whose belai has an update.
    HostUpdate,
    /// A joined node that is offline.
    NodeOffline,
    /// The factory's GitHub share ran out with `forge.quota.on_exhausted =
    /// escalate` (`github.quota.escalated`); opens Budgets > Usage.
    QuotaEscalated,
}

impl CheckpointKind {
    pub const ALL: [CheckpointKind; 18] = [
        CheckpointKind::Approval,
        CheckpointKind::Gate,
        CheckpointKind::DeployGate,
        CheckpointKind::CardHeld,
        CheckpointKind::Question,
        CheckpointKind::HostHandoff,
        CheckpointKind::NoNode,
        CheckpointKind::Paused,
        CheckpointKind::PullReady,
        CheckpointKind::PullReview,
        CheckpointKind::PullDraft,
        CheckpointKind::IssueExternal,
        CheckpointKind::ClaimLapsed,
        CheckpointKind::CardBlocked,
        CheckpointKind::CardReview,
        CheckpointKind::QuotaEscalated,
        CheckpointKind::HostUpdate,
        CheckpointKind::NodeOffline,
    ];

    pub fn words(self) -> &'static str {
        match self {
            CheckpointKind::Approval => "Approval",
            CheckpointKind::Gate => "Gate",
            CheckpointKind::DeployGate => "Deploy gate",
            CheckpointKind::NoNode => "Waiting for a node",
            CheckpointKind::Paused => "Paused run",
            CheckpointKind::Question => "Question",
            CheckpointKind::PullReview => "Review",
            CheckpointKind::PullReady => "Ready to merge",
            CheckpointKind::PullDraft => "Draft",
            CheckpointKind::IssueExternal => "External issue",
            CheckpointKind::CardReview => "Card in review",
            CheckpointKind::CardBlocked => "Blocked card",
            CheckpointKind::CardHeld => "Waiting for your decision",
            CheckpointKind::ClaimLapsed => "Claim lapsed",
            CheckpointKind::HostHandoff => "Host step for you",
            CheckpointKind::HostUpdate => "belai update",
            CheckpointKind::NodeOffline => "Node offline",
            CheckpointKind::QuotaEscalated => "GitHub quota",
        }
    }

    /// Most urgent first.
    pub fn rank(self) -> usize {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(99)
    }

    /// Settled with approve / reject in place (engine checkpoints); the
    /// others are opened where they live. A deploy gate is decidable when it
    /// is a run's step (see [`Checkpoint::decidable`]).
    pub fn decidable(self) -> bool {
        matches!(
            self,
            CheckpointKind::Approval
                | CheckpointKind::Gate
                | CheckpointKind::NoNode
                | CheckpointKind::Paused
        )
    }

    /// The snake_case name (filters, deep links).
    pub fn key(self) -> &'static str {
        match self {
            CheckpointKind::Approval => "approval",
            CheckpointKind::Gate => "gate",
            CheckpointKind::DeployGate => "deploy_gate",
            CheckpointKind::NoNode => "no_node",
            CheckpointKind::Paused => "paused",
            CheckpointKind::Question => "question",
            CheckpointKind::PullReview => "pr_review",
            CheckpointKind::PullReady => "pr_ready",
            CheckpointKind::PullDraft => "pr_draft",
            CheckpointKind::IssueExternal => "issue_external",
            CheckpointKind::CardReview => "card_review",
            CheckpointKind::CardBlocked => "card_blocked",
            CheckpointKind::CardHeld => "card_held",
            CheckpointKind::ClaimLapsed => "claim_lapsed",
            CheckpointKind::HostHandoff => "host_handoff",
            CheckpointKind::HostUpdate => "host_update",
            CheckpointKind::NodeOffline => "node_offline",
            CheckpointKind::QuotaEscalated => "quota_escalated",
        }
    }
}

/// One thing waiting on a person.
#[derive(Clone, Debug, PartialEq)]
pub struct Checkpoint {
    /// `approval:<id>`, `step:<id>`, `run:<id>`, `session:<id>`,
    /// `pull:<repo>#<n>`, `card:<id>`.
    pub key: String,
    pub kind: CheckpointKind,
    pub ts: i64,
    pub title: String,
    pub workflow: String,
    pub run_id: String,
    pub repo: String,
    pub card_id: String,
    pub step_id: Option<String>,
    pub approval: Option<Approval>,
    /// The session to open on the Sessions screen (the run's latest crew
    /// step, or the asking session).
    pub session_id: Option<String>,
    /// The pull request number (pull kinds).
    pub number: Option<i64>,
    /// A web link (the pull request).
    pub url: String,
    pub reason: String,
    pub sla_at: Option<i64>,
    /// Who opened it (pull request or issue author, deploy requester).
    pub author: String,
    /// The node it is about (node offline, approvals on a node).
    pub node_id: String,
    /// An open pull request with merge conflicts.
    pub conflict: bool,
}

impl Checkpoint {
    /// Whether approve / reject settles it in place.
    pub fn decidable(&self) -> bool {
        self.kind.decidable() || (self.kind == CheckpointKind::DeployGate && self.step_id.is_some())
    }

    fn new(kind: CheckpointKind, key: String, ts: i64, title: String) -> Self {
        Self {
            key,
            kind,
            ts,
            title,
            workflow: String::new(),
            run_id: String::new(),
            repo: String::new(),
            card_id: String::new(),
            step_id: None,
            approval: None,
            session_id: None,
            number: None,
            url: String::new(),
            reason: String::new(),
            sla_at: None,
            author: String::new(),
            node_id: String::new(),
            conflict: false,
        }
    }
}

/// A gate step that guards a deployment: its title says deploy, or its
/// details name an environment.
pub fn is_deploy_step(title: &str, details_json: &str) -> bool {
    if title.to_ascii_lowercase().contains("deploy") {
        return true;
    }
    serde_json::from_str::<serde_json::Value>(details_json)
        .ok()
        .and_then(|v| {
            v.get("environment")
                .and_then(|e| e.as_str())
                .map(|e| !e.is_empty())
        })
        .unwrap_or(false)
}

/// A login that is a bot (`dependabot[bot]`, `renovate-bot`).
fn is_bot_login(login: &str) -> bool {
    let l = login.to_ascii_lowercase();
    l.ends_with("[bot]") || l.ends_with("-bot")
}

/// Outside the team: an association other than owner, member or
/// collaborator. Unknown ("") counts as outside, so the issue is shown.
pub fn is_external(association: &str, login: &str) -> bool {
    !matches!(association, "OWNER" | "MEMBER" | "COLLABORATOR") && !is_bot_login(login)
}

/// Counts the Cockpit shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Open sessions that are live or waiting.
    pub live_sessions: usize,
    /// Open sessions of any state.
    pub open_sessions: usize,
    pub pending_approvals: usize,
    pub questions: usize,
}

/// `blocked`, `sf:blocked`, `status:blocked` (any case).
pub fn is_blocked_label(l: &str) -> bool {
    let l = l.trim().to_ascii_lowercase();
    l == "blocked" || l.ends_with(":blocked")
}

fn sort(out: &mut [Checkpoint]) {
    out.sort_by(|a, b| {
        a.kind
            .rank()
            .cmp(&b.kind.rank())
            .then(b.ts.cmp(&a.ts))
            .then(a.key.cmp(&b.key))
    });
}

impl Core {
    /// The engine's part: approvals, gates, steps waiting for a node and
    /// paused runs, most urgent kind first, newest first within a kind.
    pub async fn engine_checkpoints(&self) -> Result<Vec<Checkpoint>> {
        let s = self.store();
        let mut out = Vec::new();
        let mut names = HashMap::new();
        for w in s.workflows(true).await? {
            names.insert(w.id.clone(), w.name.clone());
        }
        for r in s.runs_in(&ACTIVE_RUN_STATUSES).await? {
            let wf = names.get(&r.workflow_id).cloned().unwrap_or_default();
            let steps = s.run_steps(&r.id).await?;
            let session_id = steps
                .iter()
                .rev()
                .find_map(|x| x.session_id.clone().filter(|v| !v.is_empty()));
            let base = |kind, key: String, ts, title: String| {
                let mut c = Checkpoint::new(kind, key, ts, title);
                c.workflow = wf.clone();
                c.run_id = r.id.clone();
                c.repo = r.repo.clone();
                c.card_id = r.card_id.clone();
                c.session_id = session_id.clone();
                c
            };
            if r.status == "paused" {
                let mut c = base(
                    CheckpointKind::Paused,
                    format!("run:{}", r.id),
                    r.updated_at,
                    format!("{wf} is paused"),
                );
                c.reason = r.reason.clone();
                out.push(c);
            }
            for a in s.approvals(Some("pending"), Some(&r.id), 50).await? {
                let mut c = base(
                    CheckpointKind::Approval,
                    format!("approval:{}", a.id),
                    a.created_at,
                    a.title.clone(),
                );
                c.step_id = Some(a.step_id.clone());
                c.sla_at = a.sla_at;
                c.node_id = a.node_id.clone();
                c.reason = a.kind.clone();
                c.approval = Some(a);
                out.push(c);
            }
            for st in steps.iter().filter(|x| x.status == "waiting") {
                let kind = match st.error.as_str() {
                    "deferred:gate" if is_deploy_step(&st.title, &st.details_json) => {
                        CheckpointKind::DeployGate
                    }
                    "deferred:gate" => CheckpointKind::Gate,
                    "deferred:no_host" => CheckpointKind::NoNode,
                    _ => continue,
                };
                let mut c = base(
                    kind,
                    format!("step:{}", st.id),
                    st.updated_at,
                    st.title.clone(),
                );
                c.step_id = Some(st.id.clone());
                c.reason = st.error.clone();
                out.push(c);
            }
        }
        sort(&mut out);
        Ok(out)
    }

    /// Everything waiting on a person, most urgent kind first.
    pub async fn needs_you(&self) -> Result<Vec<Checkpoint>> {
        let s = self.store();
        let mut out = self.engine_checkpoints().await?;

        // Sessions with a question.
        let q = SessionQuery {
            open_only: true,
            limit: 500,
            ..Default::default()
        };
        for x in s.sessions(&q).await? {
            if x.pending_question.trim().is_empty() && x.pending_question_id.is_empty() {
                continue;
            }
            let title = if x.title.is_empty() {
                x.cwd.clone()
            } else {
                x.title.clone()
            };
            let mut c = Checkpoint::new(
                CheckpointKind::Question,
                format!("session:{}", x.id),
                x.seen_at,
                title,
            );
            c.reason = x.pending_question.clone();
            c.repo = x.repo.clone();
            c.card_id = x.card_id.clone();
            c.run_id = x.run_id.clone();
            c.session_id = Some(x.id.clone());
            out.push(c);
        }

        // Pull requests of managed repositories.
        let managed: HashSet<String> = s
            .gh_managed_repos()
            .await?
            .into_iter()
            .map(|r| r.full_name)
            .collect();
        for p in s.gh_pulls(None, Some("open")).await? {
            if !managed.contains(&p.repo) {
                continue;
            }
            let conflict = p.mergeable == "conflict";
            let kind = if p.draft {
                CheckpointKind::PullDraft
            } else {
                match p.review_decision.as_str() {
                    "APPROVED"
                        if matches!(p.checks_state.as_str(), "success" | "none")
                            && p.mergeable == "clean" =>
                    {
                        CheckpointKind::PullReady
                    }
                    "REVIEW_REQUIRED" | "" => CheckpointKind::PullReview,
                    // Approved but conflicting: a person resolves it.
                    _ if conflict => CheckpointKind::PullReview,
                    _ => continue,
                }
            };
            let mut c = Checkpoint::new(
                kind,
                format!("pull:{}#{}", p.repo, p.number),
                p.updated_at,
                p.title.clone(),
            );
            c.repo = p.repo.clone();
            c.number = Some(p.number);
            c.url = p.html_url.clone();
            c.author = p.author.clone();
            c.conflict = conflict;
            c.reason = format!("#{} by {} · checks {}", p.number, p.author, p.checks_state);
            if conflict {
                c.reason.push_str(" · conflicts");
            }
            out.push(c);
        }

        // Issues from outside the team that nobody on it answered.
        let answered = self.answered_issues().await?;
        for i in s.gh_issues(None, Some("open")).await? {
            if !managed.contains(&i.repo)
                || !is_external(&i.author_association, &i.author)
                || answered.contains(&format!("{}#{}", i.repo, i.number))
            {
                continue;
            }
            let mut c = Checkpoint::new(
                CheckpointKind::IssueExternal,
                format!("issue:{}#{}", i.repo, i.number),
                i.created_at.max(i.updated_at),
                i.title.clone(),
            );
            c.repo = i.repo.clone();
            c.number = Some(i.number);
            c.url = i.html_url.clone();
            c.author = i.author.clone();
            c.card_id = i.card_id.clone().unwrap_or_default();
            c.reason = format!(
                "#{} by {}{} · {} comments",
                i.number,
                i.author,
                if i.author_association.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", i.author_association.to_lowercase())
                },
                i.comments
            );
            out.push(c);
        }

        // Deployments waiting on an environment's reviewers.
        out.extend(self.deploy_reviews().await?);

        // The GitHub share ran out and a person was asked to look.
        out.extend(self.quota_escalations().await?);

        // Cards held, with a lapsed claim, blocked or in review.
        let now = kls_store::now_ms();
        for card in self.cards().await? {
            if card.deleted || card.status == "done" {
                continue;
            }
            let kind = if crate::board::cards::is_held(&card) {
                CheckpointKind::CardHeld
            } else if crate::board::cards::claim(&card).lapsed(now) {
                CheckpointKind::ClaimLapsed
            } else if crate::board::cards::is_blocked(&card) {
                CheckpointKind::CardBlocked
            } else if card.status == "review" {
                CheckpointKind::CardReview
            } else {
                continue;
            };
            let mut c = Checkpoint::new(
                kind,
                format!("card:{}", card.id),
                card.updated_at,
                card.title.clone(),
            );
            c.repo = card.repo.clone();
            c.card_id = card.id.clone();
            c.reason = match kind {
                CheckpointKind::CardHeld => format!(
                    "{} · {}",
                    card.key,
                    crate::board::cards::checkpoint_reason(&card).unwrap_or_default()
                ),
                CheckpointKind::ClaimLapsed => format!(
                    "{} · {}",
                    card.key,
                    crate::board::cards::claim(&card).claimed_by
                ),
                _ => card.key.clone(),
            };
            c.url = card.pr_url.clone();
            out.push(c);
        }

        // Hosts: hand-offs a person runs, and belai updates (HM-14).
        for h in crate::hosts::needs(self).await? {
            let (kind, key) = if h.kind == "handoff" {
                (CheckpointKind::HostHandoff, format!("host:{}", h.action_id))
            } else {
                (
                    CheckpointKind::HostUpdate,
                    format!("host_update:{}", h.node_id),
                )
            };
            let mut c = Checkpoint::new(kind, key, h.at, format!("{} · {}", h.node_name, h.title));
            c.reason = if h.command.is_empty() {
                h.reason.clone()
            } else {
                h.command.clone()
            };
            c.url = h.link.clone();
            out.push(c);
        }

        // Joined nodes that are offline.
        out.extend(self.offline_nodes().await?);
        sort(&mut out);
        Ok(out)
    }

    /// `<repo>#<n>` of issues a team member commented on.
    async fn answered_issues(&self) -> Result<HashSet<String>> {
        let evs = self
            .store()
            .events_query(&kls_store::audit::EventQuery {
                prefixes: vec![crate::github::kinds::ISSUE_COMMENT.to_string()],
                limit: 5000,
                ..Default::default()
            })
            .await?;
        Ok(evs
            .into_iter()
            .filter(|e| {
                let v: serde_json::Value = serde_json::from_str(&e.data_json).unwrap_or_default();
                v.get("internal").and_then(|x| x.as_bool()) == Some(true)
                    || v.get("authorAssociation")
                        .and_then(|x| x.as_str())
                        .is_some_and(|a| matches!(a, "OWNER" | "MEMBER" | "COLLABORATOR"))
            })
            .map(|e| e.subject)
            .collect())
    }

    /// GitHub share escalations (`github.quota.escalated`) whose reset
    /// window is still open (one per route, the newest); without a reset
    /// time an escalation shows for [`QUOTA_ESCALATED_MAX_AGE_MS`].
    async fn quota_escalations(&self) -> Result<Vec<Checkpoint>> {
        let now = kls_store::now_ms();
        let evs = self
            .store()
            .events_query(&kls_store::audit::EventQuery {
                prefixes: vec![QUOTA_ESCALATED.to_string()],
                from_at: Some(now - QUOTA_ESCALATED_MAX_AGE_MS),
                limit: 200,
                ..Default::default()
            })
            .await?;
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for e in evs {
            if e.kind != QUOTA_ESCALATED || !seen.insert(e.subject.clone()) {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(&e.data_json).unwrap_or_default();
            let until = v.get("until").and_then(|x| x.as_i64()).unwrap_or(0);
            if until > 0 && until <= now {
                continue;
            }
            let mut c = Checkpoint::new(
                CheckpointKind::QuotaEscalated,
                format!("quota:{}", e.subject),
                e.at,
                "GitHub share used up".into(),
            );
            let path = v.get("path").and_then(|x| x.as_str()).unwrap_or("");
            c.reason = if until > 0 {
                let mins = (until - now + 59_999) / 60_000;
                format!("{path} · resets in {mins} min")
            } else {
                path.to_string()
            };
            c.url = QUOTA_USAGE_LINK.into();
            out.push(c);
        }
        Ok(out)
    }

    /// Deployment reviews still requested (the latest event per subject).
    async fn deploy_reviews(&self) -> Result<Vec<Checkpoint>> {
        let now = kls_store::now_ms();
        let evs = self
            .store()
            .events_query(&kls_store::audit::EventQuery {
                prefixes: vec![DEPLOY_REVIEW_PREFIX.to_string()],
                from_at: Some(now - DEPLOY_REVIEW_MAX_AGE_MS),
                limit: 2000,
                ..Default::default()
            })
            .await?;
        // Newest first: the first event per subject is its state.
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for e in evs {
            if !seen.insert(e.subject.clone()) {
                continue;
            }
            if !e.kind.ends_with(".requested") {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(&e.data_json).unwrap_or_default();
            let g = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let env = g("environment");
            let mut c = Checkpoint::new(
                CheckpointKind::DeployGate,
                format!("deploy:{}", e.subject),
                e.at,
                format!(
                    "Deploy to {}",
                    if env.is_empty() {
                        "an environment"
                    } else {
                        &env
                    }
                ),
            );
            c.repo = g("repo");
            c.url = g("url");
            c.author = g("requester");
            c.reason = format!("{} · approve on GitHub", g("repo"));
            out.push(c);
        }
        Ok(out)
    }

    /// Joined, unrevoked nodes that are offline (seen in the last 30 days).
    async fn offline_nodes(&self) -> Result<Vec<Checkpoint>> {
        let now = kls_store::now_ms();
        let online: Option<HashSet<String>> = match self.gateway() {
            Ok(g) => Some(g.online().await.into_iter().collect()),
            Err(_) => None,
        };
        // Without a node server, the latest online/offline event decides.
        let mut last: HashMap<String, (bool, i64)> = HashMap::new();
        if online.is_none() {
            let evs = self
                .store()
                .events_query(&kls_store::audit::EventQuery {
                    prefixes: vec![
                        crate::kinds::NODE_ONLINE.to_string(),
                        crate::kinds::NODE_OFFLINE.to_string(),
                    ],
                    limit: 2000,
                    ..Default::default()
                })
                .await?;
            for e in evs {
                last.entry(e.subject.clone())
                    .or_insert((e.kind == crate::kinds::NODE_ONLINE, e.at));
            }
        }
        let mut out = Vec::new();
        for n in self.store().nodes().await? {
            if n.revoked_at.is_some() {
                continue;
            }
            let (is_online, since) = match &online {
                Some(set) => (set.contains(&n.id), n.last_seen_at.unwrap_or(n.created_at)),
                None => match last.get(&n.id) {
                    Some((on, at)) => (*on, *at),
                    None => continue,
                },
            };
            let seen = n.last_seen_at.unwrap_or(since);
            if is_online || now - seen > NODE_OFFLINE_MAX_AGE_MS {
                continue;
            }
            let mut c = Checkpoint::new(
                CheckpointKind::NodeOffline,
                format!("node:{}", n.id),
                seen,
                format!("{} is offline", n.name),
            );
            c.node_id = n.id.clone();
            c.url = format!("keleustes://fleet/{}/overview", n.id);
            c.reason = if n.host.os.is_empty() {
                "not connected".into()
            } else {
                format!("{} · not connected", n.host.os)
            };
            out.push(c);
        }
        Ok(out)
    }

    /// Live sessions and pending approvals for the Cockpit.
    pub async fn counts(&self) -> Result<Counts> {
        let s = self.store();
        let q = SessionQuery {
            open_only: true,
            limit: 1000,
            ..Default::default()
        };
        let open = s.sessions(&q).await?;
        Ok(Counts {
            live_sessions: open
                .iter()
                .filter(|x| matches!(x.state.as_str(), "live" | "waiting"))
                .count(),
            open_sessions: open.len(),
            questions: open
                .iter()
                .filter(|x| {
                    !x.pending_question.trim().is_empty() || !x.pending_question_id.is_empty()
                })
                .count(),
            pending_approvals: s.approvals(Some("pending"), None, 1000).await?.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::CardDraft;
    use kls_store::github::{GhPull, GhRepo};
    use kls_store::sessions::Session;

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        (core, dir)
    }

    #[test]
    fn blocked_labels() {
        assert!(is_blocked_label("blocked"));
        assert!(is_blocked_label("SF:Blocked"));
        assert!(!is_blocked_label("unblocked-ish"));
    }

    #[tokio::test]
    async fn needs_you_spans_areas() {
        let (core, _d) = core().await;
        let s = core.store();
        // A card in review, one blocked, one plain.
        let review = core
            .create_card(
                CardDraft {
                    title: "check me".into(),
                    status: "review".into(),
                    ..Default::default()
                },
                "t",
            )
            .await
            .unwrap();
        let blocked = core
            .create_card(
                CardDraft {
                    title: "stuck".into(),
                    status: "doing".into(),
                    labels: vec!["blocked".into()],
                    ..Default::default()
                },
                "t",
            )
            .await
            .unwrap();
        core.create_card(
            CardDraft {
                title: "fine".into(),
                ..Default::default()
            },
            "t",
        )
        .await
        .unwrap();
        // A session with a question, one without.
        s.insert_node("n1", "box", &Default::default(), "s", "t")
            .await
            .unwrap();
        for (id, q) in [("s1", "which branch?"), ("s2", "")] {
            core.session_upsert(
                "n1",
                Session {
                    id: id.into(),
                    title: id.into(),
                    state: "waiting".into(),
                    pending_question_id: if q.is_empty() { "" } else { "q1" }.into(),
                    pending_question: q.into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        // Pull requests: one awaiting review, one ready, one draft, one unmanaged.
        s.gh_upsert_repo(&GhRepo {
            full_name: "acme/api".into(),
            owner: "acme".into(),
            name: "api".into(),
            ..Default::default()
        })
        .await
        .unwrap();
        s.gh_set_repo_managed("acme/api", true).await.unwrap();
        let pull = |repo: &str, n: i64, review: &str, draft: bool| GhPull {
            repo: repo.into(),
            number: n,
            title: format!("PR {n}"),
            state: "open".into(),
            draft,
            mergeable: "clean".into(),
            checks_state: "success".into(),
            review_decision: review.into(),
            ..Default::default()
        };
        for p in [
            pull("acme/api", 1, "REVIEW_REQUIRED", false),
            pull("acme/api", 2, "APPROVED", false),
            pull("acme/api", 3, "REVIEW_REQUIRED", true),
            pull("other/x", 4, "REVIEW_REQUIRED", false),
        ] {
            s.gh_upsert_pull(&p).await.unwrap();
        }

        let needs = core.needs_you().await.unwrap();
        let keys: Vec<(CheckpointKind, &str)> =
            needs.iter().map(|c| (c.kind, c.key.as_str())).collect();
        let blocked_key = format!("card:{}", blocked.id);
        let review_key = format!("card:{}", review.id);
        assert_eq!(
            keys,
            vec![
                (CheckpointKind::Question, "session:s1"),
                (CheckpointKind::PullReady, "pull:acme/api#2"),
                (CheckpointKind::PullReview, "pull:acme/api#1"),
                (CheckpointKind::PullDraft, "pull:acme/api#3"),
                (CheckpointKind::CardBlocked, blocked_key.as_str()),
                (CheckpointKind::CardReview, review_key.as_str()),
            ]
        );
        assert!(!needs[0].kind.decidable());
        assert_eq!(needs[0].reason, "which branch?");

        let n = core.counts().await.unwrap();
        assert_eq!(n.open_sessions, 2);
        assert_eq!(n.live_sessions, 2);
        assert_eq!(n.questions, 1);
        assert_eq!(n.pending_approvals, 0);
    }

    #[tokio::test]
    async fn held_and_lapsed_cards_wait_on_you() {
        let (core, _d) = core().await;
        let mk = |labels: Vec<&str>, meta: serde_json::Value| CardDraft {
            title: "T".into(),
            status: "backlog".into(),
            labels: labels.into_iter().map(String::from).collect(),
            meta_json: meta.to_string(),
            ..Default::default()
        };
        let held = core
            .create_card(
                mk(vec!["checkpoint:plan_approval"], serde_json::json!({})),
                "owner",
            )
            .await
            .unwrap();
        let lapsed = core
            .create_card(
                mk(
                    vec!["build"],
                    serde_json::json!({"belai": {"agent": {"claimedBy": "w", "leaseUntil": 1}}}),
                ),
                "owner",
            )
            .await
            .unwrap();
        let blocked = core
            .create_card(
                mk(vec![], serde_json::json!({"belai": {"list": "blocked"}})),
                "owner",
            )
            .await
            .unwrap();
        let blocked = core
            .move_card(&blocked.id, blocked.version, "todo", "owner")
            .await
            .unwrap();
        let n = core.needs_you().await.unwrap();
        let kind = |id: &str| n.iter().find(|c| c.card_id == id).map(|c| c.kind);
        assert_eq!(kind(&held.id), Some(CheckpointKind::CardHeld));
        assert_eq!(kind(&lapsed.id), Some(CheckpointKind::ClaimLapsed));
        assert_eq!(kind(&blocked.id), Some(CheckpointKind::CardBlocked));
        assert!(
            n.iter().position(|c| c.card_id == held.id)
                < n.iter().position(|c| c.card_id == blocked.id)
        );
    }

    #[tokio::test]
    async fn new_kinds_are_produced() {
        use kls_store::github::GhIssue;
        let (core, _d) = core().await;
        let s = core.store();
        s.gh_upsert_repo(&GhRepo {
            full_name: "acme/api".into(),
            owner: "acme".into(),
            name: "api".into(),
            ..Default::default()
        })
        .await
        .unwrap();
        s.gh_set_repo_managed("acme/api", true).await.unwrap();
        // Draft PR, conflicting approved PR.
        for (n, draft, review, mergeable) in [
            (10, true, "REVIEW_REQUIRED", "clean"),
            (11, false, "APPROVED", "conflict"),
        ] {
            s.gh_upsert_pull(&GhPull {
                repo: "acme/api".into(),
                number: n,
                title: format!("PR {n}"),
                state: "open".into(),
                draft,
                author: "ana".into(),
                mergeable: mergeable.into(),
                checks_state: "success".into(),
                review_decision: review.into(),
                ..Default::default()
            })
            .await
            .unwrap();
        }
        // Issues: external unanswered, external answered, internal, bot.
        for (n, assoc, author) in [
            (1, "CONTRIBUTOR", "mallory"),
            (2, "NONE", "eve"),
            (3, "MEMBER", "ana"),
            (4, "", "dependabot[bot]"),
        ] {
            s.gh_upsert_issue(&GhIssue {
                repo: "acme/api".into(),
                number: n,
                title: format!("Issue {n}"),
                state: "open".into(),
                author: author.into(),
                author_association: assoc.into(),
                ..Default::default()
            })
            .await
            .unwrap();
        }
        core.emit(
            crate::github::kinds::ISSUE_COMMENT,
            "acme/api#2",
            &serde_json::json!({"repo": "acme/api", "number": 2, "internal": true}),
        )
        .await
        .unwrap();
        // A deployment review requested, one requested then approved.
        for (subject, kind) in [
            ("acme/api:77:production", "requested"),
            ("acme/api:78:staging", "requested"),
            ("acme/api:78:staging", "approved"),
        ] {
            core.emit(
                &format!("{DEPLOY_REVIEW_PREFIX}{kind}"),
                subject,
                &serde_json::json!({"repo": "acme/api", "runId": "77",
                    "environment": subject.rsplit(':').next(), "requester": "ana",
                    "url": "https://github.com/acme/api/actions/runs/77"}),
            )
            .await
            .unwrap();
        }
        // A node that went offline, one that came back.
        for (id, last) in [
            ("n1", crate::kinds::NODE_OFFLINE),
            ("n2", crate::kinds::NODE_ONLINE),
        ] {
            s.insert_node(id, id, &Default::default(), "s", "t")
                .await
                .unwrap();
            core.emit(crate::kinds::NODE_ONLINE, id, &serde_json::json!({}))
                .await
                .unwrap();
            core.emit(last, id, &serde_json::json!({})).await.unwrap();
        }

        let n = core.needs_you().await.unwrap();
        let has = |k: CheckpointKind, key: &str| n.iter().any(|c| c.kind == k && c.key == key);
        assert!(has(CheckpointKind::PullDraft, "pull:acme/api#10"));
        let conflicted = n.iter().find(|c| c.key == "pull:acme/api#11").unwrap();
        assert!(conflicted.conflict && conflicted.kind == CheckpointKind::PullReview);
        assert_eq!(conflicted.author, "ana");
        assert!(has(CheckpointKind::IssueExternal, "issue:acme/api#1"));
        for k in ["issue:acme/api#2", "issue:acme/api#3", "issue:acme/api#4"] {
            assert!(!n.iter().any(|c| c.key == k), "{k}");
        }
        let deploy: Vec<&Checkpoint> = n
            .iter()
            .filter(|c| c.kind == CheckpointKind::DeployGate)
            .collect();
        assert_eq!(deploy.len(), 1);
        assert_eq!(deploy[0].key, "deploy:acme/api:77:production");
        assert!(!deploy[0].decidable(), "decided on GitHub");
        let off: Vec<&str> = n
            .iter()
            .filter(|c| c.kind == CheckpointKind::NodeOffline)
            .map(|c| c.node_id.as_str())
            .collect();
        assert_eq!(off, ["n1"]);
        // Most urgent first: the deploy gate first, the offline node last.
        assert_eq!(n.first().unwrap().kind, CheckpointKind::DeployGate);
        assert_eq!(n.last().unwrap().kind, CheckpointKind::NodeOffline);
    }

    #[test]
    fn deploy_steps_and_external_authors() {
        assert!(is_deploy_step("Deploy to prod", "{}"));
        assert!(is_deploy_step("Gate", r#"{"environment":"prod"}"#));
        assert!(!is_deploy_step("Check", "{}"));
        assert!(is_external("CONTRIBUTOR", "mallory"));
        assert!(is_external("", "eve"));
        assert!(!is_external("MEMBER", "ana"));
        assert!(!is_external("NONE", "renovate[bot]"));
        let mut c = Checkpoint::new(CheckpointKind::DeployGate, "step:s".into(), 0, "d".into());
        assert!(!c.decidable());
        c.step_id = Some("s".into());
        assert!(c.decidable());
    }

    #[tokio::test]
    async fn quota_escalations_show_until_the_window_resets() {
        let (core, _d) = core().await;
        let now = kls_store::now_ms();
        for (route, until) in [
            ("github.com|poller", now + 10 * 60_000),
            ("github.com|old", now - 60_000),
        ] {
            core.emit(
                QUOTA_ESCALATED,
                route,
                &serde_json::json!({"caller": "poller", "path": "/repos/acme/api/pulls",
                                    "until": until, "connection": "github.com"}),
            )
            .await
            .unwrap();
        }
        // A second escalation of the same route is one row.
        core.emit(
            QUOTA_ESCALATED,
            "github.com|poller",
            &serde_json::json!({"path": "/repos/acme/api/issues", "until": now + 5 * 60_000}),
        )
        .await
        .unwrap();
        let q: Vec<Checkpoint> = core
            .needs_you()
            .await
            .unwrap()
            .into_iter()
            .filter(|c| c.kind == CheckpointKind::QuotaEscalated)
            .collect();
        assert_eq!(q.len(), 1, "{q:?}");
        assert_eq!(q[0].key, "quota:github.com|poller");
        assert_eq!(q[0].url, QUOTA_USAGE_LINK);
        assert!(
            q[0].reason
                .starts_with("/repos/acme/api/issues · resets in "),
            "{}",
            q[0].reason
        );
        assert!(!q[0].decidable());
        assert_eq!(CheckpointKind::QuotaEscalated.key(), "quota_escalated");
    }
}
