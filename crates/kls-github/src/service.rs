//! One forge plane per process: the connections, the route selector, the
//! poller and the actions the screens run. `start` (from kls-app) also
//! spawns the poller; `service` alone (from the UI) shares the same
//! instance. github.com is the default connection and every github.com
//! call keeps its old shape (`client()`, `login_with_token`, `login_web`).

use crate::budget::{Budget, factory_used};
use crate::client::Client;
use crate::conflicts::{self, ConflictSet};
use crate::forge::auth::{AuthState, CliProbe, auth_for};
use crate::forge::{Connection, ForgeKind, RoutePref, guard};
use crate::local::LocalGh;
use crate::login::WebLogin;
use crate::poller::{self, PollReport};
use crate::select::{RouteStatus, Selector};
use crate::sync::{self, BulkResult, LabelSync, OrgRefresh, PullAction};
use crate::{GhError, valid_key};
use anyhow::{Result, bail};
use kls_core::Core;
use kls_core::github::kinds;
use kls_store::github::{
    CallCount, GITHUB_COM, GhPull, GhRepo, Mirroring, Quota, RepoSettings, Resolution,
};
use kls_store::now_ms;
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Caller tag for a person's button.
pub const USER: &str = "user";

pub struct Service {
    core: Core,
    selector: Selector,
    kick: tokio::sync::Notify,
    last: Mutex<Option<PollReport>>,
    started: AtomicBool,
    /// The last `gh auth status` (github.com) and when it ran.
    local_auth: Mutex<Option<(i64, crate::LocalAuth)>>,
    /// The last sign-in state per connection and when it was read.
    auth: Mutex<HashMap<String, (i64, AuthState)>>,
}

/// The rate budget as a screen shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct QuotaView {
    pub route: String,
    pub core: Option<Quota>,
    pub calls: Vec<CallCount>,
    pub budget: Option<(i64, i64)>,
    /// Factory calls this hour (304s and people's calls excluded).
    pub factory_used: i64,
    pub factory_limit: i64,
    pub reserve: i64,
    /// False when the forge sends no rate headers and calls are counted.
    pub budget_known: bool,
}

/// One connection as the connections strip shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConnView {
    pub conn: Connection,
    pub route: RouteStatus,
    pub auth: AuthState,
    pub quota: QuotaView,
}

static SERVICE: OnceLock<Arc<Service>> = OnceLock::new();

/// The process's forge plane (created on first use).
pub fn service(core: &Core) -> Arc<Service> {
    SERVICE
        .get_or_init(|| Service::new(core.clone(), LocalGh::default()))
        .clone()
}

/// The process's forge plane with its poller running.
pub fn start(core: &Core) -> Arc<Service> {
    let s = service(core);
    s.spawn_poller();
    // The external kanban binding: forge providers and its tick.
    crate::kanban::start(&s);
    s
}

impl Service {
    pub fn new(core: Core, local: LocalGh) -> Arc<Self> {
        Arc::new(Self {
            selector: Selector::new(core.clone(), local),
            core,
            kick: tokio::sync::Notify::new(),
            last: Mutex::new(None),
            started: AtomicBool::new(false),
            local_auth: Mutex::new(None),
            auth: Mutex::new(HashMap::new()),
        })
    }

    pub fn core(&self) -> &Core {
        &self.core
    }

    // ---- connections -------------------------------------------------------

    /// Every connection, github.com first.
    pub async fn connections(&self) -> Result<Vec<Connection>> {
        Ok(self
            .core
            .store()
            .forge_connections()
            .await?
            .iter()
            .filter_map(Connection::from_row)
            .collect())
    }

    pub async fn connection(&self, id: &str) -> Result<Connection> {
        if id == GITHUB_COM {
            return Ok(self.github_com().await);
        }
        match self.core.store().forge_connection(id).await? {
            Some(r) => {
                Connection::from_row(&r).ok_or_else(|| anyhow::anyhow!("bad connection {id}"))
            }
            None => bail!("no connection {id}"),
        }
    }

