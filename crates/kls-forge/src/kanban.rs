//! The forge providers of the external kanban binding
//! (`kls_core::board::kanban`): GitHub Projects (GraphQL through `gh api
//! graphql`) and GitLab issue boards (REST through `glab api`). Both run on
//! the connection's CLI route like every forge call: no token is held here,
//! every write is checked against the kill switch, counted against the
//! proactive write cap and audited by [`Client::request`], and the sync
//! engine audits each card-level change on top.
//!
//! - **GitHub Projects**: columns are the options of the project's `Status`
//!   field. Cards become draft issues; an item backed by a repository issue
//!   or pull request syncs its column only (its text belongs to the issue
//!   mirror).
//! - **GitLab boards**: columns are the board's list labels, plus `Open` and
//!   `Closed`. Moving an issue swaps its list label (or closes/reopens it).

use crate::forge::Connection;
use crate::http::encode_component;
use crate::{Client, Service};
use anyhow::{Result, anyhow, bail};
use kls_core::Core;
use kls_core::board::kanban::{self, Binding, Change, Connector, Item, Remote};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// The caller tag on every forge call the binding makes (automation).
pub const CALLER: &str = "kanban";
/// Item pages read at most per pass (100 each).
pub const MAX_PAGES: usize = 20;

/// Registers the forge connector and starts the binding's tick.
pub fn start(svc: &Arc<Service>) {
    kanban::register_connector(svc.core(), Arc::new(ForgeConnector(svc.clone())));
    if tokio::runtime::Handle::try_current().is_ok() {
        kanban::start(svc.core());
    }
}

/// Serves `github_projects` and `gitlab_boards` bindings.
pub struct ForgeConnector(pub Arc<Service>);

impl ForgeConnector {
    async fn client(&self, host: &str) -> Result<Client> {
        if host == "github.com" {
            return self.0.client().await;
        }
        let conns = self.0.connections().await?;
        let c: &Connection = conns
            .iter()
            .find(|c| c.host.eq_ignore_ascii_case(host))
            .ok_or_else(|| anyhow!("no forge connection for {host}: add it under Repos"))?;
        self.0.client_conn(c).await
    }
}

#[async_trait::async_trait]
impl Connector for ForgeConnector {
    async fn remote(&self, _core: &Core, b: &Binding) -> Result<Option<Arc<dyn Remote>>> {
        match b.provider.as_str() {
            "github_projects" => {
                let p = ProjectRef::parse(&b.board)?;
                let c = self.client(&p.host).await?;
                Ok(Some(Arc::new(Projects::new(c, p))))
            }
            "gitlab_boards" => {
                let r = BoardRef::parse(&b.board)?;
                let c = self.client(&r.host).await?;
                Ok(Some(Arc::new(GitlabBoard::new(c, r, b.columns()))))
            }
            _ => Ok(None),
        }
    }
}

fn ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

// ---- GitHub Projects -----------------------------------------------------------

/// A project address: `https://github.com/orgs/acme/projects/3`
/// (`/users/<login>/` for a personal one; a GHES host works the same).
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectRef {
    pub host: String,
    /// `organization` or `user`.
    pub owner: &'static str,
    pub login: String,
    pub number: i64,
}

impl ProjectRef {
    pub fn parse(board: &str) -> Result<Self> {
        let b = board.trim();
        let rest = b.strip_prefix("https://").unwrap_or(b);
        let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        let bad = || anyhow!("name the project as https://github.com/orgs/<org>/projects/<n>");
        if segs.len() < 5 || segs[3] != "projects" {
            return Err(bad());
        }
        let owner = match segs[1] {
            "orgs" => "organization",
            "users" => "user",
            _ => return Err(bad()),
        };
        let number: i64 = segs[4].parse().map_err(|_| bad())?;
        if !ident(segs[2]) || number <= 0 || !ident(segs[0]) {
            return Err(bad());
        }
        Ok(Self {
            host: segs[0].to_ascii_lowercase(),
            owner,
            login: segs[2].into(),
            number,
        })
    }
}

