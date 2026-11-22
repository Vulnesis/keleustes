//! Host status frames (WP-B): the wire form to the domain form.

use kls_core::belai::status::{CrewRun, NodeStatus, Worker};
use kls_proto::v1 as pb;

pub fn of(s: pb::HostStatus) -> NodeStatus {
    NodeStatus {
        at: s.at_ms,
        running_sessions: s.running_sessions,
        workers: s
            .workers
            .into_iter()
            .map(|w| Worker {
                id: w.id,
                profile: w.profile,
                crew: w.crew,
                run: w.run,
                state: w.state,
                card_id: w.card_id,
                cwd: w.cwd,
                session_id: w.session_id,
                started_at: w.started_at_ms,
                beat_at: w.beat_at_ms,
                reason: w.reason,
                done: w.done,
                failed: w.failed,
                branch: w.branch,
                tokens: w.tokens,
                cost_micro_usd: w.cost_micro_usd,
                cost_live_micro_usd: w.cost_live_micro_usd,
            })
            .collect(),
        crew_runs: s
            .crew_runs
            .into_iter()
            .map(|r| CrewRun {
                id: r.id,
                crew: r.crew,
                state: r.state,
                reason: r.reason,
                scope_dirs: r.scope_dirs,
                members_json: r.members_json,
                max_workers: r.max_workers,
                stay: r.stay,
            })
            .collect(),
        plan_limits: plan_limits_of(&s.plan_limits),
    }
}

/// A host's or a session's plan windows (row 48), the wire form to the
/// domain form; `kls_core::belai::plan::clean` checks them.
pub fn plan_limits_of(rows: &[pb::PlanLimit]) -> Vec<kls_core::belai::plan::PlanLimit> {
    rows.iter()
        .map(|l| kls_core::belai::plan::PlanLimit {
            provider: l.provider.clone(),
            window: l.window.clone(),
            used_fraction: l.used_fraction,
            reset_at: l.reset_at_ms,
            pace: l.pace,
            runway_ms: l.runway_ms,
            source: l.source.clone(),
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_status_carries_plan_limits() {
        let st = of(pb::HostStatus {
            plan_limits: vec![pb::PlanLimit {
                provider: "anthropic".into(),
                window: "5h".into(),
                used_fraction: 0.5,
                reset_at_ms: 9,
                pace: 1.2,
                runway_ms: 60_000,
                source: "exact".into(),
            }],
            ..Default::default()
        });
        let l = &st.plan_limits[0];
        assert_eq!(
            (l.provider.as_str(), l.window.as_str()),
            ("anthropic", "5h")
        );
        assert_eq!((l.used_fraction, l.reset_at, l.runway_ms), (0.5, 9, 60_000));
        assert_eq!(l.source, "exact");
    }
}
