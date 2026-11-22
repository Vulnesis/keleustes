//! End-to-end tests over the fake `gh` (no network).

use crate::fake::{FakeExec, FakeGateway, FakeGh, Reply};
use crate::merge3::Choice;
use crate::sync::{self, PullAction};
use crate::*;
use base64::Engine;
use kls_core::Core;
use kls_core::github::kinds;
use kls_store::github::{GhOrg, GhRepo, Mirroring};
use kls_store::{Capabilities, HostInfo};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

async fn core() -> (Core, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    (core, dir)
}

fn local(gh: &Arc<FakeGh>, auth: Option<bool>) -> LocalGh {
    LocalGh::new(Arc::new(FakeExec {
        auth,
        gh: gh.clone(),
    }))
}

fn client(core: &Core, gh: &Arc<FakeGh>) -> Client {
    Client::new(gh.clone(), core.clone())
}

fn events(core: &Core, rx: &mut tokio::sync::broadcast::Receiver<kls_store::Event>) -> Vec<String> {
    let _ = core;
    let mut out = Vec::new();
    while let Ok(e) = rx.try_recv() {
        out.push(e.kind);
    }
    out
}

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s)
}

fn pull_json(n: i64, head: &str, updated: &str) -> Value {
    json!({
        "number": n, "title": format!("PR {n}"), "state": "open", "draft": false,
        "user": {"login": "ana"}, "html_url": format!("https://github.com/acme/api/pull/{n}"),
        "head": {"ref": "feat", "sha": head, "repo": {"full_name": "acme/api"}},
        "base": {"ref": "main", "sha": "base1"},
        "requested_reviewers": [], "labels": [],
        "created_at": "2026-10-20T00:00:00Z", "updated_at": updated
    })
}

fn register_pull(gh: &FakeGh, n: i64, detail: Value) {
    let head = detail["head"]["sha"].as_str().unwrap().to_string();
    gh.on("GET", &format!("/repos/acme/api/pulls/{n}"), 200, detail);
    gh.on(
        "GET",
        &format!("/repos/acme/api/commits/{head}/check-runs?per_page=100"),
        200,
        json!({"total_count": 2, "check_runs": [
            {"name": "build", "status": "completed", "conclusion": "success"},
            {"name": "lint", "status": "completed", "conclusion": "failure"}
        ]}),
    );
    gh.on(
        "GET",
        &format!("/repos/acme/api/pulls/{n}/reviews?per_page=100"),
        200,
        json!([{"id": 91, "user": {"login": "bo"}, "state": "APPROVED",
                "submitted_at": "2026-10-20T02:00:00Z", "body": "lgtm", "author_association": "MEMBER"}]),
    );
}

async fn managed_repo(core: &Core, full: &str, m: Mirroring) {
    let (o, n) = full.split_once('/').unwrap();
    let s = core.store();
    s.gh_upsert_org(&GhOrg {
        login: o.into(),
        kind: "Organization".into(),
        managed: true,
        enabled: true,
        ..Default::default()
    })
    .await
    .unwrap();
    s.gh_upsert_repo(&GhRepo {
        full_name: full.into(),
        owner: o.into(),
        name: n.into(),
        default_branch: "main".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    s.gh_set_repo_managed(full, true).await.unwrap();
    s.gh_set_repo_mirroring(full, m).await.unwrap();
}

// ---- routes ------------------------------------------------------------------

#[tokio::test]
async fn route_prefers_local_gh() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let sel = Selector::new(core.clone(), local(&gh, Some(true)));
    let mut rx = core.subscribe();
    let (s, client) = sel.select().await;
    assert_eq!(s.route, Route::Local);
    assert_eq!(s.label(), "via local gh");
    assert!(client.is_some());
    assert_eq!(events(&core, &mut rx), vec![kinds::ROUTE.to_string()]);
    // Cached: no second event, no second auth check.
    sel.select().await;
    assert!(events(&core, &mut rx).is_empty());
}

#[tokio::test]
async fn route_unavailable_says_why() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let sel = Selector::new(core.clone(), local(&gh, None));
    let (s, client) = sel.select().await;
    assert!(client.is_none());
    let label = s.label();
    assert!(
        label.starts_with("unavailable: gh is not installed"),
        "{label}"
    );
    assert!(label.contains("no node server in this process"), "{label}");

    let sel = Selector::new(core.clone(), local(&gh, Some(false)));
    core.set_gateway(FakeGateway::new(&[], gh.clone()));
    let label = sel.select().await.0.label();
    assert!(label.contains("not signed in"), "{label}");
    assert!(
        label.contains("no connected node reports gh_authenticated"),
        "{label}"
    );
}

#[tokio::test]
async fn route_falls_back_to_a_gh_node() {
    let (core, _d) = core().await;
    let store = core.store();
    let host = |gh: bool| HostInfo {
        hostname: "h".into(),
        capabilities: Capabilities {
            gh_authenticated: gh,
            ..Default::default()
        },
        ..Default::default()
    };
    store
        .insert_node("n1", "laptop", &host(false), "s1", "t")
        .await
        .unwrap();
    store
        .insert_node("n2", "builder", &host(true), "s2", "t")
        .await
        .unwrap();
    store
        .insert_node("n3", "offline", &host(true), "s3", "t")
        .await
        .unwrap();
    let remote = FakeGh::new();
    remote.on("GET", "/user", 200, json!({"login": "ana"}));
    let gw = FakeGateway::new(&["n1", "n2"], remote.clone());
    core.set_gateway(gw.clone());
    let sel = Selector::new(core.clone(), local(&FakeGh::new(), Some(false)));
    let (s, gh) = sel.select().await;
    assert_eq!(
        s.route,
        Route::Node {
            id: "n2".into(),
            name: "builder".into()
        }
    );
    assert_eq!(s.label(), "via node builder");
    let c = Client::new(gh.unwrap(), core.clone());
    let me = c.get_json("/user", "user").await.unwrap();
    assert_eq!(me["login"], "ana");
    let jobs = gw.jobs.lock().unwrap().clone();
    assert_eq!(jobs.len(), 1);
    let (node, kind, payload, needs) = &jobs[0];
    assert_eq!(node.as_deref(), Some("n2"));
    assert_eq!(kind, "github");
    assert!(needs.gh);
    let p: Value = serde_json::from_str(payload).unwrap();
    assert_eq!(p["args"][0], "api");
    assert_eq!(p["args"].as_array().unwrap().last().unwrap(), "/user");
    assert!(p["stdin"].is_null());
    // Budget and cache are kept per route.
    assert!(store.gh_quota("node:n2", "core").await.unwrap().is_some());
}

/// The connection prefers a node that is offline: the selector falls back
/// to the usual order (local CLI, then another node), and goes back to the
/// preferred node once it is online.
#[tokio::test]
async fn preferred_node_offline_falls_back_then_returns() {
    use crate::forge::kind::{Connection, RoutePref};
    let (core, _d) = core().await;
    let store = core.store();
    let host = HostInfo {
        hostname: "h".into(),
        capabilities: Capabilities {
            gh_authenticated: true,
            ..Default::default()
        },
        ..Default::default()
    };
    store
        .insert_node("n2", "builder", &host, "s2", "t")
        .await
        .unwrap();
    store
        .insert_node("n3", "pref", &host, "s3", "t")
        .await
        .unwrap();
    let mut conn = Connection::github_com();
    conn.route_pref = RoutePref::Node("n3".into());
    core.set_gateway(FakeGateway::new(&["n2"], FakeGh::new()));
    let sel = Selector::new(core.clone(), local(&FakeGh::new(), Some(false)));
    let (s, gh) = sel.select_conn(&conn).await;
    assert!(gh.is_some());
    assert_eq!(
        s.route,
        Route::Node {
            id: "n2".into(),
            name: "builder".into()
        }
    );
    // Online: the preference wins (a fresh core, as the gateway is set once).
    let (core, _d2) = self::core().await;
    for (id, name) in [("n2", "builder"), ("n3", "pref")] {
        core.store()
            .insert_node(id, name, &host, id, "t")
            .await
            .unwrap();
    }
    core.set_gateway(FakeGateway::new(&["n2", "n3"], FakeGh::new()));
    let sel = Selector::new(core.clone(), local(&FakeGh::new(), Some(false)));
    assert_eq!(
        sel.select_conn(&conn).await.0.route,
        Route::Node {
            id: "n3".into(),
            name: "pref".into()
        }
    );
}

