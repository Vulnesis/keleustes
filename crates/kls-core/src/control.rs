//! The factory kill switch: the one check every automated action calls.
//!
//! **Rule for every agent and crate:** before any dispatch, job, trigger
//! fire, schedule run, workflow step, provision action or outbound node
//! frame, call [`check`] (or [`allowed`]) with the action, its target and its
//! origin. When the switch is on it refuses, emits `factory.refused` (folded
//! into the audit ledger) and returns [`Blocked`]. A2A paths call
//! [`check_a2a`] the same way.
//!
//! Modes:
//! - **pause**: nothing new starts (dispatches, jobs, triggers, schedules);
//!   steps already running may settle. Cron fires are held, not dropped.
//! - **stop**: as pause, and every open run is cancelled (its sessions are
//!   told to stop) and card claims are released, through the registered
//!   [`Stopper`]s (the engine registers one; see `kls-app`).
//!
//! Resume lifts either mode. Every change is a `factory.mode` event with the
//! actor, mode, reason and what a stop cancelled, so the ledger records it.

use crate::Core;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

pub mod kinds {
    /// The kill switch changed mode (pause / stop / resume).
    pub const FACTORY_MODE: &str = "factory.mode";
    /// An action was refused by the kill switch.
    pub const FACTORY_REFUSED: &str = "factory.refused";
    /// An A2A message was withheld by the blackout.
    pub const A2A_WITHHELD: &str = "a2a.withheld";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Running,
    Pause,
    Stop,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Running => "",
            Mode::Pause => "pause",
            Mode::Stop => "stop",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "pause" => Mode::Pause,
            "stop" => Mode::Stop,
            _ => Mode::Running,
        }
    }

    /// Whether anything new may start.
    pub fn running(self) -> bool {
        self == Mode::Running
    }
}

/// The switch as it stands.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ControlState {
    pub mode: Mode,
    pub reason: String,
    pub since: Option<i64>,
    pub a2a_blackout: bool,
    pub a2a_reason: String,
}

/// The refusal [`check`] returns while the switch is on.
#[derive(Clone, Debug, PartialEq)]
pub struct Blocked {
    pub mode: Mode,
    pub action: String,
    pub reason: String,
    /// Held by a maintenance window or schedule blackout (the switch is off).
    pub window: bool,
}

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.window {
            return write!(f, "held by the {}: {} waits", self.reason, self.action);
        }
        let m = if self.mode == Mode::Stop {
            "stopped"
        } else {
            "paused"
        };
        write!(f, "the factory is {m}: {} refused", self.action)?;
        if !self.reason.is_empty() {
            write!(f, " ({})", self.reason)?;
        }
        Ok(())
    }
}

impl std::error::Error for Blocked {}

/// Whether `e` is a kill-switch refusal.
pub fn is_blocked(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Blocked>().is_some()
}

pub async fn state(core: &Core) -> Result<ControlState> {
    let c = core.store().factory_control().await?;
    let mode = Mode::parse(&core.store().factory_mode().await?);
    Ok(ControlState {
        mode,
        reason: c.stopped_reason,
        since: c.stopped_since.filter(|_| c.stopped),
        a2a_blackout: c.a2a_blackout,
        a2a_reason: c.a2a_reason,
    })
}

/// **The kill-switch check.** `action` names what is about to happen
/// ("dispatch", "job", "trigger", "schedule", "step", "provision",
/// "node_frame", "grant"...), `target` what it acts on (run id, node id,
/// card key) and `origin` who or what caused it (workflow/run id, agent,
/// person, node). Refuses while paused or stopped, emitting an audited
/// `factory.refused` event.
pub async fn check(core: &Core, action: &str, target: &str, origin: &str) -> Result<()> {
    let st = state(core).await?;
    if st.mode.running() {
        // Inside a maintenance window nothing starts; inside a schedule
        // blackout triggers and schedules are held.
        if let Some(window) = window_hold(core, action, kls_store::now_ms()).await? {
            core.emit(
                kinds::FACTORY_REFUSED,
                target,
                &json!({
                    "action": action, "target": target, "origin": origin,
                    "mode": "window", "reason": window, "outcome": "held",
                }),
            )
            .await?;
            return Err(Blocked {
                mode: Mode::Pause,
                action: action.to_string(),
                reason: window,
                window: true,
            }
            .into());
        }
        return Ok(());
    }
    core.emit(
        kinds::FACTORY_REFUSED,
        target,
        &json!({
            "action": action, "target": target, "origin": origin,
            "mode": st.mode.as_str(), "reason": st.reason, "outcome": "refused",
        }),
    )
    .await?;
    Err(Blocked {
        mode: st.mode,
        action: action.to_string(),
        reason: st.reason,
        window: false,
    }
    .into())
}

