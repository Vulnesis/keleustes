//! Picks the route per connection: the node the connection prefers, else
//! the local CLI when it is signed in to the host, else a connected node
//! that reports a login (github.com through `gh_authenticated` until the
//! node protocol carries forge logins), else unavailable with the reasons.

use crate::forge::auth::auth_for;
use crate::forge::cli::LocalCli;
use crate::forge::{CliKind, Connection, RoutePref};
use crate::local::LocalGh;
use crate::node::{FORGE_JOB_KIND, NodeCli, NodeGh};
use crate::{ForgeCli, Route};
use kls_core::Core;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The active route of a connection and when it was checked.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RouteStatus {
    pub route: Route,
    pub checked_at: i64,
    /// The connection id ("github.com" by default).
    pub connection: String,
    pub cli: CliKind,
}

impl RouteStatus {
    pub fn label(&self) -> String {
        self.route.label_for(self.cli)
    }
}

/// How long a detected route is trusted before the CLI's status runs again.
pub const ROUTE_TTL_MS: i64 = 60_000;

type Cached = (RouteStatus, Arc<dyn ForgeCli>);

pub struct Selector {
    core: Core,
    local: Arc<LocalGh>,
    cached: Mutex<HashMap<String, Cached>>,
}

impl Selector {
    pub fn new(core: Core, local: LocalGh) -> Self {
        Self {
            core,
            local: Arc::new(local),
            cached: Mutex::new(HashMap::new()),
        }
    }

    /// The `gh` on this host.
    pub fn local(&self) -> &LocalGh {
        &self.local
    }

    /// The last detected github.com route without detecting again.
    pub fn current(&self) -> Option<RouteStatus> {
        self.current_for(kls_store::github::GITHUB_COM)
    }

    pub fn current_for(&self, conn_id: &str) -> Option<RouteStatus> {
        self.cached
            .lock()
            .unwrap()
            .get(conn_id)
            .map(|c| c.0.clone())
    }

    /// Forgets every cached route (after a call failed, or on demand).
    pub fn invalidate(&self) {
        self.cached.lock().unwrap().clear();
    }

    /// Forgets one connection's cached route.
    pub fn invalidate_conn(&self, conn_id: &str) {
        self.cached.lock().unwrap().remove(conn_id);
    }

    /// The github.com route (the stored github.com connection).
    pub async fn select(&self) -> (RouteStatus, Option<Arc<dyn ForgeCli>>) {
        let conn = self
            .core
            .store()
            .forge_connection(kls_store::github::GITHUB_COM)
            .await
            .ok()
            .flatten()
            .and_then(|r| Connection::from_row(&r))
            .unwrap_or_else(Connection::github_com);
        self.select_conn(&conn).await
    }

    /// The route of `conn`, detecting it when the cache is older than
    /// [`ROUTE_TTL_MS`]. Emits `github.route` when the route changed.
    pub async fn select_conn(&self, conn: &Connection) -> (RouteStatus, Option<Arc<dyn ForgeCli>>) {
        let now = kls_store::now_ms();
        if let Some((s, g)) = self.cached.lock().unwrap().get(&conn.id).cloned()
            && now - s.checked_at < ROUTE_TTL_MS
        {
            let gh = s.route.available().then_some(g);
            return (s, gh);
        }
        let (route, gh) = if conn.enabled {
            self.detect(conn).await
        } else {
            (
                Route::Unavailable {
                    why: format!("{} is disabled", conn.name()),
                },
                self.local.clone() as Arc<dyn ForgeCli>,
            )
        };
        let status = RouteStatus {
            route: route.clone(),
            checked_at: now,
            connection: conn.id.clone(),
            cli: conn.cli(),
        };
        let prev = self
            .cached
            .lock()
            .unwrap()
            .insert(conn.id.clone(), (status.clone(), gh.clone()))
            .map(|c| c.0.route);
        if prev.as_ref() != Some(&route) {
            let subject = if conn.is_github_com() {
                route.key()
            } else {
                conn.id.clone()
            };
            let _ = self
                .core
                .emit(
                    kls_core::github::kinds::ROUTE,
                    &subject,
                    &serde_json::json!({ "route": route, "label": status.label(),
                                         "connection": conn.id, "kind": conn.kind, "host": conn.host }),
                )
                .await;
        }
        let gh = route.available().then_some(gh);
        (status, gh)
    }