/// An older belai lists its gh login in `forges` but does not run `forge`
/// jobs: github.com goes to it as a `github` job. A node that lists `forge`
/// in `job_kinds` gets a `forge` job instead.
#[tokio::test]
async fn github_com_node_without_forge_job_gets_the_github_job() {
    use kls_store::ForgeLogin;
    let (core, _d) = core().await;
    let store = core.store();
    let host = |kinds: &[&str]| HostInfo {
        hostname: "h".into(),
        capabilities: Capabilities {
            gh_authenticated: true,
            job_kinds: kinds.iter().map(|k| k.to_string()).collect(),
            forges: vec![ForgeLogin {
                cli: "gh".into(),
                host: "github.com".into(),
                kind: "github".into(),
                login: "ana".into(),
                signed_in: true,
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    store
        .insert_node(
            "n1",
            "old",
            &host(&["github", "model", "forge_push"]),
            "s1",
            "t",
        )
        .await
        .unwrap();
    let remote = FakeGh::new();
    remote.on("GET", "/user", 200, json!({"login": "ana"}));
    let gw = FakeGateway::new(&["n1", "n2"], remote.clone());
    core.set_gateway(gw.clone());
    let sel = Selector::new(core.clone(), local(&FakeGh::new(), Some(false)));
    let (s, gh) = sel.select().await;
    assert_eq!(s.label(), "via node old");
    let c = Client::new(gh.unwrap(), core.clone());
    assert_eq!(c.get_json("/user", "user").await.unwrap()["login"], "ana");
    let (_, kind, _, needs) = gw.jobs.lock().unwrap()[0].clone();
    assert_eq!(kind, "github");
    assert!(needs.gh && needs.forge.is_none());

    // A node that runs forge jobs wins over it (sorted first, though "z-new"
    // comes after "old" by name), and github.com goes there as a `forge` job.
    store
        .insert_node(
            "n2",
            "z-new",
            &host(&["github", "forge", "forge_push"]),
            "s2",
            "t",
        )
        .await
        .unwrap();
    sel.invalidate();
    let (s, gh) = sel.select().await;
    assert_eq!(s.label(), "via node z-new");
    let c = Client::new(gh.unwrap(), core.clone());
    assert_eq!(c.get_json("/user", "user").await.unwrap()["login"], "ana");
    let (node, kind, payload, needs) = gw.jobs.lock().unwrap().last().cloned().unwrap();
    assert_eq!((node.as_deref(), kind.as_str()), (Some("n2"), "forge"));
    assert_eq!(needs.forge, Some(("gh".into(), "github.com".into())));
    let p: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(
        (p["cli"].as_str(), p["host"].as_str()),
        (Some("gh"), Some("github.com"))
    );
}

// ---- client --------------------------------------------------------------------

#[tokio::test]
async fn etag_makes_the_second_read_a_free_304() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    gh.on(
        "GET",
        "/repos/acme/api",
        200,
        json!({"full_name": "acme/api"}),
    );
    let c = client(&core, &gh);
    let r1 = c.get("/repos/acme/api", "poller").await.unwrap();
    assert!(!r1.not_modified);
    let r2 = c.get("/repos/acme/api", "poller").await.unwrap();
    assert!(r2.not_modified);
    assert_eq!(r2.json().unwrap()["full_name"], "acme/api");
    let calls = gh.calls();
    assert!(calls[0].if_none_match.is_none());
    assert!(calls[1].if_none_match.is_some());
    assert_eq!(gh.remaining(), 4999, "304 does not cost");
    let store = core.store();
    let hour = store
        .gh_calls_in_hour("local", kls_store::now_ms())
        .await
        .unwrap();
    assert_eq!((hour[0].calls, hour[0].not_modified), (2, 1));
    assert_eq!(budget::factory_used(&hour), 1);
    let q = store.gh_quota("local", "core").await.unwrap().unwrap();
    assert_eq!((q.limit, q.remaining), (5000, 4999));
    // A write drops the cache under its prefix.
    c.invalidate("/repos/acme/api").await.unwrap();
    assert!(
        !c.get("/repos/acme/api", "poller")
            .await
            .unwrap()
            .not_modified
    );
}

#[tokio::test]
async fn pages_follow_the_link_header() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    gh.on_reply(
        "GET",
        "/user/orgs?per_page=100",
        Reply {
            status: 200,
            body: json!([{"login": "a"}]).to_string(),
            headers: vec![(
                "Link".into(),
                "<https://api.github.com/user/orgs?per_page=100&page=2>; rel=\"next\"".into(),
            )],
        },
    );
    gh.on(
        "GET",
        "/user/orgs?per_page=100&page=2",
        200,
        json!([{"login": "b"}]),
    );
    let c = client(&core, &gh);
    let p = c.get_all("/user/orgs?per_page=100", "user").await.unwrap();
    assert_eq!(p.pages, 2);
    assert_eq!(p.items.len(), 2);
    assert!(!p.not_modified);
    let again = c.get_all("/user/orgs?per_page=100", "user").await.unwrap();
    assert!(again.not_modified);
    assert_eq!(again.items.len(), 2, "cached link is followed too");
}

#[tokio::test]
async fn errors_and_secondary_limit_back_off() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let c = client(&core, &gh);
    let e = c.get("/repos/acme/nope", "user").await.unwrap_err();
    assert_eq!(
        e.downcast_ref::<GhError>(),
        Some(&GhError::Http {
            status: 404,
            message: "Not Found".into()
        })
    );
    gh.on_reply(
        "GET",
        "/repos/acme/api",
        Reply {
            status: 403,
            body: r#"{"message":"You have exceeded a secondary rate limit"}"#.into(),
            headers: vec![("Retry-After".into(), "120".into())],
        },
    );
    let e = c.get("/repos/acme/api", "poller").await.unwrap_err();
    assert!(matches!(
        e.downcast_ref::<GhError>(),
        Some(GhError::RateLimited { .. })
    ));
    let q = core
        .store()
        .gh_quota("local", "core")
        .await
        .unwrap()
        .unwrap();
    assert!(q.secondary_until > kls_store::now_ms() + 100_000);
    // The poller is refused without calling GitHub; a person still may.
    let before = gh.calls().len();
    let e = c.get("/repos/acme/x", "poller").await.unwrap_err();
    assert!(matches!(
        e.downcast_ref::<GhError>(),
        Some(GhError::RateLimited { .. })
    ));
    assert_eq!(gh.calls().len(), before);
    assert!(c.get("/repos/acme/x", "user").await.is_err());
    assert_eq!(gh.calls().len(), before + 1);
}

#[tokio::test]
async fn share_exhausted_stops_the_poller_only() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    // 1000 left of 5000: below the 20% reserve.
    gh.set_rate(5000, 1000, kls_store::now_ms() / 1000 + 3000);
    gh.on("GET", "/a", 200, json!({}));
    let c = client(&core, &gh);
    c.rate_limit().await.unwrap();
    let e = c.get("/a", "poller").await.unwrap_err();
    assert_eq!(e.downcast_ref::<GhError>(), Some(&GhError::ShareExhausted));
    assert!(c.get("/a", "user").await.is_ok());
    assert!(c.get("/a", "factory:patch-train").await.is_ok());
}

// ---- sync ----------------------------------------------------------------------

#[tokio::test]
async fn orgs_and_repos_refresh() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    gh.on("GET", "/user", 200, json!({"login": "ana"}));
    gh.on(
        "GET",
        "/user/orgs?per_page=100",
        200,
        json!([{"login": "acme"}]),
    );
    gh.on(
        "GET",
        "/orgs/acme/repos?type=all&per_page=100",
        200,
        json!([{"full_name": "acme/api", "private": true, "permissions": {"push": true}},
               {"full_name": "not a repo"}]),
    );
    let c = client(&core, &gh);
    let r = sync::refresh_orgs(&c, "user", true).await.unwrap();
    assert_eq!((r.orgs, r.repos), (2, 0), "no org is managed yet");
    core.store().gh_set_org_managed("acme", true).await.unwrap();
    let r = sync::refresh_orgs(&c, "user", true).await.unwrap();
    assert_eq!(r.repos, 1);
    assert_eq!(r.added, vec!["acme/api".to_string()]);
    let org = core.store().gh_org("acme").await.unwrap().unwrap();
    assert!(org.managed, "kept across refresh");
    assert_eq!(org.repo_count, 1);
    assert_eq!(
        core.store().gh_org("ana").await.unwrap().unwrap().kind,
        "User"
    );
    assert!(
        core.store()
            .gh_repo("acme/api")
            .await
            .unwrap()
            .unwrap()
            .can_push
    );
}

