//! The poller: there are no webhooks, so managed repositories of every
//! enabled connection are read on a timer (conditional requests on
//! GitHub), inside the factory share of the rate budget. A refusal (share
//! used up, rate limited, secondary limit) ends that connection's pass and
//! the next one waits for the window to reset. A stopped factory polls
//! nothing (refused, emitted and audited).

use crate::client::Client;
use crate::forge::{Connection, guard};
use crate::select::Selector;
use crate::{GhError, events, sync};
use anyhow::Result;
use kls_core::Core;
use kls_core::github::{DEFAULT_POLL_INTERVAL_S, kinds, settings};
use kls_store::now_ms;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeSet;

pub const CALLER: &str = "poller";

/// Rate limits older than this are read again (`gh api rate_limit` is free).
pub const RATE_STALE_MS: i64 = 5 * 60_000;

/// A managed org's repository list is read again after this (new repositories).
pub const ORG_REFRESH_MS: i64 = 60 * 60_000;

/// One connection's part of a pass.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ConnPoll {
    pub connection: String,
    pub host: String,
    pub route: String,
    pub repos: usize,
    pub refused: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PollReport {
    pub at: i64,
    /// The github.com route (or the first connection's).
    pub route: String,
    pub repos: usize,
    pub pulls_changed: usize,
    pub issues_changed: usize,
    pub labels_created: usize,
    /// Cards written to their issues (outbound sync).
    pub cards_pushed: usize,
    /// Outbound pushes that failed this pass (retried later).
    pub cards_failed: usize,
    /// Push, CI, release and alert events emitted.
    pub events: usize,
    /// Why the pass stopped early, if it did.
    pub refused: Option<String>,
    /// Do not poll again before this (a rate limit), 0 when free to.
    pub resume_at: i64,
    pub errors: Vec<String>,
    pub connections: Vec<ConnPoll>,
}

fn refusal(e: &anyhow::Error) -> Option<(String, i64)> {
    match e.downcast_ref::<GhError>()? {
        GhError::RateLimited { until } => Some((format!("rate limited until {until}"), *until)),
        GhError::ShareExhausted => Some(("factory share used up this hour".into(), 0)),
        GhError::Unavailable(why) => Some((why.clone(), 0)),
        GhError::Stopped(why) => Some((format!("factory stopped: {why}"), 0)),
        GhError::Http { .. } | GhError::Unsupported(_) => None,
    }
}

/// The enabled connections, github.com first.
pub async fn connections(core: &Core) -> Result<Vec<Connection>> {
    Ok(core
        .store()
        .forge_connections()
        .await?
        .iter()
        .filter_map(Connection::from_row)
        .filter(|c| c.enabled)
        .collect())
}

