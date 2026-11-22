//! An in-memory stand-in for `gh` and GitHub, for tests (no network).
//!
//! [`FakeGh`] answers `gh api --include ...` from registered replies with
//! real ETags (a GET whose `If-None-Match` matches gets 304) and
//! rate-limit headers, records every call, and answers other `gh` commands
//! from registered outputs. [`FakeExec`] wraps it as a process runner for
//! route detection, and [`FakeGateway`] as a node gateway.

use crate::exec::Exec;
use crate::{CliKind, ForgeCli, GhOutput, Route};
use anyhow::Result;
use kls_core::{DispatchSpec, JobNeeds, JobOutcome, NodeGateway};
use serde_json::Value;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub method: String,
    pub path: String,
    pub if_none_match: Option<String>,
}

struct State {
    replies: HashMap<(String, String), Vec<Reply>>,
    cli: Vec<(Vec<String>, GhOutput)>,
    calls: Vec<Call>,
    limit: i64,
    remaining: i64,
    reset_secs: i64,
    tea_hosts: Vec<String>,
}

pub struct FakeGh {
    route: Route,
    cli: CliKind,
    state: Mutex<State>,
}

fn etag_of(body: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut h);
    format!("W/\"{:016x}\"", h.finish())
}

impl FakeGh {
    pub fn new() -> Arc<Self> {
        Self::with_route(Route::Local)
    }

    pub fn with_route(route: Route) -> Arc<Self> {
        Self::with_cli(route, CliKind::Gh)
    }

    /// A fake `glab` (GitLab headers on stdout) or `tea` (status line and
    /// headers on stderr, no rate headers, no ETags).
    pub fn with_cli(route: Route, cli: CliKind) -> Arc<Self> {
        Arc::new(Self {
            route,
            cli,
            state: Mutex::new(State {
                replies: HashMap::new(),
                cli: Vec::new(),
                calls: Vec::new(),
                limit: 5000,
                remaining: 5000,
                reset_secs: (kls_store::now_ms() / 1000) + 3600,
                tea_hosts: Vec::new(),
            }),
        })
    }

    /// Hosts `tea login list` reports a login for (through [`FakeExec`]).
    pub fn set_tea_hosts(&self, hosts: &[&str]) {
        self.state.lock().unwrap().tea_hosts = hosts.iter().map(|h| h.to_string()).collect();
    }

    pub fn tea_hosts(&self) -> Vec<String> {
        self.state.lock().unwrap().tea_hosts.clone()
    }

    /// Answers `method path` with `status` and a JSON body (replaces earlier replies).
    pub fn on(&self, method: &str, path: &str, status: u16, body: Value) {
        self.on_reply(
            method,
            path,
            Reply {
                status,
                body: if body.is_null() {
                    String::new()
                } else {
                    body.to_string()
                },
                headers: Vec::new(),
            },
        );
    }

    pub fn on_reply(&self, method: &str, path: &str, reply: Reply) {
        self.state
            .lock()
            .unwrap()
            .replies
            .insert((method.into(), path.into()), vec![reply]);
    }

    /// Queues replies answered in turn (the last one repeats).
    pub fn on_seq(&self, method: &str, path: &str, replies: Vec<Reply>) {
        self.state
            .lock()
            .unwrap()
            .replies
            .insert((method.into(), path.into()), replies);
    }

    /// Answers a non-API command whose args start with `prefix`.
    pub fn on_cli(&self, prefix: &[&str], out: GhOutput) {
        self.state
            .lock()
            .unwrap()
            .cli
            .push((crate::args(prefix.iter().copied()), out));
    }

    pub fn set_rate(&self, limit: i64, remaining: i64, reset_secs: i64) {
        let mut s = self.state.lock().unwrap();
        s.limit = limit;
        s.remaining = remaining;
        s.reset_secs = reset_secs;
    }

    pub fn remaining(&self) -> i64 {
        self.state.lock().unwrap().remaining
    }

    pub fn calls(&self) -> Vec<Call> {
        self.state.lock().unwrap().calls.clone()
    }

    pub fn clear_calls(&self) {
        self.state.lock().unwrap().calls.clear();
    }

    /// How many API calls hit `method path`.
    pub fn count(&self, method: &str, path: &str) -> usize {
        self.calls()
            .iter()
            .filter(|c| c.method == method && c.path == path)
            .count()
    }