#[tokio::test]
async fn pulls_sync_then_quiet_pass_then_merge_captures_delivery() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let list = "/repos/acme/api/pulls?state=open&per_page=100";
    let p8 = pull_json(8, "h8", "2026-10-20T01:00:00Z");
    gh.on("GET", list, 200, json!([p8]));
    let mut d8 = p8.clone();
    d8["mergeable"] = json!(false);
    d8["mergeable_state"] = json!("dirty");
    d8["additions"] = json!(120);
    d8["deletions"] = json!(30);
    d8["author_association"] = json!("MEMBER");
    register_pull(&gh, 8, d8.clone());
    let c = client(&core, &gh);
    let mut rx = core.subscribe();
    assert_eq!(sync::sync_pulls(&c, "acme/api", "poller").await.unwrap(), 1);
    assert_eq!(
        events(&core, &mut rx),
        vec![
            kinds::PULL_OPENED.to_string(),
            kinds::CONFLICT_DETECTED.to_string()
        ]
    );
    let p = core.store().gh_pull("acme/api", 8).await.unwrap().unwrap();
    assert_eq!(p.mergeable, "conflict");
    assert_eq!(p.checks_state, "failure");
    assert_eq!(p.checks.failing, vec!["lint".to_string()]);
    assert_eq!(p.review_decision, "APPROVED");
    assert_eq!(core.store().gh_conflicting_pulls().await.unwrap().len(), 1);

    // Nothing changed: one 304 for the list, nothing else, no event.
    gh.clear_calls();
    assert_eq!(sync::sync_pulls(&c, "acme/api", "poller").await.unwrap(), 0);
    assert_eq!(gh.calls().len(), 1);
    assert!(events(&core, &mut rx).is_empty());

    // It merges: gone from the open list, read once more, delivery captured.
    gh.on("GET", list, 200, json!([]));
    let mut merged = d8.clone();
    merged["state"] = json!("closed");
    merged["merged"] = json!(true);
    merged["merged_at"] = json!("2026-10-20T10:00:00Z");
    merged["closed_at"] = json!("2026-10-20T10:00:00Z");
    merged["updated_at"] = json!("2026-10-20T10:00:00Z");
    register_pull(&gh, 8, merged);
    gh.on(
        "GET",
        "/repos/acme/api/pulls/8/commits?per_page=100",
        200,
        json!([{
            "sha": "c1", "author": {"login": "ana"},
            "commit": {"author": {"name": "Ana", "email": "Ana@acme.example", "date": "2026-10-20T00:30:00Z"},
                       "message": "Fix\n\nSigned-off-by: Ana <ana@acme.example>\nCo-authored-by: Cy <cy@acme.example>",
                       "verification": {"verified": true}}
        }]),
    );
    assert_eq!(sync::sync_pulls(&c, "acme/api", "poller").await.unwrap(), 1);
    let ev = events(&core, &mut rx);
    assert!(ev.contains(&kinds::PULL_MERGED.to_string()), "{ev:?}");
    assert!(ev.contains(&kinds::DELIVERY_CAPTURED.to_string()), "{ev:?}");
    let s = core.store();
    let o = s.outcome("pr:acme/api#8").await.unwrap().unwrap();
    assert_eq!(o.result, "merged");
    assert_eq!(o.cycle_ms, Some(10 * 3_600_000));
    assert_eq!(o.first_review_ms, Some(2 * 3_600_000));
    assert_eq!(o.reviewers, vec!["bo".to_string()]);
    assert_eq!(o.commits, Some(1));
    let acts = s.actions_between(0, i64::MAX).await.unwrap();
    let kinds_of: Vec<(String, String)> = acts
        .iter()
        .map(|a| (a.source.clone(), a.action_kind.clone()))
        .collect();
    assert!(kinds_of.contains(&("admin".into(), "pr_opened".into())));
    assert!(kinds_of.contains(&("review".into(), "approved".into())));
    assert!(kinds_of.contains(&("commit".into(), "commit".into())));
    assert!(kinds_of.contains(&("commit".into(), "coauthor".into())));
    assert!(
        !kinds_of.contains(&("commit".into(), "signoff".into())),
        "the author's own sign-off is not a second person"
    );
    let commits = s.commits_between(0, i64::MAX).await.unwrap();
    assert_eq!(commits[0].signed, Some(true));
    assert_eq!(commits[0].author_email, "ana@acme.example");
    assert_eq!(
        s.person("gh:bo").await.unwrap().unwrap().is_internal,
        Some(true)
    );
    assert!(
        s.gh_pull("acme/api", 8)
            .await
            .unwrap()
            .unwrap()
            .delivery_captured
    );
    // Captured once.
    gh.clear_calls();
    sync::refresh_pull(&c, "acme/api", 8, "user").await.unwrap();
    assert_eq!(
        gh.count("GET", "/repos/acme/api/pulls/8/commits?per_page=100"),
        0
    );
}

#[tokio::test]
async fn issues_mirror_emits_card_events() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let base = "/repos/acme/api/issues?state=all&sort=updated&direction=asc&per_page=100";
    gh.on(
        "GET",
        base,
        200,
        json!([
            {"number": 3, "title": "Bug", "state": "open", "user": {"login": "zed"},
             "author_association": "NONE", "labels": [{"name": "bug"}],
             "created_at": "2026-10-20T00:00:00Z", "updated_at": "2026-10-20T00:00:00Z"},
            {"number": 4, "title": "a PR", "state": "open", "pull_request": {},
             "updated_at": "2026-10-20T00:00:00Z"}
        ]),
    );
    let c = client(&core, &gh);
    let mut rx = core.subscribe();
    assert_eq!(
        sync::sync_issues(&c, "acme/api", "poller").await.unwrap(),
        1
    );
    assert_eq!(
        events(&core, &mut rx),
        vec![kinds::ISSUE_OPENED.to_string()]
    );
    let since = format!("{base}&since=2026-10-20T00:00:00Z");
    gh.on(
        "GET",
        &since,
        200,
        json!([{"number": 3, "title": "Bug", "state": "closed", "state_reason": "completed",
                "user": {"login": "zed"}, "author_association": "NONE",
                "created_at": "2026-10-20T00:00:00Z", "updated_at": "2026-10-21T00:00:00Z",
                "closed_at": "2026-10-21T00:00:00Z"}]),
    );
    assert_eq!(
        sync::sync_issues(&c, "acme/api", "poller").await.unwrap(),
        1
    );
    let ev = events(&core, &mut rx);
    assert_eq!(ev[0], kinds::ISSUE_CLOSED);
    assert!(ev.contains(&kinds::DELIVERY_CAPTURED.to_string()));
    let o = core
        .store()
        .outcome("issue:acme/api#3")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(o.cycle_ms, Some(86_400_000));
    assert_eq!(
        core.store()
            .person("gh:zed")
            .await
            .unwrap()
            .unwrap()
            .is_internal,
        Some(false)
    );
    assert_eq!(
        core.store()
            .gh_issues(Some("acme/api"), None)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn labels_sync_creates_missing_sf_labels() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    gh.on(
        "GET",
        "/repos/acme/api/labels?per_page=100",
        200,
        json!([{"name": "bug", "color": "d73a4a"}, {"name": "sf:fix", "color": "ffffff"}]),
    );
    gh.on(
        "POST",
        "/repos/acme/api/labels",
        201,
        json!({"name": "sf:x", "color": "00e0c9"}),
    );
    let c = client(&core, &gh);
    let r = sync::sync_labels(&c, "acme/api", true, "user")
        .await
        .unwrap();
    assert_eq!(r.existing, 1);
    assert_eq!(r.created.len(), 33);
    assert!(r.error.is_empty());
    let posts: Vec<_> = gh
        .calls()
        .into_iter()
        .filter(|c| c.method == "POST")
        .collect();
    let body: Value = serde_json::from_str(posts[0].stdin.as_deref().unwrap()).unwrap();
    assert_eq!(body["name"], "sf:triage");
    assert_eq!(body["color"], "00e0c9");
    // A label that exists on GitHub (422) counts as existing.
    gh.on(
        "POST",
        "/repos/acme/api/labels",
        422,
        json!({"message": "Validation Failed"}),
    );
    gh.on("GET", "/repos/acme/api/labels?per_page=100", 200, json!([]));
    let r = sync::sync_labels(&c, "acme/api", true, "user")
        .await
        .unwrap();
    assert_eq!((r.created.len(), r.existing), (0, 34));
}

#[tokio::test]
async fn pull_actions_call_the_right_endpoints() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let mut d = pull_json(5, "h5", "2026-10-20T01:00:00Z");
    d["mergeable"] = json!(true);
    d["mergeable_state"] = json!("clean");
    register_pull(&gh, 5, d);
    gh.on(
        "PUT",
        "/repos/acme/api/pulls/5/update-branch",
        202,
        json!({"message": "Updating"}),
    );
    gh.on(
        "POST",
        "/repos/acme/api/pulls/5/requested_reviewers",
        201,
        json!({}),
    );
    gh.on(
        "PUT",
        "/repos/acme/api/pulls/5/merge",
        200,
        json!({"merged": true}),
    );
    let c = client(&core, &gh);
    sync::refresh_pull(&c, "acme/api", 5, "user").await.unwrap();

    sync::pull_action(&c, "acme/api", 5, &PullAction::Draft, "user")
        .await
        .unwrap();
    let draft = gh.calls().into_iter().find(|c| c.args[0] == "pr").unwrap();
    assert_eq!(
        draft.args,
        args(["pr", "ready", "5", "--undo", "-R", "acme/api"])
    );

    sync::pull_action(&c, "acme/api", 5, &PullAction::UpdateBranch, "user")
        .await
        .unwrap();
    let ub = gh
        .calls()
        .into_iter()
        .find(|c| c.method == "PUT" && c.path.ends_with("update-branch"))
        .unwrap();
    let body: Value = serde_json::from_str(ub.stdin.as_deref().unwrap()).unwrap();
    assert_eq!(body["expected_head_sha"], "h5");

    let who = PullAction::RequestReviewers(vec!["bo".into(), "acme/ops".into()]);
    sync::pull_action(&c, "acme/api", 5, &who, "user")
        .await
        .unwrap();
    let rr = gh.calls().into_iter().find(|c| c.method == "POST").unwrap();
    let body: Value = serde_json::from_str(rr.stdin.as_deref().unwrap()).unwrap();
    assert_eq!(
        body,
        json!({"reviewers": ["bo"], "team_reviewers": ["ops"]})
    );
    assert!(
        sync::pull_action(
            &c,
            "acme/api",
            5,
            &PullAction::RequestReviewers(vec![]),
            "user"
        )
        .await
        .is_err()
    );

    sync::pull_action(
        &c,
        "acme/api",
        5,
        &PullAction::Merge("squash".into()),
        "user",
    )
    .await
    .unwrap();
    let m = gh
        .calls()
        .into_iter()
        .find(|c| c.method == "PUT" && c.path.ends_with("/merge"))
        .unwrap();
    let body: Value = serde_json::from_str(m.stdin.as_deref().unwrap()).unwrap();
    assert_eq!(body, json!({"merge_method": "squash", "sha": "h5"}));
    assert!(
        sync::pull_action(&c, "acme/api", 5, &PullAction::Merge("yolo".into()), "user")
            .await
            .is_err()
    );

    let res = sync::bulk_pull_action(
        &c,
        &[("acme/api".into(), 5), ("acme/api".into(), 6)],
        &PullAction::Ready,
        "user",
    )
    .await
    .unwrap();
    assert!(res[0].ok);
    assert!(!res[1].ok, "pull 6 is unknown: {:?}", res[1]);
    assert!(
        sync::bulk_pull_action(
            &c,
            &[("acme/api".into(), 5), ("acme/api".into(), 5)],
            &PullAction::Ready,
            "user"
        )
        .await
        .is_err()
    );
}

