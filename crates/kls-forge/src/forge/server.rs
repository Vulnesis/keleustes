//! The server behind a connection (forges.md 2.3, 2.4, 6.2): product and
//! version from `/api/v4/version` (GitLab), `/api/v1/version` (Gitea and
//! Forgejo: Forgejo answers `<v>+gitea-<compat>`) or `/api/v3/meta`
//! (GHES `installed_version`).
//!
//! Read through the connection's route once signed in; before that, one
//! unauthenticated GET (most GitLab instances answer 401 there, so the
//! version stays unknown and the sign-in methods fall back to the CLI's
//! own flags). The result decides whether glab's device flow is offered
//! (GitLab 17.9 or later) and fills the Connect dialog's server chip.

use super::kind::{Connection, Dialect, ForgeKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// `glab auth login --device` needs GitLab 17.9 or later (glab's help).
pub const GITLAB_DEVICE_MIN: (u64, u64) = (17, 9);

/// What the server said about itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerInfo {
    /// "GitLab", "Gitea", "Forgejo", "GitHub Enterprise" ("" when unknown).
    pub product: String,
    /// The product's own version ("18.4.0-ee", "13.0.2", "3.15.2").
    pub version: String,
    /// The Gitea API level a Forgejo server reports (`+gitea-1.22.0`).
    pub gitea_compat: String,
    /// "route" (the CLI, signed in) or "http" (unauthenticated).
    pub via: String,
    pub error: String,
    /// Unix ms.
    pub at: i64,
}

impl ServerInfo {
    pub fn known(&self) -> bool {
        !self.version.is_empty()
    }

    /// The kind the server says it is (Forgejo told apart from Gitea).
    pub fn kind(&self) -> Option<ForgeKind> {
        match self.product.as_str() {
            "GitLab" => Some(ForgeKind::Gitlab),
            "Gitea" => Some(ForgeKind::Gitea),
            "Forgejo" => Some(ForgeKind::Forgejo),
            "GitHub Enterprise" => Some(ForgeKind::Ghes),
            _ => None,
        }
    }

    /// Whether glab's device flow can work here: `Some(false)` on a GitLab
    /// older than 17.9, `None` when the version is unknown or not GitLab.
    pub fn device_flow(&self) -> Option<bool> {
        (self.product == "GitLab" && self.known())
            .then(|| at_least(&self.version, GITLAB_DEVICE_MIN))
    }

    /// "GitLab 18.4", "Forgejo 13.0" ("" when unknown).
    pub fn chip(&self) -> String {
        if !self.known() {
            return String::new();
        }
        let short: Vec<&str> = self.version.split(['.', '-', '+']).take(2).collect();
        format!("{} {}", self.product, short.join("."))
    }
}

/// `(major, minor)` of a version string ("v18.4.0-ee" -> (18, 4)).
pub fn major_minor(v: &str) -> Option<(u64, u64)> {
    let mut it = v
        .trim()
        .trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .map(str::parse::<u64>);
    let major = it.next()?.ok()?;
    let minor = it.next().and_then(Result::ok).unwrap_or(0);
    Some((major, minor))
}

/// Whether `v` is at least `min` (false when unparsable).
pub fn at_least(v: &str, min: (u64, u64)) -> bool {
    major_minor(v).is_some_and(|mm| mm >= min)
}

/// The version path through the CLI's own API root.
pub fn route_path(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::GitHub => "/meta",
        Dialect::GitLab => "version",
        Dialect::Gitea => "/version",
    }
}

/// The unauthenticated URL (github.com has no server version).
pub fn http_url(conn: &Connection) -> Option<String> {
    let path = match conn.kind.dialect() {
        Dialect::GitHub if conn.host == "github.com" => return None,
        Dialect::GitHub => "/meta",
        Dialect::GitLab => "/version",
        Dialect::Gitea => "/version",
    };
    Some(format!("{}{path}", conn.api_base()))
}

/// Reads a version body for a connection of `kind`.
pub fn parse(kind: ForgeKind, body: &Value) -> ServerInfo {
    let mut s = ServerInfo::default();
    match kind.dialect() {
        Dialect::GitHub => {
            if let Some(v) = body["installed_version"].as_str() {
                s.product = "GitHub Enterprise".into();
                s.version = v.into();
            }
        }
        Dialect::GitLab => {
            if let Some(v) = body["version"].as_str() {
                s.product = "GitLab".into();
                s.version = v.into();
            }
        }
        Dialect::Gitea => {
            if let Some(v) = body["version"].as_str() {
                // Forgejo: "13.0.2+gitea-1.22.0" (as forge.IsForgejo).
                match v.split_once("+gitea-") {
                    Some((own, compat)) => {
                        s.product = "Forgejo".into();
                        s.version = own.into();
                        s.gitea_compat = compat.into();
                    }
                    None => {
                        s.product = "Gitea".into();
                        s.version = v.into();
                    }
                }
            }
        }
    }
    if !s.known() {
        s.error = "no version in the answer".into();
    }
    s
}