    async fn github_com(&self) -> Connection {
        self.core
            .store()
            .forge_connection(GITHUB_COM)
            .await
            .ok()
            .flatten()
            .and_then(|r| Connection::from_row(&r))
            .unwrap_or_else(Connection::github_com)
    }

    /// The connection a repository key belongs to: the stored row's, else
    /// the connection whose host prefixes the key, else github.com.
    pub async fn connection_of(&self, key: &str) -> Result<Connection> {
        if let Some(f) = self.core.store().gh_repo_forge(key).await? {
            return self.connection(&f.connection).await;
        }
        let conns = self.connections().await?;
        if let Some(c) = conns
            .iter()
            .find(|c| !c.is_github_com() && key.starts_with(&format!("{}/", c.host)))
        {
            return Ok(c.clone());
        }
        Ok(self.github_com().await)
    }

    /// Adds a connection of `kind` on `host` (or returns the one there).
    /// Audited with its origin (`user`, `cli`).
    pub async fn add_connection(
        &self,
        kind: ForgeKind,
        host: &str,
        label: &str,
        caller: &str,
    ) -> Result<Connection> {
        let Some(mut c) = Connection::new(kind, host, caller) else {
            bail!("not a host: {host:?}");
        };
        let store = self.core.store();
        if let Some(r) = store.forge_connection_at(kind.id(), &c.host).await? {
            return Connection::from_row(&r).ok_or_else(|| anyhow::anyhow!("bad connection"));
        }
        if c.is_github_com() {
            return Ok(self.github_com().await);
        }
        if !label.trim().is_empty() {
            c.label = label.trim().chars().take(60).collect();
        }
        store.upsert_forge_connection(&c.to_row("{}")).await?;
        guard::audit(
            &self.core,
            caller,
            "connection_add",
            &c.host,
            &c.id,
            "ok",
            json!({ "kind": kind.id(), "label": c.label }),
        )
        .await;
        self.core
            .emit(
                kinds::CONNECTION,
                &c.id,
                &json!({ "connection": c.id, "kind": kind, "host": c.host, "action": "added",
                         "by": guard::actor(caller).1 }),
            )
            .await?;
        Ok(c)
    }

    /// Turns a connection on or off (off: no route, no polling; data stays).
    pub async fn set_connection_enabled(&self, id: &str, on: bool, caller: &str) -> Result<()> {
        if !self.core.store().forge_set_enabled(id, on).await? {
            bail!("no connection {id}");
        }
        self.selector.invalidate_conn(id);
        guard::audit(
            &self.core,
            caller,
            if on {
                "connection_enable"
            } else {
                "connection_disable"
            },
            id,
            id,
            "ok",
            json!({}),
        )
        .await;
        self.core
            .emit(
                kinds::CONNECTION,
                id,
                &json!({ "connection": id, "enabled": on, "by": guard::actor(caller).1 }),
            )
            .await?;
        Ok(())
    }

    /// Where a connection's calls go (auto, local only, one node).
    pub async fn set_route_pref(&self, id: &str, pref: RoutePref, caller: &str) -> Result<()> {
        let mut c = self.connection(id).await?;
        c.route_pref = pref;
        c.updated_at = now_ms();
        let status = self
            .core
            .store()
            .forge_connection(id)
            .await?
            .map(|r| r.status_json)
            .unwrap_or_else(|| "{}".into());
        self.core
            .store()
            .upsert_forge_connection(&c.to_row(&status))
            .await?;
        self.selector.invalidate_conn(id);
        guard::audit(
            &self.core,
            caller,
            "connection_route",
            id,
            id,
            "ok",
            json!({ "pref": c.route_pref.as_str() }),
        )
        .await;
        self.core
            .emit(
                kinds::CONNECTION,
                id,
                &json!({ "connection": id, "routePref": c.route_pref.as_str() }),
            )
            .await?;
        Ok(())
    }