// ---- conflicts -----------------------------------------------------------------

fn conflict_fixture(gh: &FakeGh) {
    let mut d = pull_json(12, "head1", "2026-10-20T01:00:00Z");
    d["base"]["sha"] = json!("base1");
    d["mergeable"] = json!(false);
    d["mergeable_state"] = json!("dirty");
    register_pull(gh, 12, d);
    gh.on(
        "GET",
        "/repos/acme/api/compare/base1...head1",
        200,
        json!({"merge_base_commit": {"sha": "mb"}, "files": [
            {"filename": "src/a.rs", "status": "modified"},
            {"filename": "src/clean.rs", "status": "modified"}
        ]}),
    );
    gh.on(
        "GET",
        "/repos/acme/api/compare/mb...base1",
        200,
        json!({"files": [
            {"filename": "src/a.rs", "status": "modified", "sha": "ba"},
            {"filename": "src/clean.rs", "status": "modified", "sha": "bc"},
            {"filename": "docs/new.md", "status": "added", "sha": "bn"},
            {"filename": "old.txt", "status": "removed", "sha": "bo"}
        ]}),
    );
    let content = |path: &str, sha: &str, text: &str| {
        gh.on(
            "GET",
            &format!("/repos/acme/api/contents/{path}?ref={sha}"),
            200,
            json!({"encoding": "base64", "content": b64(text)}),
        );
    };
    content("src/a.rs", "mb", "fn a() {\n    1\n}\n");
    content("src/a.rs", "head1", "fn a() {\n    2\n}\n");
    content("src/a.rs", "base1", "fn a() {\n    3\n}\n");
    content("src/clean.rs", "mb", "a\nb\nc\nd\n");
    content("src/clean.rs", "head1", "A\nb\nc\nd\n");
    content("src/clean.rs", "base1", "a\nb\nc\nD\n");
    gh.on(
        "GET",
        "/repos/acme/api/git/trees/base1?recursive=1",
        200,
        json!({"tree": [{"path": "src/a.rs", "mode": "100755"}]}),
    );
    gh.on(
        "GET",
        "/repos/acme/api/git/commits/head1",
        200,
        json!({"tree": {"sha": "tree-head"}}),
    );
    gh.on(
        "POST",
        "/repos/acme/api/git/blobs",
        201,
        json!({"sha": "blob-x"}),
    );
    gh.on(
        "POST",
        "/repos/acme/api/git/trees",
        201,
        json!({"sha": "tree-new"}),
    );
    gh.on(
        "POST",
        "/repos/acme/api/git/commits",
        201,
        json!({"sha": "merge1", "verification": {"verified": true, "reason": "valid"}}),
    );
    gh.on(
        "PATCH",
        "/repos/acme/api/git/refs/heads/feat",
        200,
        json!({"object": {"sha": "merge1"}}),
    );
}

