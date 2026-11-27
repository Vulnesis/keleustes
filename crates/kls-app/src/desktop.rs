//! Desktop integration: `keleustes://` deep links, the single-instance
//! hand-off, and OS notifications for what needs you. Owner: M6.
//!
//! A running window listens on a Unix socket in the data directory
//! (`<data>/keleustes.sock`; on Windows a loopback port written to
//! `<data>/keleustes.port`). `keleustes open <url>` hands its URL to it and
//! exits; the window navigates and comes to the front.

use anyhow::Result;
use kls_core::Core;
use kls_core::needs::CheckpointKind;
use kls_ui::Screen;
use std::collections::HashSet;
use std::path::Path;
use tokio::sync::mpsc;

/// Maps a deep link to a screen and an item to focus there.
///
/// Accepted: `keleustes://<screen>[/<id...>][?id=…]` (screen by name, e.g.
/// `keleustes://sessions/3f2a…`, `keleustes://repos/acme/api`), old web
/// paths with or without a host (`keleustes://resolve/factory-board?card=K-1`,
/// `https://www.vulnetix.com/resolve/belai-sessions?session=…`), and a bare
/// path (`/resolve/factory-audit`).
pub fn parse_link(url: &str) -> Option<(Screen, Option<String>)> {
    let url = url.trim();
    let rest = if let Some(r) = url.strip_prefix("keleustes://") {
        r
    } else if let Some(r) = url.strip_prefix("keleustes:") {
        r
    } else if let Some(r) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        // Drop the host.
        r.split_once('/').map(|(_, p)| p).unwrap_or("")
    } else {
        url
    };
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, q),
        None => (rest, ""),
    };
    let path = path.split('#').next().unwrap_or("").trim_matches('/');
    let param = |names: &[&str]| -> Option<String> {
        query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            names
                .contains(&k)
                .then(|| percent_decode(v))
                .filter(|v| !v.is_empty())
        })
    };
    let query_focus = || {
        param(&[
            "id", "card", "session", "run", "repo", "item", "section", "teleport", "seq",
        ])
        .or_else(|| param(&["node"]).map(|n| format!("node:{n}")))
    };
    // Old web paths first.
    if let Some(s) = Screen::from_web_path(path) {
        let tail = path
            .trim_start_matches("resolve/")
            .split_once('/')
            .map(|(_, t)| percent_decode(t))
            .filter(|t| !t.is_empty());
        return Some((s, query_focus().or(tail)));
    }
    let (head, tail) = match path.split_once('/') {
        Some((h, t)) => (h, Some(t)),
        None => (path, None),
    };
    let head = head.to_ascii_lowercase();
    let screen = match head.as_str() {
        "" | "open" => {
            // keleustes://open/<screen>/…
            return match tail {
                Some(t) => parse_link(&format!("keleustes://{t}{}", requery(query))),
                None => Some((Screen::Cockpit, None)),
            };
        }
        "repos" | "repo" | "repositories" => Screen::Repos,
        "session" => Screen::Sessions,
        "run" => Screen::Runs,
        "card" => Screen::Board,
        "workflow" => Screen::Workflows,
        "approvals" | "approval" | "needs" => Screen::Checkpoints,
        "conflict" => Screen::Conflicts,
        h => *Screen::ALL
            .iter()
            .find(|s| s.title().to_ascii_lowercase() == h)?,
    };
    let focus = query_focus().or_else(|| {
        tail.map(percent_decode)
            .map(|t| t.trim_matches('/').to_string())
            .filter(|t| !t.is_empty())
    });
    Some((screen, focus))
}

