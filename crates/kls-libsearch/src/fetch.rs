//! Fetching: the [`Fetcher`] trait every source uses, [`HttpFetcher`]
//! (allowlisted hosts, https only, conditional GETs through the
//! [`Cache`], per-host budgets, bounded bodies) over a [`Transport`]
//! (reqwest, or [`FakeFetcher`] in tests), and the per-call [`Ctx`].

use crate::cache::Cache;
use crate::git::{self, Limits, Tree};
use anyhow::{Result, anyhow, bail};
use bytes::Bytes;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Runs futures concurrently (remote sources answer side by side).
pub use futures::future::join_all;

/// Who is waiting: a person (interactive) or a background refresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    User,
    Background,
}

pub struct Req<'a> {
    pub url: &'a str,
    pub accept: &'a str,
    pub class: Class,
    pub max_bytes: u64,
}

impl<'a> Req<'a> {
    pub fn new(url: &'a str) -> Self {
        Self {
            url,
            accept: "application/json",
            class: Class::User,
            max_bytes: FILE_MAX,
        }
    }
}

/// A response. `not_modified` means the body came from the cache (a 304
/// or a fresh `max-age`).
#[derive(Clone, Debug)]
pub struct Resp {
    pub status: u16,
    /// Lower-case names.
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
    pub not_modified: bool,
    /// The URL that answered (after allowlisted redirects).
    pub url: String,
}

impl Resp {
    pub fn header(&self, name: &str) -> Option<&str> {
        let n = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == n)
            .map(|(_, v)| v.as_str())
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body)
            .map_err(|e| anyhow!("{}: not the JSON expected: {e}", self.url))
    }

    pub fn text(&self) -> Result<String> {
        String::from_utf8(self.body.to_vec()).map_err(|_| anyhow!("{}: not UTF-8", self.url))
    }
}

/// A host answered 429/403 with a reset, or its budget is spent.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{host} is rate limited until {until}")]
pub struct Limited {
    pub host: String,
    pub until: i64,
}

/// The network could not be reached.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("offline: {0}")]
pub struct Offline(pub String);

/// The factory kill switch is on: nothing goes out.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the factory is stopped: no network fetch")]
pub struct Stopped;

/// The per-source deadline passed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("deadline passed")]
pub struct Deadline;

pub fn is_limited(e: &anyhow::Error) -> Option<&Limited> {
    e.downcast_ref::<Limited>()
}

pub fn is_offline(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Offline>().is_some()
}

pub fn is_stopped(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Stopped>().is_some()
}

/// Response caps.
pub const INDEX_MAX: u64 = 64 * 1024 * 1024;
pub const FILE_MAX: u64 = 1024 * 1024;
/// A compressed tarball may be at most this large.
pub const TARBALL_MAX: u64 = 64 * 1024 * 1024;

/// Allowed hosts. Redirects are followed only within this list (plus the
/// user's well-known hosts); anything else is an error.
pub const HOSTS: &[&str] = &[
    "registry.modelcontextprotocol.io",
    "clawhub.ai",
    "skills.sh",
    "geminicli.com",
    "www.aitmpl.com",
    "chat-agents.lobehub.com",
    "api.cline.bot",
    "nousresearch.github.io",
    "registry.npmjs.org",
    "pypi.org",
    "github.com",
    "api.github.com",
    "codeload.github.com",
    "raw.githubusercontent.com",
];