#[tokio::test]
async fn conflicts_load_and_commit_a_resolution() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    conflict_fixture(&gh);
    let c = client(&core, &gh);
    let set = conflicts::load(&c, "acme/api", 12, "user").await.unwrap();
    assert_eq!(set.merge_base, "mb");
    assert_eq!(set.files.len(), 2);
    let a = set.files.iter().find(|f| f.path == "src/a.rs").unwrap();
    assert!(a.conflicting);
    assert_eq!(a.merge.conflicts(), 1);
    assert_eq!(a.base.as_deref(), Some("fn a() {\n    1\n}\n"));
    let clean = set.files.iter().find(|f| f.path == "src/clean.rs").unwrap();
    assert!(!clean.conflicting);
    assert_eq!(clean.merge.render(&[]), "A\nb\nc\nD\n");
    assert_eq!(set.base_only.len(), 2);
    assert_eq!(set.conflicting().count(), 1);

    // Unresolved files are refused; markers are refused.
    assert!(
        conflicts::commit(&c, &set, &BTreeMap::new(), "", "user")
            .await
            .is_err()
    );
    let mut resolved = BTreeMap::new();
    resolved.insert("src/a.rs".to_string(), Some(a.merge.render(&[])));
    let e = conflicts::commit(&c, &set, &resolved, "", "user")
        .await
        .unwrap_err();
    assert!(format!("{e}").contains("conflict markers"));

    resolved.insert("src/a.rs".into(), Some(a.merge.render(&[Choice::Theirs])));
    let mut rx = core.subscribe();
    let res = conflicts::commit(&c, &set, &resolved, "", "user")
        .await
        .unwrap();
    assert_eq!(res.commit_sha, "merge1");
    assert_eq!(res.signing, "github_signs");
    assert_eq!(res.verified, Some(true));
    assert_eq!(
        res.parent_shas,
        vec!["head1".to_string(), "base1".to_string()]
    );
    assert!(events(&core, &mut rx).contains(&kinds::CONFLICT_RESOLVED.to_string()));
    assert_eq!(
        core.store()
            .gh_resolutions("acme/api", 12)
            .await
            .unwrap()
            .len(),
        1
    );

    let calls = gh.calls();
    let blobs: Vec<Value> = calls
        .iter()
        .filter(|c| c.path == "/repos/acme/api/git/blobs")
        .map(|c| serde_json::from_str(c.stdin.as_deref().unwrap()).unwrap())
        .collect();
    assert_eq!(blobs.len(), 2);
    assert!(blobs.iter().any(|b| b["content"] == "fn a() {\n    3\n}\n"));
    assert!(blobs.iter().any(|b| b["content"] == "A\nb\nc\nD\n"));
    let tree: Value = serde_json::from_str(
        calls
            .iter()
            .find(|c| c.path == "/repos/acme/api/git/trees")
            .unwrap()
            .stdin
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(tree["base_tree"], "tree-head");
    let entries = tree["tree"].as_array().unwrap();
    let find = |p: &str| entries.iter().find(|e| e["path"] == p).unwrap().clone();
    assert_eq!(
        find("src/a.rs")["mode"],
        "100755",
        "mode from the base tree"
    );
    assert_eq!(
        find("docs/new.md")["sha"],
        "bn",
        "base-only change taken as is"
    );
    assert!(find("old.txt")["sha"].is_null(), "base-only removal");
    let commit: Value = serde_json::from_str(
        calls
            .iter()
            .find(|c| c.path == "/repos/acme/api/git/commits")
            .unwrap()
            .stdin
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(commit["parents"], json!(["head1", "base1"]));
    assert_eq!(commit["message"], "Merge main into feat");
    let refp: Value = serde_json::from_str(
        calls
            .iter()
            .find(|c| c.method == "PATCH")
            .unwrap()
            .stdin
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(refp, json!({"sha": "merge1", "force": false}));
}

#[tokio::test]
async fn conflicts_refuse_forks_and_moved_heads() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    conflict_fixture(&gh);
    let c = client(&core, &gh);
    let mut set = conflicts::load(&c, "acme/api", 12, "user").await.unwrap();
    let mut resolved = BTreeMap::new();
    resolved.insert("src/a.rs".to_string(), Some("x\n".to_string()));
    gh.on(
        "PATCH",
        "/repos/acme/api/git/refs/heads/feat",
        422,
        json!({"message": "Update is not a fast forward"}),
    );
    let e = conflicts::commit(&c, &set, &resolved, "m", "user")
        .await
        .unwrap_err();
    assert!(format!("{e}").contains("head branch moved"), "{e}");
    set.head_repo = "someone/api".into();
    let e = conflicts::commit(&c, &set, &resolved, "m", "user")
        .await
        .unwrap_err();
    assert!(format!("{e}").contains("fork"), "{e}");
}

// ---- poller and service ---------------------------------------------------------

#[tokio::test]
async fn poll_once_reads_managed_repos_within_budget() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    managed_repo(
        &core,
        "acme/api",
        Mirroring {
            pulls: true,
            issues: true,
            labels: false,
        },
    )
    .await;
    // An unmanaged repo is never read.
    core.store()
        .gh_upsert_repo(&GhRepo {
            full_name: "acme/web".into(),
            owner: "acme".into(),
            name: "web".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    gh.on(
        "GET",
        "/repos/acme/api/pulls?state=open&per_page=100",
        200,
        json!([]),
    );
    gh.on(
        "GET",
        "/repos/acme/api/issues?state=all&sort=updated&direction=asc&per_page=100",
        200,
        json!([]),
    );
    let svc = Service::new(core.clone(), local(&gh, Some(true)));
    let rep = svc.poll_once().await.unwrap();
    assert_eq!(rep.route, "via local gh");
    assert_eq!(rep.repos, 1);
    assert!(rep.refused.is_none(), "{rep:?}");
    assert!(rep.errors.is_empty(), "{rep:?}");
    assert_eq!(gh.count("GET", "rate_limit"), 1, "budget read first");
    assert!(gh.calls().iter().all(|c| !c.path.contains("acme/web")));
    assert_eq!(svc.last_poll().unwrap().repos, 1);
    let q = svc.quota().await.unwrap();
    assert_eq!(q.factory_limit, 500);
    assert_eq!(q.reserve, 1000);

    // Disabled org: skipped.
    core.store()
        .gh_set_org_enabled("acme", false)
        .await
        .unwrap();
    assert_eq!(svc.poll_once().await.unwrap().repos, 0);
    core.store().gh_set_org_enabled("acme", true).await.unwrap();

    // Below the reserve: refused before any repository call.
    gh.set_rate(5000, 900, kls_store::now_ms() / 1000 + 3000);
    svc.client().await.unwrap().rate_limit().await.unwrap();
    gh.clear_calls();
    let rep = svc.poll_once().await.unwrap();
    assert!(rep.refused.as_deref().unwrap().contains("share"), "{rep:?}");
    assert!(
        gh.calls()
            .iter()
            .all(|c| c.path == "rate_limit" || c.path.is_empty())
    );
}

#[tokio::test]
async fn service_without_route_explains() {
    let (core, _d) = core().await;
    let svc = Service::new(core.clone(), local(&FakeGh::new(), Some(false)));
    let e = svc.refresh_orgs().await.unwrap_err();
    assert!(matches!(
        e.downcast_ref::<GhError>(),
        Some(GhError::Unavailable(_))
    ));
    let rep = svc.poll_once().await.unwrap();
    assert!(rep.refused.unwrap().starts_with("unavailable"));
    assert!(svc.sync_labels(&[]).await.is_err());
}

#[tokio::test]
async fn service_settings_and_switches() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    managed_repo(
        &core,
        "acme/api",
        Mirroring {
            pulls: true,
            issues: false,
            labels: false,
        },
    )
    .await;
    let svc = Service::new(core.clone(), local(&gh, Some(true)));
    let mut s = kls_store::github::RepoSettings {
        autonomy: "draft_pr".into(),
        ..Default::default()
    };
    assert_eq!(svc.save_repo_settings("acme/api", &s, 0).await.unwrap(), 1);
    s.autonomy = "pr".into();
    let e = svc.save_repo_settings("acme/api", &s, 0).await.unwrap_err();
    assert!(format!("{e}").contains("revision 1"));
    let n = svc
        .set_mirroring(
            &["acme/api".into()],
            Mirroring {
                pulls: false,
                issues: true,
                labels: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(n, 1);
    let r = core.store().gh_repo("acme/api").await.unwrap().unwrap();
    assert!(!r.mirror_pulls && r.mirror_issues && r.sync_labels);
    assert_eq!(
        svc.set_repos_managed(&["acme/api".into()], false)
            .await
            .unwrap(),
        1
    );
    assert!(core.store().gh_managed_repos().await.unwrap().is_empty());
}

#[tokio::test]
async fn mirrored_issues_become_board_cards() {
    let (core, _d) = core().await;
    managed_repo(
        &core,
        "acme/api",
        Mirroring {
            pulls: false,
            issues: true,
            labels: false,
        },
    )
    .await;
    let gh = FakeGh::new();
    let base = "/repos/acme/api/issues?state=all&sort=updated&direction=asc&per_page=100";
    let issue = |state: &str, title: &str, updated: &str| {
        json!({"number": 3, "title": title, "state": state, "user": {"login": "zed"},
               "body": "It breaks.", "html_url": "https://github.com/acme/api/issues/3",
               "author_association": "NONE", "labels": [{"name": "bug"}],
               "created_at": "2026-10-20T00:00:00Z", "updated_at": updated})
    };
    gh.on(
        "GET",
        base,
        200,
        json!([
            issue("open", "Bug", "2026-10-20T00:00:00Z"),
            // An issue already closed when first seen gets no card.
            {"number": 2, "title": "Old", "state": "closed", "user": {"login": "zed"},
             "created_at": "2026-10-01T00:00:00Z", "updated_at": "2026-10-02T00:00:00Z"}
        ]),
    );
    let c = client(&core, &gh);
    let mut rx = core.subscribe();
    sync::sync_issues(&c, "acme/api", "poller").await.unwrap();
    let cards = core.cards().await.unwrap();
    assert_eq!(cards.len(), 1);
    let card = &cards[0];
    assert_eq!(card.title, "#3 Bug");
    assert_eq!(card.status, "triage");
    assert_eq!(card.repo, "acme/api");
    assert_eq!(card.labels, vec!["bug".to_string()]);
    assert!(card.body.contains("https://github.com/acme/api/issues/3"));
    let stored = core.store().gh_issue("acme/api", 3).await.unwrap().unwrap();
    assert_eq!(stored.card_id.as_deref(), Some(card.id.as_str()));
    // The opened event names the new card and its labels.
    let opened: Vec<kls_store::Event> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|e| e.kind == kinds::ISSUE_OPENED)
        .collect();
    let data: Value = serde_json::from_str(&opened[0].data_json).unwrap();
    assert_eq!(data["cardId"], json!(card.id));
    assert_eq!(data["labelsAdded"], json!(["bug"]));

    // Idempotent: the issue changes title, the same card follows.
    let since = format!("{base}&since=2026-10-20T00:00:00Z");
    gh.on(
        "GET",
        &since,
        200,
        json!([issue("open", "Bug in login", "2026-10-21T00:00:00Z")]),
    );
    sync::sync_issues(&c, "acme/api", "poller").await.unwrap();
    let cards = core.cards().await.unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].title, "#3 Bug in login");
    // Calling the card mirror again changes nothing.
    let again = core.store().gh_issue("acme/api", 3).await.unwrap().unwrap();
    let v = cards[0].version;
    sync::issue_card(&core, &again).await.unwrap();
    assert_eq!(core.card(&card.id).await.unwrap().unwrap().version, v);

    // Closing the issue moves the card to done.
    let since = format!("{base}&since=2026-10-21T00:00:00Z");
    let mut closed = issue("closed", "Bug in login", "2026-10-22T00:00:00Z");
    closed["state_reason"] = json!("completed");
    closed["closed_at"] = json!("2026-10-22T00:00:00Z");
    gh.on("GET", &since, 200, json!([closed]));
    sync::sync_issues(&c, "acme/api", "poller").await.unwrap();
    let cards = core.cards().await.unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].status, "done");
}

// ---- outbound card-to-issue sync, review/package/risk events (WP3) -----------

const ISSUES: &str = "/repos/acme/api/issues?state=all&sort=updated&direction=asc&per_page=100";

fn issue3(state: &str, title: &str, updated: &str) -> Value {
    json!({"number": 3, "title": title, "state": state, "user": {"login": "zed"},
           "body": "It breaks.", "html_url": "https://github.com/acme/api/issues/3",
           "author_association": "NONE", "labels": [{"name": "bug"}],
           "created_at": "2026-10-20T00:00:00Z", "updated_at": updated})
}

/// A mirrored repo with issue #3 on a card; returns the client and the card id.
async fn mirrored_card(core: &Core, gh: &Arc<FakeGh>) -> (Client, String) {
    managed_repo(
        core,
        "acme/api",
        Mirroring {
            pulls: false,
            issues: true,
            labels: false,
        },
    )
    .await;
    gh.on(
        "GET",
        ISSUES,
        200,
        json!([issue3("open", "Fix login", "2026-10-20T00:00:00Z")]),
    );
    let c = client(core, gh);
    sync::sync_issues(&c, "acme/api", "poller").await.unwrap();
    let card = core.cards().await.unwrap().remove(0);
    (c, card.id)
}

async fn forge_audit(core: &Core) -> Vec<(String, String, String)> {
    core.audit_entries(&kls_store::audit::LedgerQuery {
        kinds: vec!["forge".into()],
        ..Default::default()
    })
    .await
    .unwrap()
    .into_iter()
    .map(|e| {
        let v: Value = serde_json::to_value(&e).unwrap();
        (
            v["subject"]["action"].as_str().unwrap_or("").to_string(),
            v["actor"].as_str().unwrap_or("").to_string(),
            v["payload"]["outcome"].as_str().unwrap_or("").to_string(),
        )
    })
    .collect()
}

fn reply(status: u16, body: Value) -> Reply {
    Reply {
        status,
        body: body.to_string(),
        headers: Vec::new(),
    }
}

