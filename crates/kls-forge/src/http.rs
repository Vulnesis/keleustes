//! Reading `gh api --include` output: the status line, headers, then the body.

use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Raw {
    pub status: u16,
    /// Lower-cased header names.
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

impl Raw {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn header_i64(&self, name: &str) -> Option<i64> {
        self.header(name).and_then(|v| v.trim().parse().ok())
    }
}

/// Parses `HTTP/2.0 200 OK\nHeader: v\n...\n\nbody`. `None` when the text
/// does not start with a status line (gh failed before any response).
pub fn parse_include(out: &str) -> Option<Raw> {
    let out = out.trim_start_matches(['\r', '\n']);
    if !out.starts_with("HTTP/") {
        return None;
    }
    let (head, body) = match (out.find("\r\n\r\n"), out.find("\n\n")) {
        (Some(a), Some(b)) if a < b => (&out[..a], &out[a + 4..]),
        (_, Some(b)) => (&out[..b], &out[b + 2..]),
        (Some(a), None) => (&out[..a], &out[a + 4..]),
        (None, None) => (out, ""),
    };
    let mut lines = head.lines();
    let status = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let mut headers = BTreeMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Some(Raw {
        status,
        headers,
        body: body.to_string(),
    })
}

/// The `rel="next"` target of a `Link` header, as an API path.
pub fn next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, rest) = part.split_once(';')?;
        if !rest.contains("rel=\"next\"") {
            return None;
        }
        let url = url.trim().trim_start_matches('<').trim_end_matches('>');
        Some(api_path(url))
    })
}

/// `https://api.github.com/repos/a/b?page=2` -> `/repos/a/b?page=2`.
pub fn api_path(url: &str) -> String {
    match url.find("://") {
        Some(i) => {
            let rest = &url[i + 3..];
            match rest.find('/') {
                Some(j) => {
                    let p = &rest[j..];
                    // GHES serves the API under /api/v3, GitLab /api/v4, Gitea /api/v1.
                    ["/api/v3", "/api/v4", "/api/v1"]
                        .iter()
                        .find_map(|pre| p.strip_prefix(pre))
                        .unwrap_or(p)
                        .to_string()
                }
                None => "/".into(),
            }
        }
        None => url.to_string(),
    }
}

/// `path` with its `page` query parameter set to `page` (GitLab's
/// `x-next-page`); `None` when `page` is empty.
pub fn with_page(path: &str, page: &str) -> Option<String> {
    if page.is_empty() || !page.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (base, query) = path.split_once('?').unwrap_or((path, ""));
    let mut q: Vec<String> = query
        .split('&')
        .filter(|p| !p.is_empty() && !p.starts_with("page="))
        .map(String::from)
        .collect();
    q.push(format!("page={page}"));
    Some(format!("{base}?{}", q.join("&")))
}

/// Percent-encodes everything but unreserved characters (a GitLab project
/// path `a/b` becomes `a%2Fb`).
pub fn encode_component(p: &str) -> String {
    encode_path(p).replace('/', "%2F")
}

/// Percent-encodes a path segment (file paths keep their slashes).
pub fn encode_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_include_output() {
        let r = parse_include(
            "HTTP/2.0 200 OK\r\nEtag: W/\"abc\"\r\nX-Ratelimit-Remaining: 42\r\n\r\n{\"a\":1}",
        )
        .unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.header("ETag"), Some("W/\"abc\""));
        assert_eq!(r.header_i64("x-ratelimit-remaining"), Some(42));
        assert_eq!(r.body, "{\"a\":1}");
        let r = parse_include("HTTP/1.1 304 Not Modified\nEtag: x\n\n").unwrap();
        assert_eq!((r.status, r.body.as_str()), (304, ""));
        assert!(parse_include("gh: not found").is_none());
    }

    #[test]
    fn link_and_paths() {
        let l = "<https://api.github.com/repositories/1/pulls?page=2>; rel=\"next\", <https://api.github.com/repositories/1/pulls?page=5>; rel=\"last\"";
        assert_eq!(
            next_link(l).as_deref(),
            Some("/repositories/1/pulls?page=2")
        );
        assert_eq!(next_link("<https://x/a>; rel=\"prev\""), None);
        assert_eq!(api_path("https://ghe.example/api/v3/user"), "/user");
        assert_eq!(encode_path("src/a b#.rs"), "src/a%20b%23.rs");
        assert_eq!(
            api_path("https://gitlab.com/api/v4/projects?page=2"),
            "/projects?page=2"
        );
        assert_eq!(
            with_page("projects/1/issues?per_page=100&page=1", "2").as_deref(),
            Some("projects/1/issues?per_page=100&page=2")
        );
        assert_eq!(with_page("x", ""), None);
        assert_eq!(encode_component("grp/sub app"), "grp%2Fsub%20app");
    }
}