// ---- windows -------------------------------------------------------------------

pub mod keys {
    /// No run starts inside it.
    pub const MAINTENANCE: &str = "hosts.maintenance";
    /// Schedule and event triggers are held inside it.
    pub const BLACKOUT: &str = "schedules.blackout";
    /// The zone windows are read in ("" = this machine's).
    pub const TZ: &str = "schedules.tz";
}

/// Actions a maintenance window holds.
const MAINTENANCE_HOLDS: [&str; 7] = [
    "dispatch",
    "job",
    "step",
    "trigger",
    "schedule",
    "provision",
    "node_frame",
];
/// Actions a schedule blackout holds.
const BLACKOUT_HOLDS: [&str; 2] = ["trigger", "schedule"];

/// One window: weekdays (Mon = 0) and minutes of the day `[start, end)`;
/// `end <= start` wraps past midnight into the next day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub days: [bool; 7],
    pub start: u32,
    pub end: u32,
}

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

fn day_index(s: &str) -> Option<usize> {
    let s = s.trim().to_ascii_lowercase();
    DAYS.iter().position(|d| s.starts_with(d))
}

fn minutes(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h <= 24 && m < 60 && h * 60 + m <= 1440).then_some(h * 60 + m)
}

/// Parses `Sat 02:00-06:00; Mon-Fri 23:00-01:00; daily 12:00-13:00`.
/// Empty is no window.
pub fn parse_windows(spec: &str) -> std::result::Result<Vec<Window>, String> {
    let mut out = Vec::new();
    for part in spec.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let (days, times) = part.rsplit_once(' ').ok_or_else(|| {
            format!("{part:?}: write days then a time range, e.g. Sat 02:00-06:00")
        })?;
        let mut set = [false; 7];
        for d in days.split(',').map(str::trim) {
            let dl = d.to_ascii_lowercase();
            if dl == "daily" || dl == "*" || dl == "every" {
                set = [true; 7];
            } else if let Some((a, b)) = d.split_once('-') {
                let (a, b) = (
                    day_index(a).ok_or_else(|| format!("{a:?} is not a day"))?,
                    day_index(b).ok_or_else(|| format!("{b:?} is not a day"))?,
                );
                let mut i = a;
                loop {
                    set[i] = true;
                    if i == b {
                        break;
                    }
                    i = (i + 1) % 7;
                }
            } else {
                set[day_index(d).ok_or_else(|| format!("{d:?} is not a day"))?] = true;
            }
        }
        let (a, b) = times
            .split_once('-')
            .ok_or_else(|| format!("{times:?}: write HH:MM-HH:MM"))?;
        let (start, end) = (
            minutes(a).ok_or_else(|| format!("{a:?} is not HH:MM"))?,
            minutes(b).ok_or_else(|| format!("{b:?} is not HH:MM"))?,
        );
        if start == end {
            return Err(format!("{times:?} is empty"));
        }
        out.push(Window {
            days: set,
            start,
            end,
        });
    }
    Ok(out)
}

/// Whether `windows` cover (weekday, minute of day).
pub fn in_windows(windows: &[Window], weekday: usize, minute: u32) -> bool {
    windows.iter().any(|w| {
        if w.start < w.end {
            w.days[weekday] && minute >= w.start && minute < w.end
        } else {
            // Wraps: the start day's evening, or the next day's morning.
            (w.days[weekday] && minute >= w.start) || (w.days[(weekday + 6) % 7] && minute < w.end)
        }
    })
}