#[tokio::test]
async fn outbound_sync_retries_then_pushes_and_closes_on_done() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let (c, id) = mirrored_card(&core, &gh).await;
    let card = core.card(&id).await.unwrap().unwrap();
    // Nothing changed: nothing is written.
    let s = sync::sync_cards(&c, "acme/api", "poller").await.unwrap();
    assert_eq!(s, sync::CardSync::default());

    // The board retitles the card and moves it to done.
    core.update_card(
        &id,
        card.version,
        kls_core::board::CardPatch {
            title: Some("#3 Fix the login".into()),
            status: Some("done".into()),
            ..Default::default()
        },
        "owner",
    )
    .await
    .unwrap();
    let patch = "/repos/acme/api/issues/3";
    gh.on_seq(
        "PATCH",
        patch,
        vec![
            reply(502, json!({"message": "Bad gateway"})),
            reply(200, json!({"number": 3})),
        ],
    );
    let mut rx = core.subscribe();
    let s = sync::sync_cards(&c, "acme/api", "poller").await.unwrap();
    assert_eq!((s.failed, s.pushed), (1, 0), "{s:?}");
    let evs = events(&core, &mut rx);
    assert!(
        evs.contains(&kinds::ISSUE_PUSH_FAILED.to_string()),
        "{evs:?}"
    );
    let card = core.card(&id).await.unwrap().unwrap();
    let r = kls_core::board::sync::retry_of(&card).unwrap();
    assert_eq!(r.attempt, 1);
    assert!(r.retry_at > kls_store::now_ms(), "backoff");
    // The failure is audited: the write itself and the scheduled retry.
    let audit = forge_audit(&core).await;
    assert!(
        audit
            .iter()
            .any(|(a, who, o)| a == "issue.push" && who == "poller" && o == "retry"),
        "{audit:?}"
    );
    assert!(
        audit
            .iter()
            .any(|(a, _, o)| a == "PATCH /repos/acme/api/issues/3" && o.starts_with("error"))
    );

    // Before the retry time: waits, no call.
    let s = sync::sync_cards(&c, "acme/api", "poller").await.unwrap();
    assert_eq!(s.waiting, 1);
    assert_eq!(gh.count("PATCH", patch), 1);

    // The retry time passes: pushed, the issue closes.
    let back = kls_core::board::sync::Retry {
        retry_at: 0,
        ..r.clone()
    };
    core.update_card(
        &id,
        card.version,
        kls_core::board::CardPatch {
            meta_json: Some(kls_core::board::sync::with_retry(
                &card.meta_json,
                Some(&back),
            )),
            ..Default::default()
        },
        "owner",
    )
    .await
    .unwrap();
    let s = sync::sync_cards(&c, "acme/api", "poller").await.unwrap();
    assert_eq!(s.pushed, 1, "{s:?}");
    assert_eq!(gh.count("PATCH", patch), 2);
    let last = gh
        .calls()
        .into_iter()
        .rfind(|c| c.method == "PATCH")
        .unwrap();
    let body: Value = serde_json::from_str(last.stdin.as_deref().unwrap()).unwrap();
    assert_eq!(body["title"], "Fix the login");
    assert_eq!(body["state"], "closed");
    assert!(body.get("labels").is_none(), "only changed fields");
    let evs = events(&core, &mut rx);
    assert!(evs.contains(&kinds::ISSUE_PUSHED.to_string()));
    let stored = core.store().gh_issue("acme/api", 3).await.unwrap().unwrap();
    assert_eq!(
        (stored.state.as_str(), stored.title.as_str()),
        ("closed", "Fix the login")
    );
    let card = core.card(&id).await.unwrap().unwrap();
    assert!(kls_core::board::sync::retry_of(&card).is_none());
    // In sync now: the next pass writes nothing.
    let s = sync::sync_cards(&c, "acme/api", "poller").await.unwrap();
    assert_eq!(s, sync::CardSync::default());
    assert_eq!(gh.count("PATCH", patch), 2);
    assert!(
        forge_audit(&core)
            .await
            .iter()
            .any(|(a, who, o)| a == "PATCH /repos/acme/api/issues/3"
                && who == "poller"
                && o == "ok")
    );
}

#[tokio::test]
async fn outbound_sync_is_refused_under_the_kill_switch() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let (c, id) = mirrored_card(&core, &gh).await;
    let card = core.card(&id).await.unwrap().unwrap();
    core.update_card(
        &id,
        card.version,
        kls_core::board::CardPatch {
            status: Some("done".into()),
            ..Default::default()
        },
        "owner",
    )
    .await
    .unwrap();
    gh.on("PATCH", "/repos/acme/api/issues/3", 200, json!({}));
    core.set_factory_stopped(true, "drill").await.unwrap();
    let mut rx = core.subscribe();
    let s = sync::sync_cards(&c, "acme/api", "poller").await.unwrap();
    assert_eq!((s.refused, s.pushed, s.failed), (1, 0, 0), "{s:?}");
    assert_eq!(gh.count("PATCH", "/repos/acme/api/issues/3"), 0);
    assert!(events(&core, &mut rx).contains(&kinds::REFUSED.to_string()));
    assert!(
        forge_audit(&core)
            .await
            .iter()
            .any(|(a, who, o)| a == "issue.push" && who == "poller" && o == "refused")
    );
    // No attempt spent: once resumed, it pushes.
    let card = core.card(&id).await.unwrap().unwrap();
    assert!(kls_core::board::sync::retry_of(&card).is_none());
    core.set_factory_stopped(false, "done").await.unwrap();
    let s = sync::sync_cards(&c, "acme/api", "poller").await.unwrap();
    assert_eq!(s.pushed, 1);
}

#[tokio::test]
async fn both_sides_changed_is_a_conflict_not_an_overwrite() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let (c, id) = mirrored_card(&core, &gh).await;
    let card = core.card(&id).await.unwrap().unwrap();
    core.update_card(
        &id,
        card.version,
        kls_core::board::CardPatch {
            title: Some("#3 Fix the login".into()),
            ..Default::default()
        },
        "owner",
    )
    .await
    .unwrap();
    // GitHub retitles it as well.
    gh.on(
        "GET",
        &format!("{ISSUES}&since=2026-10-20T00:00:00Z"),
        200,
        json!([issue3("open", "Login fails", "2026-10-21T00:00:00Z")]),
    );
    sync::sync_issues(&c, "acme/api", "poller").await.unwrap();
    gh.on("PATCH", "/repos/acme/api/issues/3", 200, json!({}));
    let s = sync::sync_cards(&c, "acme/api", "poller").await.unwrap();
    assert_eq!((s.conflicts, s.pushed), (1, 0), "{s:?}");
    assert_eq!(gh.count("PATCH", "/repos/acme/api/issues/3"), 0);
    let card = core.card(&id).await.unwrap().unwrap();
    assert_eq!(card.title, "#3 Fix the login", "the board's edit stays");
    let conf = core.sync_conflicts().await.unwrap();
    assert_eq!(conf.len(), 1);
    assert_eq!(conf[0].fields[0].field, "title");
    assert_eq!(conf[0].fields[0].github, "#3 Login fails");
    // Keep board: the next pass pushes the board's title.
    core.resolve_sync_conflict(&id, "title", false)
        .await
        .unwrap();
    let s = sync::sync_cards(&c, "acme/api", "poller").await.unwrap();
    assert_eq!(s.pushed, 1, "{s:?}");
    let body: Value = serde_json::from_str(
        gh.calls()
            .into_iter()
            .find(|c| c.method == "PATCH")
            .unwrap()
            .stdin
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["title"], "Fix the login");
    assert!(core.sync_conflicts().await.unwrap().is_empty());
}

#[tokio::test]
async fn project_mirror_lands_issues_in_its_list() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    // Per-repo switch off; the project row mirrors the repo.
    managed_repo(
        &core,
        "acme/api",
        Mirroring {
            pulls: false,
            issues: false,
            labels: false,
        },
    )
    .await;
    core.save_issue_mirrors(
        &[kls_core::board::sync::MirrorRow {
            project: "web".into(),
            repo: "acme/api".into(),
            inbound: "review".into(),
            outbound: true,
            rules: Default::default(),
            escalate_hours: 0,
        }],
        "owner",
    )
    .await
    .unwrap();
    gh.on(
        "GET",
        ISSUES,
        200,
        json!([issue3("open", "Fix login", "2026-10-20T00:00:00Z")]),
    );
    let svc = Service::new(core.clone(), local(&gh, Some(true)));
    let rep = svc.poll_once().await.unwrap();
    assert!(rep.errors.is_empty(), "{rep:?}");
    let cards = core.cards().await.unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].status, "review");
    assert!(cards[0].labels.contains(&"project:web".to_string()));
    // The project label never reaches the issue.
    assert_eq!(gh.count("PATCH", "/repos/acme/api/issues/3"), 0);
    assert!(
        core.save_issue_mirrors(
            &[kls_core::board::sync::MirrorRow {
                project: "".into(),
                repo: "acme/api".into(),
                inbound: "review".into(),
                outbound: true,
                rules: Default::default(),
                escalate_hours: 0,
            }],
            "owner",
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn polled_review_submitted_is_emitted_once() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let c = client(&core, &gh);
    register_pull(&gh, 4, pull_json(4, "h1", "2026-10-20T00:00:00Z"));
    // First sight: the old review only sets the cursor.
    let mut rx = core.subscribe();
    sync::refresh_pull(&c, "acme/api", 4, "poller")
        .await
        .unwrap();
    assert!(!events(&core, &mut rx).contains(&kinds::REVIEW_SUBMITTED.to_string()));
    gh.on(
        "GET",
        "/repos/acme/api/pulls/4/reviews?per_page=100",
        200,
        json!([
            {"id": 91, "user": {"login": "bo"}, "state": "APPROVED", "body": "lgtm"},
            {"id": 92, "user": {"login": "cy"}, "state": "CHANGES_REQUESTED", "body": "nope",
             "author_association": "MEMBER"}
        ]),
    );
    sync::refresh_pull(&c, "acme/api", 4, "poller")
        .await
        .unwrap();
    let evs: Vec<kls_store::Event> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|e| e.kind == kinds::REVIEW_SUBMITTED)
        .collect();
    assert_eq!(evs.len(), 1);
    let d: Value = serde_json::from_str(&evs[0].data_json).unwrap();
    assert_eq!(
        (
            d["state"].as_str(),
            d["author"].as_str(),
            d["number"].as_i64()
        ),
        (Some("changes_requested"), Some("cy"), Some(4))
    );
    sync::refresh_pull(&c, "acme/api", 4, "poller")
        .await
        .unwrap();
    assert!(!events(&core, &mut rx).contains(&kinds::REVIEW_SUBMITTED.to_string()));
}