    /// Removes a connection and its orgs and repositories (not github.com).
    /// The CLI's own login is left alone.
    pub async fn remove_connection(&self, id: &str, caller: &str) -> Result<()> {
        let r = self.core.store().delete_forge_connection(id).await;
        guard::audit(
            &self.core,
            caller,
            "connection_remove",
            id,
            id,
            &guard::outcome(&r),
            json!({}),
        )
        .await;
        if !r? {
            bail!("no connection {id}");
        }
        self.selector.invalidate_conn(id);
        self.auth.lock().unwrap().remove(id);
        self.core
            .emit(
                kinds::CONNECTION,
                id,
                &json!({ "connection": id, "action": "removed", "by": guard::actor(caller).1 }),
            )
            .await?;
        Ok(())
    }

    /// Is the connection's CLI installed, which version, which flags.
    pub async fn probe(&self, conn: &Connection) -> CliProbe {
        let exec = self.selector.local().exec();
        auth_for(conn.cli()).probe(exec.as_ref(), &conn.host).await
    }

    /// Whether this host's CLI is signed in to the connection's host
    /// (cached for [`ROUTE_TTL_MS`](crate::select::ROUTE_TTL_MS)).
    pub async fn auth_state(&self, conn: &Connection) -> AuthState {
        let now = now_ms();
        if let Some((at, a)) = self.auth.lock().unwrap().get(&conn.id).cloned()
            && now - at < crate::select::ROUTE_TTL_MS
        {
            return a;
        }
        let exec = self.selector.local().exec();
        let a = auth_for(conn.cli()).status(exec.as_ref(), conn).await;
        self.auth
            .lock()
            .unwrap()
            .insert(conn.id.clone(), (now, a.clone()));
        if let Ok(js) = serde_json::to_string(&a) {
            let _ = self
                .core
                .store()
                .forge_set_status(&conn.id, a.login(), &js)
                .await;
        }
        a
    }

    /// Everything the connections strip shows, for every connection.
    pub async fn connection_views(&self) -> Result<Vec<ConnView>> {
        let mut out = Vec::new();
        for conn in self.connections().await? {
            let route = self.selector.select_conn(&conn).await.0;
            let auth = self.auth_state(&conn).await;
            let quota = self.quota_for(&conn, &route).await?;
            out.push(ConnView {
                conn,
                route,
                auth,
                quota,
            });
        }
        Ok(out)
    }

    /// Detects one connection's route now.
    pub async fn recheck_conn(&self, conn: &Connection) -> RouteStatus {
        self.selector.invalidate_conn(&conn.id);
        self.auth.lock().unwrap().remove(&conn.id);
        if conn.is_github_com() {
            *self.local_auth.lock().unwrap() = None;
        }
        self.selector.select_conn(conn).await.0
    }

    // ---- github.com route (unchanged) ----------------------------------------

    /// The github.com route, detected again when stale.
    pub async fn route(&self) -> RouteStatus {
        self.selector.select().await.0
    }

    /// Detects every route now (returns github.com's).
    pub async fn recheck_route(&self) -> RouteStatus {
        self.selector.invalidate();
        *self.local_auth.lock().unwrap() = None;
        self.auth.lock().unwrap().clear();
        self.route().await
    }

    /// Whether this host's `gh` is installed and signed in to github.com,
    /// checked again after [`ROUTE_TTL_MS`](crate::select::ROUTE_TTL_MS) or a recheck.
    pub async fn local_auth(&self) -> crate::LocalAuth {
        let now = now_ms();
        if let Some((at, a)) = self.local_auth.lock().unwrap().clone()
            && now - at < crate::select::ROUTE_TTL_MS
        {
            return a;
        }
        let a = self.selector.local().auth().await;
        *self.local_auth.lock().unwrap() = Some((now, a.clone()));
        a
    }

    /// The program the github.com local route runs.
    pub fn local_program(&self) -> String {
        self.selector.local().program().to_string()
    }