/// One pass over the managed repositories of enabled orgs, per connection.
pub async fn poll_once(core: &Core, selector: &Selector) -> Result<PollReport> {
    let mut rep = PollReport {
        at: now_ms(),
        ..Default::default()
    };
    if let Err(e) = guard::check_kill(core, CALLER, "poll", "all", "").await {
        rep.refused = Some(format!("{e:#}"));
        rep.route = "stopped".into();
        return Ok(rep);
    }
    let conns = connections(core).await?;
    let orgs = core.store().gh_orgs().await?;
    let managed = core.store().gh_managed_repos().await?;
    // Repositories a board project mirrors (setting `github.issue_mirror`).
    let mirrored: BTreeSet<String> = core
        .issue_mirrors()
        .await?
        .into_iter()
        .map(|r| r.repo)
        .collect();
    for conn in &conns {
        let (status, gh) = selector.select_conn(conn).await;
        let mut part = ConnPoll {
            connection: conn.id.clone(),
            host: conn.host.clone(),
            route: status.label(),
            ..Default::default()
        };
        if conn.is_github_com() || rep.route.is_empty() {
            rep.route = status.label();
        }
        let keys: BTreeSet<String> = if conn.is_github_com() {
            BTreeSet::new()
        } else {
            core.store()
                .gh_repo_keys_of(&conn.id)
                .await?
                .into_iter()
                .collect()
        };
        let others: BTreeSet<String> = if conn.is_github_com() {
            let mut s = BTreeSet::new();
            for c in conns.iter().filter(|c| !c.is_github_com()) {
                s.extend(core.store().gh_repo_keys_of(&c.id).await?);
            }
            s
        } else {
            BTreeSet::new()
        };
        let mine: Vec<_> = managed
            .iter()
            .filter(|r| {
                if conn.is_github_com() {
                    !others.contains(&r.full_name) && crate::valid_repo(&r.full_name)
                } else {
                    keys.contains(&r.full_name)
                }
            })
            .collect();
        let Some(gh) = gh else {
            if !mine.is_empty() || conn.is_github_com() {
                part.refused = Some(status.label());
                if conn.is_github_com() || rep.refused.is_none() {
                    rep.refused = Some(status.label());
                }
            }
            rep.connections.push(part);
            continue;
        };
        let c = Client::with_conn(gh, core.clone(), conn.clone());
        let store = core.store();
        let stale = store
            .gh_quota(c.route_key(), "core")
            .await?
            .is_none_or(|q| rep.at - q.observed_at > RATE_STALE_MS);
        if stale && let Err(e) = c.rate_limit().await {
            rep.errors.push(format!("{} rate_limit: {e:#}", conn.host));
        }
        if let Err(e) = c.guard(CALLER, 1).await {
            let (why, until) = refusal(&e).unwrap_or((format!("{e:#}"), 0));
            part.refused = Some(why.clone());
            rep.refused = Some(why);
            rep.resume_at = rep.resume_at.max(until);
            rep.connections.push(part);
            continue;
        }
        // New repositories in managed orgs, at most hourly.
        for o in orgs.iter().filter(|o| o.managed && o.enabled) {
            let ours = if conn.is_github_com() {
                !o.login.contains('/')
                    || !conns
                        .iter()
                        .any(|x| !x.is_github_com() && o.login.starts_with(&format!("{}/", x.host)))
            } else {
                o.login.starts_with(&format!("{}/", conn.host))
            };
            if !ours || o.synced_at == 0 || rep.at - o.synced_at < ORG_REFRESH_MS {
                continue;
            }
            if let Err(e) = sync::refresh_org_repos(&c, o, CALLER).await
                && !crate::forge::api::absent(&e)
            {
                if let Some((why, until)) = refusal(&e) {
                    part.refused = Some(why.clone());
                    rep.refused = Some(why);
                    rep.resume_at = rep.resume_at.max(until);
                    break;
                }
                rep.errors.push(format!("{}: {e:#}", o.login));
            }
        }
        // github-risk: managed orgs' audit logs (hourly, GitHub only).
        for o in orgs
            .iter()
            .filter(|o| o.managed && o.enabled && o.kind == "Organization")
        {
            if part.refused.is_some() {
                break;
            }
            let ours = if conn.is_github_com() {
                !o.login.contains('/')
            } else {
                o.login.starts_with(&format!("{}/", conn.host))
            };
            if !ours {
                continue;
            }
            match events::poll_org_risk(&c, &o.login, CALLER).await {
                Ok(n) => rep.events += n,
                Err(e) => {
                    if let Some((why, until)) = refusal(&e) {
                        part.refused = Some(why.clone());
                        rep.refused = Some(why);
                        rep.resume_at = rep.resume_at.max(until);
                        break;
                    }
                    rep.errors.push(format!("{} audit-log: {e:#}", o.login));
                }
            }
        }
        'repos: for r in mine {
            if part.refused.is_some() {
                break;
            }
            if orgs.iter().any(|o| o.login == r.owner && !o.enabled) {
                continue;
            }
            rep.repos += 1;
            part.repos += 1;
            let steps: [(bool, u8); 4] = [
                (r.mirror_pulls, 0),
                (r.mirror_issues || mirrored.contains(&r.full_name), 1),
                (r.sync_labels, 2),
                (true, 3),
            ];
            for (on, step) in steps {
                if !on {
                    continue;
                }
                let res = match step {
                    0 => sync::sync_pulls(&c, &r.full_name, CALLER)
                        .await
                        .map(|n| rep.pulls_changed += n),
                    1 => match sync::sync_issues(&c, &r.full_name, CALLER).await {
                        Ok(n) => {
                            rep.issues_changed += n;
                            // Then the board side: take, push (with retry), conflicts.
                            sync::sync_cards(&c, &r.full_name, CALLER).await.map(|s| {
                                rep.cards_pushed += s.pushed;
                                rep.cards_failed += s.failed;
                            })
                        }
                        Err(e) => Err(e),
                    },
                    2 => sync::sync_labels(&c, &r.full_name, true, CALLER)
                        .await
                        .map(|l| rep.labels_created += l.created.len()),
                    _ => events::poll_repo(&c, r, CALLER)
                        .await
                        .map(|e| rep.events += e.total()),
                };
                if let Err(e) = res {
                    if let Some((why, until)) = refusal(&e) {
                        part.refused = Some(why.clone());
                        rep.refused = Some(why);
                        rep.resume_at = rep.resume_at.max(until);
                        break 'repos;
                    }
                    rep.errors.push(format!("{}: {e:#}", r.full_name));
                }
            }
        }
        if part.refused.is_some() {
            let _ = core.emit(kinds::QUOTA_LIMITED, c.route_key(), &rep).await;
        }
        rep.connections.push(part);
    }
    let changed = rep.pulls_changed
        + rep.issues_changed
        + rep.labels_created
        + rep.events
        + rep.cards_pushed
        + rep.cards_failed;
    if changed > 0 || rep.refused.is_some() || !rep.errors.is_empty() {
        guard::audit(
            core,
            CALLER,
            "poll",
            "all",
            "",
            if rep.refused.is_some() {
                "refused"
            } else if rep.errors.is_empty() {
                "ok"
            } else {
                "errors"
            },
            json!({ "repos": rep.repos, "pulls": rep.pulls_changed, "issues": rep.issues_changed,
                    "labels": rep.labels_created, "events": rep.events,
                    "cardsPushed": rep.cards_pushed, "cardsFailed": rep.cards_failed, "errors": rep.errors.len(),
                    "refused": rep.refused }),
        )
        .await;
    }
    let subject = selector
        .current()
        .map(|s| s.route.key())
        .unwrap_or_else(|| "local".into());
    core.emit(kinds::SYNCED, &subject, &rep).await?;
    Ok(rep)
}

/// Seconds between passes (setting `github.poll_interval_s`, at least 30).
pub async fn interval_ms(core: &Core) -> i64 {
    core.store()
        .setting(settings::POLL_INTERVAL_S)
        .await
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_POLL_INTERVAL_S)
        .max(30)
        * 1000
}

pub async fn enabled(core: &Core) -> bool {
    core.store()
        .setting(settings::POLL_ENABLED)
        .await
        .ok()
        .flatten()
        .is_none_or(|v| v.trim() != "off")
}

/// When the next pass may start after `rep`.
pub fn next_at(rep: &PollReport, interval: i64) -> i64 {
    let normal = rep.at + interval;
    if rep.resume_at > normal {
        rep.resume_at
    } else {
        normal
    }
}
