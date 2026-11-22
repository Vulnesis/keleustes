//! The rate budget (factory.md 6.5, single tenant): people's calls may use
//! everything GitHub has left; factory writes that must not be skipped may
//! draw on the reserve; deferrable reads (the poller) stay inside the
//! factory share and leave the reserve alone. A secondary limit (403/429
//! with Retry-After) stops everything but people's calls until it passes.

use anyhow::Result;
use kls_core::Core;
use kls_core::github::{DEFAULT_FACTORY_SHARE, DEFAULT_RESERVE, settings};
use kls_store::Store;
use kls_store::github::{CallCount, Quota};
use serde::{Deserialize, Serialize};

/// Settings keys of the quota rules enforced here.
pub mod keys {
    /// What a deferrable read does once the factory share is used up:
    /// `defer`, `use_cached` or `escalate`.
    pub const ON_EXHAUSTED: &str = "forge.quota.on_exhausted";
    /// Writes automation may make per rolling hour (0: no cap).
    pub const PROACTIVE_WRITES: &str = "forge.quota.proactive_writes_per_hour";
}

/// Events of the quota rules.
pub mod kinds {
    /// A read waited for the reset (`on_exhausted = defer`).
    pub const QUOTA_DEFERRED: &str = "github.quota.deferred";
    /// A read was answered from the ETag cache (`use_cached`).
    pub const QUOTA_CACHED: &str = "github.quota.cached";
    /// A person was asked to look (`escalate`). Notifies like an escalation.
    pub const QUOTA_ESCALATED: &str = "github.quota.escalated";
    /// An automated write was held by the proactive write cap.
    pub const WRITE_CAPPED: &str = "github.quota.write_capped";
}

pub const DEFAULT_PROACTIVE_WRITES: i64 = 60;

/// `forge.quota.on_exhausted`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnExhausted {
    #[default]
    Defer,
    UseCached,
    Escalate,
}

impl OnExhausted {
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "use_cached" | "cached" => Self::UseCached,
            "escalate" => Self::Escalate,
            _ => Self::Defer,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Defer => "defer",
            Self::UseCached => "use_cached",
            Self::Escalate => "escalate",
        }
    }

    pub async fn load(store: &Store) -> Self {
        Self::parse(
            &store
                .setting(keys::ON_EXHAUSTED)
                .await
                .ok()
                .flatten()
                .unwrap_or_default(),
        )
    }
}

/// The proactive write cap per rolling hour (0: none).
pub async fn proactive_cap(store: &Store) -> i64 {
    store
        .setting(keys::PROACTIVE_WRITES)
        .await
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_PROACTIVE_WRITES)
        .max(0)
}

/// Automated forge writes that went out since `since`, oldest first (their
/// times), read from the audit ledger every write lands in.
pub async fn proactive_writes_since(core: &Core, since: i64) -> Result<Vec<i64>> {
    let q = kls_store::audit::LedgerQuery {
        kinds: vec!["forge".into()],
        from_at: Some(since),
        ascending: true,
        limit: 10_000,
        ..Default::default()
    };
    Ok(core
        .audit_entries(&q)
        .await?
        .into_iter()
        .filter(|e| {
            e.payload["outcome"] == "ok"
                && e.payload["detail"]["write"] == true
                && e.payload["caller"]
                    .as_str()
                    .is_some_and(crate::forge::guard::automated)
        })
        .map(|e| e.at)
        .collect())
}

/// Whether one more automated write fits under `cap` given the times of the
/// last hour's writes; when not, until when to wait.
pub fn write_cap_check(cap: i64, writes: &[i64], now: i64) -> Result<(), Refusal> {
    if cap <= 0 {
        return Ok(());
    }
    let recent: Vec<i64> = writes
        .iter()
        .copied()
        .filter(|t| *t > now - 3_600_000)
        .collect();
    if (recent.len() as i64) < cap {
        return Ok(());
    }
    let oldest = recent.iter().copied().min().unwrap_or(now);
    Err(Refusal::WriteCapped {
        until: oldest + 3_600_000,
    })
}