/// (weekday Mon = 0, minute of day) of `now_ms` in zone `tz` ("" = system).
pub fn local_slot(now_ms: i64, tz: &str) -> (usize, u32) {
    let zone = if tz.trim().is_empty() {
        jiff::tz::TimeZone::system()
    } else {
        jiff::tz::TimeZone::get(tz.trim()).unwrap_or_else(|_| jiff::tz::TimeZone::system())
    };
    match jiff::Timestamp::from_millisecond(now_ms) {
        Ok(ts) => {
            let z = ts.to_zoned(zone);
            (
                z.weekday().to_monday_zero_offset() as usize,
                z.hour() as u32 * 60 + z.minute() as u32,
            )
        }
        Err(_) => (0, 0),
    }
}

/// The window that holds `action` at `now`, if any ("maintenance window
/// Sat 02:00-06:00").
pub async fn window_hold(core: &Core, action: &str, now: i64) -> Result<Option<String>> {
    let s = core.store();
    let tz = s.setting(keys::TZ).await?.unwrap_or_default();
    let slot = local_slot(now, &tz);
    for (key, holds, name) in [
        (
            keys::MAINTENANCE,
            &MAINTENANCE_HOLDS[..],
            "maintenance window",
        ),
        (keys::BLACKOUT, &BLACKOUT_HOLDS[..], "schedule blackout"),
    ] {
        if !holds.contains(&action) {
            continue;
        }
        let spec = s.setting(key).await?.unwrap_or_default();
        if let Ok(w) = parse_windows(&spec)
            && in_windows(&w, slot.0, slot.1)
        {
            return Ok(Some(format!("{name} {}", spec.trim())));
        }
    }
    Ok(None)
}

/// [`check`] as a bool (true: go ahead). A store error counts as blocked.
pub async fn allowed(core: &Core, action: &str, target: &str, origin: &str) -> bool {
    check(core, action, target, origin).await.is_ok()
}

/// **The A2A check**: refuses (and audits `a2a.withheld`) while the A2A
/// blackout or the kill switch is on. No bypass for built-ins or crews.
pub async fn check_a2a(core: &Core, from: &str, to: &str, origin: &str) -> Result<()> {
    let st = state(core).await?;
    if st.a2a_blackout || !st.mode.running() {
        let why = if st.a2a_blackout {
            "a2a_blackout"
        } else {
            "kill_switch"
        };
        core.emit(
            kinds::A2A_WITHHELD,
            to,
            &json!({"from": from, "to": to, "origin": origin, "reason": why, "outcome": "refused"}),
        )
        .await?;
        bail!("A2A withheld: {why}");
    }
    Ok(())
}

// ---- stop hooks ----------------------------------------------------------------

/// What a stop undid.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopReport {
    pub runs_cancelled: usize,
    pub sessions_stopped: usize,
    pub claims_released: usize,
    pub errors: Vec<String>,
}

impl StopReport {
    fn add(&mut self, o: StopReport) {
        self.runs_cancelled += o.runs_cancelled;
        self.sessions_stopped += o.sessions_stopped;
        self.claims_released += o.claims_released;
        self.errors.extend(o.errors);
    }
}

/// Something a stop must undo: open runs (the engine), card claims (the
/// board). Registered once per process with [`register_stopper`].
#[async_trait::async_trait]
pub trait Stopper: Send + Sync {
    async fn stop(&self, core: &Core, reason: &str) -> StopReport;
}

type Registry = Mutex<Vec<(PathBuf, Arc<dyn Stopper>)>>;

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Adds a stop hook for this core's data directory.
pub fn register_stopper(core: &Core, s: Arc<dyn Stopper>) {
    registry()
        .lock()
        .unwrap()
        .push((core.data_dir().to_path_buf(), s));
}

fn stoppers(core: &Core) -> Vec<Arc<dyn Stopper>> {
    registry()
        .lock()
        .unwrap()
        .iter()
        .filter(|(d, _)| d == core.data_dir())
        .map(|(_, s)| s.clone())
        .collect()
}

// ---- switching ------------------------------------------------------------------