/// The host of an https URL; an error for anything else.
pub fn https_host(url: &str) -> Result<String> {
    let Some(rest) = url.strip_prefix("https://") else {
        bail!("only https URLs are fetched ({url})");
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if host.is_empty() || host.contains('@') || host.contains(':') {
        bail!("not a plain https host: {url}");
    }
    Ok(host)
}

/// Percent-encodes a query or path component.
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---- the trait ----------------------------------------------------------------

#[async_trait::async_trait]
pub trait Fetcher: Send + Sync {
    /// A GET, conditional through the cache where there is one.
    async fn get(&self, req: Req<'_>) -> Result<Resp>;

    /// `ls-remote` over smart HTTP: `(ref, sha)` pairs, `HEAD` first when
    /// the server advertises it.
    async fn ls_refs(
        &self,
        owner: &str,
        repo: &str,
        class: Class,
    ) -> Result<Vec<(String, String)>> {
        let url =
            format!("https://github.com/{owner}/{repo}.git/info/refs?service=git-upload-pack");
        let r = self
            .get(Req {
                url: &url,
                accept: "application/x-git-upload-pack-advertisement",
                class,
                max_bytes: 8 * 1024 * 1024,
            })
            .await?;
        if !r.ok() {
            bail!("{owner}/{repo}: refs answered {}", r.status);
        }
        git::parse_refs(&r.body)
    }

    /// The repo at `sha` from codeload, keeping only entries `keep` matches
    /// (plus licence and manifest files), within `lim`.
    async fn tarball(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        keep: &globset::GlobSet,
        lim: Limits,
        class: Class,
    ) -> Result<Tree> {
        if !git::is_sha(sha) {
            bail!("tarballs are fetched by commit SHA only");
        }
        let url = format!("https://codeload.github.com/{owner}/{repo}/tar.gz/{sha}");
        let r = self
            .get(Req {
                url: &url,
                accept: "application/x-gzip",
                class,
                max_bytes: TARBALL_MAX,
            })
            .await?;
        if !r.ok() {
            bail!("{owner}/{repo}@{sha}: tarball answered {}", r.status);
        }
        let mut t = git::extract(&r.body, keep, lim)?;
        t.url = url;
        Ok(t)
    }

    /// The per-host budgets as they stand (for the GitHub line).
    fn budgets(&self) -> Vec<BudgetView> {
        vec![]
    }

    /// Adds a host the person trusts (a well-known skills site).
    fn allow_host(&self, host: &str) {
        let _ = host;
    }
}

// ---- the kill switch and gh -----------------------------------------------------

/// Checked before every request: the factory kill switch.
#[async_trait::async_trait]
pub trait Gate: Send + Sync {
    /// True when requests may go out.
    async fn open(&self) -> bool;
}

/// A signed-in local `gh` (WP8), budgeted by kls-github.
#[async_trait::async_trait]
pub trait GhApi: Send + Sync {
    /// `gh api <path>` as JSON. `class` picks the kls-github call class.
    async fn get_json(&self, path: &str, class: Class) -> Result<serde_json::Value>;
    /// `gh ✓ 4980/5000`, when known.
    fn quota(&self) -> Option<(i64, i64)> {
        None
    }
}

// ---- the per-call context --------------------------------------------------------

/// What a source call gets: the fetcher, the kill-switch gate, the
/// deadline, the class and (optionally) `gh`.
#[derive(Clone)]
pub struct Ctx {
    pub http: Arc<dyn Fetcher>,
    pub deadline: Option<Instant>,
    pub class: Class,
    pub gate: Option<Arc<dyn Gate>>,
    pub gh: Option<Arc<dyn GhApi>>,
    /// Clock for `fetched_at` (passed in so converters stay pure).
    pub now_ms: i64,
}

impl Ctx {
    pub fn new(http: Arc<dyn Fetcher>) -> Self {
        Self {
            http,
            deadline: None,
            class: Class::User,
            gate: None,
            gh: None,
            now_ms: kls_store::now_ms(),
        }
    }

    pub fn with_deadline(mut self, d: std::time::Duration) -> Self {
        self.deadline = Some(Instant::now() + d);
        self
    }

    pub fn background(mut self) -> Self {
        self.class = Class::Background;
        self
    }

    /// Kill switch and deadline: checked between requests.
    pub async fn check(&self) -> Result<()> {
        if let Some(g) = &self.gate
            && !g.open().await
        {
            return Err(Stopped.into());
        }
        if self.deadline.is_some_and(|d| Instant::now() > d) {
            return Err(Deadline.into());
        }
        Ok(())
    }

    pub async fn get_req(&self, url: &str, accept: &str, max_bytes: u64) -> Result<Resp> {
        self.check().await?;
        self.http
            .get(Req {
                url,
                accept,
                class: self.class,
                max_bytes,
            })
            .await
    }

    /// A GET that must answer 2xx.
    pub async fn get(&self, url: &str, max_bytes: u64) -> Result<Resp> {
        let r = self.get_req(url, "*/*", max_bytes).await?;
        if !r.ok() {
            bail!("{url} answered {}", r.status);
        }
        Ok(r)
    }

    pub async fn get_json<T: DeserializeOwned>(&self, url: &str, max_bytes: u64) -> Result<T> {
        let r = self.get_req(url, "application/json", max_bytes).await?;
        if !r.ok() {
            bail!("{url} answered {}", r.status);
        }
        r.json()
    }

    pub async fn get_text(&self, url: &str) -> Result<String> {
        self.get(url, FILE_MAX).await?.text()
    }

    /// `raw.githubusercontent.com/{o}/{r}/{sha}/{path}`.
    pub fn raw_url(owner: &str, repo: &str, sha: &str, path: &str) -> String {
        format!("https://raw.githubusercontent.com/{owner}/{repo}/{sha}/{path}")
    }

    pub async fn raw(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        path: &str,
    ) -> Result<(String, Bytes)> {
        let url = Self::raw_url(owner, repo, sha, path);
        let r = self.get(&url, FILE_MAX).await?;
        Ok((url, r.body))
    }

    pub async fn ls_refs(&self, owner: &str, repo: &str) -> Result<Vec<(String, String)>> {
        self.check().await?;
        self.http.ls_refs(owner, repo, self.class).await
    }

    /// The commit a ref names: a 40-hex ref is already one; `None` is HEAD.
    pub async fn resolve(&self, owner: &str, repo: &str, git_ref: Option<&str>) -> Result<String> {
        if let Some(r) = git_ref
            && git::is_sha(r)
        {
            return Ok(r.to_string());
        }
        let refs = self.ls_refs(owner, repo).await?;
        git::pick_ref(&refs, git_ref)
            .ok_or_else(|| anyhow!("{owner}/{repo}: no ref {}", git_ref.unwrap_or("HEAD")))
    }

    pub async fn tarball(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        keep: &globset::GlobSet,
        lim: Limits,
    ) -> Result<Tree> {
        self.check().await?;
        self.http
            .tarball(owner, repo, sha, keep, lim, self.class)
            .await
    }
}

// ---- budgets ----------------------------------------------------------------------

/// One host's budget (a token bucket) as the UI shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BudgetView {
    pub key: String,
    pub remaining: i64,
    pub capacity: i64,
    /// When the bucket is full again (ms), or the park ends.
    pub reset_at: i64,
    pub parked_until: i64,
}

/// A sliding window: at most `cap` requests in any `window` ms.
#[derive(Clone, Debug)]
struct Window {
    cap: usize,
    window: i64,
    sent: std::collections::VecDeque<i64>,
    parked_until: i64,
}

impl Window {
    fn new(cap: usize, window: i64) -> Self {
        Self {
            cap,
            window,
            sent: Default::default(),
            parked_until: 0,
        }
    }

    fn trim(&mut self, now: i64) {
        while self.sent.front().is_some_and(|t| *t <= now - self.window) {
            self.sent.pop_front();
        }
    }
}

/// Request windows per budget key: `api.github.com` core 55/h (5 kept back
/// of the anonymous 60), its search 9/min, `clawhub.ai` 600/min. A 429 or
/// a 403 with a reset parks the host.
#[derive(Default)]
pub struct Budgets {
    windows: Mutex<HashMap<String, Window>>,
}

/// The budget key of a URL, its cap and window (`None`: unmetered host).
pub fn budget_key(url: &str) -> Option<(&'static str, usize, i64)> {
    let host = https_host(url).ok()?;
    const HOUR: i64 = 3_600_000;
    const MIN: i64 = 60_000;
    match host.as_str() {
        "api.github.com" if url.contains("api.github.com/search/") => {
            Some(("api.github.com/search", 9, MIN))
        }
        "api.github.com" => Some(("api.github.com", 55, HOUR)),
        "clawhub.ai" => Some(("clawhub.ai", 600, MIN)),
        _ => None,
    }
}

impl Budgets {
    /// Takes a slot for `url` at `now`, or says when to come back.
    pub fn take(&self, url: &str, now: i64) -> Result<(), Limited> {
        let host = https_host(url).unwrap_or_default();
        let mut w = self.windows.lock().unwrap();
        if let Some(p) = w
            .iter()
            .filter(|(k, _)| k.split('/').next() == Some(host.as_str()))
            .map(|(_, v)| v.parked_until)
            .max()
            && p > now
        {
            return Err(Limited { host, until: p });
        }
        let Some((key, cap, window)) = budget_key(url) else {
            return Ok(());
        };
        let win = w
            .entry(key.to_string())
            .or_insert_with(|| Window::new(cap, window));
        win.trim(now);
        if win.sent.len() >= win.cap {
            let until = win.sent.front().copied().unwrap_or(now) + win.window;
            return Err(Limited {
                host: key.to_string(),
                until,
            });
        }
        win.sent.push_back(now);
        Ok(())
    }

    /// Parks the host until `until`.
    pub fn park(&self, url: &str, until: i64) {
        let host = https_host(url).unwrap_or_default();
        let key = budget_key(url).map(|k| k.0.to_string()).unwrap_or(host);
        let mut w = self.windows.lock().unwrap();
        let e = w.entry(key).or_insert_with(|| Window::new(usize::MAX, 1));
        e.parked_until = e.parked_until.max(until);
    }

    pub fn view(&self, now: i64) -> Vec<BudgetView> {
        let mut w = self.windows.lock().unwrap();
        let mut out: Vec<BudgetView> = w
            .iter_mut()
            .map(|(k, v)| {
                v.trim(now);
                let metered = v.cap != usize::MAX;
                BudgetView {
                    key: k.clone(),
                    remaining: if metered {
                        (v.cap - v.sent.len()) as i64
                    } else {
                        -1
                    },
                    capacity: if metered { v.cap as i64 } else { -1 },
                    reset_at: v.sent.front().map(|t| t + v.window).unwrap_or(now),
                    parked_until: v.parked_until,
                }
            })
            .collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }
}

/// When a 429 / 403 says to come back (`Retry-After` seconds or
/// `x-ratelimit-reset` epoch seconds), in ms.
pub fn retry_at(status: u16, headers: &[(String, String)], now: i64) -> Option<i64> {
    let h = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.trim().to_string())
    };
    if status == 429 || status == 403 || status == 503 {
        if let Some(s) = h("retry-after").and_then(|v| v.parse::<i64>().ok()) {
            return Some(now + s * 1000);
        }
        if h("x-ratelimit-remaining").as_deref() == Some("0")
            && let Some(r) = h("x-ratelimit-reset").and_then(|v| v.parse::<i64>().ok())
        {
            return Some(r * 1000);
        }
        if status == 429 {
            return Some(now + 60_000);
        }
    }
    None
}

