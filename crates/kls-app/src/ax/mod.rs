//! Agent usability (AX) of the `keleustes` CLI, after Orca's lane contract
//! (docs/orca-features.md, area G).
//!
//! With `--json` every verb prints exactly one envelope as the last stdout
//! line, on success and on failure; dispatch prints a handle line first and
//! transcript prints entry lines before it. The envelope is checked against
//! its own schema before it is printed, and the exit code is derived from
//! it in one place ([`exit_of`]), so a command never exits 0 with
//! `ok:false`. Errors carry a code from a closed enum ([`Code`]) and a
//! remediation; `next[]` holds literal follow-up commands.

use anyhow::Result;
use kls_core::Core;
use serde_json::{Value, json};
use std::io::Write;

pub mod analyze;
pub mod contract;
pub mod schema;
pub mod view;

/// The contract version (`v` in every line).
pub const V: u64 = 1;

/// The closed set of error codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    UsageError,
    Ambiguous,
    NotFound,
    InvalidState,
    NodeUnavailable,
    DispatchFailed,
    AgentFailed,
    Timeout,
    RefusedPaused,
    RefusedStopped,
    RefusedWindow,
    RefusedA2a,
    RefusedNode,
    RefusedPolicy,
    Failed,
    Internal,
}

/// One row of the error-code table.
pub struct CodeInfo {
    pub code: Code,
    pub name: &'static str,
    pub exit: i32,
    pub meaning: &'static str,
    pub remediation: &'static str,
}

pub const CODES: [CodeInfo; 16] = [
    CodeInfo {
        code: Code::UsageError,
        name: "usage_error",
        exit: 2,
        meaning: "the command line is malformed (unknown verb or flag, bad value)",
        remediation: "Run `keleustes contract` for every verb's flags, or `keleustes <verb> --help`.",
    },
    CodeInfo {
        code: Code::Ambiguous,
        name: "ambiguous",
        exit: 2,
        meaning: "a name matches several nodes",
        remediation: "Pass the node id from `keleustes nodes list --json` instead of its name.",
    },
    CodeInfo {
        code: Code::NotFound,
        name: "not_found",
        exit: 4,
        meaning: "no such session, dispatch, node, ask or request",
        remediation: "Check the id with `keleustes sessions list --json` or `keleustes nodes list --json`.",
    },
    CodeInfo {
        code: Code::InvalidState,
        name: "invalid_state",
        exit: 4,
        meaning: "the target exists but cannot take this now (ended session, ask no longer open, wrong option)",
        remediation: "Read the current state with `keleustes sessions show <id> --json` and follow its next[].",
    },
    CodeInfo {
        code: Code::NodeUnavailable,
        name: "node_unavailable",
        exit: 3,
        meaning: "the node is revoked or does not take this kind of work",
        remediation: "Pick another node from `keleustes nodes list --json`, or update belai on the host.",
    },
    CodeInfo {
        code: Code::DispatchFailed,
        name: "dispatch_failed",
        exit: 3,
        meaning: "the node could not start the work",
        remediation: "Read error.message, fix the cause and dispatch again.",
    },
    CodeInfo {
        code: Code::AgentFailed,
        name: "agent_failed",
        exit: 3,
        meaning: "the session ended in failure or was lost (session.agent is failed or lost)",
        remediation: "Read `keleustes sessions transcript <id>` and session.endReason, then dispatch again.",
    },
    CodeInfo {
        code: Code::Timeout,
        name: "timeout",
        exit: 5,
        meaning: "the deadline passed before the wait was satisfied; the work goes on",
        remediation: "Wait again with the next[] command or raise --timeout; stop it with `keleustes sessions command <id> stop`.",
    },
    CodeInfo {
        code: Code::RefusedPaused,
        name: "refused_paused",
        exit: 6,
        meaning: "the factory kill switch is on pause: nothing new starts",
        remediation: "Do not retry. The owner lifts the kill switch with `keleustes host resume`.",
    },
    CodeInfo {
        code: Code::RefusedStopped,
        name: "refused_stopped",
        exit: 6,
        meaning: "the factory kill switch is on stop: nothing new starts and open runs were cancelled",
        remediation: "Do not retry. The owner lifts the kill switch with `keleustes host resume`.",
    },
    CodeInfo {
        code: Code::RefusedWindow,
        name: "refused_window",
        exit: 6,
        meaning: "a maintenance window or schedule blackout holds new work",
        remediation: "Retry after the window ends (`keleustes config hosts.maintenance`).",
    },
    CodeInfo {
        code: Code::RefusedA2a,
        name: "refused_a2a",
        exit: 7,
        meaning: "the A2A blackout or the kill switch withheld an agent-to-agent message",
        remediation: "Do not retry. The owner turns the blackout off with `keleustes sessions a2a-blackout off`.",
    },
    CodeInfo {
        code: Code::RefusedNode,
        name: "refused_node",
        exit: 7,
        meaning: "the node refused the work under its own policy",
        remediation: "Read error.message; change the request or the host's policy.",
    },
    CodeInfo {
        code: Code::RefusedPolicy,
        name: "refused_policy",
        exit: 7,
        meaning: "a Keleustes policy refused it (an approval was declined, a key mode)",
        remediation: "Read error.message; the owner changes the policy.",
    },
    CodeInfo {
        code: Code::Failed,
        name: "failed",
        exit: 1,
        meaning: "a verb without structured errors failed (its own output says why)",
        remediation: "Read the verb's stdout and stderr above the envelope.",
    },
    CodeInfo {
        code: Code::Internal,
        name: "internal",
        exit: 1,
        meaning: "an unexpected error (database, I/O)",
        remediation: "Read error.message; report it if it repeats.",
    },
];