    fn api(&self, args: &[String], stdin: Option<&str>) -> GhOutput {
        let mut method = "GET".to_string();
        let mut inm = None;
        let mut path = String::new();
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--method" | "-X" => {
                    method = args.get(i + 1).cloned().unwrap_or_default();
                    i += 1;
                }
                "-H" | "--header" => {
                    if let Some(h) = args.get(i + 1)
                        && let Some(v) = h.strip_prefix("If-None-Match: ")
                    {
                        inm = Some(v.to_string());
                    }
                    i += 1;
                }
                "--input" | "--hostname" | "-l" | "-d" => i += 1,
                "--include" | "-i" => {}
                a => path = a.to_string(),
            }
            i += 1;
        }
        let mut s = self.state.lock().unwrap();
        s.calls.push(Call {
            args: args.to_vec(),
            stdin: stdin.map(String::from),
            method: method.clone(),
            path: path.clone(),
            if_none_match: inm.clone(),
        });
        if path == "rate_limit" && self.cli == CliKind::Gh {
            let body = serde_json::json!({ "resources": {
                "core": { "limit": s.limit, "remaining": s.remaining, "used": s.limit - s.remaining, "reset": s.reset_secs },
                "graphql": { "limit": 5000, "remaining": 5000, "used": 0, "reset": s.reset_secs },
            }});
            return GhOutput {
                stdout: body.to_string(),
                stderr: String::new(),
                exit: 0,
            };
        }
        let reply = match s.replies.get_mut(&(method.clone(), path.clone())) {
            Some(q) if q.len() > 1 => q.remove(0),
            Some(q) if !q.is_empty() => q[0].clone(),
            _ => Reply {
                status: 404,
                body: r#"{"message":"Not Found"}"#.into(),
                headers: Vec::new(),
            },
        };
        let etag = etag_of(&reply.body);
        let not_modified = method == "GET" && reply.status == 200 && inm.as_deref() == Some(&etag);
        if !not_modified {
            s.remaining = (s.remaining - 1).max(0);
        }
        let status = if not_modified { 304 } else { reply.status };
        let mut out = format!("HTTP/2.0 {status} X\r\n");
        match self.cli {
            CliKind::Gh => out.push_str(&format!(
                "X-Ratelimit-Limit: {}\r\nX-Ratelimit-Remaining: {}\r\nX-Ratelimit-Used: {}\r\nX-Ratelimit-Reset: {}\r\nX-Ratelimit-Resource: core\r\n",
                s.limit,
                s.remaining,
                s.limit - s.remaining,
                s.reset_secs
            )),
            CliKind::Glab => out.push_str(&format!(
                "RateLimit-Limit: {}\r\nRateLimit-Remaining: {}\r\nRateLimit-Observed: {}\r\nRateLimit-Reset: {}\r\n",
                s.limit,
                s.remaining,
                s.limit - s.remaining,
                s.reset_secs
            )),
            // Gitea sends no rate headers.
            CliKind::Tea => {}
        }
        if method == "GET" && reply.status == 200 && self.cli != CliKind::Tea {
            out.push_str(&format!("Etag: {etag}\r\n"));
        }
        for (k, v) in &reply.headers {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        out.push_str("\r\n");
        let body = if not_modified {
            String::new()
        } else {
            reply.body.clone()
        };
        let exit = if status >= 400 { 1 } else { 0 };
        if self.cli == CliKind::Tea {
            // tea prints the status line and headers on stderr, the body on stdout.
            return GhOutput {
                stdout: body,
                stderr: out.trim_end().to_string(),
                exit,
            };
        }
        out.push_str(&body);
        GhOutput {
            stdout: out,
            stderr: if status >= 400 {
                format!("{}: HTTP {status}", self.cli.name())
            } else {
                String::new()
            },
            exit,
        }
    }
}

#[async_trait::async_trait]
impl ForgeCli for FakeGh {
    fn route(&self) -> Route {
        self.route.clone()
    }

    fn cli(&self) -> CliKind {
        self.cli
    }

    async fn run(&self, args: &[String], stdin: Option<&str>) -> Result<GhOutput> {
        if args.first().map(String::as_str) == Some("api") {
            return Ok(self.api(args, stdin));
        }
        let mut s = self.state.lock().unwrap();
        s.calls.push(Call {
            args: args.to_vec(),
            stdin: stdin.map(String::from),
            method: String::new(),
            path: String::new(),
            if_none_match: None,
        });
        let hit = s
            .cli
            .iter()
            .find(|(p, _)| args.len() >= p.len() && args[..p.len()] == p[..])
            .map(|(_, o)| o.clone());
        Ok(hit.unwrap_or_default())
    }
}

/// A process runner where each CLI's sign-in check succeeds or not, and
/// anything else goes to a [`FakeGh`]. `gh auth status`, `glab auth status`
/// (+ `glab api user`) and `tea login list` (+ `tea api /user`) answer for
/// every host when `auth` is `Some(true)`, and for the hosts in `hosts` only
/// when that list is not empty.
pub struct FakeExec {
    /// `None`: the CLI is not installed. `Some(false)`: installed, signed out.
    pub auth: Option<bool>,
    pub gh: Arc<FakeGh>,
}