/// Turns the switch to `mode` (pause or stop) or lifts it (running). `by`
/// is the actor. Pause -> stop escalates and runs the stop hooks; a mode
/// already set is an error.
pub async fn set_mode(core: &Core, mode: Mode, reason: &str, by: &str) -> Result<StopReport> {
    let before = state(core).await?;
    if before.mode == mode {
        bail!(
            "the factory is already {}",
            match mode {
                Mode::Running => "running",
                Mode::Pause => "paused",
                Mode::Stop => "stopped",
            }
        );
    }
    core.store().set_factory_mode(mode.as_str()).await?;
    match (before.mode.running(), mode.running()) {
        (true, false) => {
            if let Err(e) = core.set_factory_stopped_by(true, reason, by).await {
                core.store().set_factory_mode(before.mode.as_str()).await?;
                return Err(e);
            }
        }
        (false, true) => {
            core.set_factory_stopped_by(false, "", by).await?;
        }
        _ => {}
    }
    let mut report = StopReport::default();
    if mode == Mode::Stop {
        for s in stoppers(core) {
            report.add(s.stop(core, reason).await);
        }
    }
    core.emit(
        kinds::FACTORY_MODE,
        "factory",
        &json!({
            "mode": mode.as_str(), "from": before.mode.as_str(), "reason": reason,
            "by": by, "runsCancelled": report.runs_cancelled,
            "sessionsStopped": report.sessions_stopped,
            "claimsReleased": report.claims_released, "errors": report.errors,
        }),
    )
    .await?;
    Ok(report)
}

/// Turns the A2A blackout on or off; `by` is the actor the ledger records
/// (through the `factory.control` event). Errors when already in that state.
pub async fn set_a2a_blackout(core: &Core, on: bool, reason: &str, by: &str) -> Result<()> {
    core.set_a2a_blackout_by(on, reason, by).await?;
    Ok(())
}

pub async fn pause(core: &Core, reason: &str) -> Result<StopReport> {
    set_mode(core, Mode::Pause, reason, "owner").await
}

pub async fn stop(core: &Core, reason: &str) -> Result<StopReport> {
    set_mode(core, Mode::Stop, reason, "owner").await
}