fn requery(q: &str) -> String {
    if q.is_empty() {
        String::new()
    } else {
        format!("?{q}")
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        match b[i] {
            b'%' if i + 2 < b.len() => match (hex(b[i + 1]), hex(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---- single instance -------------------------------------------------------

#[cfg(unix)]
/// `<data>/keleustes.sock`, or, when that is too long for a Unix socket
/// (about 108 bytes), a short path in the runtime directory named by a hash
/// of the data directory, so each data directory still has its own.
fn socket_path(data: &Path) -> std::path::PathBuf {
    let direct = data.join("keleustes.sock");
    if direct.as_os_str().len() < 100 {
        return direct;
    }
    use sha2::{Digest, Sha256};
    let canon = std::fs::canonicalize(data).unwrap_or_else(|_| data.to_path_buf());
    let h = hex::encode(&Sha256::digest(canon.as_os_str().as_encoded_bytes())[..8]);
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|d| d.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("keleustes-{h}.sock"))
}

/// Hands `url` (may be empty: just come to the front) to a running window
/// for this data directory. False when none is running.
pub fn hand_off(data: &Path, url: &str) -> bool {
    use std::io::Write;
    #[cfg(unix)]
    let conn = std::os::unix::net::UnixStream::connect(socket_path(data));
    #[cfg(not(unix))]
    let conn = std::fs::read_to_string(data.join("keleustes.port"))
        .ok()
        .and_then(|p| p.trim().parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::other("no port"))
        .and_then(|p| std::net::TcpStream::connect(("127.0.0.1", p)));
    match conn {
        Ok(mut c) => {
            let line = format!("{}\n", url.replace(['\n', '\r'], ""));
            c.write_all(line.as_bytes()).is_ok()
        }
        Err(_) => false,
    }
}

/// Listens for hand-offs and forwards each URL to `tx`. Call from within
/// the runtime.
pub fn listen(data: &Path, tx: mpsc::UnboundedSender<String>) -> Result<()> {
    use tokio::io::AsyncBufReadExt;
    #[cfg(unix)]
    {
        let path = socket_path(data);
        // A socket left by a crash: nobody answers it.
        let _ = std::fs::remove_file(&path);
        let l = tokio::net::UnixListener::bind(&path)?;
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut lines = tokio::io::BufReader::new(s).lines();
                    if let Ok(Some(l)) = lines.next_line().await {
                        let _ = tx.send(l.chars().take(4096).collect());
                    }
                });
            }
        });
    }
    #[cfg(not(unix))]
    {
        let l = std::net::TcpListener::bind("127.0.0.1:0")?;
        std::fs::write(
            data.join("keleustes.port"),
            l.local_addr()?.port().to_string(),
        )?;
        l.set_nonblocking(true)?;
        let l = tokio::net::TcpListener::from_std(l)?;
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut lines = tokio::io::BufReader::new(s).lines();
                    if let Ok(Some(l)) = lines.next_line().await {
                        let _ = tx.send(l.chars().take(4096).collect());
                    }
                });
            }
        });
    }
    Ok(())
}

/// Removes the socket when the window closes.
pub fn stop_listening(data: &Path) {
    #[cfg(unix)]
    let _ = std::fs::remove_file(socket_path(data));
    #[cfg(not(unix))]
    let _ = std::fs::remove_file(data.join("keleustes.port"));
}

// ---- notifications ---------------------------------------------------------

/// One OS notification and the link it opens.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub key: String,
    pub summary: String,
    pub body: String,
    pub link: String,
}

async fn enabled(core: &Core, key: &str) -> bool {
    use kls_core::audit::config::{self, keys};
    let read = |k: &'static str| async move {
        let f = config::field(k)?;
        let raw = core.store().setting(k).await.ok().flatten();
        Some(config::decode(f, raw.as_deref().unwrap_or(f.default)) == serde_json::json!(true))
    };
    // Mute holds everything but escalations (which are events, not these).
    !read(keys::NOTIFY_MUTED).await.unwrap_or(false)
        && read(keys::NOTIFY_DESKTOP).await.unwrap_or(true)
        && match key {
            "approval" => read(keys::NOTIFY_APPROVALS).await.unwrap_or(true),
            "question" => read(keys::NOTIFY_QUESTIONS).await.unwrap_or(true),
            "conflict" => read(keys::NOTIFY_CONFLICTS).await.unwrap_or(true),
            _ => true,
        }
}