    async fn after_login(
        &self,
        conn: &Connection,
        method: &str,
        caller: &str,
        outcome: &str,
    ) -> RouteStatus {
        let r = self.recheck_conn(conn).await;
        let login = self.auth_state(conn).await.login().to_string();
        guard::audit(
            &self.core,
            caller,
            "sign_in",
            &conn.host,
            &conn.id,
            outcome,
            json!({ "method": method, "cli": conn.cli().name(), "login": login, "route": r.label() }),
        )
        .await;
        let subject = if conn.is_github_com() {
            "local".to_string()
        } else {
            conn.id.clone()
        };
        let _ = self
            .core
            .emit(
                kinds::LOGIN,
                &subject,
                &json!({ "route": r.label(), "connection": conn.id, "host": conn.host,
                         "login": login, "method": method, "by": guard::actor(caller).1 }),
            )
            .await;
        r
    }

    /// Signs the local `gh` in to github.com with a token, then detects the route again.
    pub async fn login_with_token(&self, token: &str) -> Result<RouteStatus> {
        let conn = self.github_com().await;
        self.login_token_conn(&conn, token, USER).await
    }

    /// Signs the connection's local CLI in with a token (never kept).
    pub async fn login_token_conn(
        &self,
        conn: &Connection,
        token: &str,
        caller: &str,
    ) -> Result<RouteStatus> {
        let program = if conn.is_github_com() {
            self.local_program()
        } else {
            conn.cli().program()
        };
        let mut c = conn.clone();
        if c.cli() == crate::CliKind::Tea {
            let exec = self.selector.local().exec();
            c.cli_name = crate::forge::auth::TeaAuth::login_name(exec.as_ref(), &c).await;
        }
        let r = crate::login::login_with_token_for(&c, &program, token).await;
        if let Err(e) = &r {
            guard::audit(
                &self.core,
                caller,
                "sign_in",
                &conn.host,
                &conn.id,
                &format!("error: {e:#}"),
                json!({ "method": "token" }),
            )
            .await;
        }
        r?;
        self.remember_cli_name(&c).await?;
        Ok(self.after_login(&c, "token", caller, "ok").await)
    }

    async fn remember_cli_name(&self, c: &Connection) -> Result<()> {
        if c.is_github_com() {
            return Ok(());
        }
        if let Some(mut row) = self.core.store().forge_connection(&c.id).await?
            && row.cli_name != c.cli_name
        {
            row.cli_name = c.cli_name.clone();
            row.updated_at = now_ms();
            self.core.store().upsert_forge_connection(&row).await?;
        }
        Ok(())
    }

    /// Runs a browser sign-in of the local `gh` for github.com (or of the
    /// login's connection), then detects the route again.
    pub async fn login_web(
        &self,
        login: &WebLogin,
        on_change: impl Fn() + Send + Sync,
    ) -> RouteStatus {
        self.login_web_as(login, on_change, USER).await
    }

    pub async fn login_web_as(
        &self,
        login: &WebLogin,
        on_change: impl Fn() + Send + Sync,
        caller: &str,
    ) -> RouteStatus {
        let conn = login.connection().clone();
        let program = if conn.is_github_com() {
            self.local_program()
        } else {
            conn.cli().program()
        };
        login.run(&program, on_change).await;
        if conn.cli() == crate::CliKind::Tea {
            let mut c = conn.clone();
            let exec = self.selector.local().exec();
            c.cli_name = crate::forge::auth::TeaAuth::login_name(exec.as_ref(), &c).await;
            let _ = self.remember_cli_name(&c).await;
        }
        let outcome = match login.state() {
            crate::login::WebState::Done => "ok".to_string(),
            crate::login::WebState::Failed(e) => format!("error: {e}"),
            crate::login::WebState::Cancelled => "cancelled".to_string(),
            _ => "unfinished".to_string(),
        };
        let method = if login.is_refresh() {
            "refresh"
        } else {
            login.method().label()
        };
        self.after_login(&conn, &method.to_lowercase(), caller, &outcome)
            .await
    }

    /// A github.com client on the active route, or why there is none.
    pub async fn client(&self) -> Result<Client> {
        let conn = self.github_com().await;
        self.client_conn(&conn).await
    }

    /// A client for one connection on its active route.
    pub async fn client_conn(&self, conn: &Connection) -> Result<Client> {
        let (status, gh) = self.selector.select_conn(conn).await;
        match gh {
            Some(gh) => Ok(Client::with_conn(gh, self.core.clone(), conn.clone())),
            None => Err(GhError::Unavailable(status.label()).into()),
        }
    }

