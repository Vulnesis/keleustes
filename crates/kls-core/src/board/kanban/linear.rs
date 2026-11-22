//! Linear: one team's issues, columns are its workflow states. GraphQL at
//! `https://api.linear.app/graphql`, the sealed personal API key in
//! `Authorization`.

use super::http::{self, Http, Request};
use super::{Binding, Change, Item, Remote};
use anyhow::{Result, anyhow, bail};
use kls_vault::SecretString;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub const API: &str = "https://api.linear.app/graphql";
/// Issue pages read at most per pass (100 each).
pub const MAX_PAGES: usize = 20;

/// The team key a board names: `ENG`, or a Linear URL holding `/team/ENG`.
pub fn team_key(board: &str) -> Result<String> {
    let b = board.trim().trim_end_matches('/');
    let key = match b.find("/team/") {
        Some(i) => b[i + 6..].split('/').next().unwrap_or(""),
        None if !b.contains('/') => b,
        None => "",
    };
    let ok = !key.is_empty()
        && key.len() <= 16
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        bail!("name the Linear team by its key (ENG) or a linear.app/…/team/ENG address");
    }
    Ok(key.to_ascii_uppercase())
}

/// A team id and its states `(id, name)`.
type Team = (String, Vec<(String, String)>);

pub struct Linear {
    http: Arc<dyn Http>,
    token: SecretString,
    key: String,
    url: String,
    /// The team id and its states `(id, name)`, from the last list.
    team: Mutex<Option<Team>>,
}

const LIST: &str = "query($key:String!,$after:String){teams(filter:{key:{eq:$key}}){nodes{id states{nodes{id name}} issues(first:100,after:$after){nodes{id url title description state{name}} pageInfo{hasNextPage endCursor}}}}}";
const CREATE: &str = "mutation($input:IssueCreateInput!){issueCreate(input:$input){success issue{id url title description state{name}}}}";
const UPDATE: &str =
    "mutation($id:String!,$input:IssueUpdateInput!){issueUpdate(id:$id,input:$input){success}}";

fn item(n: &Value) -> Item {
    let s = |k: &str| n[k].as_str().unwrap_or("").to_string();
    super::clean_item(Item {
        id: s("id"),
        url: s("url"),
        title: s("title"),
        body: s("description"),
        column: n["state"]["name"].as_str().unwrap_or("").into(),
        on_board: true,
        ..Default::default()
    })
}

impl Linear {
    pub fn new(http: Arc<dyn Http>, token: SecretString, b: &Binding) -> Result<Self> {
        let key = team_key(&b.board)?;
        Ok(Self {
            http,
            token,
            url: API.into(),
            key,
            team: Mutex::new(None),
        })
    }

    async fn gql(&self, query: &str, vars: Value) -> Result<Value> {
        let r = self
            .http
            .send(Request {
                method: "POST".into(),
                url: self.url.clone(),
                headers: vec![("authorization".into(), self.token.expose().to_string())],
                body: Some(json!({ "query": query, "variables": vars })),
            })
            .await?;
        http::check(&r, "Linear")?;
        let v = r.json()?;
        if let Some(e) = v["errors"].as_array().and_then(|a| a.first()) {
            let m = super::clean_title(e["message"].as_str().unwrap_or("error"));
            bail!("Linear: {}", m.chars().take(160).collect::<String>());
        }
        Ok(v["data"].clone())
    }

    async fn team(&self) -> Result<Team> {
        if let Some(t) = self.team.lock().unwrap().clone() {
            return Ok(t);
        }
        self.list().await?;
        self.team
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("no Linear team {}", self.key))
    }

    async fn state_id(&self, column: &str) -> Result<String> {
        let (_, states) = self.team().await?;
        states
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(column.trim()))
            .map(|(id, _)| id.clone())
            .ok_or_else(|| anyhow!("Linear team {} has no state {column}", self.key))
    }
}

#[async_trait::async_trait]
impl Remote for Linear {
    async fn list(&self) -> Result<Vec<Item>> {
        let mut out = Vec::new();
        let mut after = Value::Null;
        for _ in 0..MAX_PAGES {
            let d = self
                .gql(LIST, json!({ "key": self.key, "after": after }))
                .await?;
            let Some(t) = d["teams"]["nodes"]
                .as_array()
                .and_then(|a| a.first())
                .cloned()
            else {
                bail!("no Linear team {}", self.key);
            };
            let states = t["states"]["nodes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|s| {
                            (
                                s["id"].as_str().unwrap_or("").to_string(),
                                s["name"].as_str().unwrap_or("").to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            *self.team.lock().unwrap() = Some((t["id"].as_str().unwrap_or("").to_string(), states));
            let issues = &t["issues"];
            out.extend(issues["nodes"].as_array().into_iter().flatten().map(item));
            if issues["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
                break;
            }
            after = issues["pageInfo"]["endCursor"].clone();
        }
        Ok(out)
    }

    async fn create(&self, title: &str, body: &str, column: Option<&str>) -> Result<Item> {
        let (team, _) = self.team().await?;
        let mut input = json!({ "teamId": team, "title": title, "description": body });
        if let Some(c) = column {
            input["stateId"] = json!(self.state_id(c).await?);
        }
        let d = self.gql(CREATE, json!({ "input": input })).await?;
        let i = &d["issueCreate"]["issue"];
        if d["issueCreate"]["success"].as_bool() != Some(true) || i["id"].as_str().is_none() {
            bail!("Linear did not create the issue");
        }
        Ok(item(i))
    }

    async fn update(&self, it: &Item, ch: &Change) -> Result<()> {
        let mut input = json!({});
        if let Some(t) = &ch.title {
            input["title"] = json!(t);
        }
        if let Some(b) = &ch.body {
            input["description"] = json!(b);
        }
        if let Some(c) = &ch.column {
            input["stateId"] = json!(self.state_id(c).await?);
        }
        let d = self
            .gql(UPDATE, json!({ "id": it.id, "input": input }))
            .await?;
        if d["issueUpdate"]["success"].as_bool() != Some(true) {
            bail!("Linear did not update {}", it.id);
        }
        Ok(())
    }
}