/// Needs-you items worth a notification: approvals, gates, questions and
/// host steps only a person can do.
async fn needs_notes(core: &Core) -> Vec<Note> {
    let Ok(items) = core.needs_you().await else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|c| {
            let (summary, link, gate) = match c.kind {
                CheckpointKind::Approval | CheckpointKind::Gate => (
                    "Approval waiting",
                    format!("keleustes://checkpoints/{}", c.key),
                    "approval",
                ),
                CheckpointKind::Question => (
                    "A session asks",
                    match &c.session_id {
                        Some(s) => format!("keleustes://sessions/{s}"),
                        None => "keleustes://checkpoints".into(),
                    },
                    "question",
                ),
                CheckpointKind::HostHandoff => ("A host step needs you", c.url.clone(), "host"),
                _ => return None,
            };
            Some((
                gate,
                Note {
                    key: c.key.clone(),
                    summary: summary.into(),
                    body: if c.reason.is_empty() {
                        c.title.clone()
                    } else {
                        format!("{}: {}", c.title, c.reason)
                    },
                    link,
                },
            ))
        })
        .map(|(g, n)| Note {
            key: format!("{g}|{}", n.key),
            ..n
        })
        .collect()
}

/// The note for a pull request event that reports a merge conflict.
pub fn conflict_note(ev: &kls_store::Event) -> Option<Note> {
    if !ev.kind.starts_with("github.pull.") {
        return None;
    }
    let d: serde_json::Value = serde_json::from_str(&ev.data_json).ok()?;
    if d.get("mergeable").and_then(|m| m.as_str()) != Some("conflict")
        || d.get("state").and_then(|s| s.as_str()) == Some("closed")
    {
        return None;
    }
    let title = d.get("title").and_then(|t| t.as_str()).unwrap_or("");
    Some(Note {
        key: format!("conflict|{}", ev.subject),
        summary: "Merge conflict".into(),
        body: format!("{} {title}", ev.subject),
        link: format!("keleustes://conflicts/{}", ev.subject.replace('#', "%23")),
    })
}

/// The kind `notify.events` filters on (the web's `factory.run.*`): a run
/// that finished reads as `run.failed`, `run.succeeded` or `run.cancelled`,
/// one that started or paused as `run.started` or `run.paused`. Failed and
/// held runs pass the default level; the rest only `everything`.
fn notice_kind(ev: &kls_store::Event, d: &serde_json::Value) -> String {
    let s = |k: &str| d.get(k).and_then(|s| s.as_str()).unwrap_or("");
    match (ev.kind.as_str(), s("status"), s("action")) {
        ("run.finished", "failed", _) => "run.failed".into(),
        ("run.finished", "succeeded", _) => "run.succeeded".into(),
        ("run.finished", "cancelled", _) => "run.cancelled".into(),
        ("run.updated", _, "start") => "run.started".into(),
        ("run.updated", _, "pause") => "run.paused".into(),
        _ => ev.kind.clone(),
    }
}

/// Words for a failed run's machine reason (`stopped_line:<node>`,
/// `budget_exceeded`, `no_host_timeout`...).
pub fn failure_cause(reason: &str) -> String {
    let (head, tail) = reason.split_once(':').unwrap_or((reason, ""));
    let words = match head {
        "stopped_line" => "the line stopped after repeated failures at",
        "budget_exceeded" => "a budget stopped it",
        "no_host_timeout" => "no node was free in time",
        "dispatch_error" => "the node refused the work",
        "declined" => "a person declined at",
        "approval_expired" => "an approval expired",
        "timeout" => "a step ran out of time",
        "" => "it failed",
        other => other,
    };
    if tail.is_empty() {
        words.to_string()
    } else {
        format!("{words} {tail}")
    }
}

