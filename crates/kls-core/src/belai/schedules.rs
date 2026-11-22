//! Central schedules (WP-G). Two halves:
//!
//! - **Host schedules.** A node's own scheduled workers (belai
//!   `internal/schedule`) sync with Keleustes like cards: versioned
//!   definitions both ways, the run record (last fired, status, next run)
//!   written by the host only. The owner may add, edit, pause or delete one
//!   here; the host still checks the profile, the directory and the cron, and
//!   fires it under its own kill-switch check.
//! - **Factory schedules.** A schedule trigger whose target is a node, or any
//!   online node with the profile and directory, fires a `worker` dispatch
//!   with `trigger.source = "schedule"` ([`Core::dispatch_scheduled`]).
//!
//! Every owner change and every scheduled dispatch is audited with its origin.

use crate::Core;
use crate::gateway::{DispatchSpec, Trigger};
use anyhow::{Result, bail};
use kls_store::audit::LedgerDraft;
use serde_json::json;

pub use kls_store::belai::HostSchedule;

pub mod kinds {
    /// A host schedule changed (owner edit or host push). Subject: node id.
    pub const SCHEDULE_CHANGED: &str = "schedule.changed";
}

/// Whether `cron` has the shape belai's scheduler takes (five fields or a
/// macro). The host parses it again and turns off one it cannot read.
pub fn cron_shape(cron: &str) -> bool {
    let c = cron.trim();
    if matches!(c, "@hourly" | "@daily" | "@weekly" | "@monthly") {
        return true;
    }
    let f: Vec<&str> = c.split_whitespace().collect();
    f.len() == 5
        && f.iter().all(|x| {
            x.len() <= 32
                && x.chars()
                    .all(|ch| ch.is_ascii_digit() || matches!(ch, '*' | '/' | ',' | '-'))
        })
}