// ---- transport ------------------------------------------------------------------------

/// What one wire request returns (no redirects followed).
#[derive(Clone, Debug)]
pub struct RawResp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

/// One request on the wire. `headers` are the conditional and accept
/// headers; the body is cut at `max_bytes` (an error past it).
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn send(
        &self,
        url: &str,
        headers: &[(String, String)],
        max_bytes: u64,
    ) -> Result<RawResp>;
}

/// reqwest with rustls, no redirects (the fetcher follows them within the
/// allowlist), gzip, a 20 s timeout.
pub struct Reqwest {
    client: reqwest::Client,
}

impl Reqwest {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .gzip(true)
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(8))
            .user_agent(concat!("keleustes-libsearch/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { client })
    }
}

#[async_trait::async_trait]
impl Transport for Reqwest {
    async fn send(
        &self,
        url: &str,
        headers: &[(String, String)],
        max_bytes: u64,
    ) -> Result<RawResp> {
        use futures::StreamExt;
        let mut rb = self.client.get(url);
        for (k, v) in headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| Offline(format!("{url}: {e}")))?;
        let status = resp.status().as_u16();
        let hs: Vec<(String, String)> = resp
            .headers()
            .iter()
            .filter_map(|(k, v)| {
                Some((
                    k.as_str().to_ascii_lowercase(),
                    v.to_str().ok()?.to_string(),
                ))
            })
            .collect();
        if let Some(n) = resp.content_length()
            && n > max_bytes
        {
            bail!("{url}: body of {n} bytes is over the {max_bytes} byte cap");
        }
        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| Offline(format!("{url}: {e}")))?;
            if body.len() as u64 + chunk.len() as u64 > max_bytes {
                bail!("{url}: body is over the {max_bytes} byte cap");
            }
            body.extend_from_slice(&chunk);
        }
        Ok(RawResp {
            status,
            headers: hs,
            body: Bytes::from(body),
        })
    }
}

