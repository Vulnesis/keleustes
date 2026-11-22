//! The one HTTP seam the Linear and Jira clients use, so tests answer
//! from memory ([`Fake`]) and nothing in a test reaches the network.

use anyhow::{Result, anyhow};
use serde_json::Value;
use std::sync::Mutex;

/// One request. Header values may hold the token: never log a request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub body: String,
}

impl Reply {
    pub fn json(&self) -> Result<Value> {
        if self.body.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&self.body)
            .map_err(|_| anyhow!("the board sent a body that is not JSON"))
    }
}

#[async_trait::async_trait]
pub trait Http: Send + Sync {
    async fn send(&self, r: Request) -> Result<Reply>;
}

/// `reqwest`, 20 s, no redirects (a token never follows one elsewhere).
pub struct Web(reqwest::Client);

impl Web {
    pub fn new() -> Result<Self> {
        Ok(Self(
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("keleustes/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|e| anyhow!("http client: {e}"))?,
        ))
    }
}

#[async_trait::async_trait]
impl Http for Web {
    async fn send(&self, r: Request) -> Result<Reply> {
        let method = reqwest::Method::from_bytes(r.method.as_bytes())?;
        let mut req = self.0.request(method, &r.url);
        for (k, v) in &r.headers {
            req = req.header(k, v);
        }
        if let Some(b) = &r.body {
            req = req
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        // without_url: no address (or query) reaches the error text.
        let res = req.send().await.map_err(|e| {
            if e.is_timeout() {
                anyhow!("no answer in 20 s")
            } else {
                anyhow!("{}", e.without_url())
            }
        })?;
        let status = res.status().as_u16();
        let body = res
            .text()
            .await
            .map_err(|e| anyhow!("{}", e.without_url()))?;
        Ok(Reply { status, body })
    }
}

type Handler = Box<dyn Fn(&Request) -> Option<Reply> + Send + Sync>;

/// Answers from registered handlers (first that answers wins), records
/// every request. Unanswered: 404.
#[derive(Default)]
pub struct Fake {
    handlers: Mutex<Vec<Handler>>,
    pub seen: Mutex<Vec<Request>>,
}

impl Fake {
    pub fn on(&self, f: impl Fn(&Request) -> Option<Reply> + Send + Sync + 'static) {
        self.handlers.lock().unwrap().push(Box::new(f));
    }

    pub fn seen(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Http for Fake {
    async fn send(&self, r: Request) -> Result<Reply> {
        self.seen.lock().unwrap().push(r.clone());
        let h = self.handlers.lock().unwrap();
        Ok(h.iter().find_map(|f| f(&r)).unwrap_or(Reply {
            status: 404,
            body: r#"{"message":"not found"}"#.into(),
        }))
    }
}

/// A JSON reply.
pub fn ok(v: Value) -> Option<Reply> {
    Some(Reply {
        status: 200,
        body: v.to_string(),
    })
}

/// A status with no body.
pub fn status(code: u16) -> Option<Reply> {
    Some(Reply {
        status: code,
        body: String::new(),
    })
}

/// The error for a status outside 2xx, with a short cleaned reason.
pub fn check(r: &Reply, what: &str) -> Result<()> {
    if (200..300).contains(&r.status) {
        return Ok(());
    }
    let why: String = super::clean_title(&r.body).chars().take(160).collect();
    Err(anyhow!("{what}: the board answered {} {why}", r.status))
}