fn ident(s: &str, n: usize) -> bool {
    !s.is_empty()
        && s.len() <= n
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

fn clean(s: &str, n: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(n).collect()
}

fn refuse(why: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(crate::sessions::Refusal::Invalid(why.into()))
}

impl Core {
    /// What a node pulls: its schedules changed after `since`.
    pub async fn schedule_pull(
        &self,
        node_id: &str,
        since: i64,
        limit: i64,
    ) -> Result<(Vec<HostSchedule>, i64, bool)> {
        self.store()
            .host_schedules_since(node_id, since, limit)
            .await
    }

    /// A node's pushed schedules. A definition based on an older version
    /// than the server's is a conflict (the server copy is returned); the run
    /// record is the host's and is always taken. Returns (accepted, conflicts).
    pub async fn schedule_push(
        &self,
        node_id: &str,
        items: Vec<HostSchedule>,
    ) -> Result<(Vec<HostSchedule>, Vec<HostSchedule>)> {
        let mut accepted = Vec::new();
        let mut conflicts = Vec::new();
        for mut it in items.into_iter().take(500) {
            if !ident(&it.id, 64) {
                continue;
            }
            it.node_id = node_id.into();
            it.profile = clean(&it.profile, 128);
            it.crew = clean(&it.crew, 128);
            it.cron = clean(&it.cron, 120);
            it.tz = clean(&it.tz, 64);
            it.cwd = clean(&it.cwd, 1024);
            it.last_status = clean(&it.last_status, 40);
            if it.meta_json.len() > 16 * 1024 {
                it.meta_json.clear();
            }
            let prev = self.store().host_schedule(node_id, &it.id).await?;
            match prev {
                Some(mut p) if p.version > it.version => {
                    // Stale definition: keep ours, take the host's run record.
                    p.last_fired = it.last_fired;
                    p.last_status = it.last_status.clone();
                    p.next_run = it.next_run;
                    let p = self.store().put_host_schedule(&p).await?;
                    conflicts.push(p);
                }
                Some(p) => {
                    it.version = p.version + 1;
                    it.created_at = p.created_at;
                    accepted.push(self.store().put_host_schedule(&it).await?);
                }
                None => {
                    it.version = it.version.max(1);
                    accepted.push(self.store().put_host_schedule(&it).await?);
                }
            }
        }
        if !accepted.is_empty() || !conflicts.is_empty() {
            self.emit(
                kinds::SCHEDULE_CHANGED,
                node_id,
                &json!({ "accepted": accepted.len(), "conflicts": conflicts.len(), "by": "host" }),
            )
            .await?;
        }
        Ok((accepted, conflicts))
    }

    /// The owner adds or edits a host schedule (`deleted` deletes it). The
    /// definition is checked against what the node advertises; the host
    /// checks it again before it fires.
    pub async fn save_host_schedule(&self, mut s: HostSchedule) -> Result<HostSchedule> {
        let Some(node) = self.store().node(&s.node_id).await? else {
            bail!("unknown node");
        };
        if node.revoked_at.is_some() {
            return Err(refuse("the node is revoked"));
        }
        if s.id.is_empty() {
            s.id = uuid::Uuid::now_v7().to_string();
        }
        if !ident(&s.id, 64) {
            return Err(refuse("the schedule id is not an identifier"));
        }
        if !s.deleted {
            if !cron_shape(&s.cron) {
                return Err(refuse(
                    "the cron is five fields or @hourly, @daily, @weekly, @monthly",
                ));
            }
            if s.profile.trim().is_empty() {
                return Err(refuse("a schedule names a worker profile"));
            }
            if super::advertises_agents(&node.host) && !super::offers_agent(&node.host, &s.profile)
            {
                return Err(refuse(format!(
                    "node {} has no profile {}",
                    node.name, s.profile
                )));
            }
            if !node.host.trusted_dirs.contains(&s.cwd) {
                return Err(refuse(format!(
                    "node {} does not offer {}",
                    node.name, s.cwd
                )));
            }
        }
        let prev = self.store().host_schedule(&s.node_id, &s.id).await?;
        let now = kls_store::now_ms();
        if let Some(p) = &prev {
            // The run record stays the host's.
            s.last_fired = p.last_fired;
            s.last_status = p.last_status.clone();
            s.next_run = p.next_run;
            s.created_at = p.created_at;
            s.version = p.version + 1;
        } else {
            s.version = 1;
            s.created_at = now;
        }
        s.updated_at = now;
        let saved = self.store().put_host_schedule(&s).await?;
        self.emit(
            kinds::SCHEDULE_CHANGED,
            &saved.node_id,
            &json!({ "id": saved.id, "by": "owner", "deleted": saved.deleted }),
        )
        .await?;
        self.audit_record(LedgerDraft {
            kind: "schedule".into(),
            actor: "owner".into(),
            actor_kind: "user".into(),
            msg: format!(
                "host schedule {} {}",
                saved.id,
                if saved.deleted { "deleted" } else { "saved" }
            ),
            subject: json!({ "node": saved.node_id, "schedule": saved.id }),
            payload: json!({ "profile": saved.profile, "cron": saved.cron, "cwd": saved.cwd,
                             "enabled": saved.enabled, "version": saved.version }),
            origin: "factory".into(),
            node_id: saved.node_id.clone(),
            ..Default::default()
        })
        .await?;
        Ok(saved)
    }

    /// Fires a factory schedule: a `worker` dispatch of `profile` in `cwd` on
    /// `target`, or on the least busy online node that has both. The gateway
    /// checks the kill switch and audits a refusal; the dispatch carries
    /// `trigger.source = "schedule"`. Returns (node id, dispatch id).
    pub async fn dispatch_scheduled(
        &self,
        target: Option<&str>,
        profile: &str,
        cwd: &str,
        schedule_name: &str,
    ) -> Result<(String, String)> {
        let gw = self.gateway()?;
        let online = gw.online().await;
        let statuses = self.node_statuses().await.unwrap_or_default();
        let load = |id: &str| {
            statuses
                .iter()
                .find(|(n, _, _)| n == id)
                .map(|(_, _, s)| s.running_sessions as usize + s.workers.len())
                .unwrap_or(0)
        };
        let mut pick: Option<(usize, String)> = None;
        for n in self.store().nodes().await? {
            let fits = n.revoked_at.is_none()
                && n.host.capabilities.remote_control
                && n.host.capabilities.takes_dispatch("worker")
                && super::offers_agent(&n.host, profile)
                && n.host.trusted_dirs.iter().any(|d| d == cwd);
            let named = target.is_none_or(|t| t == n.id || t == n.name);
            if !fits || !named || !online.contains(&n.id) {
                continue;
            }
            let l = load(&n.id);
            if pick
                .as_ref()
                .is_none_or(|(pl, pid)| (l, &n.id) < (*pl, pid))
            {
                pick = Some((l, n.id.clone()));
            }
        }
        let Some((_, node)) = pick else {
            bail!("no online node has profile {profile} and offers {cwd}");
        };
        let id = gw
            .dispatch(
                &node,
                DispatchSpec {
                    kind: "worker".into(),
                    cwd: cwd.into(),
                    agent: profile.into(),
                    trigger: Some(Trigger {
                        source: "schedule".into(),
                        workflow_name: clean(schedule_name, 80),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await?;
        Ok((node, id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_shapes() {
        for ok in ["@daily", "0 3 * * 1-5", "*/15 * * * *"] {
            assert!(cron_shape(ok), "{ok}");
        }
        for bad in ["", "0 3 * *", "rm -rf /", "@reboot", "0 3 * * MON; x"] {
            assert!(!cron_shape(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn push_conflicts_keep_the_owner_edit_and_take_the_run_record() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        let host = kls_store::HostInfo {
            trusted_dirs: vec!["/w".into()],
            agents: vec!["belai:scout".into()],
            ..Default::default()
        };
        c.store()
            .insert_node("n1", "box", &host, "serial", "t")
            .await
            .unwrap();
        let s = HostSchedule {
            node_id: "n1".into(),
            id: "s1".into(),
            profile: "belai:scout".into(),
            cron: "@daily".into(),
            cwd: "/w".into(),
            enabled: true,
            ..Default::default()
        };
        // The host creates it.
        let (acc, _) = c.schedule_push("n1", vec![s.clone()]).await.unwrap();
        assert_eq!(acc[0].version, 1);
        // The owner pauses it (version 2).
        let mut edit = acc[0].clone();
        edit.enabled = false;
        let saved = c.save_host_schedule(edit).await.unwrap();
        assert_eq!(saved.version, 2);
        // The host pushes a run against version 1: a conflict, run kept.
        let mut run = acc[0].clone();
        run.last_fired = 99;
        run.last_status = "worked".into();
        let (acc, conf) = c.schedule_push("n1", vec![run]).await.unwrap();
        assert!(acc.is_empty());
        assert_eq!(conf.len(), 1);
        assert!(!conf[0].enabled);
        assert_eq!(
            (conf[0].last_fired, conf[0].last_status.as_str()),
            (99, "worked")
        );
        // An owner edit the node cannot take is refused.
        let mut bad = saved.clone();
        bad.cwd = "/elsewhere".into();
        assert!(c.save_host_schedule(bad).await.is_err());
        let audited = c
            .audit_entries(&Default::default())
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "schedule")
            .count();
        assert_eq!(audited, 1);
    }
}