impl Code {
    pub fn info(self) -> &'static CodeInfo {
        CODES
            .iter()
            .find(|c| c.code == self)
            .expect("every code has a row")
    }
    pub fn name(self) -> &'static str {
        self.info().name
    }
    pub fn exit(self) -> i32 {
        self.info().exit
    }
    pub fn of_name(s: &str) -> Option<Code> {
        CODES.iter().find(|c| c.name == s).map(|c| c.code)
    }
}

/// A structured failure. Verbs raise it with [`fail`]; anything else is
/// classified by [`classify`].
#[derive(Clone, Debug)]
pub struct AxError {
    pub code: Code,
    pub message: String,
    /// Replaces the code's default remediation.
    pub remediation: Option<String>,
    pub data: Option<Value>,
    pub session: Option<Value>,
    pub next: Vec<String>,
}

impl std::fmt::Display for AxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AxError {}

impl AxError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        AxError {
            code,
            message: message.into(),
            remediation: None,
            data: None,
            session: None,
            next: Vec::new(),
        }
    }
    pub fn session(mut self, v: Value) -> Self {
        self.session = Some(v);
        self
    }
    pub fn next(mut self, next: Vec<String>) -> Self {
        self.next = next;
        self
    }
}

/// An `anyhow` error with a code.
pub fn fail(code: Code, message: impl Into<String>) -> anyhow::Error {
    AxError::new(code, message).into()
}

/// The code of any error: ours, a kill-switch refusal or a session refusal
/// (typed, never read from prose); everything else is `internal`.
pub fn classify(e: &anyhow::Error) -> AxError {
    if let Some(a) = e.downcast_ref::<AxError>() {
        return a.clone();
    }
    if let Some(b) = e.downcast_ref::<kls_core::control::Blocked>() {
        let code = if b.window {
            Code::RefusedWindow
        } else if b.mode == kls_core::control::Mode::Stop {
            Code::RefusedStopped
        } else {
            Code::RefusedPaused
        };
        return AxError::new(code, b.to_string());
    }
    if let Some(r) = e.downcast_ref::<kls_core::sessions::Refusal>() {
        use kls_core::sessions::Refusal;
        let code = match r {
            Refusal::NotFound(_) => Code::NotFound,
            Refusal::NotOwner | Refusal::Ended | Refusal::Invalid(_) => Code::InvalidState,
        };
        return AxError::new(code, r.to_string());
    }
    AxError::new(Code::Internal, format!("{e:#}"))
}

/// What a verb returns on success.
#[derive(Default, Debug)]
pub struct Done {
    pub data: Value,
    pub session: Option<Value>,
    pub next: Vec<String>,
    pub warnings: Vec<String>,
}

impl Done {
    pub fn data(data: Value) -> Self {
        Done {
            data,
            ..Default::default()
        }
    }
    pub fn with_session(mut self, v: Value) -> Self {
        self.next = view::next_for(&v);
        self.session = Some(v);
        self
    }
}

/// How the verb prints: human (the default, unchanged) or the contract.
pub struct Ctx {
    pub json: bool,
    /// The dotted verb, e.g. `sessions.dispatch` (the envelope's kind).
    pub kind: String,
    /// `keleustes` plus the global flags the follow-up commands need.
    pub prefix: String,
    /// Writes an audit entry whatever the outcome.
    pub mutating: bool,
}