#[derive(Clone, Debug, Default)]
struct ProjectMeta {
    id: String,
    url: String,
    field: String,
    /// Status options `(id, name)`.
    options: Vec<(String, String)>,
}

pub struct Projects {
    c: Client,
    p: ProjectRef,
    meta: Mutex<Option<ProjectMeta>>,
}

const ITEM_FIELDS: &str = "id type fieldValueByName(name:\"Status\"){... on ProjectV2ItemFieldSingleSelectValue{name}} content{... on DraftIssue{id title body} ... on Issue{title body url} ... on PullRequest{title body url}}";

impl Projects {
    pub fn new(c: Client, p: ProjectRef) -> Self {
        Self {
            c,
            p,
            meta: Mutex::new(None),
        }
    }

    async fn gql(&self, query: &str, vars: Value) -> Result<Value> {
        let body = json!({ "query": query, "variables": vars });
        let v = self
            .c
            .request("POST", "graphql", Some(&body), CALLER)
            .await?
            .json()?;
        if let Some(e) = v["errors"].as_array().and_then(|a| a.first()) {
            let m = kanban::clean_title(e["message"].as_str().unwrap_or("error"));
            bail!("GitHub: {}", m.chars().take(160).collect::<String>());
        }
        Ok(v["data"].clone())
    }