/// The OS notice for an event other than needs-you items, if any: notify
/// nodes, failed runs, sessions that ended badly, budget alerts and stops,
/// GitHub share escalations, egress reviews, failed key rotations, the kill
/// switch, lost sessions and refused keys.
pub fn event_note(ev: &kls_store::Event) -> Option<Note> {
    let d: serde_json::Value = serde_json::from_str(&ev.data_json).unwrap_or_default();
    let s = |k: &str| d.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let short: String = ev.subject.chars().take(8).collect();
    let (summary, body, link) = match ev.kind.as_str() {
        "run.notify" => (
            "Workflow".to_string(),
            s("message"),
            format!("keleustes://runs/{}", ev.subject),
        ),
        "run.finished" if s("status") == "failed" => (
            if s("reason").starts_with("stopped_line") {
                "Line stopped".into()
            } else {
                "Run failed".into()
            },
            format!("Run {short}: {}", failure_cause(&s("reason"))),
            format!("keleustes://runs/{}", ev.subject),
        ),
        "run.finished" if matches!(s("status").as_str(), "succeeded" | "cancelled") => (
            if s("status") == "succeeded" {
                "Run succeeded".into()
            } else {
                "Run cancelled".into()
            },
            format!("Run {short}"),
            format!("keleustes://runs/{}", ev.subject),
        ),
        "run.updated" if matches!(s("action").as_str(), "start" | "pause") => (
            if s("action") == "start" {
                "Run started".into()
            } else {
                "Run paused".into()
            },
            format!("Run {short}"),
            format!("keleustes://runs/{}", ev.subject),
        ),
        "run.held" => (
            "Run held".into(),
            match s("reason") {
                r if r.is_empty() => format!("Run {short}"),
                r => format!("Run {short}: {r}"),
            },
            format!("keleustes://runs/{}", ev.subject),
        ),
        // `session.finished` carries the outcome (`session.ended` fires with
        // it and stays quiet); an `info` outcome (completed, stopped) is not
        // worth a notice.
        "session.finished" if s("severity") != "info" => (
            match s("headline") {
                h if h.is_empty() => "Session finished".to_string(),
                h => h,
            },
            format!("Session {short}"),
            format!("keleustes://sessions/{}", ev.subject),
        ),
        "github.quota.escalated" => (
            "GitHub share used up".into(),
            s("path"),
            "keleustes://budgets/usage".into(),
        ),
        "trust.egress_reviewed" => (
            "Egress reviewed".into(),
            format!("{}: {}", s("host"), s("decision")),
            "keleustes://trust".into(),
        ),
        "vault.rotate_failed" => (
            "Key rotation failed".into(),
            format!("{} · {}", s("provider"), s("step")),
            "keleustes://vault".into(),
        ),
        // WP-J: a host asks for something. Fixed words and identifiers only.
        "attention.requested" => (
            "A host asks".into(),
            format!("{} ({})", s("kind").replace('_', " "), s("subject")),
            "keleustes://fleet".into(),
        ),
        "session.lost" => (
            "Session lost".into(),
            format!("Session {short}: its node went offline"),
            format!("keleustes://sessions/{}", ev.subject),
        ),
        "budget.alert" => (
            "Budget".into(),
            format!(
                "{} passed {}%",
                s("name"),
                d.get("pct").and_then(|p| p.as_i64()).unwrap_or(0)
            ),
            "keleustes://budgets".into(),
        ),
        "budget.blocked" => (
            "Budget stopped work".into(),
            s("reason"),
            "keleustes://budgets".into(),
        ),
        "factory.mode" => (
            match s("mode").as_str() {
                "pause" => "Factory paused",
                "stop" => "Factory stopped",
                _ => "Factory resumed",
            }
            .into(),
            s("reason"),
            "keleustes://settings/control".into(),
        ),
        "vault.secret_tested" if s("outcome") == "refused" => (
            "Key refused".into(),
            format!(
                "{} refused the key{}",
                s("provider"),
                d.get("status")
                    .and_then(|x| x.as_i64())
                    .map(|c| format!(" (HTTP {c})"))
                    .unwrap_or_default()
            ),
            "keleustes://vault".into(),
        ),
        _ => return None,
    };
    Some(Note {
        key: format!("event|{}|{}", ev.kind, ev.id),
        summary,
        body,
        link,
    })
}

