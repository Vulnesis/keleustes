//! Live suite against a real Gitea or Forgejo (forges.md 8.2), driven by
//! `just forge-live` (crates/kls-github/live/forge-live.sh), which starts
//! the container, creates the account and token, seeds `ana/app` with
//! `main` and `feature` conflicting on `app.txt`, opens the pull request and
//! signs `tea` in. Skipped unless `KLS_LIVE_FORGE_HOST` is set.

use kls_core::Core;
use kls_github::forge::{Connection, ForgeKind};
use kls_github::{Service, sync};
use std::collections::BTreeMap;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[tokio::test]
async fn live_gitea_family_plane() {
    let Some(host) = env("KLS_LIVE_FORGE_HOST") else {
        eprintln!("KLS_LIVE_FORGE_HOST not set: skipped (run `just forge-live`)");
        return;
    };
    let kind = env("KLS_LIVE_FORGE_KIND")
        .and_then(|k| ForgeKind::parse(&k))
        .unwrap_or(ForgeKind::Gitea);
    let user = env("KLS_LIVE_FORGE_USER").unwrap_or_else(|| "ana".into());
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    let svc = Service::new(core.clone(), kls_github::LocalGh::default());
    let mut conn = svc.add_connection(kind, &host, "", "user").await.unwrap();
    conn.cli_name = host.clone();
    core.store()
        .upsert_forge_connection(&conn.to_row("{}"))
        .await
        .unwrap();
    let conn: Connection = svc.connection(&conn.id).await.unwrap();
    let auth = svc.auth_state(&conn).await;
    assert!(auth.signed_in(), "{auth:?}");
    assert_eq!(auth.login(), user);
    let c = svc.client_conn(&conn).await.unwrap();

    // The server probe through the route: Forgejo told apart from Gitea.
    let info = svc.server_probe(&conn, "user").await.unwrap();
    eprintln!("server: {} {} via {}", info.product, info.version, info.via);
    assert!(info.known(), "{info:?}");
    assert_eq!(info.via, "route");
    assert_eq!(info.kind(), Some(kind), "{info:?}");
    assert_eq!(svc.server_info(&conn.id).await, Some(info));

    // Discovery.
    let r = sync::refresh_orgs(&c, "user", false).await.unwrap();
    assert!(r.orgs >= 1);
    let me = core
        .store()
        .gh_org(&conn.org_key(&user))
        .await
        .unwrap()
        .unwrap();
    let rr = sync::refresh_org_repos(&c, &me, "user").await.unwrap();
    let key = conn.repo_key(&format!("{user}/app"));
    assert!(rr.repos >= 1, "{rr:?}");
    // Pulls, checks as statuses, labels by id, issues.
    sync::sync_pulls(&c, &key, "user").await.unwrap();
    let pulls = core
        .store()
        .gh_pulls(Some(&key), Some("open"))
        .await
        .unwrap();
    let pr = pulls.first().expect("the seeded pull request").clone();
    // The next poll is bounded by the cursor (`state=all`, stops at it).
    sync::sync_pulls(&c, &key, "user").await.unwrap();
    assert_eq!(
        core.store()
            .gh_pulls(Some(&key), Some("open"))
            .await
            .unwrap()
            .len(),
        pulls.len()
    );
    let ls = sync::sync_labels(&c, &key, true, "user").await.unwrap();
    assert!(ls.error.is_empty(), "{}", ls.error);
    let api = c.api();
    api.add_labels(&c, &key, pr.number, &["sf:fix".into()], "user")
        .await
        .unwrap();
    api.set_status(
        &c,
        &key,
        &pr.head_sha,
        &kls_github::forge::api::StatusIn {
            state: "success".into(),
            context: "keleustes/live".into(),
            description: "live suite".into(),
            target_url: String::new(),
        },
        "user",
    )
    .await
    .unwrap();
    let p = sync::refresh_pull(&c, &key, pr.number, "user")
        .await
        .unwrap();
    assert!(p.labels.contains(&"sf:fix".to_string()), "{:?}", p.labels);
    assert_eq!(p.checks_state, "success");
    let issue = api
        .create_issue(
            &c,
            &key,
            "live issue",
            "from the live suite",
            &["sf:triage".into()],
            "user",
        )
        .await
        .unwrap();
    assert!(issue["number"].as_i64().unwrap_or(0) > 0);
    sync::sync_issues(&c, &key, "user").await.unwrap();
    // Draft and ready.
    sync::pull_action(&c, &key, pr.number, &sync::PullAction::Draft, "user")
        .await
        .unwrap();
    let p = sync::pull_action(&c, &key, pr.number, &sync::PullAction::Ready, "user")
        .await
        .unwrap();
    assert!(!p.draft);
    // Conflict commit pushed through `tea login helper`, then a stale push refused.
    let set = kls_github::conflicts::load(&c, &key, pr.number, "user")
        .await
        .unwrap();
    assert!(set.conflicting().count() >= 1, "{set:?}");
    let mut resolved = BTreeMap::new();
    for f in set.conflicting() {
        resolved.insert(
            f.path.clone(),
            Some("resolved by the live suite\n".to_string()),
        );
    }
    let res = kls_github::conflicts::commit(&c, &set, &resolved, "", "user")
        .await
        .unwrap();
    assert_eq!(res.signing, "unsigned_push");
    assert_eq!(res.parent_shas.len(), 2);
    let again = kls_github::conflicts::commit(&c, &set, &resolved, "", "user").await;
    assert!(
        format!("{:#}", again.unwrap_err()).contains("moved"),
        "a stale push must be refused"
    );
}