/// GitHub calls of one caller this hour. `workflow` is set for a
/// `factory:<workflow>` caller.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallerCalls {
    pub caller: String,
    pub workflow: String,
    pub calls: i64,
    pub not_modified: i64,
}

/// Folds call counts (any number of routes) into one row per caller, the
/// busiest first.
pub fn by_caller(rows: &[CallCount]) -> Vec<CallerCalls> {
    let mut out: Vec<CallerCalls> = Vec::new();
    for r in rows {
        match out.iter_mut().find(|c| c.caller == r.caller) {
            Some(c) => {
                c.calls += r.calls;
                c.not_modified += r.not_modified;
            }
            None => out.push(CallerCalls {
                caller: r.caller.clone(),
                workflow: r
                    .caller
                    .strip_prefix("factory:")
                    .unwrap_or_default()
                    .to_string(),
                calls: r.calls,
                not_modified: r.not_modified,
            }),
        }
    }
    out.sort_by(|a, b| b.calls.cmp(&a.calls).then(a.caller.cmp(&b.caller)));
    out
}

/// **The per-workflow GitHub call count** this hour, across every route the
/// factory has seen (people, the poller and each workflow apart).
pub async fn calls_this_hour(store: &Store, now: i64) -> Result<Vec<CallerCalls>> {
    let mut routes: Vec<String> = store
        .gh_quotas()
        .await?
        .into_iter()
        .map(|q| q.route)
        .collect();
    routes.push("local".into());
    routes.sort();
    routes.dedup();
    let mut rows = Vec::new();
    for r in routes {
        rows.extend(store.gh_calls_in_hour(&r, now).await?);
    }
    Ok(by_caller(&rows))
}

