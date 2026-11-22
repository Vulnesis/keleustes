//! The signed decision ledger and the weekly report. Owner: WP1.
//!
//! - **Decision ledger**: the ledger entries that record a decision
//!   (approvals, System One decisions, overrides, kill-switch and A2A
//!   changes), exported in the signed format of [`Core::audit_export_to`],
//!   so `verify_export` checks it.
//! - **Weekly report**: a summary of one ISO week of the ledger (Monday
//!   00:00 UTC): counts per kind, refusals, decisions, runs, budget alerts,
//!   vault changes, top actors and the chain check. The background loop
//!   writes last week's report once (`<data>/reports/weekly-<date>.md`) and
//!   emits the audited `audit.report`.

use crate::Core;
use anyhow::Result;
use kls_store::audit::{LedgerEntry, LedgerQuery};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

/// Ledger kinds that hold decisions.
pub const DECISION_KINDS: [&str; 4] = ["approval", "decision", "factory_control", "override"];
const WEEK_MS: i64 = 7 * 86_400_000;

pub mod kinds {
    /// A weekly report was written: `weekStart`, `path`, `entries`.
    pub const AUDIT_REPORT: &str = "audit.report";
}

pub mod keys {
    /// Start (ms) of the last week a report was written for.
    pub const WEEKLY_LAST: &str = "audit.weekly_last";
}

/// The ledger query of the decision ledger in `[from, to)`.
pub fn decision_query(from: Option<i64>, to: Option<i64>) -> LedgerQuery {
    LedgerQuery {
        kinds: DECISION_KINDS.iter().map(|k| k.to_string()).collect(),
        from_at: from,
        to_at: to,
        limit: 10_000,
        ..Default::default()
    }
}

/// Decisions, newest first.
pub async fn decisions(core: &Core, from: Option<i64>, limit: i64) -> Result<Vec<LedgerEntry>> {
    let mut q = decision_query(from, None);
    q.limit = limit.clamp(1, 10_000);
    core.audit_entries(&q).await
}

/// Writes the signed decision ledger since `from` to `path`. Audited as an
/// export. Returns the entries written.
pub async fn export_decisions(
    core: &Core,
    from: Option<i64>,
    path: &std::path::Path,
) -> Result<i64> {
    core.audit_sync().await?;
    core.audit_export_to(&decision_query(from, None), path)
        .await
}

/// Monday 00:00 UTC of the week holding `now`.
pub fn week_start(now: i64) -> i64 {
    crate::vault::period_start("week", now)
}

/// One week of the ledger, summed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeeklyReport {
    pub week_start: i64,
    pub week_end: i64,
    pub entries: i64,
    pub by_kind: Vec<(String, i64)>,
    pub refusals: i64,
    pub decisions: i64,
    pub runs_finished: i64,
    pub runs_failed: i64,
    pub budget_alerts: i64,
    pub vault_changes: i64,
    pub settings_changed: i64,
    pub top_actors: Vec<(String, i64)>,
    pub chain_ok: bool,
    pub generated_at: i64,
}

fn top(m: BTreeMap<String, i64>, n: usize) -> Vec<(String, i64)> {
    let mut v: Vec<_> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(n);
    v
}

/// Sums the week starting at `start`.
pub async fn weekly_report(core: &Core, start: i64) -> Result<WeeklyReport> {
    core.audit_sync().await?;
    let end = start + WEEK_MS;
    let mut q = LedgerQuery {
        from_at: Some(start),
        to_at: Some(end - 1),
        ascending: true,
        limit: 5000,
        ..Default::default()
    };
    let mut r = WeeklyReport {
        week_start: start,
        week_end: end,
        generated_at: kls_store::now_ms(),
        ..Default::default()
    };
    let (mut kinds, mut actors) = (BTreeMap::new(), BTreeMap::new());
    loop {
        let page = core.audit_entries(&q).await?;
        let Some(last) = page.last().map(|e| e.seq) else {
            break;
        };
        for e in &page {
            r.entries += 1;
            *kinds.entry(e.kind.clone()).or_insert(0) += 1;
            *actors.entry(e.actor.clone()).or_insert(0) += 1;
            let outcome = e.payload["outcome"].as_str().unwrap_or("");
            if matches!(e.kind.as_str(), "kill_switch" | "a2a_withheld")
                || outcome == "refused"
                || e.msg.ends_with(" refused")
            {
                r.refusals += 1;
            }
            if DECISION_KINDS.contains(&e.kind.as_str()) {
                r.decisions += 1;
            }
            if e.msg.starts_with("run.finished") {
                r.runs_finished += 1;
                if e.msg.contains("status=failed") {
                    r.runs_failed += 1;
                }
            }
            if e.msg.starts_with("budget.alert") || e.msg.starts_with("budget.blocked") {
                r.budget_alerts += 1;
            }
            if e.kind == "vault" {
                r.vault_changes += 1;
            }
            if e.kind == "setting" {
                r.settings_changed += 1;
            }
        }
        if page.len() < 5000 {
            break;
        }
        q.after_seq = Some(last);
    }
    r.by_kind = top(kinds, 50);
    r.top_actors = top(actors, 10);
    r.chain_ok = core
        .audit_verify()
        .await
        .map(|v| v.factory().is_none_or(|c| c.ok))
        .unwrap_or(false);
    Ok(r)
}

