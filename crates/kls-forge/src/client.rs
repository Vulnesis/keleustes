//! REST over the forge CLI's API passthrough (`gh api -i`, `glab api -i`,
//! `tea api -i`): conditional GETs with the stored ETag (GitHub), rate
//! headers observed into the store per dialect (GitHub `x-ratelimit-*`,
//! GitLab `RateLimit-*`, Gitea none: calls are counted against
//! `forge.calls_per_hour`), every call counted, the budget checked before a
//! call goes, and every write checked against the kill switch and audited.

use crate::budget::{Budget, CallClass, Refusal, factory_used};
use crate::forge::api::{self, ForgeApi};
use crate::forge::guard;
use crate::forge::{CliKind, Connection, Dialect};
use crate::http::{self, Raw};
use crate::{ForgeCli, GhError, GhOutput, Route};
use anyhow::{Context, Result, anyhow};
use kls_core::Core;
use kls_core::github::{kinds, settings};
use kls_store::github::{Quota, hour_bucket};
use kls_store::now_ms;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

/// Last time a `github.quota.observed` event went out (at most one a minute).
static LAST_QUOTA_EVENT: AtomicI64 = AtomicI64::new(0);

/// Calls per hour counted against a forge that sends no rate headers.
pub const DEFAULT_CALLS_PER_HOUR: i64 = 5000;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Response {
    pub status: u16,
    pub body: String,
    pub link: String,
    /// GitHub answered 304 and `body` is the cached one.
    pub not_modified: bool,
    /// GitLab's `x-next-page`, when it sent one.
    pub next_page: String,
}

impl Response {
    pub fn json(&self) -> Result<Value> {
        if self.body.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&self.body).context("the forge sent a body that is not JSON")
    }
}

/// Every item of a paged list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Paged {
    pub items: Vec<Value>,
    /// Every page answered 304: nothing changed since the last read.
    pub not_modified: bool,
    pub pages: usize,
}

#[derive(Clone)]
pub struct Client {
    gh: Arc<dyn ForgeCli>,
    core: Core,
    route: Route,
    conn: Connection,
    route_key: String,
}

/// Pages read at most per list (100 items each).
pub const MAX_PAGES: usize = 50;

impl Client {
    /// A client for github.com (the default connection).
    pub fn new(gh: Arc<dyn ForgeCli>, core: Core) -> Self {
        Self::with_conn(gh, core, Connection::github_com())
    }

    /// A client for one connection.
    pub fn with_conn(gh: Arc<dyn ForgeCli>, core: Core, conn: Connection) -> Self {
        let route = gh.route();
        let route_key = conn.route_key(&route);
        Self {
            gh,
            core,
            route,
            conn,
            route_key,
        }
    }

    pub fn route(&self) -> &Route {
        &self.route
    }

    /// The key the budget, the call counts and the ETags are kept under.
    pub fn route_key(&self) -> &str {
        &self.route_key
    }