pub async fn resume(core: &Core) -> Result<StopReport> {
    set_mode(core, Mode::Running, "", "owner").await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Core::open(dir.path(), None).await.unwrap(), dir)
    }

    struct Fake;
    #[async_trait::async_trait]
    impl Stopper for Fake {
        async fn stop(&self, _core: &Core, _reason: &str) -> StopReport {
            StopReport {
                runs_cancelled: 2,
                claims_released: 1,
                ..Default::default()
            }
        }
    }

    #[tokio::test]
    async fn check_passes_when_running_and_refuses_with_audit() {
        let (c, _d) = core().await;
        check(&c, "dispatch", "run-1", "workflow:w1").await.unwrap();
        pause(&c, "incident").await.unwrap();
        let e = check(&c, "dispatch", "run-1", "workflow:w1")
            .await
            .unwrap_err();
        assert!(is_blocked(&e));
        assert!(e.to_string().contains("paused"));
        assert!(!allowed(&c, "job", "n1", "owner").await);
        let evs = c.store().recent_events(20).await.unwrap();
        let r = evs
            .iter()
            .find(|e| e.kind == kinds::FACTORY_REFUSED)
            .unwrap();
        assert!(r.data_json.contains("workflow:w1"));
        c.audit_sync().await.unwrap();
        let ledger = c
            .audit_entries(&kls_store::audit::LedgerQuery::default())
            .await
            .unwrap();
        assert!(ledger.iter().any(|e| e.msg.contains("factory.refused")));
        assert!(ledger.iter().any(|e| e.msg.contains("factory.mode")));
    }

    #[tokio::test]
    async fn pause_then_stop_runs_hooks_and_resume_lifts() {
        let (c, _d) = core().await;
        register_stopper(&c, Arc::new(Fake));
        let r = pause(&c, "look").await.unwrap();
        assert_eq!(r.runs_cancelled, 0, "pause cancels nothing");
        assert_eq!(state(&c).await.unwrap().mode, Mode::Pause);
        assert!(c.factory_control().await.unwrap().stopped);
        assert!(pause(&c, "again").await.is_err());
        let r = stop(&c, "bad").await.unwrap();
        assert_eq!((r.runs_cancelled, r.claims_released), (2, 1));
        assert_eq!(state(&c).await.unwrap().mode, Mode::Stop);
        resume(&c).await.unwrap();
        let st = state(&c).await.unwrap();
        assert_eq!(st.mode, Mode::Running);
        assert!(!c.factory_control().await.unwrap().stopped);
        check(&c, "job", "x", "owner").await.unwrap();
        let modes: Vec<_> = c
            .store()
            .recent_events(50)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == kinds::FACTORY_MODE)
            .collect();
        assert_eq!(modes.len(), 3);
        assert!(
            modes
                .iter()
                .any(|e| e.data_json.contains("\"runsCancelled\":2"))
        );
    }

    #[test]
    fn windows_parse_and_wrap_midnight() {
        let w = parse_windows("Mon-Fri 23:00-01:00; Sat 02:00-06:00").unwrap();
        assert!(in_windows(&w, 0, 23 * 60 + 30)); // Mon 23:30
        assert!(in_windows(&w, 1, 30)); // Tue 00:30, from Mon
        assert!(!in_windows(&w, 0, 30)); // Mon 00:30: Sunday is not listed
        assert!(in_windows(&w, 5, 3 * 60));
        assert!(!in_windows(&w, 6, 3 * 60));
        assert!(in_windows(
            &parse_windows("daily 12:00-13:00").unwrap(),
            6,
            12 * 60
        ));
        assert!(parse_windows("").unwrap().is_empty());
        assert!(parse_windows("Funday 01:00-02:00").is_err());
        assert!(parse_windows("Mon 25:00-26:00").is_err());
        assert!(parse_windows("Mon 01:00-01:00").is_err());
        assert_eq!(local_slot(0, "UTC"), (3, 0)); // 1970-01-01 was a Thursday
    }

    #[tokio::test]
    async fn windows_hold_actions_and_are_audited() {
        let (c, _d) = core().await;
        c.store().set_setting(keys::TZ, "UTC").await.unwrap();
        c.store()
            .set_setting(keys::BLACKOUT, "daily 00:00-24:00")
            .await
            .unwrap();
        // Blackout holds schedules, not dispatches.
        assert!(check(&c, "dispatch", "r", "run:r").await.is_ok());
        let e = check(&c, "schedule", "s1", "workflow:w").await.unwrap_err();
        assert!(is_blocked(&e) && e.to_string().contains("schedule blackout"));
        c.store().set_setting(keys::BLACKOUT, "").await.unwrap();
        c.store()
            .set_setting(keys::MAINTENANCE, "daily 00:00-24:00")
            .await
            .unwrap();
        assert!(check(&c, "dispatch", "r", "run:r").await.is_err());
        assert!(
            check(&c, "grant", "x", "y").await.is_ok(),
            "a grant is not a start"
        );
        let held = c
            .store()
            .recent_events(20)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == kinds::FACTORY_REFUSED && e.data_json.contains("\"held\""))
            .count();
        assert_eq!(held, 2);
        // Settings validate windows.
        let doc = c.settings_doc().await.unwrap();
        let mut ch = std::collections::BTreeMap::new();
        ch.insert(keys::MAINTENANCE.to_string(), json!("Someday 01:00"));
        assert!(c.settings_save(doc.revision, ch).await.is_err());
    }

    #[tokio::test]
    async fn a2a_refused_under_blackout_and_kill_switch() {
        let (c, _d) = core().await;
        check_a2a(&c, "a", "b", "crew:x").await.unwrap();
        c.set_a2a_blackout(true, "quiet").await.unwrap();
        assert!(check_a2a(&c, "a", "b", "crew:x").await.is_err());
        c.set_a2a_blackout(false, "").await.unwrap();
        stop(&c, "").await.unwrap();
        assert!(check_a2a(&c, "a", "b", "builtin").await.is_err());
        let n = c
            .store()
            .recent_events(50)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.kind == kinds::A2A_WITHHELD)
            .count();
        assert_eq!(n, 2);
    }
}