fn day(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| t.strftime("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// The report as Markdown.
pub fn render_markdown(r: &WeeklyReport) -> String {
    let mut s = format!(
        "# Keleustes weekly report\n\nWeek {} to {} (UTC). Chain {}.\n\n",
        day(r.week_start),
        day(r.week_end - 1),
        if r.chain_ok {
            "verified"
        } else {
            "NOT verified"
        }
    );
    s.push_str("| measure | count |\n|---|---|\n");
    for (k, v) in [
        ("Ledger entries", r.entries),
        ("Decisions", r.decisions),
        ("Refusals", r.refusals),
        ("Runs finished", r.runs_finished),
        ("Runs failed", r.runs_failed),
        ("Budget alerts and holds", r.budget_alerts),
        ("Vault changes", r.vault_changes),
        ("Settings changed", r.settings_changed),
    ] {
        s.push_str(&format!("| {k} | {v} |\n"));
    }
    s.push_str("\n## By kind\n\n");
    for (k, v) in &r.by_kind {
        s.push_str(&format!("- {k}: {v}\n"));
    }
    s.push_str("\n## Top actors\n\n");
    for (k, v) in &r.top_actors {
        s.push_str(&format!("- {k}: {v}\n"));
    }
    s
}

/// Writes the week's report to `<data>/reports/` and emits `audit.report`.
pub async fn save_weekly_report(
    core: &Core,
    start: i64,
) -> Result<(std::path::PathBuf, WeeklyReport)> {
    let r = weekly_report(core, start).await?;
    let dir = core.data_dir().join("reports");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("weekly-{}.md", day(start)));
    tokio::fs::write(&path, render_markdown(&r)).await?;
    core.emit(
        kinds::AUDIT_REPORT,
        &path.display().to_string(),
        &json!({"weekStart": start, "entries": r.entries, "refusals": r.refusals,
                "decisions": r.decisions, "chainOk": r.chain_ok, "by": "keleustes"}),
    )
    .await?;
    Ok((path, r))
}

/// Writes last week's report once it has ended. Returns its path when one
/// was written.
pub async fn weekly_tick(core: &Core, now: i64) -> Result<Option<std::path::PathBuf>> {
    let last_week = week_start(now) - WEEK_MS;
    let done: i64 = core
        .store()
        .setting(keys::WEEKLY_LAST)
        .await?
        .and_then(|s| s.parse().ok())
        .unwrap_or(i64::MIN);
    if done >= last_week {
        return Ok(None);
    }
    let (path, _) = save_weekly_report(core, last_week).await?;
    core.store()
        .set_setting(keys::WEEKLY_LAST, &last_week.to_string())
        .await?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        crate::vault::install_vault(
            &c,
            crate::vault::KeyBackend::File(dir.path().join("vault.key")),
        )
        .await
        .unwrap();
        (c, dir)
    }

    #[tokio::test]
    async fn decision_ledger_is_signed_and_filtered() {
        let (c, d) = core().await;
        crate::control::pause(&c, "drill").await.unwrap();
        crate::control::resume(&c).await.unwrap();
        c.store().set_setting("x.y", "1").await.unwrap();
        c.audit_sync().await.unwrap();
        let ds = decisions(&c, None, 100).await.unwrap();
        assert_eq!(
            ds.len(),
            4,
            "pause and resume: the switch and the mode each"
        );
        assert!(ds.iter().all(|e| DECISION_KINDS.contains(&e.kind.as_str())));
        let path = d.path().join("out/decisions.jsonl");
        let n = export_decisions(&c, None, &path).await.unwrap();
        assert_eq!(n, 4);
        let text = std::fs::read_to_string(&path).unwrap();
        let v = super::super::sign::verify_export(&text).unwrap();
        assert_eq!(v.entries, 4);
        let tampered = text.replacen("drill", "drilx", 1);
        assert!(super::super::sign::verify_export(&tampered).is_err());
    }

    #[tokio::test]
    async fn weekly_report_sums_and_is_written_once() {
        let (c, _d) = core().await;
        crate::control::pause(&c, "drill").await.unwrap();
        let _ = crate::control::check(&c, "dispatch", "r1", "test").await;
        crate::control::resume(&c).await.unwrap();
        let now = kls_store::now_ms();
        let r = weekly_report(&c, week_start(now)).await.unwrap();
        assert!(r.entries >= 3);
        assert_eq!(r.decisions, 4);
        assert!(r.refusals >= 1);
        assert!(r.chain_ok);
        let md = render_markdown(&r);
        assert!(md.contains("| Decisions | 4 |"));
        // Next week: last week's report is written once.
        let next = now + WEEK_MS;
        let p = weekly_tick(&c, next).await.unwrap().unwrap();
        assert!(
            std::fs::read_to_string(&p)
                .unwrap()
                .contains("weekly report")
        );
        assert!(weekly_tick(&c, next).await.unwrap().is_none());
        c.audit_sync().await.unwrap();
        assert!(
            c.audit_entries(&LedgerQuery::default())
                .await
                .unwrap()
                .iter()
                .any(|e| e.msg.contains("audit.report"))
        );
    }
}