impl Ctx {
    /// A human stdout line (suppressed under `--json`).
    pub fn out(&self, line: impl std::fmt::Display) {
        if !self.json {
            println!("{line}");
        }
    }
    /// A contract line before the envelope (handle, entry). Flushed at once
    /// so a caller that stops reading still holds it.
    pub fn stream(&self, v: Value) {
        if self.json {
            let mut o = std::io::stdout().lock();
            let _ = writeln!(o, "{v}");
            let _ = o.flush();
        }
    }
    /// A follow-up command with the global flags (and `--json` when on).
    pub fn cmd(&self, rest: &str) -> String {
        let mut s = format!("{} {rest}", self.prefix);
        if self.json && !s.contains(" --json") {
            s.push_str(" --json");
        }
        s
    }
    /// Rewrites `keleustes …` commands for this caller.
    pub fn fix_next(&self, next: Vec<String>) -> Vec<String> {
        next.into_iter()
            .map(|n| match n.strip_prefix("keleustes ") {
                Some(rest) => self.cmd(rest),
                None => n,
            })
            .collect()
    }
}

/// Shell-quotes a value for a literal next[] command.
pub fn q(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/@+-=,".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Builds the envelope for a verb's outcome.
pub fn envelope(ctx: &Ctx, res: &std::result::Result<Done, AxError>) -> Value {
    let mut env = json!({ "v": V, "kind": ctx.kind });
    let o = env.as_object_mut().unwrap();
    match res {
        Ok(d) => {
            o.insert("ok".into(), json!(true));
            if !d.data.is_null() {
                o.insert("data".into(), d.data.clone());
            }
            if let Some(s) = &d.session {
                o.insert("session".into(), s.clone());
            }
            o.insert("next".into(), json!(ctx.fix_next(d.next.clone())));
            o.insert("warnings".into(), json!(d.warnings));
        }
        Err(e) => {
            o.insert("ok".into(), json!(false));
            o.insert("code".into(), json!(e.code.name()));
            o.insert(
                "error".into(),
                json!({
                    "message": e.message,
                    "remediation": e.remediation.clone().unwrap_or_else(|| e.code.info().remediation.to_string()),
                }),
            );
            if let Some(d) = &e.data {
                o.insert("data".into(), d.clone());
            }
            if let Some(s) = &e.session {
                o.insert("session".into(), s.clone());
            }
            o.insert("next".into(), json!(ctx.fix_next(e.next.clone())));
            o.insert("warnings".into(), json!([]));
        }
    }
    env
}

/// The exit code of an envelope: 0 when ok, else its code's exit.
pub fn exit_of(env: &Value) -> i32 {
    if env.get("ok").and_then(Value::as_bool) == Some(true) {
        return 0;
    }
    env.get("code")
        .and_then(Value::as_str)
        .and_then(Code::of_name)
        .unwrap_or(Code::Internal)
        .exit()
}

/// The envelope after its schema check: one that fails it (a bug) becomes
/// an `internal` envelope, which always passes.
pub fn checked(ctx: &Ctx, env: Value) -> Value {
    match schema::validate(&contract::envelope_schema(), &env) {
        Ok(()) => env,
        Err(why) => envelope(
            ctx,
            &Err(AxError::new(
                Code::Internal,
                format!("envelope failed its schema ({why}); this is a contract bug"),
            )),
        ),
    }
}

/// Prints the outcome (the envelope under `--json`; the error, its
/// remediation and next[] on stderr otherwise), audits it, and returns the
/// exit code.
pub async fn finish(ctx: &Ctx, core: Option<&Core>, res: Result<Done>) -> i32 {
    let res = res.map_err(|e| classify(&e));
    let env = checked(ctx, envelope(ctx, &res));
    let code = exit_of(&env);
    if let Some(core) = core
        && (ctx.mutating || code != 0)
    {
        audit(core, ctx, &env).await;
    }
    if ctx.json {
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{env}");
        let _ = o.flush();
    } else {
        if let Err(e) = &res {
            eprintln!("error[{}]: {}", e.code.name(), e.message);
            eprintln!(
                "  fix: {}",
                e.remediation
                    .clone()
                    .unwrap_or_else(|| e.code.info().remediation.to_string())
            );
        }
        if let Some(w) = env.get("warnings").and_then(Value::as_array) {
            for w in w.iter().filter_map(Value::as_str) {
                eprintln!("warning: {w}");
            }
        }
        if let Some(n) = env.get("next").and_then(Value::as_array) {
            for n in n.iter().filter_map(Value::as_str) {
                eprintln!("next: {n}");
            }
        }
    }
    code
}

/// The CLI's own audit entry: the verb, its outcome and code, with the
/// surface `cli` as provenance. Best effort, retried like the gateway's.
pub async fn audit(core: &Core, ctx: &Ctx, env: &Value) {
    let ok = env["ok"].as_bool() == Some(true);
    let code = env.get("code").and_then(Value::as_str).unwrap_or("");
    let outcome = if ok {
        "ok"
    } else if code.starts_with("refused") {
        "refused"
    } else {
        "failed"
    };
    let session = env
        .pointer("/session/id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let dispatch = env
        .pointer("/session/dispatchId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let node = env
        .pointer("/session/nodeId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let draft = kls_store::audit::LedgerDraft {
        kind: "cli".into(),
        actor: "owner".into(),
        actor_kind: "user".into(),
        msg: format!(
            "keleustes {}: {outcome}{}",
            ctx.kind.replace('.', " "),
            if code.is_empty() {
                String::new()
            } else {
                format!(" ({code})")
            }
        ),
        subject: json!({ "verb": ctx.kind, "session": session, "dispatch": dispatch }),
        payload: json!({ "surface": "cli", "verb": ctx.kind, "outcome": outcome, "code": code,
                         "json": ctx.json, "exit": exit_of(env) }),
        origin: "factory".into(),
        node_id: node,
        session_id: session,
        ..Default::default()
    };
    for i in 1..=5u64 {
        match core.audit_record(draft.clone()).await {
            Ok(_) => break,
            Err(e) if i == 5 => tracing::warn!("audit of a CLI action: {e:#}"),
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(40 * i)).await,
        }
    }
}

/// Parses a duration: `500ms`, `30s`, `10m`, `1h`, `7d`, or bare seconds.
pub fn parse_duration(s: &str) -> std::result::Result<std::time::Duration, String> {
    let s = s.trim();
    let (num, mul) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1000)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3_600_000)
    } else if let Some(n) = s.strip_suffix('d') {
        (n, 86_400_000)
    } else {
        (s, 1000)
    };
    let n: u64 = num
        .trim()
        .parse()
        .map_err(|_| format!("{s:?} is not a duration (500ms, 30s, 10m, 1h or seconds)"))?;
    Ok(std::time::Duration::from_millis(n.saturating_mul(mul)))
}