// ---- the fetcher ------------------------------------------------------------------------

/// The real fetcher: allowlist, https only, redirects within the allowlist,
/// per-host budgets, conditional GETs through the cache, bounded bodies.
pub struct HttpFetcher {
    transport: Arc<dyn Transport>,
    cache: Option<Cache>,
    budgets: Budgets,
    extra_hosts: Mutex<Vec<String>>,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl HttpFetcher {
    /// Over reqwest.
    pub fn new(cache: Option<Cache>) -> Result<Self> {
        Ok(Self::with_transport(Arc::new(Reqwest::new()?), cache))
    }

    pub fn with_transport(transport: Arc<dyn Transport>, cache: Option<Cache>) -> Self {
        Self {
            transport,
            cache,
            budgets: Budgets::default(),
            extra_hosts: Mutex::new(vec![]),
            clock: Arc::new(kls_store::now_ms),
        }
    }

    /// A fixed or test clock.
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    pub fn cache(&self) -> Option<&Cache> {
        self.cache.as_ref()
    }

    fn allowed(&self, host: &str) -> bool {
        HOSTS.contains(&host) || self.extra_hosts.lock().unwrap().iter().any(|h| h == host)
    }
}

#[async_trait::async_trait]
impl Fetcher for HttpFetcher {
    async fn get(&self, req: Req<'_>) -> Result<Resp> {
        let now = (self.clock)();
        let mut url = req.url.to_string();
        let host = https_host(&url)?;
        if !self.allowed(&host) {
            bail!("{host} is not on the library search allowlist");
        }
        // Fresh in the cache: no request at all.
        let cached = match &self.cache {
            Some(c) => c.lookup(&url).await.ok().flatten(),
            None => None,
        };
        if let Some(c) = &cached
            && c.fresh(now)
            && let Some(body) = c.body.clone()
        {
            return Ok(Resp {
                status: 200,
                headers: vec![],
                body,
                not_modified: true,
                url,
            });
        }
        self.budgets.take(&url, now)?;
        let mut headers = vec![("accept".to_string(), req.accept.to_string())];
        if let Some(c) = &cached
            && c.body.is_some()
        {
            if !c.etag.is_empty() {
                headers.push(("if-none-match".into(), c.etag.clone()));
            }
            if !c.last_modified.is_empty() {
                headers.push(("if-modified-since".into(), c.last_modified.clone()));
            }
        }
        let mut hops = 0;
        let raw = loop {
            let raw = match self.transport.send(&url, &headers, req.max_bytes).await {
                Ok(r) => r,
                // Offline: a stale cached body still answers.
                Err(e) if is_offline(&e) => {
                    if let Some(c) = &cached
                        && let Some(body) = c.body.clone()
                    {
                        return Ok(Resp {
                            status: 200,
                            headers: vec![("x-kls-stale".into(), c.fetched_at.to_string())],
                            body,
                            not_modified: true,
                            url,
                        });
                    }
                    return Err(e);
                }
                Err(e) => return Err(e),
            };
            if matches!(raw.status, 301 | 302 | 303 | 307 | 308) {
                hops += 1;
                if hops > 5 {
                    bail!("{url}: too many redirects");
                }
                let loc = raw
                    .headers
                    .iter()
                    .find(|(k, _)| k == "location")
                    .map(|(_, v)| v.clone())
                    .ok_or_else(|| anyhow!("{url}: redirect without a location"))?;
                let next = if loc.starts_with('/') {
                    format!("https://{}{loc}", https_host(&url)?)
                } else {
                    loc
                };
                let h = https_host(&next)?;
                if !self.allowed(&h) {
                    bail!("{url} redirects to {h}, which is not on the allowlist");
                }
                url = next;
                continue;
            }
            break raw;
        };
        if let Some(until) = retry_at(raw.status, &raw.headers, now) {
            self.budgets.park(&url, until);
            return Err(Limited { host, until }.into());
        }
        if raw.status == 304
            && let Some(c) = cached
            && let Some(body) = c.body
        {
            if let Some(cache) = &self.cache {
                let _ = cache.touch(&url, now, max_age(&raw.headers)).await;
            }
            return Ok(Resp {
                status: 200,
                headers: raw.headers,
                body,
                not_modified: true,
                url,
            });
        }
        if raw.status == 200
            && let Some(cache) = &self.cache
        {
            // Every body is kept so search, preview and import work offline;
            // only validators and max-age decide whether it is reused online.
            let etag = header(&raw.headers, "etag");
            let lm = header(&raw.headers, "last-modified");
            let ma = max_age(&raw.headers);
            let _ = cache.store(req.url, &etag, &lm, ma, now, &raw.body).await;
        }
        Ok(Resp {
            status: raw.status,
            headers: raw.headers,
            body: raw.body,
            not_modified: false,
            url,
        })
    }