impl FakeExec {
    fn signed_out(&self) -> GhOutput {
        GhOutput {
            stderr: "You are not logged into any GitHub hosts. To log in, run: gh auth login\n"
                .into(),
            exit: 1,
            ..Default::default()
        }
    }
}

fn base(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

#[async_trait::async_trait]
impl Exec for FakeExec {
    async fn exec(&self, program: &str, args: &[String], stdin: Option<&str>) -> Result<GhOutput> {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let prog = base(program);
        let signed = match self.auth {
            None if matches!(
                a.as_slice(),
                ["auth", "status", ..] | ["login", "list", ..] | ["--version"]
            ) =>
            {
                return Err(anyhow::anyhow!("No such file or directory (os error 2)"));
            }
            None => false,
            Some(b) => b,
        };
        let ok = |stdout: &str| {
            Ok(GhOutput {
                stdout: stdout.into(),
                ..Default::default()
            })
        };
        match a.as_slice() {
            ["--version"] => return ok(&format!("{prog} version 9.9.9\n")),
            ["auth", "status", "--hostname", host, ..] if prog.contains("glab") => {
                return if signed {
                    ok(&format!("{host}\n  ✓ Logged in\n"))
                } else {
                    Ok(self.signed_out())
                };
            }
            ["api", "--hostname", _, "user"] if prog.contains("glab") => {
                return if signed {
                    ok(r#"{"username":"ana"}"#)
                } else {
                    Ok(self.signed_out())
                };
            }
            ["api", "--hostname", _, "personal_access_tokens/self"] if prog.contains("glab") => {
                return ok(r#"{"scopes":["api"]}"#);
            }
            ["login", "list", "-o", "json"] if prog.contains("tea") => {
                let hosts = self.gh.tea_hosts();
                let list: Vec<Value> = if signed {
                    hosts
                        .iter()
                        .map(|h| serde_json::json!({ "name": h, "url": format!("https://{h}"), "user": "ana" }))
                        .collect()
                } else {
                    Vec::new()
                };
                return ok(&Value::from(list).to_string());
            }
            ["api", "-l", _, "/user"] if prog.contains("tea") => {
                return if signed {
                    ok(r#"{"login":"ana"}"#)
                } else {
                    Ok(self.signed_out())
                };
            }
            ["auth", "status", "--hostname", host, ..] => {
                return if signed {
                    ok(&format!("{host}\n  ✓ Logged in to {host} account ana\n"))
                } else {
                    Ok(self.signed_out())
                };
            }
            _ => {}
        }
        self.gh.run(args, stdin).await
    }
}

/// One recorded job: node, kind, payload, needs.
pub type FakeJob = (Option<String>, String, String, JobNeeds);

/// A node gateway whose `github` jobs run on a [`FakeGh`].
pub struct FakeGateway {
    pub online: Vec<String>,
    pub gh: Arc<FakeGh>,
    pub jobs: Mutex<Vec<FakeJob>>,
}

impl FakeGateway {
    pub fn new(online: &[&str], gh: Arc<FakeGh>) -> Arc<Self> {
        Arc::new(Self {
            online: online.iter().map(|s| s.to_string()).collect(),
            gh,
            jobs: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait::async_trait]
impl NodeGateway for FakeGateway {
    async fn online(&self) -> Vec<String> {
        self.online.clone()
    }

    async fn dispatch(&self, _node_id: &str, _spec: DispatchSpec) -> Result<String> {
        anyhow::bail!("fake gateway: no dispatch")
    }

    async fn job(
        &self,
        node_id: Option<&str>,
        kind: &str,
        payload_json: &str,
        needs: JobNeeds,
        _timeout_ms: i64,
    ) -> Result<JobOutcome> {
        self.jobs.lock().unwrap().push((
            node_id.map(String::from),
            kind.into(),
            payload_json.into(),
            needs,
        ));
        let p: Value = serde_json::from_str(payload_json)?;
        if kind == crate::node::PUSH_JOB_KIND {
            // A node that built and pushed the merge commit.
            return Ok(JobOutcome {
                status: "ok".into(),
                output_json: serde_json::json!({ "commit": "c0ffee", "pushed": true }).to_string(),
                node_id: node_id.unwrap_or_default().into(),
                ..Default::default()
            });
        }
        let args: Vec<String> = serde_json::from_value(p["args"].clone())?;
        let stdin = p["stdin"].as_str();
        let out = self.gh.run(&args, stdin).await?;
        Ok(JobOutcome {
            status: "ok".into(),
            output_json: serde_json::to_string(&out)?,
            node_id: node_id.unwrap_or_default().into(),
            ..Default::default()
        })
    }
}