    /// A client for the connection a repository key belongs to.
    pub async fn client_for(&self, key: &str) -> Result<Client> {
        let conn = self.connection_of(key).await?;
        self.client_conn(&conn).await
    }

    pub fn last_poll(&self) -> Option<PollReport> {
        self.last.lock().unwrap().clone()
    }

    /// Wakes the poller for a pass now.
    pub fn poll_now(&self) {
        self.kick.notify_one();
    }

    /// Runs one pass (also what the poller does).
    pub async fn poll_once(&self) -> Result<PollReport> {
        let rep = poller::poll_once(&self.core, &self.selector).await?;
        if !rep.errors.is_empty() {
            self.selector.invalidate();
        }
        *self.last.lock().unwrap() = Some(rep.clone());
        Ok(rep)
    }

    /// Spawns the poller on the current runtime (once).
    pub fn spawn_poller(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                let interval = poller::interval_ms(&me.core).await;
                let next = if poller::enabled(&me.core).await {
                    match me.poll_once().await {
                        Ok(rep) => poller::next_at(&rep, interval),
                        Err(e) => {
                            tracing::warn!("forge poll: {e:#}");
                            now_ms() + interval
                        }
                    }
                } else {
                    now_ms() + interval
                };
                let wait = (next - now_ms()).max(1000) as u64;
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(wait)) => {}
                    _ = me.kick.notified() => {}
                }
            }
        });
    }

    /// The github.com rate budget.
    pub async fn quota(&self) -> Result<QuotaView> {
        let route = self
            .selector
            .current()
            .map(|s| s.route.key())
            .unwrap_or_else(|| "local".into());
        self.quota_at(&Connection::github_com(), &route).await
    }

    async fn quota_for(&self, conn: &Connection, route: &RouteStatus) -> Result<QuotaView> {
        self.quota_at(conn, &conn.route_key(&route.route)).await
    }

    async fn quota_at(&self, conn: &Connection, route: &str) -> Result<QuotaView> {
        let store = self.core.store();
        let calls = store.gh_calls_in_hour(route, now_ms()).await?;
        let mut core = store.gh_quota(route, "core").await?;
        let budget_known = core.is_some() || conn.dialect() != crate::Dialect::Gitea;
        if core.is_none() && conn.dialect() == crate::Dialect::Gitea {
            let limit = store
                .setting(kls_core::github::settings::CALLS_PER_HOUR)
                .await?
                .and_then(|s| s.trim().parse::<i64>().ok())
                .unwrap_or(crate::client::DEFAULT_CALLS_PER_HOUR);
            let used: i64 = calls.iter().map(|c| c.calls - c.not_modified).sum();
            core = Some(Quota {
                route: route.into(),
                resource: "core".into(),
                limit,
                remaining: (limit - used).max(0),
                used,
                reset_at: kls_store::github::hour_bucket(now_ms()) + 3_600_000,
                observed_at: now_ms(),
                secondary_until: 0,
            });
        }
        let b = Budget::load(store).await?;
        let limit = core.as_ref().map(|q| q.limit).unwrap_or(0);
        Ok(QuotaView {
            factory_used: factory_used(&calls),
            factory_limit: limit * b.factory_share_pct / 100,
            reserve: limit * b.reserve_pct / 100,
            budget: Some((b.factory_share_pct, b.reserve_pct)),
            route: route.into(),
            core,
            calls,
            budget_known,
        })
    }

    // ---- actions the screens run ------------------------------------------

    /// Lists accounts and orgs on every enabled, reachable connection. Fails
    /// only when none could be read (github.com's error first).
    pub async fn refresh_orgs(&self) -> Result<OrgRefresh> {
        let mut total = OrgRefresh::default();
        let mut first_err = None;
        let mut ok = false;
        for conn in self.connections().await?.into_iter().filter(|c| c.enabled) {
            let r = match self.client_conn(&conn).await {
                Ok(c) => sync::refresh_orgs(&c, USER, true).await,
                Err(e) => Err(e),
            };
            match r {
                Ok(r) => {
                    ok = true;
                    total.orgs += r.orgs;
                    total.repos += r.repos;
                    total.added.extend(r.added);
                }
                Err(e) if conn.is_github_com() || first_err.is_none() => first_err = Some(e),
                Err(e) => tracing::warn!(host = %conn.host, "refresh orgs: {e:#}"),
            }
        }
        match (ok, first_err) {
            (false, Some(e)) => Err(e),
            _ => Ok(total),
        }
    }

    async fn client_for_org(&self, login: &str) -> Result<Client> {
        match self.core.store().gh_org_forge(login).await? {
            Some((conn, _)) => {
                let c = self.connection(&conn).await?;
                self.client_conn(&c).await
            }
            None => self.client().await,
        }
    }

    pub async fn refresh_org_repos(&self, login: &str) -> Result<OrgRefresh> {
        let c = self.client_for_org(login).await?;
        let Some(org) = self.core.store().gh_org(login).await? else {
            bail!("unknown org {login}");
        };
        sync::refresh_org_repos(&c, &org, USER).await
    }

    pub async fn set_org_managed(&self, login: &str, managed: bool) -> Result<()> {
        if !self.core.store().gh_set_org_managed(login, managed).await? {
            bail!("unknown org {login}");
        }
        guard::audit(
            &self.core,
            USER,
            "org_managed",
            login,
            "",
            "ok",
            json!({ "managed": managed }),
        )
        .await;
        self.core
            .emit(
                kinds::ORG_UPDATED,
                login,
                &json!({ "org": login, "managed": managed, "by": "owner" }),
            )
            .await?;
        if managed && let Ok(c) = self.client_for_org(login).await {
            let org = self.core.store().gh_org(login).await?;
            if let Some(org) = org {
                sync::refresh_org_repos(&c, &org, USER).await?;
            }
        }
        Ok(())
    }

    pub async fn set_org_enabled(&self, login: &str, enabled: bool) -> Result<()> {
        if !self.core.store().gh_set_org_enabled(login, enabled).await? {
            bail!("unknown org {login}");
        }
        guard::audit(
            &self.core,
            USER,
            "org_enabled",
            login,
            "",
            "ok",
            json!({ "enabled": enabled }),
        )
        .await;
        self.core
            .emit(
                kinds::ORG_UPDATED,
                login,
                &json!({ "org": login, "enabled": enabled, "by": "owner" }),
            )
            .await?;
        Ok(())
    }

    /// Manage (or stop managing) repositories; a newly managed one is read at once.
    pub async fn set_repos_managed(&self, repos: &[String], managed: bool) -> Result<usize> {
        let store = self.core.store();
        let mut n = 0;
        for r in repos {
            if store.gh_set_repo_managed(r, managed).await? {
                n += 1;
            }
        }
        guard::audit(
            &self.core,
            USER,
            "repos_managed",
            &repos.join(","),
            "",
            "ok",
            json!({ "managed": managed, "count": n }),
        )
        .await;
        self.core
            .emit(
                kinds::REPO_UPDATED,
                "",
                &json!({ "repos": repos, "managed": managed, "by": "owner" }),
            )
            .await?;
        if managed {
            self.poll_now();
        }
        Ok(n)
    }

    pub async fn set_mirroring(&self, repos: &[String], m: Mirroring) -> Result<usize> {
        let store = self.core.store();
        let mut n = 0;
        for r in repos {
            if store.gh_set_repo_mirroring(r, m).await? {
                n += 1;
            }
        }
        guard::audit(
            &self.core,
            USER,
            "repos_mirroring",
            &repos.join(","),
            "",
            "ok",
            json!({ "pulls": m.pulls, "issues": m.issues, "labels": m.labels }),
        )
        .await;
        self.core
            .emit(
                kinds::REPO_UPDATED,
                "",
                &json!({ "repos": repos, "mirrorPulls": m.pulls, "mirrorIssues": m.issues, "syncLabels": m.labels, "by": "owner" }),
            )
            .await?;
        self.poll_now();
        Ok(n)
    }

    /// Saves settings at `revision`; a conflict is an error naming the stored revision.
    pub async fn save_repo_settings(
        &self,
        repo: &str,
        settings: &RepoSettings,
        revision: i64,
    ) -> Result<i64> {
        let store = self.core.store();
        match store.gh_set_repo_settings(repo, settings, revision).await? {
            Some(rev) => {
                guard::audit(
                    &self.core,
                    USER,
                    "repo_settings",
                    repo,
                    "",
                    "ok",
                    json!({ "revision": rev, "autonomy": settings.autonomy }),
                )
                .await;
                self.core
                    .emit(
                        kinds::REPO_SETTINGS,
                        repo,
                        &json!({ "repo": repo, "revision": rev, "by": "owner" }),
                    )
                    .await?;
                Ok(rev)
            }
            None => {
                let now = store.gh_repo(repo).await?.map(|r| r.revision);
                bail!(
                    "{repo} changed since it was opened (revision {}); reload and save again",
                    now.map(|r| r.to_string())
                        .unwrap_or_else(|| "unknown".into())
                )
            }
        }
    }

    pub async fn refresh_repo(&self, repo: &str) -> Result<GhRepo> {
        let c = self.client_for(repo).await?;
        let r = sync::refresh_repo(&c, repo, USER).await?;
        sync::sync_pulls(&c, repo, USER).await?;
        Ok(r)
    }

    pub async fn refresh_pull(&self, repo: &str, number: i64) -> Result<GhPull> {
        let c = self.client_for(repo).await?;
        sync::refresh_pull(&c, repo, number, USER).await
    }

    pub async fn pull_action(&self, repo: &str, number: i64, action: PullAction) -> Result<GhPull> {
        let c = self.client_for(repo).await?;
        sync::pull_action(&c, repo, number, &action, USER).await
    }

    pub async fn bulk_pull_action(
        &self,
        targets: &[(String, i64)],
        action: PullAction,
    ) -> Result<Vec<BulkResult>> {
        if targets.is_empty() || targets.len() > 50 {
            bail!("choose 1 to 50 pull requests");
        }
        // One client per connection; each target runs on its own.
        let mut by: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
        for (r, n) in targets {
            by.entry(self.connection_of(r).await?.id)
                .or_default()
                .push((r.clone(), *n));
        }
        let mut out = Vec::new();
        for (conn, ts) in by {
            let c = self.client_conn(&self.connection(&conn).await?).await?;
            out.extend(sync::bulk_pull_action(&c, &ts, &action, USER).await?);
        }
        Ok(out)
    }

    pub async fn sync_issues(&self, repo: &str) -> Result<usize> {
        let c = self.client_for(repo).await?;
        sync::sync_issues(&c, repo, USER).await
    }

    /// Creates the missing `sf:` labels in 1 to 20 repositories.
    pub async fn sync_labels(&self, repos: &[String]) -> Result<Vec<LabelSync>> {
        if repos.is_empty() || repos.len() > 20 {
            bail!("choose 1 to 20 repositories");
        }
        let mut out = Vec::new();
        for r in repos {
            if !valid_key(r) {
                bail!("not a repository: {r:?}");
            }
            let res = match self.client_for(r).await {
                Ok(c) => sync::sync_labels(&c, r, true, USER).await,
                Err(e) => Err(e),
            };
            out.push(match res {
                Ok(l) => l,
                Err(e) => LabelSync {
                    repo: r.clone(),
                    error: format!("{e:#}"),
                    ..Default::default()
                },
            });
        }
        Ok(out)
    }

    pub async fn load_conflicts(&self, repo: &str, number: i64) -> Result<ConflictSet> {
        let c = self.client_for(repo).await?;
        conflicts::load(&c, repo, number, USER).await
    }

    pub async fn commit_resolution(
        &self,
        set: &ConflictSet,
        resolved: &BTreeMap<String, Option<String>>,
        message: &str,
    ) -> Result<Resolution> {
        let c = self.client_for(&set.repo).await?;
        conflicts::commit(&c, set, resolved, message, USER).await
    }
}