/// The usage-error envelope for a command line clap rejected.
pub fn usage_envelope(kind: &str, message: &str) -> Value {
    let ctx = Ctx {
        json: true,
        kind: kind.into(),
        prefix: "keleustes".into(),
        mutating: false,
    };
    checked(
        &ctx,
        envelope(
            &ctx,
            &Err(AxError::new(Code::UsageError, message).next(vec!["keleustes contract".into()])),
        ),
    )
}

/// Exposed for the tests: `Result` of a verb as an envelope.
#[cfg(test)]
pub fn envelope_of(kind: &str, res: Result<Done>) -> Value {
    let ctx = Ctx {
        json: true,
        kind: kind.into(),
        prefix: "keleustes".into(),
        mutating: false,
    };
    checked(&ctx, envelope(&ctx, &res.map_err(|e| classify(&e))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_the_envelope() {
        let ok = envelope_of("x", Ok(Done::data(json!({"id": "1"}))));
        assert_eq!(exit_of(&ok), 0);
        assert!(ok.get("code").is_none());
        for c in &CODES {
            let env = envelope_of("x", Err(fail(c.code, "boom")));
            assert_eq!(env["ok"], false);
            assert_eq!(env["code"], c.name);
            assert_eq!(exit_of(&env), c.exit);
            assert_ne!(exit_of(&env), 0, "never 0 with ok:false");
            assert!(!env["error"]["remediation"].as_str().unwrap().is_empty());
        }
        let names: std::collections::BTreeSet<_> = CODES.iter().map(|c| c.name).collect();
        assert_eq!(names.len(), CODES.len());
    }

    #[test]
    fn kill_switch_refusals_are_typed() {
        let b = kls_core::control::Blocked {
            mode: kls_core::control::Mode::Stop,
            action: "dispatch".into(),
            reason: "incident".into(),
            window: false,
        };
        let e = classify(&anyhow::Error::new(b));
        assert_eq!(e.code, Code::RefusedStopped);
        assert_eq!(e.code.exit(), 6);
        let e = classify(&anyhow::Error::new(kls_core::sessions::Refusal::NotFound(
            "session",
        )));
        assert_eq!(e.code, Code::NotFound);
        assert_eq!(classify(&anyhow::anyhow!("disk")).code, Code::Internal);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms").unwrap().as_millis(), 500);
        assert_eq!(parse_duration("30s").unwrap().as_secs(), 30);
        assert_eq!(parse_duration("10m").unwrap().as_secs(), 600);
        assert_eq!(parse_duration("2").unwrap().as_secs(), 2);
        assert!(parse_duration("soon").is_err());
    }

    #[test]
    fn quoting() {
        assert_eq!(q("allow_once"), "allow_once");
        assert_eq!(q("it's"), r"'it'\''s'");
        assert_eq!(q(""), "''");
    }
}