    pub fn core(&self) -> &Core {
        &self.core
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn cli_kind(&self) -> CliKind {
        self.gh.cli()
    }

    /// The forge's own `owner/name` of a repository key.
    pub fn forge_name<'a>(&self, key: &'a str) -> &'a str {
        self.conn.forge_name(key)
    }

    /// The dialect translation for this connection.
    pub fn api(&self) -> &'static dyn ForgeApi {
        api::for_dialect(self.conn.dialect())
    }

    fn key(&self, path: &str) -> String {
        format!("{}|{}", self.route_key, path)
    }

    /// The quota a call is checked against: observed, or counted for a
    /// forge without rate headers.
    pub async fn quota(&self) -> Result<Option<Quota>> {
        let store = self.core.store();
        let observed = store.gh_quota(&self.route_key, "core").await?;
        if self.conn.dialect() != Dialect::Gitea || observed.is_some() {
            return Ok(observed);
        }
        let now = now_ms();
        let limit = store
            .setting(settings::CALLS_PER_HOUR)
            .await?
            .and_then(|s| s.trim().parse::<i64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_CALLS_PER_HOUR);
        let used: i64 = store
            .gh_calls_in_hour(&self.route_key, now)
            .await?
            .iter()
            .map(|c| c.calls - c.not_modified)
            .sum();
        Ok(Some(Quota {
            route: self.route_key.clone(),
            resource: "core".into(),
            limit,
            remaining: (limit - used).max(0),
            used,
            reset_at: hour_bucket(now) + 3_600_000,
            observed_at: now,
            secondary_until: 0,
        }))
    }

    /// Checks the budget for a call of `caller` costing `cost`.
    pub async fn guard(&self, caller: &str, cost: i64) -> Result<()> {
        let store = self.core.store();
        let quota = self.quota().await?;
        let now = now_ms();
        let used = factory_used(&store.gh_calls_in_hour(&self.route_key, now).await?);
        let budget = Budget::load(store).await?;
        budget
            .check(CallClass::of(caller), quota.as_ref(), used, cost, now)
            .map_err(|r: Refusal| anyhow::Error::new(GhError::from(r)))
    }

    /// Drops cached ETags under `prefix` (an API path), after a write.
    pub async fn invalidate(&self, prefix: &str) -> Result<()> {
        self.core.store().gh_drop_etags(&self.key(prefix)).await?;
        Ok(())
    }

    pub async fn get(&self, path: &str, caller: &str) -> Result<Response> {
        self.request("GET", path, None, caller).await
    }

    pub async fn get_json(&self, path: &str, caller: &str) -> Result<Value> {
        self.get(path, caller).await?.json()
    }

    /// Follows `Link: rel="next"` (or GitLab's `x-next-page`) up to
    /// [`MAX_PAGES`]; each page is conditional.
    pub async fn get_all(&self, path: &str, caller: &str) -> Result<Paged> {
        self.get_until(path, caller, |_| false).await
    }

    /// [`get_all`](Self::get_all) over a list sorted newest first that
    /// stops at the first item `older` says is past the cursor (that item
    /// and the rest of its page are dropped; no further page is read).
    pub async fn get_until(
        &self,
        path: &str,
        caller: &str,
        older: impl Fn(&Value) -> bool,
    ) -> Result<Paged> {
        let mut out = Paged {
            not_modified: true,
            ..Default::default()
        };
        let mut next = Some(path.to_string());
        while let Some(p) = next.take() {
            if out.pages >= MAX_PAGES {
                break;
            }
            let r = self.get(&p, caller).await?;
            out.pages += 1;
            out.not_modified &= r.not_modified;
            let page = match r.json()? {
                Value::Array(a) => a,
                // Some lists wrap items: {"total_count": n, "check_runs": [...]}.
                Value::Object(o) => o
                    .values()
                    .find_map(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            let (n, before) = (page.len(), out.items.len());
            out.items.extend(page.into_iter().take_while(|v| !older(v)));
            if out.items.len() - before < n {
                // An item past the cursor was met on this page: stop here.
                break;
            }
            next = http::next_link(&r.link).or_else(|| http::with_page(&p, &r.next_page));
        }
        Ok(out)
    }

    /// The argv of one API call for this CLI (forges.md 3.2).
    fn api_args(&self, method: &str, path: &str, etag: Option<&str>, body: bool) -> Vec<String> {
        let host = self.conn.host.clone();
        let mut args = vec!["api".to_string()];
        match self.gh.cli() {
            CliKind::Gh => {
                if !self.conn.is_github_com() {
                    args.extend(crate::args(["--hostname", &host]));
                }
                args.extend(crate::args([
                    "--include",
                    "--method",
                    method,
                    "-H",
                    "Accept: application/vnd.github+json",
                    "-H",
                    "X-GitHub-Api-Version: 2022-11-28",
                ]));
                if let Some(e) = etag {
                    args.push("-H".into());
                    args.push(format!("If-None-Match: {e}"));
                }
                if body {
                    args.extend(crate::args(["--input", "-"]));
                }
                args.push(path.to_string());
            }
            CliKind::Glab => {
                args.extend(crate::args([
                    "--hostname",
                    &host,
                    "--include",
                    "--method",
                    method,
                ]));
                if let Some(e) = etag {
                    args.push("-H".into());
                    args.push(format!("If-None-Match: {e}"));
                }
                if body {
                    args.extend(crate::args([
                        "-H",
                        "Content-Type: application/json",
                        "--input",
                        "-",
                    ]));
                }
                args.push(path.trim_start_matches('/').to_string());
            }
            CliKind::Tea => {
                let name = if self.conn.cli_name.is_empty() {
                    host
                } else {
                    self.conn.cli_name.clone()
                };
                args.extend(crate::args(["-l", &name, "-i", "-X", method]));
                if body {
                    args.extend(crate::args([
                        "-H",
                        "Content-Type: application/json",
                        "-d",
                        "@-",
                    ]));
                }
                args.push(path.to_string());
            }
        }
        args
    }

    /// One REST call. GETs send `If-None-Match` when an ETag is stored
    /// (GitHub) and return the cached body on 304. Errors are [`GhError`]s
    /// where the forge answered. Writes are refused for automation while the
    /// factory is stopped, and every write is audited.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        caller: &str,
    ) -> Result<Response> {
        if method == "GET" {
            return self.call(method, path, body, caller).await;
        }
        let action = format!("{method} {path}");
        guard::check_kill(&self.core, caller, &action, path, &self.conn.id).await?;
        self.check_write_cap(caller, &action, path).await?;
        let r = self.call(method, path, body, caller).await;
        guard::audit(
            &self.core,
            caller,
            &action,
            path,
            &self.conn.id,
            &guard::outcome(&r),
            serde_json::json!({ "route": self.route_key, "status": r.as_ref().map(|r| r.status).unwrap_or(0), "write": true }),
        )
        .await;
        r
    }

    /// The proactive write cap (`forge.quota.proactive_writes_per_hour`):
    /// automation may make at most that many writes per rolling hour.
    /// People's writes never count and are never held. A held write emits
    /// `github.quota.write_capped` and is audited.
    pub async fn check_write_cap(&self, caller: &str, action: &str, target: &str) -> Result<()> {
        if !guard::automated(caller) {
            return Ok(());
        }
        let cap = crate::budget::proactive_cap(self.core.store()).await;
        if cap == 0 {
            return Ok(());
        }
        let now = now_ms();
        let writes = crate::budget::proactive_writes_since(&self.core, now - 3_600_000).await?;
        let Err(refusal) = crate::budget::write_cap_check(cap, &writes, now) else {
            return Ok(());
        };
        let until = refusal.until();
        let _ = self
            .core
            .emit(
                crate::budget::kinds::WRITE_CAPPED,
                &self.route_key,
                &serde_json::json!({ "caller": caller, "action": action, "cap": cap, "until": until, "connection": self.conn.id }),
            )
            .await;
        guard::audit(
            &self.core,
            caller,
            action,
            target,
            &self.conn.id,
            "capped",
            serde_json::json!({ "route": self.route_key, "cap": cap, "until": until }),
        )
        .await;
        Err(GhError::from(refusal).into())
    }

    /// The factory share ran out for a deferrable read: apply
    /// `forge.quota.on_exhausted`. Each path is refused under the kill
    /// switch first and every outcome is audited.
    async fn on_exhausted(&self, method: &str, path: &str, caller: &str) -> Result<Response> {
        let store = self.core.store();
        let policy = crate::budget::OnExhausted::load(store).await;
        let action = format!("quota.{} {method} {path}", policy.as_str());
        guard::check_kill(&self.core, caller, &action, path, &self.conn.id).await?;
        let until = self
            .quota()
            .await
            .ok()
            .flatten()
            .map(|q| q.reset_at)
            .unwrap_or(0);
        let detail = serde_json::json!({ "route": self.route_key, "policy": policy.as_str(), "until": until });
        use crate::budget::{OnExhausted, kinds as qk};
        match policy {
            OnExhausted::Defer => {
                self.deferred(caller, &action, path, "deferred", detail)
                    .await
            }
            OnExhausted::UseCached => {
                let cached = if method == "GET" && self.conn.dialect() != Dialect::Gitea {
                    store.gh_etag(&self.key(path)).await?
                } else {
                    None
                };
                let Some(c) = cached else {
                    return self
                        .deferred(caller, &action, path, "deferred: nothing cached", detail)
                        .await;
                };
                self.note(qk::QUOTA_CACHED, caller, path, until).await;
                guard::audit(
                    &self.core,
                    caller,
                    &action,
                    path,
                    &self.conn.id,
                    "cached",
                    detail.clone(),
                )
                .await;
                Ok(Response {
                    status: 304,
                    body: c.body,
                    link: c.link,
                    not_modified: true,
                    next_page: String::new(),
                })
            }
            OnExhausted::Escalate => {
                // One escalation per route and reset window; the reads still wait.
                if escalate_once(
                    &format!("{}|{}", self.core.data_dir().display(), self.route_key),
                    until,
                ) {
                    let _ = self
                        .core
                        .emit(
                            qk::QUOTA_ESCALATED,
                            &self.route_key,
                            &serde_json::json!({ "caller": caller, "path": path, "until": until,
                                                 "connection": self.conn.id, "reason": "factory share used up" }),
                        )
                        .await;
                }
                guard::audit(
                    &self.core,
                    caller,
                    &action,
                    path,
                    &self.conn.id,
                    "escalated",
                    detail.clone(),
                )
                .await;
                Err(GhError::ShareExhausted.into())
            }
        }
    }

    async fn deferred(
        &self,
        caller: &str,
        action: &str,
        path: &str,
        outcome: &str,
        detail: Value,
    ) -> Result<Response> {
        let until = detail["until"].as_i64().unwrap_or(0);
        self.note(crate::budget::kinds::QUOTA_DEFERRED, caller, path, until)
            .await;
        guard::audit(
            &self.core,
            caller,
            action,
            path,
            &self.conn.id,
            outcome,
            detail,
        )
        .await;
        Err(GhError::ShareExhausted.into())
    }

    /// A quota event, at most one a minute per kind.
    async fn note(&self, kind: &str, caller: &str, path: &str, until: i64) {
        static LAST: std::sync::Mutex<Vec<(String, i64)>> = std::sync::Mutex::new(Vec::new());
        let now = now_ms();
        {
            let mut l = LAST.lock().unwrap();
            let k = format!(
                "{kind}|{}|{}",
                self.core.data_dir().display(),
                self.route_key
            );
            match l.iter_mut().find(|(x, _)| *x == k) {
                Some((_, t)) if now - *t < 60_000 => return,
                Some((_, t)) => *t = now,
                None => l.push((k, now)),
            }
        }
        let _ = self
            .core
            .emit(
                kind,
                &self.route_key,
                &serde_json::json!({ "caller": caller, "path": path, "until": until, "connection": self.conn.id }),
            )
            .await;
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        caller: &str,
    ) -> Result<Response> {
        if !self.route.available() {
            return Err(GhError::Unavailable(self.route.label_for(self.gh.cli())).into());
        }
        if let Err(e) = self.guard(caller, 1).await {
            if matches!(e.downcast_ref::<GhError>(), Some(GhError::ShareExhausted)) {
                return self.on_exhausted(method, path, caller).await;
            }
            return Err(e);
        }
        let store = self.core.store();
        let key = self.key(path);
        let etags = self.conn.dialect() != Dialect::Gitea;
        let cached = if method == "GET" && etags {
            store.gh_etag(&key).await?
        } else {
            None
        };
        let stdin = body.map(|b| b.to_string());
        let args = self.api_args(
            method,
            path,
            cached.as_ref().map(|c| c.etag.as_str()),
            stdin.is_some(),
        );
        let out = self.gh.run(&args, stdin.as_deref()).await?;
        let text = match self.gh.cli() {
            // tea writes the status line and headers to stderr.
            CliKind::Tea if !out.stdout.trim_start().starts_with("HTTP/") => {
                format!("{}\n\n{}", out.stderr.trim_end(), out.stdout)
            }
            _ => out.stdout.clone(),
        };
        let raw = match http::parse_include(&text) {
            Some(r) => r,
            None => return Err(cli_error(&out)),
        };
        let now = now_ms();
        self.observe(&raw, now).await?;
        store
            .gh_count_call(&self.route_key, caller, now, raw.status == 304)
            .await?;
        if raw.status == 304 {
            let c = cached.ok_or_else(|| anyhow!("304 for {path} with no cached body"))?;
            return Ok(Response {
                status: 304,
                body: c.body,
                link: c.link,
                not_modified: true,
                next_page: String::new(),
            });
        }
        if (raw.status == 403 || raw.status == 429)
            && let Some(until) = backoff_until(&raw, now)
        {
            store
                .gh_set_secondary_until(&self.route_key, "core", until)
                .await?;
            let _ = self
                .core
                .emit(
                    kinds::QUOTA_LIMITED,
                    &self.route_key,
                    &serde_json::json!({ "until": until, "status": raw.status, "path": path, "connection": self.conn.id }),
                )
                .await;
            return Err(GhError::RateLimited { until }.into());
        }
        if raw.status >= 400 {
            return Err(GhError::Http {
                status: raw.status,
                message: message_of(&raw.body),
            }
            .into());
        }
        let link = raw.header("link").unwrap_or("").to_string();
        if method == "GET"
            && etags
            && let Some(etag) = raw.header("etag")
        {
            store.gh_put_etag(&key, etag, &link, &raw.body).await?;
        }
        Ok(Response {
            status: raw.status,
            next_page: raw.header("x-next-page").unwrap_or("").trim().to_string(),
            body: raw.body,
            link,
            not_modified: false,
        })
    }

    async fn observe(&self, raw: &Raw, now: i64) -> Result<()> {
        let Some(q) = observe_quota(self.conn.dialect(), &self.route_key, raw, now) else {
            return Ok(());
        };
        self.core.store().gh_put_quota(&q).await?;
        let last = LAST_QUOTA_EVENT.load(Ordering::Relaxed);
        if now - last > 60_000
            && LAST_QUOTA_EVENT
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            let _ = self.core.emit(kinds::QUOTA_OBSERVED, &q.route, &q).await;
        }
        Ok(())
    }

    /// `gh api rate_limit` (free) into the store. Other dialects read their
    /// budget from headers (GitLab) or count calls (Gitea): nothing to do.
    pub async fn rate_limit(&self) -> Result<Vec<Quota>> {
        if !self.route.available() {
            return Err(GhError::Unavailable(self.route.label_for(self.gh.cli())).into());
        }
        if self.conn.dialect() != Dialect::GitHub {
            return Ok(Vec::new());
        }
        let mut args = crate::args(["api"]);
        if !self.conn.is_github_com() {
            args.extend(crate::args(["--hostname", &self.conn.host]));
        }
        args.push("rate_limit".into());
        let out = self.gh.run(&args, None).await?;
        if out.exit != 0 {
            // GHES with rate limiting off: the budget is unknown, not an error.
            if format!("{}{}", out.stdout, out.stderr).contains("Rate limiting is not enabled") {
                self.core
                    .store()
                    .gh_set_budget_known(&self.route_key, false)
                    .await?;
                return Ok(Vec::new());
            }
            return Err(cli_error(&out));
        }
        let v: Value = serde_json::from_str(&out.stdout).context("rate_limit JSON")?;
        let now = now_ms();
        let mut got = Vec::new();
        if let Some(res) = v.get("resources").and_then(|r| r.as_object()) {
            for name in ["core", "graphql", "search"] {
                let Some(r) = res.get(name) else { continue };
                let i = |k: &str| r.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
                let q = Quota {
                    route: self.route_key.clone(),
                    resource: name.into(),
                    limit: i("limit"),
                    remaining: i("remaining"),
                    used: i("used"),
                    reset_at: i("reset") * 1000,
                    observed_at: now,
                    secondary_until: 0,
                };
                self.core.store().gh_put_quota(&q).await?;
                got.push(q);
            }
        }
        Ok(got)
    }

    /// Hands a merge-commit push to the route (a node's `forge_push` job).
    /// `None` when the route cannot (the local CLI pushes with git here).
    pub async fn push_on_route(&self, payload: &Value) -> Result<Option<String>> {
        self.gh.push_merge(payload).await
    }

    /// A non-API CLI command (`gh pr ready ...`), budgeted, counted, under
    /// the kill switch and audited.
    pub async fn cli(&self, args: Vec<String>, caller: &str) -> Result<GhOutput> {
        if !self.route.available() {
            return Err(GhError::Unavailable(self.route.label_for(self.gh.cli())).into());
        }
        let action = args.iter().take(2).cloned().collect::<Vec<_>>().join(" ");
        let target = args.join(" ");
        guard::check_kill(&self.core, caller, &action, &target, &self.conn.id).await?;
        self.check_write_cap(caller, &action, &target).await?;
        self.guard(caller, 1).await?;
        let r = async {
            let out = self.gh.run(&args, None).await?;
            self.core
                .store()
                .gh_count_call(&self.route_key, caller, now_ms(), false)
                .await?;
            if out.exit != 0 {
                return Err(cli_error(&out));
            }
            Ok(out)
        }
        .await;
        guard::audit(
            &self.core,
            caller,
            &action,
            &target,
            &self.conn.id,
            &guard::outcome(&r),
            serde_json::json!({ "route": self.route_key, "write": true }),
        )
        .await;
        r
    }
}