    async fn meta(&self) -> Result<ProjectMeta> {
        if let Some(m) = self.meta.lock().unwrap().clone() {
            return Ok(m);
        }
        self.list().await?;
        self.meta
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("project not found"))
    }

    fn item(&self, n: &Value, project_url: &str) -> Option<Item> {
        let kind = n["type"].as_str().unwrap_or("");
        let content = &n["content"];
        if content.is_null() {
            return None;
        }
        let s = |v: &Value| v.as_str().unwrap_or("").to_string();
        let draft = kind == "DRAFT_ISSUE";
        Some(kanban::clean_item(Item {
            id: s(&n["id"]),
            url: if draft {
                project_url.to_string()
            } else {
                s(&content["url"])
            },
            title: s(&content["title"]),
            body: s(&content["body"]),
            column: s(&n["fieldValueByName"]["name"]),
            locked: !draft,
            on_board: true,
            handle: if draft {
                s(&content["id"])
            } else {
                String::new()
            },
        }))
    }

    async fn set_status(&self, m: &ProjectMeta, item: &str, column: &str) -> Result<()> {
        let opt = m
            .options
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(column.trim()))
            .map(|(id, _)| id.clone())
            .ok_or_else(|| anyhow!("the project's Status has no option {column}"))?;
        self.gql(
            "mutation($input:UpdateProjectV2ItemFieldValueInput!){updateProjectV2ItemFieldValue(input:$input){projectV2Item{id}}}",
            json!({ "input": { "projectId": m.id, "itemId": item, "fieldId": m.field, "value": { "singleSelectOptionId": opt } } }),
        )
        .await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Remote for Projects {
    async fn list(&self) -> Result<Vec<Item>> {
        let q = format!(
            "query($login:String!,$n:Int!,$after:String){{{}(login:$login){{projectV2(number:$n){{id url field(name:\"Status\"){{... on ProjectV2SingleSelectField{{id options{{id name}}}}}} items(first:100,after:$after){{nodes{{{ITEM_FIELDS}}} pageInfo{{hasNextPage endCursor}}}}}}}}}}",
            self.p.owner
        );
        let mut out = Vec::new();
        let mut after = Value::Null;
        for _ in 0..MAX_PAGES {
            let d = self
                .gql(
                    &q,
                    json!({ "login": self.p.login, "n": self.p.number, "after": after }),
                )
                .await?;
            let pv = &d[self.p.owner]["projectV2"];
            if pv.is_null() {
                bail!("no project {} of {}", self.p.number, self.p.login);
            }
            let m = ProjectMeta {
                id: pv["id"].as_str().unwrap_or("").into(),
                url: pv["url"].as_str().unwrap_or("").into(),
                field: pv["field"]["id"].as_str().unwrap_or("").into(),
                options: pv["field"]["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|o| {
                        (
                            o["id"].as_str().unwrap_or("").to_string(),
                            o["name"].as_str().unwrap_or("").to_string(),
                        )
                    })
                    .collect(),
            };
            let url = m.url.clone();
            *self.meta.lock().unwrap() = Some(m);
            let items = &pv["items"];
            out.extend(
                items["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|n| self.item(n, &url)),
            );
            if items["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
                break;
            }
            after = items["pageInfo"]["endCursor"].clone();
        }
        Ok(out)
    }

    async fn create(&self, title: &str, body: &str, column: Option<&str>) -> Result<Item> {
        let m = self.meta().await?;
        let d = self
            .gql(
                "mutation($input:AddProjectV2DraftIssueInput!){addProjectV2DraftIssue(input:$input){projectItem{id content{... on DraftIssue{id}}}}}",
                json!({ "input": { "projectId": m.id, "title": title, "body": body } }),
            )
            .await?;
        let pi = &d["addProjectV2DraftIssue"]["projectItem"];
        let id = pi["id"]
            .as_str()
            .ok_or_else(|| anyhow!("GitHub returned no project item"))?
            .to_string();
        let mut it = kanban::clean_item(Item {
            id: id.clone(),
            url: m.url.clone(),
            title: title.into(),
            body: body.into(),
            on_board: true,
            handle: pi["content"]["id"].as_str().unwrap_or("").into(),
            ..Default::default()
        });
        if let Some(c) = column {
            self.set_status(&m, &id, c).await?;
            it.column = c.into();
        }
        Ok(it)
    }

    async fn update(&self, it: &Item, ch: &Change) -> Result<()> {
        let m = self.meta().await?;
        if ch.title.is_some() || ch.body.is_some() {
            if it.locked || it.handle.is_empty() {
                bail!(
                    "{} is a repository item: its text is synced by the issue mirror",
                    it.id
                );
            }
            let mut input = json!({ "draftIssueId": it.handle });
            if let Some(t) = &ch.title {
                input["title"] = json!(t);
            }
            if let Some(b) = &ch.body {
                input["body"] = json!(b);
            }
            self.gql(
                "mutation($input:UpdateProjectV2DraftIssueInput!){updateProjectV2DraftIssue(input:$input){draftIssue{id}}}",
                json!({ "input": input }),
            )
            .await?;
        }
        if let Some(c) = &ch.column {
            self.set_status(&m, &it.id, c).await?;
        }
        Ok(())
    }
}

// ---- GitLab boards -----------------------------------------------------------------

/// A GitLab board: `https://gitlab.com/grp/app/-/boards/1`, or `grp/app`
/// on gitlab.com.
#[derive(Clone, Debug, PartialEq)]
pub struct BoardRef {
    pub host: String,
    pub project: String,
}

impl BoardRef {
    pub fn parse(board: &str) -> Result<Self> {
        let b = board.trim().trim_end_matches('/');
        let bad = || anyhow!("name the board as https://gitlab.com/<group>/<project>/-/boards/<n>");
        let (host, path) = match b.strip_prefix("https://") {
            Some(r) => r.split_once('/').ok_or_else(bad)?,
            None => ("gitlab.com", b),
        };
        let path = path.split("/-/").next().unwrap_or("");
        let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if segs.len() < 2 || !segs.iter().all(|s| ident(s)) || !ident(host) {
            return Err(bad());
        }
        Ok(Self {
            host: host.to_ascii_lowercase(),
            project: segs.join("/"),
        })
    }
}

pub struct GitlabBoard {
    c: Client,
    r: BoardRef,
    /// The mapped columns (list labels, `Open`, `Closed`).
    columns: Vec<String>,
}