/// One unauthenticated GET of `url` (5 s): status and JSON body.
pub async fn http_get(url: &str) -> anyhow::Result<(u16, Value)> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .user_agent(concat!("keleustes/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let r = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await?;
    let status = r.status().as_u16();
    let text = r.text().await.unwrap_or_default();
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

/// A [`ServerInfo`] from an unauthenticated answer.
pub fn from_http(kind: ForgeKind, status: u16, body: &Value) -> ServerInfo {
    let mut s = if (200..300).contains(&status) {
        parse(kind, body)
    } else {
        ServerInfo {
            error: match status {
                401 | 403 => "the version needs a sign-in on this server".into(),
                _ => format!("version: HTTP {status}"),
            },
            ..Default::default()
        }
    };
    s.via = "http".into();
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions_and_products() {
        let gl = parse(
            ForgeKind::Gitlab,
            &json!({"version": "18.4.0-ee", "revision": "x"}),
        );
        assert_eq!(
            (gl.product.as_str(), gl.chip().as_str()),
            ("GitLab", "GitLab 18.4")
        );
        assert_eq!(gl.device_flow(), Some(true));
        let old = parse(ForgeKind::Gitlab, &json!({"version": "17.8.2"}));
        assert_eq!(old.device_flow(), Some(false));
        assert_eq!(
            parse(ForgeKind::Gitlab, &json!({"version": "17.9.0"})).device_flow(),
            Some(true)
        );
        // Recorded 2026-11-28: codeberg.org/forgejo/forgejo:13 and gitea/gitea:1.22.
        let fj = parse(ForgeKind::Gitea, &json!({"version": "13.0.5+gitea-1.22.0"}));
        assert_eq!(fj.kind(), Some(ForgeKind::Forgejo));
        assert_eq!(
            (fj.version.as_str(), fj.gitea_compat.as_str()),
            ("13.0.5", "1.22.0")
        );
        assert_eq!(fj.chip(), "Forgejo 13.0");
        assert_eq!(fj.device_flow(), None);
        let gt = parse(ForgeKind::Forgejo, &json!({"version": "1.22.6"}));
        assert_eq!(gt.kind(), Some(ForgeKind::Gitea));
        let ghes = parse(ForgeKind::Ghes, &json!({"installed_version": "3.15.2"}));
        assert_eq!(ghes.chip(), "GitHub Enterprise 3.15");
        let none = parse(ForgeKind::Gitlab, &json!({"message": "401 Unauthorized"}));
        assert!(!none.known() && none.device_flow().is_none() && none.chip().is_empty());
        assert_eq!(major_minor("v18.10.1"), Some((18, 10)));
        assert!(at_least("18.10.1", (17, 9)) && !at_least("garbage", (1, 0)));
    }

    #[test]
    fn unauthenticated_answers() {
        let s = from_http(
            ForgeKind::Gitlab,
            401,
            &json!({"message": "401 Unauthorized"}),
        );
        assert!(!s.known() && s.error.contains("sign-in") && s.via == "http");
        let s = from_http(ForgeKind::Gitea, 200, &json!({"version": "1.22.3"}));
        assert_eq!((s.product.as_str(), s.via.as_str()), ("Gitea", "http"));
    }

    #[test]
    fn urls_per_kind() {
        let gl = Connection::new(ForgeKind::Gitlab, "gitlab.acme.dev", "t").unwrap();
        assert_eq!(
            http_url(&gl).as_deref(),
            Some("https://gitlab.acme.dev/api/v4/version")
        );
        let cb = Connection::new(ForgeKind::Forgejo, "codeberg.org", "t").unwrap();
        assert_eq!(
            http_url(&cb).as_deref(),
            Some("https://codeberg.org/api/v1/version")
        );
        assert_eq!(http_url(&Connection::github_com()), None);
        assert_eq!(route_path(Dialect::GitLab), "version");
        assert_eq!(route_path(Dialect::Gitea), "/version");
    }
}
