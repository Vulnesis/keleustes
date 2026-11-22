//! Jira Cloud: one project's issues, columns are their statuses (moved by
//! transition). REST v3 on `https://<site>.atlassian.net` with the sealed
//! `email:api-token` as basic auth. Descriptions are Atlassian Document
//! Format, read and written as plain paragraphs ([`adf`], [`from_adf`]).

use super::http::{self, Http, Request};
use super::{Binding, Change, Item, Remote};
use anyhow::{Result, anyhow, bail};
use base64::Engine;
use kls_vault::SecretString;
use serde_json::{Value, json};
use std::sync::Arc;

/// Issue pages read at most per pass (100 each).
pub const MAX_PAGES: usize = 20;

/// `(site host, project key)` from `https://acme.atlassian.net/browse/ENG`,
/// `…/jira/software/projects/ENG/boards/1` or `acme.atlassian.net/ENG`.
pub fn site_and_key(board: &str) -> Result<(String, String)> {
    let b = board.trim().trim_end_matches('/');
    let rest = b.strip_prefix("https://").unwrap_or(b);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    let site_ok = host.ends_with(".atlassian.net")
        && host.len() > ".atlassian.net".len()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    if !site_ok {
        bail!("name the Jira project as https://<site>.atlassian.net/browse/KEY");
    }
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let key = segs
        .iter()
        .position(|s| *s == "projects" || *s == "browse")
        .and_then(|i| segs.get(i + 1))
        .or(segs.last())
        .map(|s| s.split('-').next().unwrap_or(s).to_ascii_uppercase())
        .unwrap_or_default();
    let key_ok = key.len() >= 2
        && key.len() <= 20
        && key.starts_with(|c: char| c.is_ascii_uppercase())
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !key_ok {
        bail!("name the Jira project key (ENG) after the site");
    }
    Ok((host, key))
}

/// Plain text as an ADF document: one paragraph per line.
pub fn adf(text: &str) -> Value {
    let content: Vec<Value> = text
        .split('\n')
        .map(|l| {
            if l.is_empty() {
                json!({ "type": "paragraph", "content": [] })
            } else {
                json!({ "type": "paragraph", "content": [{ "type": "text", "text": l }] })
            }
        })
        .collect();
    json!({ "type": "doc", "version": 1, "content": content })
}

fn text_of(n: &Value, out: &mut String) {
    match n["type"].as_str() {
        Some("text") => out.push_str(n["text"].as_str().unwrap_or("")),
        Some("hardBreak") => out.push('\n'),
        _ => {
            for c in n["content"].as_array().into_iter().flatten() {
                text_of(c, out);
            }
        }
    }
}

/// An ADF document as plain text: each top-level block one line.
pub fn from_adf(doc: &Value) -> String {
    if let Some(s) = doc.as_str() {
        return s.to_string();
    }
    doc["content"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|b| {
            let mut s = String::new();
            text_of(b, &mut s);
            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub struct Jira {
    http: Arc<dyn Http>,
    auth: SecretString,
    site: String,
    key: String,
}

impl Jira {
    pub fn new(http: Arc<dyn Http>, token: SecretString, b: &Binding) -> Result<Self> {
        let (site, key) = site_and_key(&b.board)?;
        if !token.expose().contains(':') {
            bail!("the Jira token is email:api-token");
        }
        let auth = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(token.expose())
        );
        Ok(Self {
            http,
            auth: SecretString::new(auth),
            site,
            key,
        })
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let r = self
            .http
            .send(Request {
                method: method.into(),
                url: format!("https://{}{path}", self.site),
                headers: vec![
                    ("authorization".into(), self.auth.expose().to_string()),
                    ("accept".into(), "application/json".into()),
                ],
                body,
            })
            .await?;
        http::check(&r, "Jira")?;
        r.json()
    }

    fn item(&self, n: &Value) -> Item {
        let f = &n["fields"];
        let key = n["key"].as_str().unwrap_or("");
        super::clean_item(Item {
            id: n["id"].as_str().unwrap_or("").into(),
            url: format!("https://{}/browse/{key}", self.site),
            title: f["summary"].as_str().unwrap_or("").into(),
            body: from_adf(&f["description"]),
            column: f["status"]["name"].as_str().unwrap_or("").into(),
            on_board: true,
            handle: key.into(),
            ..Default::default()
        })
    }

    async fn move_to(&self, id: &str, column: &str) -> Result<()> {
        let v = self
            .call("GET", &format!("/rest/api/3/issue/{id}/transitions"), None)
            .await?;
        let t = v["transitions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| {
                t["to"]["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(column.trim()))
            })
            .and_then(|t| t["id"].as_str())
            .ok_or_else(|| anyhow!("Jira has no transition of {id} to {column}"))?;
        self.call(
            "POST",
            &format!("/rest/api/3/issue/{id}/transitions"),
            Some(json!({ "transition": { "id": t } })),
        )
        .await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Remote for Jira {
    async fn list(&self) -> Result<Vec<Item>> {
        let mut out = Vec::new();
        let mut token = Value::Null;
        for _ in 0..MAX_PAGES {
            let mut q = json!({
                "jql": format!("project = \"{}\" ORDER BY created ASC", self.key),
                "fields": ["summary", "description", "status"],
                "maxResults": 100,
            });
            if !token.is_null() {
                q["nextPageToken"] = token.clone();
            }
            let v = self.call("POST", "/rest/api/3/search/jql", Some(q)).await?;
            out.extend(
                v["issues"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|n| self.item(n)),
            );
            token = v["nextPageToken"].clone();
            if v["isLast"].as_bool() != Some(false) || token.is_null() {
                break;
            }
        }
        Ok(out)
    }

    async fn create(&self, title: &str, body: &str, column: Option<&str>) -> Result<Item> {
        let mut fields = json!({
            "project": { "key": self.key },
            "summary": title,
            "issuetype": { "name": "Task" },
        });
        if !body.is_empty() {
            fields["description"] = adf(body);
        }
        let v = self
            .call(
                "POST",
                "/rest/api/3/issue",
                Some(json!({ "fields": fields })),
            )
            .await?;
        let id = v["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Jira returned no issue id"))?;
        let key = v["key"].as_str().unwrap_or("");
        let mut it = super::clean_item(Item {
            id: id.into(),
            url: format!("https://{}/browse/{key}", self.site),
            title: title.into(),
            body: body.into(),
            on_board: true,
            handle: key.into(),
            ..Default::default()
        });
        if let Some(c) = column {
            self.move_to(id, c).await?;
            it.column = c.into();
        }
        Ok(it)
    }

    async fn update(&self, it: &Item, ch: &Change) -> Result<()> {
        let mut fields = json!({});
        if let Some(t) = &ch.title {
            fields["summary"] = json!(t);
        }
        if let Some(b) = &ch.body {
            fields["description"] = if b.is_empty() { Value::Null } else { adf(b) };
        }
        if fields.as_object().is_some_and(|o| !o.is_empty()) {
            self.call(
                "PUT",
                &format!("/rest/api/3/issue/{}", it.id),
                Some(json!({ "fields": fields })),
            )
            .await?;
        }
        if let Some(c) = &ch.column {
            self.move_to(&it.id, c).await?;
        }
        Ok(())
    }
}