/// Only the workflow rows of [`calls_this_hour`].
pub async fn workflow_calls_this_hour(store: &Store, now: i64) -> Result<Vec<CallerCalls>> {
    Ok(calls_this_hour(store, now)
        .await?
        .into_iter()
        .filter(|c| !c.workflow.is_empty())
        .collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallClass {
    /// A person pressed a button.
    User,
    /// A factory write that must not be skipped.
    TimeCritical,
    /// A factory read that can wait (the poller).
    Deferrable,
}

impl CallClass {
    /// The class of a caller tag: `user`, `poller`, `factory:<workflow>`.
    pub fn of(caller: &str) -> Self {
        match caller {
            "user" => CallClass::User,
            "poller" => CallClass::Deferrable,
            _ => CallClass::TimeCritical,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    RateLimited {
        until: i64,
    },
    ShareExhausted {
        until: i64,
    },
    Secondary {
        until: i64,
    },
    /// The proactive write cap is reached for this rolling hour.
    WriteCapped {
        until: i64,
    },
}

impl Refusal {
    pub fn until(&self) -> i64 {
        match *self {
            Refusal::RateLimited { until }
            | Refusal::ShareExhausted { until }
            | Refusal::Secondary { until }
            | Refusal::WriteCapped { until } => until,
        }
    }
}

impl From<Refusal> for crate::GhError {
    fn from(r: Refusal) -> Self {
        match r {
            Refusal::ShareExhausted { .. } => crate::GhError::ShareExhausted,
            other => crate::GhError::RateLimited {
                until: other.until(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub factory_share_pct: i64,
    pub reserve_pct: i64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            factory_share_pct: DEFAULT_FACTORY_SHARE,
            reserve_pct: DEFAULT_RESERVE,
        }
    }
}

/// Calls that count toward the factory share this hour (not people's, not 304s).
pub fn factory_used(calls: &[CallCount]) -> i64 {
    calls
        .iter()
        .filter(|c| c.caller != "user")
        .map(|c| c.calls - c.not_modified)
        .sum()
}

impl Budget {
    /// Reads the shares from settings, clamped so share + reserve <= 100.
    pub async fn load(store: &Store) -> Result<Self> {
        let get = |v: Option<String>, d: i64| v.and_then(|s| s.trim().parse().ok()).unwrap_or(d);
        let share = get(
            store.setting(settings::FACTORY_SHARE).await?,
            DEFAULT_FACTORY_SHARE,
        )
        .clamp(0, 100);
        let reserve = get(store.setting(settings::RESERVE).await?, DEFAULT_RESERVE)
            .clamp(0, 90)
            .min(100 - share);
        Ok(Self {
            factory_share_pct: share,
            reserve_pct: reserve,
        })
    }

    /// Whether a call of `class` costing up to `cost` may go now.
    pub fn check(
        &self,
        class: CallClass,
        quota: Option<&Quota>,
        factory_used: i64,
        cost: i64,
        now: i64,
    ) -> Result<(), Refusal> {
        let Some(q) = quota else {
            return Ok(());
        };
        if q.secondary_until > now && class != CallClass::User {
            return Err(Refusal::Secondary {
                until: q.secondary_until,
            });
        }
        // Never observed, or the window has rolled over since.
        if q.limit <= 0 || (q.reset_at > 0 && q.reset_at <= now) {
            return Ok(());
        }
        match class {
            CallClass::User | CallClass::TimeCritical => {
                if q.remaining - cost < 0 {
                    return Err(Refusal::RateLimited { until: q.reset_at });
                }
            }
            CallClass::Deferrable => {
                let share = q.limit * self.factory_share_pct / 100;
                let reserve = q.limit * self.reserve_pct / 100;
                if factory_used + cost > share || q.remaining - cost < reserve {
                    return Err(Refusal::ShareExhausted { until: q.reset_at });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(limit: i64, remaining: i64) -> Quota {
        Quota {
            route: "local".into(),
            resource: "core".into(),
            limit,
            remaining,
            reset_at: 10_000,
            ..Default::default()
        }
    }

    #[test]
    fn classes_and_rules() {
        let b = Budget::default();
        assert_eq!(CallClass::of("user"), CallClass::User);
        assert_eq!(CallClass::of("poller"), CallClass::Deferrable);
        assert_eq!(CallClass::of("factory:patch"), CallClass::TimeCritical);
        // Unknown quota: go.
        assert!(b.check(CallClass::Deferrable, None, 0, 1, 0).is_ok());
        let full = q(5000, 4000);
        assert!(
            b.check(CallClass::Deferrable, Some(&full), 499, 1, 0)
                .is_ok()
        );
        assert_eq!(
            b.check(CallClass::Deferrable, Some(&full), 500, 1, 0),
            Err(Refusal::ShareExhausted { until: 10_000 })
        );
        // Remaining under the reserve (20% of 5000 = 1000) stops the poller only.
        let low = q(5000, 1000);
        assert!(b.check(CallClass::Deferrable, Some(&low), 0, 1, 0).is_err());
        assert!(
            b.check(CallClass::TimeCritical, Some(&low), 0, 1, 0)
                .is_ok()
        );
        let empty = q(5000, 0);
        assert_eq!(
            b.check(CallClass::User, Some(&empty), 0, 1, 0),
            Err(Refusal::RateLimited { until: 10_000 })
        );
        // After the reset the stale remaining no longer counts.
        assert!(b.check(CallClass::User, Some(&empty), 0, 1, 10_001).is_ok());
        let mut sec = q(5000, 4000);
        sec.secondary_until = 500;
        assert_eq!(
            b.check(CallClass::Deferrable, Some(&sec), 0, 1, 100),
            Err(Refusal::Secondary { until: 500 })
        );
        assert!(b.check(CallClass::User, Some(&sec), 0, 1, 100).is_ok());
        assert!(
            b.check(CallClass::Deferrable, Some(&sec), 0, 1, 600)
                .is_ok()
        );
    }

    #[test]
    fn factory_used_skips_people_and_304s() {
        let c = |caller: &str, calls, nm| CallCount {
            caller: caller.into(),
            calls,
            not_modified: nm,
        };
        assert_eq!(
            factory_used(&[c("user", 10, 0), c("poller", 8, 5), c("factory:x", 2, 0)]),
            5
        );
    }
}