fn is(c: &str, name: &str) -> bool {
    c.trim().eq_ignore_ascii_case(name)
}

impl GitlabBoard {
    pub fn new(c: Client, r: BoardRef, columns: Vec<String>) -> Self {
        Self { c, r, columns }
    }

    fn base(&self) -> String {
        format!("projects/{}/issues", encode_component(&self.r.project))
    }

    fn list_labels(&self) -> impl Iterator<Item = &String> {
        self.columns
            .iter()
            .filter(|c| !is(c, "open") && !is(c, "closed"))
    }

    fn item(&self, n: &Value) -> Item {
        let labels: Vec<&str> = n["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let closed = n["state"].as_str() == Some("closed");
        let on_list = self
            .list_labels()
            .find(|c| labels.iter().any(|l| is(l, c)))
            .cloned();
        let column = match (&on_list, closed) {
            (Some(c), _) => c.clone(),
            (None, true) => "Closed".into(),
            (None, false) => "Open".into(),
        };
        let mapped = self.columns.iter().any(|c| is(c, &column));
        kanban::clean_item(Item {
            id: n["iid"].as_i64().map(|i| i.to_string()).unwrap_or_default(),
            url: n["web_url"].as_str().unwrap_or("").into(),
            title: n["title"].as_str().unwrap_or("").into(),
            body: n["description"].as_str().unwrap_or("").into(),
            column,
            on_board: mapped,
            ..Default::default()
        })
    }

    /// The label and state changes that put an issue in `column`.
    fn moves(&self, column: &str, body: &mut Value) {
        let others: Vec<String> = self
            .list_labels()
            .filter(|c| !is(c, column))
            .cloned()
            .collect();
        if is(column, "closed") {
            body["state_event"] = json!("close");
        } else {
            body["state_event"] = json!("reopen");
            if !is(column, "open") {
                body["add_labels"] = json!(column);
            }
        }
        if !others.is_empty() {
            body["remove_labels"] = json!(others.join(","));
        }
    }
}

#[async_trait::async_trait]
impl Remote for GitlabBoard {
    async fn list(&self) -> Result<Vec<Item>> {
        let p = self
            .c
            .get_all(&format!("{}?state=all&per_page=100", self.base()), CALLER)
            .await?;
        Ok(p.items
            .iter()
            .take(MAX_PAGES * 100)
            .map(|n| self.item(n))
            .filter(|i| !i.id.is_empty())
            .collect())
    }

    async fn create(&self, title: &str, body: &str, column: Option<&str>) -> Result<Item> {
        let mut req = json!({ "title": title, "description": body });
        if let Some(c) = column.filter(|c| !is(c, "open") && !is(c, "closed")) {
            req["labels"] = json!(c);
        }
        let v = self
            .c
            .request("POST", &self.base(), Some(&req), CALLER)
            .await?
            .json()?;
        let mut it = self.item(&v);
        if it.id.is_empty() {
            bail!("GitLab returned no issue");
        }
        if let Some(c) = column.filter(|c| is(c, "closed")) {
            self.update(
                &it,
                &Change {
                    column: Some(c.into()),
                    ..Default::default()
                },
            )
            .await?;
            it.column = c.into();
        }
        Ok(it)
    }

    async fn update(&self, it: &Item, ch: &Change) -> Result<()> {
        let mut req = json!({});
        if let Some(t) = &ch.title {
            req["title"] = json!(t);
        }
        if let Some(b) = &ch.body {
            req["description"] = json!(b);
        }
        if let Some(c) = &ch.column {
            self.moves(c, &mut req);
        }
        self.c
            .request(
                "PUT",
                &format!("{}/{}", self.base(), it.id),
                Some(&req),
                CALLER,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeGh, Reply};
    use crate::forge::{CliKind, ForgeKind};
    use crate::{GhError, Route};
    use kls_core::board::kanban::{Direction, parse_lists, sync_with};

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Core::open(dir.path(), None).await.unwrap(), dir)
    }