    fn budgets(&self) -> Vec<BudgetView> {
        self.budgets.view((self.clock)())
    }

    fn allow_host(&self, host: &str) {
        let h = host.to_ascii_lowercase();
        let mut x = self.extra_hosts.lock().unwrap();
        if !x.contains(&h) {
            x.push(h);
        }
    }
}

fn header(h: &[(String, String)], n: &str) -> String {
    h.iter()
        .find(|(k, _)| k == n)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

/// `max-age` from `cache-control`, in seconds (0 for `no-store`/none).
pub fn max_age(h: &[(String, String)]) -> i64 {
    let cc = header(h, "cache-control").to_ascii_lowercase();
    if cc.contains("no-store") || cc.contains("no-cache") {
        return 0;
    }
    cc.split(',')
        .filter_map(|p| p.trim().strip_prefix("max-age="))
        .filter_map(|v| v.trim().parse::<i64>().ok())
        .next()
        .unwrap_or(0)
}

// ---- the fake ---------------------------------------------------------------------------

/// A canned response.
#[derive(Clone, Debug)]
pub struct Canned {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

/// Serves canned bodies by URL and records every request. Use it directly
/// as a [`Fetcher`] (no cache, no budget) or as the [`Transport`] under an
/// [`HttpFetcher`] to test caching, budgets and redirects. ETag: a canned
/// response with an `etag` header answers 304 to a matching
/// `if-none-match`.
#[derive(Default)]
pub struct FakeFetcher {
    routes: Mutex<HashMap<String, Canned>>,
    log: Mutex<Vec<String>>,
}

impl FakeFetcher {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn serve(&self, url: &str, status: u16, body: impl Into<Bytes>) {
        self.serve_with(url, status, vec![], body);
    }

    pub fn serve_with(
        &self,
        url: &str,
        status: u16,
        headers: Vec<(&str, &str)>,
        body: impl Into<Bytes>,
    ) {
        self.routes.lock().unwrap().insert(
            url.to_string(),
            Canned {
                status,
                headers: headers
                    .into_iter()
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
                    .collect(),
                body: body.into(),
            },
        );
    }

    /// Serves a JSON value.
    pub fn serve_json(&self, url: &str, v: &serde_json::Value) {
        self.serve(url, 200, serde_json::to_vec(v).unwrap());
    }

    /// Every URL requested, in order.
    pub fn requests(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    pub fn clear_log(&self) {
        self.log.lock().unwrap().clear();
    }

    fn answer(&self, url: &str, headers: &[(String, String)]) -> Result<RawResp> {
        self.log.lock().unwrap().push(url.to_string());
        let routes = self.routes.lock().unwrap();
        let Some(c) = routes.get(url) else {
            return Err(Offline(format!("no fake route for {url}")).into());
        };
        let etag = header(&c.headers, "etag");
        let inm = header(headers, "if-none-match");
        if !etag.is_empty() && etag == inm {
            return Ok(RawResp {
                status: 304,
                headers: c.headers.clone(),
                body: Bytes::new(),
            });
        }
        Ok(RawResp {
            status: c.status,
            headers: c.headers.clone(),
            body: c.body.clone(),
        })
    }
}

#[async_trait::async_trait]
impl Transport for FakeFetcher {
    async fn send(
        &self,
        url: &str,
        headers: &[(String, String)],
        max_bytes: u64,
    ) -> Result<RawResp> {
        let r = self.answer(url, headers)?;
        if r.body.len() as u64 > max_bytes {
            bail!("{url}: body is over the {max_bytes} byte cap");
        }
        Ok(r)
    }
}

#[async_trait::async_trait]
impl Fetcher for FakeFetcher {
    async fn get(&self, req: Req<'_>) -> Result<Resp> {
        https_host(req.url)?;
        let r = self.answer(req.url, &[])?;
        if r.body.len() as u64 > req.max_bytes {
            bail!("{}: body is over the {} byte cap", req.url, req.max_bytes);
        }
        Ok(Resp {
            status: r.status,
            headers: r.headers,
            body: r.body,
            not_modified: false,
            url: req.url.to_string(),
        })
    }
}

/// A gate that is always open or always shut (tests).
pub struct FixedGate(pub bool);

#[async_trait::async_trait]
impl Gate for FixedGate {
    async fn open(&self) -> bool {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_https_only() {
        assert_eq!(
            https_host("https://clawhub.ai/api/v1/search?q=x").unwrap(),
            "clawhub.ai"
        );
        assert!(https_host("http://clawhub.ai/").is_err());
        assert!(https_host("https://user@evil.example/").is_err());
        assert!(https_host("https://clawhub.ai:8443/").is_err());
        assert_eq!(urlencode("a b/c"), "a%20b%2Fc");
    }

    #[tokio::test]
    async fn off_allowlist_and_redirects_refused() {
        let fake = FakeFetcher::new();
        fake.serve_with(
            "https://skills.sh/api/search?q=x",
            302,
            vec![("location", "https://evil.example/x")],
            "",
        );
        let f = HttpFetcher::with_transport(fake.clone(), None);
        let e = f.get(Req::new("https://evil.example/")).await.unwrap_err();
        assert!(e.to_string().contains("allowlist"));
        let e = f
            .get(Req::new("https://skills.sh/api/search?q=x"))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("not on the allowlist"), "{e}");
        assert!(f.get(Req::new("http://skills.sh/")).await.is_err());
        // Only the redirecting request went out.
        assert_eq!(fake.requests().len(), 1);
    }

    #[tokio::test]
    async fn redirect_within_allowlist_followed() {
        let fake = FakeFetcher::new();
        fake.serve_with(
            "https://geminicli.com/old.json",
            301,
            vec![("location", "/extensions.json")],
            "",
        );
        fake.serve("https://geminicli.com/extensions.json", 200, "[]");
        let f = HttpFetcher::with_transport(fake.clone(), None);
        let r = f
            .get(Req::new("https://geminicli.com/old.json"))
            .await
            .unwrap();
        assert_eq!(r.url, "https://geminicli.com/extensions.json");
        assert_eq!(&r.body[..], b"[]");
    }

    #[tokio::test]
    async fn retry_after_parks_the_host() {
        let fake = FakeFetcher::new();
        fake.serve_with(
            "https://clawhub.ai/api/v1/search?q=a",
            429,
            vec![("retry-after", "30")],
            "",
        );
        fake.serve("https://clawhub.ai/api/v1/search?q=b", 200, "{}");
        let now = Arc::new(std::sync::atomic::AtomicI64::new(1_000_000));
        let n2 = now.clone();
        let f = HttpFetcher::with_transport(fake.clone(), None).with_clock(Arc::new(move || {
            n2.load(std::sync::atomic::Ordering::Relaxed)
        }));
        let e = f
            .get(Req::new("https://clawhub.ai/api/v1/search?q=a"))
            .await
            .unwrap_err();
        assert_eq!(is_limited(&e).unwrap().until, 1_030_000);
        // Parked: the next call does not go out.
        let e = f
            .get(Req::new("https://clawhub.ai/api/v1/search?q=b"))
            .await
            .unwrap_err();
        assert!(is_limited(&e).is_some());
        assert_eq!(fake.requests().len(), 1);
        now.store(1_031_000, std::sync::atomic::Ordering::Relaxed);
        f.get(Req::new("https://clawhub.ai/api/v1/search?q=b"))
            .await
            .unwrap();
    }

    #[test]
    fn budget_never_exceeds_55_in_any_hour() {
        let b = Budgets::default();
        let mut sent = vec![];
        // One attempt every 10 s for three hours.
        let mut t = 0i64;
        while t < 3 * 3_600_000 {
            if b.take("https://api.github.com/repos/o/r", t).is_ok() {
                sent.push(t);
            }
            t += 10_000;
        }
        for (i, s) in sent.iter().enumerate() {
            let in_hour = sent[i..].iter().filter(|x| **x < s + 3_600_000).count();
            assert!(in_hour <= 55, "{in_hour} in the hour from {s}");
        }
        assert!(sent.len() >= 3 * 55 - 1);
        // Search has its own 9/min.
        let n = (0..20)
            .filter(|_| {
                b.take("https://api.github.com/search/repositories?q=x", 0)
                    .is_ok()
            })
            .count();
        assert_eq!(n, 9);
        let v = b.view(3 * 3_600_000);
        assert!(
            v.iter()
                .any(|x| x.key == "api.github.com" && x.capacity == 55)
        );
    }

    #[test]
    fn retry_headers() {
        let h = |v: Vec<(&str, &str)>| {
            v.into_iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(retry_at(429, &h(vec![("retry-after", "5")]), 0), Some(5000));
        assert_eq!(
            retry_at(
                403,
                &h(vec![
                    ("x-ratelimit-remaining", "0"),
                    ("x-ratelimit-reset", "100")
                ]),
                0
            ),
            Some(100_000)
        );
        assert_eq!(retry_at(403, &h(vec![]), 0), None);
        assert_eq!(
            max_age(&h(vec![("cache-control", "public, max-age=600")])),
            600
        );
        assert_eq!(max_age(&h(vec![("cache-control", "no-store")])), 0);
    }

    #[tokio::test]
    async fn closed_gate_sends_nothing() {
        let fake = FakeFetcher::new();
        fake.serve("https://geminicli.com/extensions.json", 200, "[]");
        let mut cx = Ctx::new(fake.clone());
        cx.gate = Some(Arc::new(FixedGate(false)));
        let e = cx
            .get("https://geminicli.com/extensions.json", FILE_MAX)
            .await
            .unwrap_err();
        assert!(is_stopped(&e));
        assert!(fake.requests().is_empty());
    }
}