/// True once per (route, reset window): the first escalation of a window.
fn escalate_once(route: &str, until: i64) -> bool {
    static SEEN: std::sync::Mutex<Vec<(String, i64)>> = std::sync::Mutex::new(Vec::new());
    let mut s = SEEN.lock().unwrap();
    match s.iter_mut().find(|(r, _)| r == route) {
        Some((_, u)) if *u == until => false,
        Some((_, u)) => {
            *u = until;
            true
        }
        None => {
            s.push((route.to_string(), until));
            true
        }
    }
}

/// The quota a response's headers state, per dialect (`None`: no headers).
pub fn observe_quota(dialect: Dialect, route: &str, raw: &Raw, now: i64) -> Option<Quota> {
    let (limit, remaining, used, reset, resource) = match dialect {
        Dialect::GitHub => {
            let limit = raw.header_i64("x-ratelimit-limit")?;
            let remaining = raw.header_i64("x-ratelimit-remaining")?;
            (
                limit,
                remaining,
                raw.header_i64("x-ratelimit-used")
                    .unwrap_or(limit - remaining),
                raw.header_i64("x-ratelimit-reset").unwrap_or(0),
                raw.header("x-ratelimit-resource")
                    .unwrap_or("core")
                    .to_string(),
            )
        }
        Dialect::GitLab => {
            let limit = raw.header_i64("ratelimit-limit")?;
            let remaining = raw.header_i64("ratelimit-remaining")?;
            (
                limit,
                remaining,
                raw.header_i64("ratelimit-observed")
                    .unwrap_or(limit - remaining),
                raw.header_i64("ratelimit-reset").unwrap_or(0),
                "core".to_string(),
            )
        }
        Dialect::Gitea => return None,
    };
    Some(Quota {
        route: route.into(),
        resource,
        limit,
        remaining,
        used,
        reset_at: reset * 1000,
        observed_at: now,
        secondary_until: 0,
    })
}