    fn reply(v: Value) -> Reply {
        Reply {
            status: 200,
            body: v.to_string(),
            headers: Vec::new(),
        }
    }

    fn project(items: Value) -> Value {
        json!({ "data": { "organization": { "projectV2": {
            "id": "PVT_1", "url": "https://github.com/orgs/acme/projects/3",
            "field": { "id": "F_status", "options": [{ "id": "o_todo", "name": "Todo" }, { "id": "o_doing", "name": "In Progress" }] },
            "items": { "nodes": items, "pageInfo": { "hasNextPage": false, "endCursor": null } }
        } } } })
    }

    fn binding(provider: &str, board: &str) -> Binding {
        Binding {
            provider: provider.into(),
            board: board.into(),
            sync: Direction::Both,
            lists: parse_lists(&[
                "todo=Todo".into(),
                "doing=In Progress".into(),
                "done=Closed".into(),
            ]),
        }
    }

    #[test]
    fn addresses_parse() {
        let p = ProjectRef::parse("https://github.com/orgs/acme/projects/3/views/1").unwrap();
        assert_eq!(
            (p.host.as_str(), p.owner, p.login.as_str(), p.number),
            ("github.com", "organization", "acme", 3)
        );
        assert_eq!(
            ProjectRef::parse("github.com/users/bob/projects/2")
                .unwrap()
                .owner,
            "user"
        );
        assert!(ProjectRef::parse("https://github.com/acme/app").is_err());
        let b = BoardRef::parse("https://gitlab.acme.dev/grp/sub/app/-/boards/4").unwrap();
        assert_eq!(
            (b.host.as_str(), b.project.as_str()),
            ("gitlab.acme.dev", "grp/sub/app")
        );
        assert_eq!(BoardRef::parse("grp/app").unwrap().host, "gitlab.com");
        assert!(BoardRef::parse("app").is_err());
    }