    async fn detect(&self, conn: &Connection) -> (Route, Arc<dyn ForgeCli>) {
        if let RoutePref::Node(id) = &conn.route_pref
            && let Ok(pick) = self.pick_node(conn, Some(id)).await
        {
            return self.node_route(conn, pick);
        }
        let local_why = if conn.is_github_com() {
            match self.local.auth_status().await {
                Ok(()) => return (Route::Local, self.local.clone()),
                Err(why) => why,
            }
        } else {
            let exec = self.local.exec();
            let st = auth_for(conn.cli()).status(exec.as_ref(), conn).await;
            if st.signed_in() {
                let cli = Arc::new(LocalCli::new(exec, conn));
                return (Route::Local, cli);
            }
            st.why().to_string()
        };
        let node_why = if conn.route_pref == RoutePref::LocalOnly {
            "the connection is set to the local CLI only".to_string()
        } else {
            match self.pick_node(conn, None).await {
                Ok(pick) => return self.node_route(conn, pick),
                Err(why) => why,
            }
        };
        (
            Route::Unavailable {
                why: format!("{local_why}; {node_why}"),
            },
            self.local.clone(),
        )
    }

    /// A `forge` job runner when the node advertises the login, else the
    /// old `github` job (github.com through `gh_authenticated`).
    fn node_route(
        &self,
        conn: &Connection,
        (id, name, forge): Picked,
    ) -> (Route, Arc<dyn ForgeCli>) {
        let cli: Arc<dyn ForgeCli> = if forge {
            Arc::new(NodeCli::new(
                self.core.clone(),
                &id,
                &name,
                conn.cli(),
                &conn.host,
            ))
        } else {
            Arc::new(NodeGh::new(self.core.clone(), &id, &name))
        };
        (Route::Node { id, name }, cli)
    }

    /// A non-revoked, online node that can reach `conn` (only `id` when
    /// given), by name: one whose `Capabilities.forges` lists a signed-in
    /// login for the connection's CLI and host, or (github.com only) an
    /// older node that reports `gh_authenticated`.
    async fn pick_node(&self, conn: &Connection, only: Option<&str>) -> Result<Picked, String> {
        let gw = self
            .core
            .gateway()
            .map_err(|_| "no node server in this process".to_string())?;
        let online = gw.online().await;
        let nodes = self
            .core
            .store()
            .nodes()
            .await
            .map_err(|e| format!("nodes: {e:#}"))?;
        let cli = conn.cli().name();
        let mut ok: Vec<(String, String, bool)> = nodes
            .into_iter()
            .filter(|n| {
                n.revoked_at.is_none() && online.contains(&n.id) && only.is_none_or(|id| n.id == id)
            })
            .filter_map(|n| node_reach(&n.host.capabilities, conn).map(|f| (n.id, n.name, f)))
            .collect();
        // Nodes with the forge login first, then by name.
        ok.sort_by(|a, b| b.2.cmp(&a.2).then(a.1.cmp(&b.1)).then(a.0.cmp(&b.0)));
        ok.into_iter().next().ok_or_else(|| {
            if conn.is_github_com() {
                "no connected node reports gh_authenticated".into()
            } else {
                format!("no connected node has a {cli} login for {}", conn.host)
            }
        })
    }
}

/// (node id, node name, runs `forge` jobs).
type Picked = (String, String, bool);

/// How a node with `caps` reaches `conn`: `Some(true)` by a `forge` job
/// (it lists a signed-in login for the CLI and host *and* runs `forge`
/// jobs), `Some(false)` by the old `github` job (github.com through
/// `gh_authenticated`: an older belai lists its logins but refuses `forge`
/// jobs), `None` not at all.
pub fn node_reach(caps: &kls_store::Capabilities, conn: &Connection) -> Option<bool> {
    let cli = conn.cli().name();
    let forge_jobs = caps.job_kinds.iter().any(|k| k == FORGE_JOB_KIND);
    let login = caps
        .forges
        .iter()
        .any(|f| f.signed_in && f.cli == cli && f.host.eq_ignore_ascii_case(&conn.host));
    if forge_jobs && login {
        Some(true)
    } else if conn.is_github_com() && caps.gh_authenticated {
        Some(false)
    } else {
        None
    }
}

impl Selector {
    /// The runner for `conn` on one node, by [`node_reach`] (`None` when
    /// the node cannot reach it). For drivers that name the node.
    pub fn node_runner(
        &self,
        conn: &Connection,
        node: &kls_store::Node,
    ) -> Option<(Route, Arc<dyn ForgeCli>)> {
        let forge = node_reach(&node.host.capabilities, conn)?;
        Some(self.node_route(conn, (node.id.clone(), node.name.clone(), forge)))
    }
}