#[tokio::test]
async fn packages_posture_and_audit_log_emit_events() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    managed_repo(
        &core,
        "acme/api",
        Mirroring {
            pulls: false,
            issues: false,
            labels: false,
        },
    )
    .await;
    let r = core.store().gh_repo("acme/api").await.unwrap().unwrap();
    let c = client(&core, &gh);
    let pkgs = "/orgs/acme/packages?package_type=container&per_page=100";
    let pkg = |updated: &str| {
        json!([{"name": "api", "package_type": "container", "updated_at": updated,
                "html_url": "https://github.com/orgs/acme/packages/container/package/api",
                "repository": {"full_name": "acme/api"}},
               {"name": "other", "package_type": "container", "updated_at": updated,
                "repository": {"full_name": "acme/web"}}])
    };
    gh.on("GET", pkgs, 200, pkg("2026-11-01T00:00:00Z"));
    gh.on(
        "GET",
        "/repos/acme/api",
        200,
        json!({"full_name": "acme/api", "visibility": "private",
               "security_and_analysis": {"secret_scanning": {"status": "enabled"}}}),
    );
    gh.on(
        "GET",
        "/repos/acme/api/branches/main",
        200,
        json!({"name": "main", "protected": true}),
    );
    let mut rx = core.subscribe();
    let rep = crate::events::poll_repo(&c, &r, "poller").await.unwrap();
    assert_eq!((rep.packages, rep.risks), (0, 0), "first read sets cursors");

    gh.on("GET", pkgs, 200, pkg("2026-11-02T00:00:00Z"));
    gh.on(
        "GET",
        "/orgs/acme/packages/container/api/versions?per_page=1",
        200,
        json!([{"name": "sha256:abc", "metadata": {"container": {"tags": ["v2"]}}}]),
    );
    gh.on(
        "GET",
        "/repos/acme/api",
        200,
        json!({"full_name": "acme/api", "visibility": "public",
               "security_and_analysis": {"secret_scanning": {"status": "disabled"}}}),
    );
    gh.on(
        "GET",
        "/repos/acme/api/branches/main",
        200,
        json!({"name": "main", "protected": false}),
    );
    let rep = crate::events::poll_repo(&c, &r, "poller").await.unwrap();
    assert_eq!(rep.packages, 1, "{rep:?}");
    assert_eq!(rep.risks, 3, "{rep:?}");
    let evs: Vec<kls_store::Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let pkg_ev = evs
        .iter()
        .find(|e| e.kind == kinds::PACKAGE_PUBLISHED)
        .unwrap();
    let d: Value = serde_json::from_str(&pkg_ev.data_json).unwrap();
    assert_eq!(
        (d["package"].as_str(), d["version"].as_str()),
        (Some("api"), Some("v2"))
    );
    let rules: Vec<String> = evs
        .iter()
        .filter(|e| e.kind == kinds::RISK)
        .map(|e| {
            let d: Value = serde_json::from_str(&e.data_json).unwrap();
            assert_eq!(d["source"], "github-posture");
            d["rule"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(
        rules,
        [
            "repo-made-public",
            "secret-scanning-off",
            "branch-protection-removed"
        ]
    );

    // github-risk: the org audit log, first read sets the cursor.
    let log = "/orgs/acme/audit-log?per_page=100&order=desc";
    gh.on(
        "GET",
        log,
        200,
        json!([{"@timestamp": 1000, "action": "org.add_member", "actor": "ana"}]),
    );
    assert_eq!(
        crate::events::poll_org_risk(&c, "acme", "poller")
            .await
            .unwrap(),
        0
    );
    gh.on(
        "GET",
        log,
        200,
        json!([
            {"@timestamp": 3000, "action": "org.disable_two_factor_requirement", "actor": "eve"},
            {"@timestamp": 2000, "action": "repo.create", "actor": "ana"},
            {"@timestamp": 1000, "action": "org.add_member", "actor": "ana"}
        ]),
    );
    // Read at most hourly: move the check back.
    core.store()
        .gh_put_etag("cursor|github.com|risk:checked|acme", "cursor", "", "0")
        .await
        .unwrap();
    let n = crate::events::poll_org_risk(&c, "acme", "poller")
        .await
        .unwrap();
    assert_eq!(n, 1);
    let ev = std::iter::from_fn(|| rx.try_recv().ok())
        .find(|e| e.kind == kinds::RISK)
        .unwrap();
    let d: Value = serde_json::from_str(&ev.data_json).unwrap();
    assert_eq!(
        (
            d["source"].as_str(),
            d["rule"].as_str(),
            d["severity"].as_str()
        ),
        (
            Some("github-risk"),
            Some("two-factor-off"),
            Some("critical")
        )
    );
    // Off switch.
    core.store()
        .set_setting(kls_core::github::settings::RISK_POLL, "off")
        .await
        .unwrap();
    core.store()
        .gh_put_etag("cursor|github.com|risk:checked|acme", "cursor", "", "0")
        .await
        .unwrap();
    assert_eq!(
        crate::events::poll_org_risk(&c, "acme", "poller")
            .await
            .unwrap(),
        0
    );
}

// ---- unlink, sync base, open issue (WP4 requests) ----------------------------

#[tokio::test]
async fn unlinked_issue_is_mirrored_with_no_card() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let (c, id) = mirrored_card(&core, &gh).await;
    core.unlink_card_issue(&id, "owner").await.unwrap();
    // The issue changes: no new card, the old card does not follow.
    gh.on(
        "GET",
        &format!("{ISSUES}&since=2026-10-20T00:00:00Z"),
        200,
        json!([issue3("open", "Login fails", "2026-10-21T00:00:00Z")]),
    );
    let mut rx = core.subscribe();
    sync::sync_issues(&c, "acme/api", "poller").await.unwrap();
    let cards = core.cards().await.unwrap();
    assert_eq!(cards.len(), 1, "no new triage card");
    assert_eq!(cards[0].title, "#3 Fix login", "the unlinked card stays");
    let stored = core.store().gh_issue("acme/api", 3).await.unwrap().unwrap();
    assert!(sync::unlinked(&stored));
    assert_eq!(stored.title, "Login fails", "still mirrored");
    let ev = std::iter::from_fn(|| rx.try_recv().ok())
        .find(|e| e.kind == kinds::ISSUE_UPDATED)
        .unwrap();
    let d: Value = serde_json::from_str(&ev.data_json).unwrap();
    assert!(d["cardId"].is_null(), "{d}");
    // The outbound pass skips it as well.
    let s = sync::sync_cards(&c, "acme/api", "poller").await.unwrap();
    assert_eq!(s, sync::CardSync::default());
    assert!(sync::issue_card(&core, &stored).await.unwrap().is_none());
}

#[tokio::test]
async fn sync_base_is_written_on_create_and_follow() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let (c, id) = mirrored_card(&core, &gh).await;
    let card = core.card(&id).await.unwrap().unwrap();
    let base = kls_core::board::sync::base_of(&card);
    assert_eq!(base.get("title").map(String::as_str), Some("#3 Fix login"));
    assert_eq!(base.get("state").map(String::as_str), Some("open"));
    // GitHub retitles and closes it: the card follows, the base moves.
    let mut closed = issue3("closed", "Login fixed", "2026-10-21T00:00:00Z");
    closed["state_reason"] = json!("completed");
    gh.on(
        "GET",
        &format!("{ISSUES}&since=2026-10-20T00:00:00Z"),
        200,
        json!([closed]),
    );
    sync::sync_issues(&c, "acme/api", "poller").await.unwrap();
    let card = core.card(&id).await.unwrap().unwrap();
    assert_eq!(
        (card.title.as_str(), card.status.as_str()),
        ("#3 Login fixed", "done")
    );
    let base = kls_core::board::sync::base_of(&card);
    assert_eq!(
        base.get("title").map(String::as_str),
        Some("#3 Login fixed")
    );
    assert_eq!(base.get("state").map(String::as_str), Some("closed"));
}