/// When a 403/429 is a rate limit, until when to back off.
pub fn backoff_until(raw: &Raw, now: i64) -> Option<i64> {
    if let Some(s) = raw.header_i64("retry-after") {
        return Some(now + s.max(1) * 1000);
    }
    if raw.header_i64("x-ratelimit-remaining") == Some(0)
        || raw.header_i64("ratelimit-remaining") == Some(0)
    {
        let reset = raw
            .header_i64("x-ratelimit-reset")
            .or_else(|| raw.header_i64("ratelimit-reset"))
            .unwrap_or(0)
            * 1000;
        return Some(reset.max(now + 60_000));
    }
    let msg = raw.body.to_lowercase();
    (msg.contains("secondary rate limit") || msg.contains("abuse") || raw.status == 429)
        .then_some(now + 60_000)
}

fn message_of(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            let m = v
                .get("message")
                .or_else(|| v.get("error"))
                .map(|m| match m {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })?;
            let errs = v
                .get("errors")
                .and_then(|e| e.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|e| {
                            e.get("message")
                                .or_else(|| e.get("code"))
                                .and_then(|m| m.as_str())
                        })
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            Some(if errs.is_empty() {
                m
            } else {
                format!("{m} ({errs})")
            })
        })
        .unwrap_or_else(|| body.chars().take(200).collect())
}

