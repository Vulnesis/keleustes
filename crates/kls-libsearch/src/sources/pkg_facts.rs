//! S10 / S11: npm and PyPI package facts (licence, latest version) for an
//! MCP preview. Lookups only; nothing is installed.

use crate::fetch::{Ctx, urlencode};
use anyhow::{Result, bail};
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PkgFacts {
    pub licence: Option<String>,
    pub latest: Option<String>,
    pub repository: Option<String>,
    /// The URL read.
    pub url: String,
}

fn valid_pkg(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 214
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '/' | '-' | '_' | '.'))
}

/// `registry.npmjs.org/<pkg>` (scoped names keep their `@scope/`).
pub async fn npm(cx: &Ctx, pkg: &str) -> Result<PkgFacts> {
    if !valid_pkg(pkg) {
        bail!("not an npm package name: {pkg}");
    }
    let path = match pkg.strip_prefix('@').and_then(|r| r.split_once('/')) {
        Some((scope, name)) => format!("@{}%2F{}", urlencode(scope), urlencode(name)),
        None => urlencode(pkg),
    };
    let url = format!("https://registry.npmjs.org/{path}");
    let v: Value = cx.get_json(&url, 32 * 1024 * 1024).await?;
    let licence = match v.get("license") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Object(o)) => o.get("type").and_then(|t| t.as_str()).map(str::to_string),
        _ => None,
    };
    let repository = match v.get("repository") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Object(o)) => o.get("url").and_then(|t| t.as_str()).map(str::to_string),
        _ => None,
    };
    Ok(PkgFacts {
        licence,
        latest: v
            .get("dist-tags")
            .and_then(|d| d.get("latest"))
            .and_then(|x| x.as_str())
            .map(str::to_string),
        repository,
        url,
    })
}

/// `pypi.org/pypi/<pkg>/json`.
pub async fn pypi(cx: &Ctx, pkg: &str) -> Result<PkgFacts> {
    if !valid_pkg(pkg) || pkg.contains(['@', '/']) {
        bail!("not a PyPI package name: {pkg}");
    }
    let url = format!("https://pypi.org/pypi/{}/json", urlencode(pkg));
    let v: Value = cx.get_json(&url, 16 * 1024 * 1024).await?;
    let info = v.get("info").cloned().unwrap_or_default();
    let mut licence = info
        .get("license")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty() && s.len() < 64)
        .map(str::to_string);
    if licence.is_none() {
        // "License :: OSI Approved :: MIT License"
        licence = info
            .get("classifiers")
            .and_then(|c| c.as_array())
            .into_iter()
            .flatten()
            .filter_map(|c| c.as_str())
            .find_map(|c| c.strip_prefix("License :: OSI Approved :: "))
            .map(|s| s.trim_end_matches(" License").to_string());
    }
    Ok(PkgFacts {
        licence,
        latest: info
            .get("version")
            .and_then(|x| x.as_str())
            .map(str::to_string),
        repository: info
            .get("project_urls")
            .and_then(|p| p.get("Repository").or_else(|| p.get("Source")))
            .and_then(|x| x.as_str())
            .map(str::to_string),
        url,
    })
}