async fn board_card(core: &Core) -> kls_store::board::Card {
    core.create_card(
        kls_core::board::CardDraft {
            title: "Add export".into(),
            status: "todo".into(),
            repo: "acme/api".into(),
            ..Default::default()
        },
        "owner",
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn open_issue_is_audited_links_and_sets_the_base() {
    let (core, _d) = core().await;
    managed_repo(
        &core,
        "acme/api",
        Mirroring {
            pulls: false,
            issues: false,
            labels: false,
        },
    )
    .await;
    let gh = FakeGh::new();
    gh.on(
        "POST",
        "/repos/acme/api/issues",
        201,
        json!({"number": 9, "html_url": "https://github.com/acme/api/issues/9"}),
    );
    let c = client(&core, &gh);
    let card = board_card(&core).await;
    let (n, url) = sync::open_issue(&c, &card, "Body", "user").await.unwrap();
    assert_eq!(
        (n, url.as_str()),
        (9, "https://github.com/acme/api/issues/9")
    );
    let stored = core.store().gh_issue("acme/api", 9).await.unwrap().unwrap();
    assert_eq!(stored.card_id.as_deref(), Some(card.id.as_str()));
    let after = core.card(&card.id).await.unwrap().unwrap();
    assert_eq!(after.title, "#9 Add export");
    let base = kls_core::board::sync::base_of(&after);
    assert_eq!(base.get("title").map(String::as_str), Some("#9 Add export"));
    assert_eq!(base.get("state").map(String::as_str), Some("open"));
    let audit = forge_audit(&core).await;
    assert!(
        audit
            .iter()
            .any(|(a, who, o)| a == "issue.open" && who == "owner" && o == "ok"),
        "{audit:?}"
    );
    let card_audit = core
        .audit_entries(&kls_store::audit::LedgerQuery::default())
        .await
        .unwrap();
    assert!(card_audit.iter().any(|e| {
        serde_json::to_string(e)
            .unwrap()
            .contains("card.github.link")
    }));
}

#[tokio::test]
async fn open_issue_by_automation_is_refused_under_the_kill_switch() {
    let (core, _d) = core().await;
    managed_repo(
        &core,
        "acme/api",
        Mirroring {
            pulls: false,
            issues: false,
            labels: false,
        },
    )
    .await;
    let gh = FakeGh::new();
    gh.on("POST", "/repos/acme/api/issues", 201, json!({"number": 9}));
    let c = client(&core, &gh);
    let card = board_card(&core).await;
    core.set_factory_stopped(true, "drill").await.unwrap();
    let e = sync::open_issue(&c, &card, "Body", "factory:wf")
        .await
        .unwrap_err();
    assert!(matches!(
        e.downcast_ref::<GhError>(),
        Some(GhError::Stopped(_))
    ));
    assert_eq!(gh.count("POST", "/repos/acme/api/issues"), 0);
    assert!(
        forge_audit(&core)
            .await
            .iter()
            .any(|(a, _, o)| a == "issue.open" && o == "refused")
    );
    assert!(
        core.store()
            .gh_issue("acme/api", 9)
            .await
            .unwrap()
            .is_none()
    );
}

// ---- own-identity facts on forge events (WP2 requests) -----------------------

fn data_of(rx: &mut tokio::sync::broadcast::Receiver<kls_store::Event>, kind: &str) -> Vec<Value> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|e| e.kind == kind)
        .map(|e| serde_json::from_str(&e.data_json).unwrap())
        .collect()
}

#[tokio::test]
async fn pull_events_say_from_factory_and_carry_the_head_sha() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let list = "/repos/acme/api/pulls?state=open&per_page=100";
    let (p8, p9) = (
        pull_json(8, "h8", "2026-10-20T01:00:00Z"),
        pull_json(9, "h9", "2026-10-20T01:00:00Z"),
    );
    gh.on("GET", list, 200, json!([p8, p9]));
    register_pull(&gh, 8, p8.clone());
    register_pull(&gh, 9, p9.clone());
    let c = client(&core, &gh);
    // The factory opened #8; a person's click opens nothing on its behalf.
    sync::note_authored(&c, "acme/api", sync::Authored::Pull, 8, "factory:wf").await;
    sync::note_authored(&c, "acme/api", sync::Authored::Pull, 9, "user").await;
    let mut rx = core.subscribe();
    sync::sync_pulls(&c, "acme/api", "poller").await.unwrap();
    let opened = data_of(&mut rx, kinds::PULL_OPENED);
    let by = |n: i64| {
        opened
            .iter()
            .find(|d| d["number"] == json!(n))
            .unwrap()
            .clone()
    };
    assert_eq!(by(8)["fromFactory"], json!(true));
    assert_eq!(by(8)["headSha"], json!("h8"));
    assert_eq!(by(9)["fromFactory"], json!(false));
    assert_eq!(by(9)["headSha"], json!("h9"));
}

#[tokio::test]
async fn issue_opened_carries_author_association_and_factory_facts() {
    let (core, _d) = core().await;
    managed_repo(
        &core,
        "acme/api",
        Mirroring {
            pulls: false,
            issues: true,
            labels: false,
        },
    )
    .await;
    let gh = FakeGh::new();
    let mut mine = issue3("open", "Scan findings", "2026-10-20T00:00:00Z");
    mine["number"] = json!(4);
    mine["labels"] = json!([]);
    mine["body"] = json!("Found 2\n<!-- keleustes:findings -->");
    let mut theirs = issue3("open", "Bug", "2026-10-20T00:00:00Z");
    theirs["labels"] = json!([]);
    gh.on("GET", ISSUES, 200, json!([theirs, mine]));
    let c = client(&core, &gh);
    let mut rx = core.subscribe();
    sync::sync_issues(&c, "acme/api", "poller").await.unwrap();
    let opened = data_of(&mut rx, kinds::ISSUE_OPENED);
    let by = |n: i64| {
        opened
            .iter()
            .find(|d| d["number"] == json!(n))
            .unwrap()
            .clone()
    };
    assert_eq!(by(3)["authorAssociation"], json!("NONE"));
    assert_eq!(by(3)["fromFactory"], json!(false));
    assert_eq!(by(4)["fromFactory"], json!(true));
}

#[tokio::test]
async fn factory_authored_reads_marks_markers_and_identities() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let c = client(&core, &gh);
    let comment = sync::Authored::Comment;
    assert!(!sync::factory_authored(&c, "acme/api", comment, 77, "ana", "hi").await);
    sync::note_authored(&c, "acme/api", comment, 77, "factory:wf").await;
    assert!(sync::factory_authored(&c, "acme/api", comment, 77, "ana", "hi").await);
    // Another kind with the same id is not marked.
    assert!(!sync::factory_authored(&c, "acme/api", sync::Authored::Issue, 77, "ana", "").await);
    assert!(
        sync::factory_authored(&c, "acme/api", comment, 1, "", "x <!-- keleustes:run -->").await
    );
    assert!(sync::factory_authored(&c, "acme/api", comment, 1, "Belai[bot]", "").await);
    assert!(!sync::factory_authored(&c, "acme/api", comment, 1, "sf-bot", "").await);
    core.store()
        .set_setting("forge.identity", "\"sf-bot, ops\"")
        .await
        .unwrap();
    assert!(sync::factory_authored(&c, "acme/api", comment, 1, "sf-bot", "").await);
}

// ---- deployment reviews (WP4 request 2) ---------------------------------------

#[tokio::test]
async fn waiting_runs_emit_deployment_reviews_once_then_their_outcome() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    managed_repo(
        &core,
        "acme/api",
        Mirroring {
            pulls: false,
            issues: false,
            labels: false,
        },
    )
    .await;
    let r = core.store().gh_repo("acme/api").await.unwrap().unwrap();
    let c = client(&core, &gh);
    let waiting = "/repos/acme/api/actions/runs?status=waiting&per_page=20";
    gh.on(
        "GET",
        waiting,
        200,
        json!({"workflow_runs": [{"id": 77, "name": "deploy", "status": "waiting",
               "html_url": "https://github.com/acme/api/actions/runs/77",
               "triggering_actor": {"login": "ana"}}]}),
    );
    gh.on(
        "GET",
        "/repos/acme/api/actions/runs/77/pending_deployments",
        200,
        json!([{"environment": {"name": "production"}}, {"environment": {"name": "staging"}}]),
    );
    let mut rx = core.subscribe();
    let rep = crate::events::poll_repo(&c, &r, "poller").await.unwrap();
    assert_eq!(rep.deploy_reviews, 2);
    let req = data_of(&mut rx, kinds::DEPLOY_REVIEW_REQUESTED);
    assert_eq!(req.len(), 2);
    let prod = req
        .iter()
        .find(|d| d["environment"] == "production")
        .unwrap();
    assert_eq!(
        (
            prod["repo"].as_str(),
            prod["runId"].as_str(),
            prod["requester"].as_str()
        ),
        (Some("acme/api"), Some("77"), Some("ana"))
    );
    assert_eq!(prod["url"], "https://github.com/acme/api/actions/runs/77");
    // Checkpoints shows a deploy gate per environment.
    let gates: Vec<_> = core
        .needs_you()
        .await
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == kls_core::needs::CheckpointKind::DeployGate)
        .collect();
    assert_eq!(gates.len(), 2, "{gates:?}");

    // Still pending: nothing new.
    let rep = crate::events::poll_repo(&c, &r, "poller").await.unwrap();
    assert_eq!(rep.deploy_reviews, 0);

    // Production approved, staging rejected: two outcome events.
    gh.on("GET", waiting, 200, json!({"workflow_runs": []}));
    gh.on(
        "GET",
        "/repos/acme/api/actions/runs/77/approvals",
        200,
        json!([{"state": "approved", "user": {"login": "bo"}, "comment": "ship",
                "environments": [{"name": "production"}]},
               {"state": "rejected", "user": {"login": "cy"}, "comment": "",
                "environments": [{"name": "staging"}]}]),
    );
    gh.on(
        "GET",
        "/repos/acme/api/actions/runs/77",
        200,
        json!({"id": 77, "status": "in_progress"}),
    );
    let rep = crate::events::poll_repo(&c, &r, "poller").await.unwrap();
    assert_eq!(rep.deploy_reviews, 2);
    let ok = data_of(&mut rx, kinds::DEPLOY_REVIEW_APPROVED);
    assert_eq!(ok.len(), 1);
    assert_eq!(
        (ok[0]["environment"].as_str(), ok[0]["reviewer"].as_str()),
        (Some("production"), Some("bo"))
    );
    assert!(
        core.needs_you()
            .await
            .unwrap()
            .iter()
            .all(|c| c.kind != kls_core::needs::CheckpointKind::DeployGate),
        "decided reviews leave Checkpoints"
    );
    // Settled: the next pass reads nothing more.
    let rep = crate::events::poll_repo(&c, &r, "poller").await.unwrap();
    assert_eq!(rep.deploy_reviews, 0);
}