/// Whether the `notify.events` level lets `ev`'s notice through.
async fn event_allowed(core: &Core, ev: &kls_store::Event) -> bool {
    let Ok(eff) = kls_core::audit::config::Effective::load(core).await else {
        return true;
    };
    let d: serde_json::Value = serde_json::from_str(&ev.data_json).unwrap_or_default();
    let kind = notice_kind(ev, &d);
    // Quiet hours (and Mute) hold everything but escalations.
    let quiet = kls_core::control::parse_windows(&eff.notify_quiet).unwrap_or_default();
    let (day, minute) = kls_core::control::local_slot(kls_store::now_ms(), &eff.schedules_tz);
    if (eff.notify_muted || kls_core::control::in_windows(&quiet, day, minute))
        && !kls_core::audit::config::notify_allows("escalations_only", &kind)
    {
        return false;
    }
    eff.notify_desktop && kls_core::audit::config::notify_allows(&eff.notify_events, &kind)
}

fn show(n: &Note, links: mpsc::UnboundedSender<String>) {
    let n = n.clone();
    // The notification server call and the wait for a click block.
    std::thread::spawn(move || {
        let mut note = notify_rust::Notification::new();
        note.appname("Keleustes")
            .summary(&n.summary)
            .body(&n.body)
            .icon("keleustes");
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            note.action("default", "Open");
            match note.show() {
                Ok(h) => h.wait_for_action(|a| {
                    if a == "default" {
                        let _ = links.send(n.link.clone());
                    }
                }),
                Err(e) => tracing::debug!("notification: {e}"),
            }
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            let _ = links;
            if let Err(e) = note.show() {
                tracing::debug!("notification: {e}");
            }
        }
    });
}

/// Watches the bus for what needs you and raises OS notifications; a click
/// sends the item's link to `links`. Items present at start are not
/// announced.
pub fn start_notifications(core: Core, links: mpsc::UnboundedSender<String>) {
    tokio::spawn(async move {
        let mut bus = core.subscribe();
        let mut seen: HashSet<String> = needs_notes(&core)
            .await
            .into_iter()
            .map(|n| n.key)
            .collect();
        loop {
            let ev = match bus.recv().await {
                Ok(ev) => Some(ev),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
                Err(_) => return,
            };
            if let Some(ev) = &ev {
                if let Some(n) = conflict_note(ev)
                    && seen.insert(n.key.clone())
                    && enabled(&core, "conflict").await
                {
                    show(&n, links.clone());
                }
                if let Some(n) = event_note(ev)
                    && event_allowed(&core, ev).await
                {
                    show(&n, links.clone());
                }
                // A re-notified approval shows its notice again.
                if ev.kind == "approval.renotified" {
                    seen.retain(|k| !(k.starts_with("approval|") && k.contains(&ev.subject)));
                }
                if !kls_core::needs::RELOAD_ON
                    .iter()
                    .any(|p| ev.kind.starts_with(p))
                {
                    continue;
                }
            }
            // Coalesce bursts.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            while bus.try_recv().is_ok() {}
            let now = needs_notes(&core).await;
            let keys: HashSet<String> = now.iter().map(|n| n.key.clone()).collect();
            for n in now {
                if seen.insert(n.key.clone()) {
                    let gate = n.key.split('|').next().unwrap_or("");
                    if enabled(&core, gate).await {
                        show(&n, links.clone());
                    }
                }
            }
            // Forget settled items so a new request with the same key notifies.
            seen.retain(|k| keys.contains(k) || k.starts_with("conflict|"));
        }
    });
}