/// A browser for the OAuth approval: a cookie jar over reqwest, no redirects
/// followed (the last one goes to tea's loopback callback).
struct Browser {
    http: reqwest::Client,
    cookies: BTreeMap<String, String>,
}

impl Browser {
    fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            cookies: BTreeMap::new(),
        }
    }

    async fn send(&mut self, req: reqwest::RequestBuilder) -> (u16, String, String) {
        let jar: Vec<String> = self
            .cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let r = req.header("Cookie", jar.join("; ")).send().await.unwrap();
        for c in r.headers().get_all("set-cookie") {
            if let Some((k, v)) = c
                .to_str()
                .unwrap_or("")
                .split(';')
                .next()
                .and_then(|kv| kv.split_once('='))
            {
                self.cookies.insert(k.trim().into(), v.trim().into());
            }
        }
        let status = r.status().as_u16();
        let to = r
            .headers()
            .get("location")
            .and_then(|l| l.to_str().ok())
            .unwrap_or("")
            .to_string();
        (status, to, r.text().await.unwrap_or_default())
    }
}

/// `name="<field>" value="<v>"` of the first such input in a page.
fn input(page: &str, field: &str) -> String {
    let pat = format!("name=\"{field}\" value=\"");
    page.split_once(&pat)
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(v, _)| v.replace("&amp;", "&"))
        .unwrap_or_default()
}

/// forges.md 8.2 "tea OAuth": Keleustes drives the real `tea login add
/// --oauth` (WebLogin, BrowserLoopback) and this test plays the browser:
/// form login, the authorize page tea printed, Authorize, then the redirect
/// to tea's loopback. Needs `KLS_LIVE_FORGE_PASSWORD` (env only).
#[tokio::test]
async fn live_tea_oauth_headless() {
    let (Some(host), Some(pass)) = (env("KLS_LIVE_FORGE_HOST"), env("KLS_LIVE_FORGE_PASSWORD"))
    else {
        eprintln!("KLS_LIVE_FORGE_HOST / KLS_LIVE_FORGE_PASSWORD not set: skipped");
        return;
    };
    let kind = env("KLS_LIVE_FORGE_KIND")
        .and_then(|k| ForgeKind::parse(&k))
        .unwrap_or(ForgeKind::Gitea);
    let user = env("KLS_LIVE_FORGE_USER").unwrap_or_else(|| "ana".into());
    let mut conn = Connection::new(kind, &host, "user").unwrap();
    conn.cli_name = format!("oauth-{host}");
    let base = conn.web_base();
    let login = kls_github::login::WebLogin::for_conn(
        &conn,
        kls_github::forge::auth::SignInMethod::BrowserLoopback,
        false,
    );
    let (l2, tea) = (login.clone(), kls_github::forge::CliKind::Tea.program());
    let run = tokio::spawn(async move { l2.run(&tea, || {}).await });
    let started = std::time::Instant::now();
    let url = loop {
        if let kls_github::login::WebState::Browser { url, .. } = login.state()
            && !url.is_empty()
        {
            break url;
        }
        assert!(started.elapsed().as_secs() < 20, "{:?}", login.state());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert!(url.contains("/login/oauth/authorize"), "{url}");
    let mut b = Browser::new();
    let (_, _, page) = b.send(b.http.get(format!("{base}/user/login"))).await;
    let (st, _, _) = b
        .send(b.http.post(format!("{base}/user/login")).form(&[
            ("_csrf", input(&page, "_csrf").as_str()),
            ("user_name", user.as_str()),
            ("password", pass.as_str()),
        ]))
        .await;
    assert_eq!(st, 303, "form login");
    let (st, mut callback, page) = b.send(b.http.get(&url)).await;
    if callback.is_empty() {
        assert_eq!(st, 200, "the grant page");
        let fields: Vec<(&str, String)> = [
            "_csrf",
            "client_id",
            "state",
            "scope",
            "nonce",
            "redirect_uri",
        ]
        .into_iter()
        .map(|f| (f, input(&page, f)))
        .collect();
        let mut form: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
        form.push(("granted", "true"));
        let (st, to, _) = b
            .send(b.http.post(format!("{base}/login/oauth/grant")).form(&form))
            .await;
        assert!((300..400).contains(&st), "grant: {st}");
        callback = to;
    }
    assert!(callback.starts_with("http://127.0.0.1:"), "{callback}");
    let (st, _, _) = b.send(b.http.get(&callback)).await;
    assert_eq!(st, 200, "tea's loopback callback");
    tokio::time::timeout(std::time::Duration::from_secs(30), run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(login.state(), kls_github::login::WebState::Done);
    eprintln!(
        "tea OAuth approved headlessly in {} ms",
        started.elapsed().as_millis()
    );
}