pub(crate) fn cli_error(out: &GhOutput) -> anyhow::Error {
    let msg = out
        .stderr
        .lines()
        .chain(out.stdout.lines())
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("the forge CLI failed")
        .to_string();
    // gh prints "HTTP 404: Not Found (https://...)" on API errors without -i.
    if let Some(rest) = msg.split("HTTP ").nth(1)
        && let Some(code) = rest.split(|c: char| !c.is_ascii_digit()).next()
        && let Ok(status) = code.parse::<u16>()
    {
        return GhError::Http {
            status,
            message: msg,
        }
        .into();
    }
    anyhow!("the forge CLI exited {}: {msg}", out.exit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_headers_per_dialect() {
        let gl = http::parse_include(
            "HTTP/1.1 200 OK\nRateLimit-Limit: 2000\nRateLimit-Remaining: 1990\nRateLimit-Reset: 100\n\n[]",
        )
        .unwrap();
        let q = observe_quota(Dialect::GitLab, "r", &gl, 1).unwrap();
        assert_eq!((q.limit, q.remaining, q.reset_at), (2000, 1990, 100_000));
        assert!(observe_quota(Dialect::GitHub, "r", &gl, 1).is_none());
        assert!(observe_quota(Dialect::Gitea, "r", &gl, 1).is_none());
        let limited = http::parse_include("HTTP/1.1 429 Too Many\nRetry-After: 5\n\n").unwrap();
        assert_eq!(backoff_until(&limited, 1000), Some(6000));
    }

    use crate::budget::{self, keys, kinds as qk};
    use crate::fake::FakeGh;
    use serde_json::json;

    async fn exhausted(policy: &str) -> (Core, tempfile::TempDir, Arc<FakeGh>, Client) {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        core.store()
            .set_setting(keys::ON_EXHAUSTED, policy)
            .await
            .unwrap();
        let gh = FakeGh::new();
        gh.on("GET", "/repos/acme/api", 200, json!({"name": "api"}));
        let c = Client::new(gh.clone(), core.clone());
        // A person's read fills the ETag cache while the budget is full.
        c.get("/repos/acme/api", "user").await.unwrap();
        // 1000 left of 5000: below the 20% reserve, the poller's share is out.
        gh.set_rate(5000, 1000, now_ms() / 1000 + 3000);
        c.rate_limit().await.unwrap();
        (core, dir, gh, c)
    }

    async fn forge_ledger(core: &Core, outcome: &str) -> usize {
        core.audit_entries(&kls_store::audit::LedgerQuery {
            kinds: vec!["forge".into()],
            limit: 1000,
            ..Default::default()
        })
        .await
        .unwrap()
        .iter()
        .filter(|e| e.payload["outcome"] == outcome)
        .count()
    }

    async fn event_count(core: &Core, kind: &str) -> usize {
        core.store()
            .recent_events(200)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.kind == kind)
            .count()
    }

    #[tokio::test]
    async fn on_exhausted_defer_waits_and_is_audited() {
        let (core, _d, gh, c) = exhausted("defer").await;
        let before = gh.calls().len();
        let e = c.get("/repos/acme/api", "poller").await.unwrap_err();
        assert_eq!(e.downcast_ref::<GhError>(), Some(&GhError::ShareExhausted));
        assert_eq!(gh.calls().len(), before, "no call went out");
        assert_eq!(forge_ledger(&core, "deferred").await, 1);
        assert!(event_count(&core, qk::QUOTA_DEFERRED).await >= 1);
        // People are never deferred.
        assert!(c.get("/repos/acme/api", "user").await.is_ok());
    }

    #[tokio::test]
    async fn on_exhausted_cached_answers_from_the_etag_cache() {
        let (core, _d, gh, c) = exhausted("use_cached").await;
        let before = gh.calls().len();
        let r = c.get("/repos/acme/api", "poller").await.unwrap();
        assert!(r.not_modified);
        assert_eq!(r.json().unwrap()["name"], "api");
        assert_eq!(gh.calls().len(), before, "answered from the cache");
        assert_eq!(forge_ledger(&core, "cached").await, 1);
        // Nothing cached: it defers.
        let e = c.get("/repos/acme/other", "poller").await.unwrap_err();
        assert_eq!(e.downcast_ref::<GhError>(), Some(&GhError::ShareExhausted));
        assert_eq!(forge_ledger(&core, "deferred: nothing cached").await, 1);
    }

    #[tokio::test]
    async fn on_exhausted_escalate_asks_a_person_once() {
        let (core, _d, _gh, c) = exhausted("escalate").await;
        for _ in 0..2 {
            let e = c.get("/repos/acme/api", "poller").await.unwrap_err();
            assert_eq!(e.downcast_ref::<GhError>(), Some(&GhError::ShareExhausted));
        }
        assert_eq!(forge_ledger(&core, "escalated").await, 2);
        assert_eq!(event_count(&core, qk::QUOTA_ESCALATED).await, 1);
        assert!(kls_core::audit::config::notify_allows(
            "escalations_only",
            qk::QUOTA_ESCALATED
        ));
    }

    #[tokio::test]
    async fn on_exhausted_paths_are_refused_under_the_kill_switch() {
        for policy in ["defer", "use_cached", "escalate"] {
            let (core, _d, gh, c) = exhausted(policy).await;
            kls_core::control::pause(&core, "drill").await.unwrap();
            let before = gh.calls().len();
            let e = c.get("/repos/acme/api", "poller").await.unwrap_err();
            assert!(
                matches!(e.downcast_ref::<GhError>(), Some(GhError::Stopped(_))),
                "{policy}: {e:#}"
            );
            assert_eq!(gh.calls().len(), before, "{policy}");
            assert_eq!(forge_ledger(&core, "refused").await, 1, "{policy}");
            for o in ["deferred", "cached", "escalated"] {
                assert_eq!(forge_ledger(&core, o).await, 0, "{policy} {o}");
            }
            assert!(event_count(&core, kls_core::control::kinds::FACTORY_REFUSED).await >= 1);
        }
    }

    #[tokio::test]
    async fn proactive_write_cap_holds_automation_only() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        core.store()
            .set_setting(keys::PROACTIVE_WRITES, "2")
            .await
            .unwrap();
        let gh = FakeGh::new();
        gh.on("POST", "/repos/acme/api/issues/1/comments", 201, json!({}));
        let c = Client::new(gh.clone(), core.clone());
        let body = json!({"body": "hi"});
        let path = "/repos/acme/api/issues/1/comments";
        for _ in 0..2 {
            c.request("POST", path, Some(&body), "factory:triage")
                .await
                .unwrap();
        }
        let e = c
            .request("POST", path, Some(&body), "factory:triage")
            .await
            .unwrap_err();
        assert!(matches!(
            e.downcast_ref::<GhError>(),
            Some(GhError::RateLimited { .. })
        ));
        assert_eq!(gh.count("POST", path), 2);
        assert_eq!(forge_ledger(&core, "capped").await, 1);
        assert_eq!(event_count(&core, qk::WRITE_CAPPED).await, 1);
        // A person's write is not capped and not counted.
        c.request("POST", path, Some(&body), "user").await.unwrap();
        assert_eq!(gh.count("POST", path), 3);
        // The per-workflow count is public.
        let w = budget::workflow_calls_this_hour(core.store(), now_ms())
            .await
            .unwrap();
        assert_eq!(w.len(), 1);
        assert_eq!((w[0].workflow.as_str(), w[0].calls), ("triage", 2));
        let all = budget::calls_this_hour(core.store(), now_ms())
            .await
            .unwrap();
        assert!(all.iter().any(|x| x.caller == "user" && x.calls == 1));
    }

    #[test]
    fn write_cap_rolls_with_the_hour() {
        assert!(budget::write_cap_check(0, &[1, 2, 3], 10).is_ok());
        let now = 10_000_000;
        assert!(budget::write_cap_check(2, &[now - 10], now).is_ok());
        let e = budget::write_cap_check(2, &[now - 20, now - 10], now).unwrap_err();
        assert_eq!(e.until(), now - 20 + 3_600_000);
        // Older than an hour no longer counts.
        assert!(budget::write_cap_check(2, &[now - 3_700_000, now - 10], now).is_ok());
    }
}