/// Opens an https URL with the platform's opener; false when none ran.
pub fn open_in_browser(url: &str) -> bool {
    if !url.starts_with("https://") {
        return false;
    }
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn socket_path_stays_short_and_distinct() {
        let short = std::path::Path::new("/tmp/k");
        assert_eq!(socket_path(short), short.join("keleustes.sock"));
        let long_a = std::path::PathBuf::from(format!("/tmp/{}", "a".repeat(150)));
        let long_b = std::path::PathBuf::from(format!("/tmp/{}", "b".repeat(150)));
        let (pa, pb) = (socket_path(&long_a), socket_path(&long_b));
        assert!(pa.as_os_str().len() < 100, "{pa:?}");
        assert_ne!(pa, pb);
        assert_eq!(pa, socket_path(&long_a));
    }

    fn ev(kind: &str, subject: &str, data: &str) -> kls_store::Event {
        kls_store::Event {
            id: 1,
            at: 0,
            kind: kind.into(),
            subject: subject.into(),
            data_json: data.into(),
        }
    }

    #[test]
    fn event_notes() {
        let n = event_note(&ev(
            "run.finished",
            "run-12345678",
            r#"{"status":"failed","reason":"stopped_line:build"}"#,
        ))
        .unwrap();
        assert_eq!(n.summary, "Line stopped");
        assert!(n.body.contains("the line stopped") && n.body.contains("build"));
        assert_eq!(parse_link(&n.link).unwrap().0, Screen::Runs);
        let failed = r#"{"status":"failed","reason":"timeout"}"#;
        assert_eq!(
            event_note(&ev("run.finished", "r", failed))
                .unwrap()
                .summary,
            "Run failed"
        );
        // The rest of the web's factory.run.* notices.
        for (kind, data, summary) in [
            ("run.finished", r#"{"status":"succeeded"}"#, "Run succeeded"),
            ("run.finished", r#"{"status":"cancelled"}"#, "Run cancelled"),
            (
                "run.updated",
                r#"{"status":"running","action":"start"}"#,
                "Run started",
            ),
            (
                "run.updated",
                r#"{"status":"paused","action":"pause"}"#,
                "Run paused",
            ),
            ("run.held", r#"{"reason":"window:deploys"}"#, "Run held"),
        ] {
            let n = event_note(&ev(kind, "run-abcdefgh", data)).unwrap();
            assert_eq!(n.summary, summary, "{kind} {data}");
            assert_eq!(parse_link(&n.link).unwrap().0, Screen::Runs);
        }
        assert!(event_note(&ev("run.updated", "r", r#"{"status":"waiting_approval"}"#)).is_none());
        let n = event_note(&ev("run.notify", "r", r#"{"message":"deployed"}"#)).unwrap();
        assert_eq!(n.body, "deployed");
        let n = event_note(&ev("budget.alert", "b", r#"{"name":"Month","pct":80}"#)).unwrap();
        assert_eq!(n.body, "Month passed 80%");
        assert_eq!(parse_link(&n.link).unwrap().0, Screen::Budgets);
        let n = event_note(&ev(
            "factory.mode",
            "factory",
            r#"{"mode":"stop","reason":"x"}"#,
        ))
        .unwrap();
        assert_eq!(n.summary, "Factory stopped");
        assert!(
            event_note(&ev("session.ended", "s", "{}")).is_none(),
            "session.finished notifies"
        );
        let n = event_note(&ev(
            "session.finished",
            "sess-1234567",
            r#"{"severity":"warning","headline":"Session timed out","outcome":"timeout"}"#,
        ))
        .unwrap();
        assert_eq!(
            (n.summary.as_str(), n.body.as_str()),
            ("Session timed out", "Session sess-123")
        );
        assert_eq!(parse_link(&n.link).unwrap().0, Screen::Sessions);
        let info = r#"{"severity":"info","headline":"Session stopped","outcome":"stopped"}"#;
        assert!(
            event_note(&ev("session.finished", "s", info)).is_none(),
            "info is quiet"
        );
        let n = event_note(&ev(
            "github.quota.escalated",
            "github.com|poller",
            r#"{"path":"/repos/a/b"}"#,
        ))
        .unwrap();
        assert_eq!(
            parse_link(&n.link),
            Some((Screen::Budgets, Some("usage".into())))
        );
        let n = event_note(&ev(
            "trust.egress_reviewed",
            "api.example",
            r#"{"host":"api.example","decision":"allow"}"#,
        ))
        .unwrap();
        assert_eq!(parse_link(&n.link).unwrap().0, Screen::Trust);
        assert_eq!(n.body, "api.example: allow");
        let n = event_note(&ev(
            "vault.rotate_failed",
            "k1",
            r#"{"provider":"openai","step":"mint"}"#,
        ))
        .unwrap();
        assert_eq!(parse_link(&n.link).unwrap().0, Screen::Vault);
        assert!(event_note(&ev("vault.secret_tested", "k", r#"{"outcome":"accepted"}"#)).is_none());
        let n = event_note(&ev(
            "vault.secret_tested",
            "k",
            r#"{"outcome":"refused","provider":"openai","status":401}"#,
        ))
        .unwrap();
        assert!(n.body.contains("HTTP 401"));
        assert_eq!(failure_cause("budget_exceeded"), "a budget stopped it");
    }

    #[tokio::test]
    async fn notify_events_setting_filters_notes() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        let fin = ev("session.finished", "s", r#"{"severity":"warning"}"#);
        let alert = ev("budget.alert", "b", "{}");
        assert!(
            !event_allowed(&core, &fin).await,
            "default level skips finished sessions"
        );
        assert!(event_allowed(&core, &alert).await);
        // Held and failed runs pass the default level; started and
        // succeeded ones wait for `everything`.
        let held = ev("run.held", "r", r#"{"reason":"w"}"#);
        let failed = ev("run.finished", "r", r#"{"status":"failed"}"#);
        let ok = ev("run.finished", "r", r#"{"status":"succeeded"}"#);
        let started = ev("run.updated", "r", r#"{"action":"start"}"#);
        assert!(event_allowed(&core, &held).await && event_allowed(&core, &failed).await);
        assert!(!event_allowed(&core, &ok).await && !event_allowed(&core, &started).await);
        core.store()
            .set_setting("notify.events", "everything")
            .await
            .unwrap();
        assert!(event_allowed(&core, &fin).await);
        assert!(event_allowed(&core, &ok).await && event_allowed(&core, &started).await);
        core.store()
            .set_setting("notify.events", "escalations_only")
            .await
            .unwrap();
        assert!(!event_allowed(&core, &alert).await);
        // Quiet hours hold all but escalations.
        core.store()
            .set_setting("notify.events", "everything")
            .await
            .unwrap();
        core.store()
            .set_setting("notify.quiet", "daily 00:00-24:00")
            .await
            .unwrap();
        assert!(!event_allowed(&core, &fin).await);
        assert!(event_allowed(&core, &ev("budget.blocked", "b", "{}")).await);
        core.store()
            .set_setting("notify.desktop", "false")
            .await
            .unwrap();
        assert!(!event_allowed(&core, &ev("budget.blocked", "b", "{}")).await);
    }

    #[test]
    fn links() {
        assert_eq!(
            parse_link("keleustes://audit?node=n1"),
            Some((Screen::Audit, Some("node:n1".into())))
        );
        assert_eq!(
            parse_link("keleustes://sessions/3f2a"),
            Some((Screen::Sessions, Some("3f2a".into())))
        );
        assert_eq!(
            parse_link("keleustes://repos/acme/api"),
            Some((Screen::Repos, Some("acme/api".into())))
        );
        assert_eq!(
            parse_link("keleustes://repos/signin:refresh:browser"),
            Some((Screen::Repos, Some("signin:refresh:browser".into())))
        );
        assert_eq!(
            parse_link("keleustes://resolve/factory-board?card=K-1"),
            Some((Screen::Board, Some("K-1".into())))
        );
        assert_eq!(
            parse_link("https://www.vulnetix.com/resolve/belai-sessions?session=s%201"),
            Some((Screen::Sessions, Some("s 1".into())))
        );
        assert_eq!(
            parse_link("/resolve/factory-audit"),
            Some((Screen::Audit, None))
        );
        assert_eq!(
            parse_link("keleustes://open/settings?section=node"),
            Some((Screen::Settings, Some("node".into())))
        );
        assert_eq!(
            parse_link("keleustes://conflicts/acme/api%2312"),
            Some((Screen::Conflicts, Some("acme/api#12".into())))
        );
        // Host management: node, tab and item reach the Host view intact.
        assert_eq!(
            parse_link("keleustes://fleet/n1"),
            Some((Screen::Fleet, Some("n1".into())))
        );
        assert_eq!(
            parse_link("keleustes://fleet/n1/products"),
            Some((Screen::Fleet, Some("n1/products".into())))
        );
        assert_eq!(
            parse_link("keleustes://fleet/n1/products/cli"),
            Some((Screen::Fleet, Some("n1/products/cli".into())))
        );
        let (_, focus) = parse_link("keleustes://fleet/0192-ab/agents/acme%2Fapi").unwrap();
        let t = kls_ui::screens::host::parse_target(&focus.unwrap()).unwrap();
        assert_eq!(
            (t.node.as_str(), t.tab, t.item.as_deref()),
            (
                "0192-ab",
                kls_ui::screens::host::Tab::Agents,
                Some("acme/api")
            )
        );
        let (_, focus) = parse_link("keleustes://fleet/n1/nonsense").unwrap();
        assert_eq!(
            kls_ui::screens::host::parse_target(&focus.unwrap())
                .unwrap()
                .tab,
            kls_ui::screens::host::Tab::Overview
        );
        assert_eq!(parse_link("keleustes://"), Some((Screen::Cockpit, None)));
        assert_eq!(parse_link("keleustes://nowhere"), None);
        assert_eq!(percent_decode("a%2"), "a%2");
    }

    #[test]
    fn conflict_events() {
        let ev = |data: &str| kls_store::Event {
            id: 1,
            at: 0,
            kind: "github.pull.updated".into(),
            subject: "acme/api#12".into(),
            data_json: data.into(),
        };
        let n = conflict_note(&ev(
            r#"{"mergeable":"conflict","state":"open","title":"Fix"}"#,
        ))
        .unwrap();
        assert_eq!(n.key, "conflict|acme/api#12");
        assert_eq!(
            parse_link(&n.link),
            Some((Screen::Conflicts, Some("acme/api#12".into())))
        );
        assert!(conflict_note(&ev(r#"{"mergeable":"clean"}"#)).is_none());
    }

    #[tokio::test]
    async fn hand_off_reaches_listener() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!hand_off(dir.path(), "keleustes://audit"));
        let (tx, mut rx) = mpsc::unbounded_channel();
        listen(dir.path(), tx).unwrap();
        let d = dir.path().to_path_buf();
        let ok = tokio::task::spawn_blocking(move || hand_off(&d, "keleustes://audit"))
            .await
            .unwrap();
        assert!(ok);
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got, "keleustes://audit");
        stop_listening(dir.path());
        assert!(!hand_off(dir.path(), "x"));
    }
}