    #[tokio::test]
    async fn github_projects_round_trip_through_gh_graphql() {
        let (core, _d) = core().await;
        let gh = FakeGh::new();
        let draft = json!([
            { "id": "PVTI_d", "type": "DRAFT_ISSUE", "fieldValueByName": { "name": "Todo" },
              "content": { "id": "DI_1", "title": "Draft \u{202e}one", "body": "b" } },
            { "id": "PVTI_i", "type": "ISSUE", "fieldValueByName": { "name": "In Progress" },
              "content": { "title": "Repo issue", "body": "x", "url": "https://github.com/acme/app/issues/7" } },
            { "id": "PVTI_r", "type": "REDACTED", "content": null }
        ]);
        let ok = reply(json!({ "data": {} }));
        gh.on_seq(
            "POST",
            "graphql",
            vec![
                reply(project(draft.clone())),
                // The local card's create: draft, then its status.
                reply(json!({ "data": { "addProjectV2DraftIssue": { "projectItem": { "id": "PVTI_new", "content": { "id": "DI_2" } } } } })),
                ok.clone(),
            ],
        );
        let c = Client::new(gh.clone(), core.clone());
        let p = Projects::new(
            c,
            ProjectRef::parse("https://github.com/orgs/acme/projects/3").unwrap(),
        );
        let items = p.list().await.unwrap();
        assert_eq!(items.len(), 2, "redacted items skipped");
        assert_eq!(items[0].title, "Draft one");
        assert!(items[1].locked && items[1].url.ends_with("/issues/7"));

        let b = binding("github_projects", "https://github.com/orgs/acme/projects/3");
        core.create_card(
            kls_core::board::CardDraft {
                title: "Local".into(),
                status: "doing".into(),
                ..Default::default()
            },
            "owner",
        )
        .await
        .unwrap();
        gh.on_seq(
            "POST",
            "graphql",
            vec![
                reply(project(draft.clone())),
                reply(json!({ "data": { "addProjectV2DraftIssue": { "projectItem": { "id": "PVTI_new", "content": { "id": "DI_2" } } } } })),
                ok.clone(),
            ],
        );
        gh.clear_calls();
        let rep = sync_with(&core, &b, &p, "tick").await.unwrap();
        assert_eq!((rep.imported, rep.created), (2, 1), "{rep:?}");
        let calls = gh.calls();
        let status = calls
            .iter()
            .filter_map(|c| c.stdin.as_deref())
            .find(|s| s.contains("updateProjectV2ItemFieldValue"))
            .unwrap();
        let v: Value = serde_json::from_str(status).unwrap();
        assert_eq!(
            v["variables"]["input"]["value"]["singleSelectOptionId"],
            "o_doing"
        );
        assert_eq!(v["variables"]["input"]["itemId"], "PVTI_new");
        // Writes are forge-audited with the kanban caller.
        let forge = core
            .audit_entries(&kls_store::audit::LedgerQuery {
                kinds: vec!["forge".into()],
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(forge.iter().any(|e| e.payload["caller"] == CALLER));
        // A repository item's text is not written from here.
        let e = p
            .update(
                &items[1],
                &Change {
                    title: Some("x".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("issue mirror"));
    }

    #[tokio::test]
    async fn gitlab_board_moves_by_list_label_and_obeys_the_kill_switch() {
        let (core, _d) = core().await;
        let conn = Connection::new(ForgeKind::Gitlab, "gitlab.acme.dev", "user").unwrap();
        let gl = FakeGh::with_cli(Route::Local, CliKind::Glab);
        let c = Client::with_conn(gl.clone(), core.clone(), conn);
        let r = BoardRef::parse("https://gitlab.acme.dev/grp/app/-/boards/1").unwrap();
        let b = binding(
            "gitlab_boards",
            "https://gitlab.acme.dev/grp/app/-/boards/1",
        );
        let g = GitlabBoard::new(c, r, b.columns());
        let base = "projects/grp%2Fapp/issues";
        gl.on(
            "GET",
            &format!("{base}?state=all&per_page=100"),
            200,
            json!([
                { "iid": 1, "title": "On a list", "description": "", "state": "opened", "labels": ["Todo", "bug"], "web_url": "https://gitlab.acme.dev/grp/app/-/issues/1" },
                { "iid": 2, "title": "Not on the board", "description": "", "state": "opened", "labels": ["bug"], "web_url": "u2" },
                { "iid": 3, "title": "Old", "description": "", "state": "closed", "labels": [], "web_url": "u3" }
            ]),
        );
        gl.on("PUT", &format!("{base}/1"), 200, json!({ "iid": 1 }));
        let items = g.list().await.unwrap();
        assert_eq!(items[0].column, "Todo");
        assert!(items[0].on_board && !items[1].on_board);
        assert_eq!(items[2].column, "Closed");
        // Imports: only the listed open one (closed maps to done, not imported).
        let rep = sync_with(&core, &b, &g, "tick").await.unwrap();
        assert_eq!(rep.imported, 1, "{rep:?}");
        // Move to In Progress: label swapped, reopened.
        g.update(
            &items[0],
            &Change {
                column: Some("In Progress".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let put = gl
            .calls()
            .into_iter()
            .rev()
            .find(|c| c.method == "PUT")
            .unwrap();
        let v: Value = serde_json::from_str(put.stdin.as_deref().unwrap()).unwrap();
        assert_eq!(v["add_labels"], "In Progress");
        assert_eq!(v["remove_labels"], "Todo");
        // Stopped: the forge write is refused and audited.
        kls_core::control::stop(&core, "drill").await.unwrap();
        let e = g
            .update(
                &items[0],
                &Change {
                    column: Some("Closed".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            e.downcast_ref::<GhError>(),
            Some(GhError::Stopped(_))
        ));
    }
}
