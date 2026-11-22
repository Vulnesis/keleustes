//! Every forge over the fake CLI (no network): GitLab through `glab api`,
//! Gitea/Forgejo through `tea api`, per-connection routes, the kill switch,
//! audit and provenance, the polled webhook events, and the factory's
//! no-merge rule.

use crate::fake::{FakeExec, FakeGh};
use crate::forge::{CliKind, Connection, ForgeKind};
use crate::parse;
use crate::sync::{self, PullAction};
use crate::*;
use kls_core::Core;
use kls_core::github::kinds;
use kls_store::audit::LedgerQuery;
use kls_store::github::{GhOrg, GhRepo, Mirroring, RepoForge};
use serde_json::{Value, json};
use std::sync::Arc;

async fn core() -> (Core, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    (core, dir)
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<kls_store::Event>) -> Vec<kls_store::Event> {
    let mut out = Vec::new();
    while let Ok(e) = rx.try_recv() {
        out.push(e);
    }
    out
}

async fn audit_actions(core: &Core) -> Vec<(String, String, String)> {
    core.audit_entries(&LedgerQuery {
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

async fn add(core: &Core, kind: ForgeKind, host: &str) -> Connection {
    let c = Connection::new(kind, host, "user").unwrap();
    core.store()
        .upsert_forge_connection(&c.to_row("{}"))
        .await
        .unwrap();
    c
}

/// A recorded forge reply under `testdata/<dialect>/`.
fn fixture(name: &str) -> Value {
    let text = match name {
        "gitlab/merge_request" => include_str!("../../testdata/gitlab/merge_request.json"),
        "gitlab/approvals" => include_str!("../../testdata/gitlab/approvals.json"),
        "gitlab/discussion" => include_str!("../../testdata/gitlab/discussion.json"),
        "gitea/repo" => include_str!("../../testdata/gitea/repo.json"),
        "gitea/labels" => include_str!("../../testdata/gitea/labels.json"),
        _ => panic!("no fixture {name}"),
    };
    serde_json::from_str(text).unwrap()
}

/// The recorded merge request with its iid, head and URL replaced.
fn mr(iid: i64, sha: &str) -> Value {
    let mut v = fixture("gitlab/merge_request");
    v["iid"] = json!(iid);
    v["title"] = json!(format!("MR {iid}"));
    v["sha"] = json!(sha);
    v["diff_refs"]["head_sha"] = json!(sha);
    v["web_url"] = json!(format!(
        "https://gitlab.acme.dev/grp/app/-/merge_requests/{iid}"
    ));
    v
}

#[tokio::test]
async fn gitlab_discovery_pulls_issues_labels_and_draft() {
    let (core, _d) = core().await;
    let conn = add(&core, ForgeKind::Gitlab, "gitlab.acme.dev").await;
    let gl = FakeGh::with_cli(Route::Local, CliKind::Glab);
    gl.on("GET", "user", 200, json!({"username": "ana", "id": 9}));
    gl.on(
        "GET",
        "groups?min_access_level=30&per_page=100",
        200,
        json!([{"full_path": "grp/sub", "id": 4}]),
    );
    gl.on(
        "GET",
        "groups/grp%2Fsub/projects?include_subgroups=true&per_page=100",
        200,
        json!([{"id": 77, "path_with_namespace": "grp/sub/app", "visibility": "private",
                "default_branch": "main", "web_url": "u",
                "permissions": {"project_access": {"access_level": 40}}}]),
    );
    let c = Client::with_conn(gl.clone(), core.clone(), conn.clone());
    let r = sync::refresh_orgs(&c, "user", false).await.unwrap();
    assert_eq!(r.orgs, 2);
    let store = core.store();
    let org = store
        .gh_org("gitlab.acme.dev/grp/sub")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.gh_org_forge(&org.login).await.unwrap(),
        Some((conn.id.clone(), "grp/sub".into()))
    );
    let rr = sync::refresh_org_repos(&c, &org, "user").await.unwrap();
    let key = "gitlab.acme.dev/grp/sub/app";
    assert_eq!(rr.added, vec![key.to_string()]);
    let f = store.gh_repo_forge(key).await.unwrap().unwrap();
    assert_eq!(
        f,
        RepoForge {
            connection: conn.id.clone(),
            forge_full_name: "grp/sub/app".into(),
            forge_id: Some(77)
        }
    );
    // GitLab rate headers are observed under the connection's route key.
    let q = store
        .gh_quota(c.route_key(), "core")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(q.limit, 5000);
    assert!(c.route_key().starts_with(&conn.id));

    // Merge requests become pulls (iid as number).
    let p = "projects/grp%2Fsub%2Fapp";
    gl.on(
        "GET",
        &format!("{p}/merge_requests?state=opened&per_page=100"),
        200,
        json!([mr(3, "h3")]),
    );
    gl.on("GET", &format!("{p}/merge_requests/3"), 200, mr(3, "h3"));
    gl.on(
        "GET",
        &format!("{p}/repository/commits/h3/statuses?per_page=100"),
        200,
        json!([{"name": "ci", "status": "failed"}]),
    );
    gl.on(
        "GET",
        &format!("{p}/merge_requests/3/approvals"),
        200,
        fixture("gitlab/approvals"),
    );
    let mut rx = core.subscribe();
    assert_eq!(sync::sync_pulls(&c, key, "poller").await.unwrap(), 1);
    let pull = store.gh_pull(key, 3).await.unwrap().unwrap();
    assert_eq!(
        (pull.mergeable.as_str(), pull.checks_state.as_str()),
        ("clean", "failure")
    );
    assert_eq!(pull.review_decision, "APPROVED");
    assert!(!pull.is_fork(), "{}", pull.head_repo);
    let evs = drain(&mut rx);
    let opened = evs.iter().find(|e| e.kind == kinds::PULL_OPENED).unwrap();
    assert!(opened.data_json.contains(&conn.id));

    // Issues since the cursor; labels with 409 for an existing one.
    gl.on(
        "GET",
        &format!("{p}/issues?scope=all&order_by=updated_at&sort=asc&per_page=100"),
        200,
        json!([{"iid": 5, "title": "Bug", "state": "opened", "author": {"username": "cy"},
                "labels": ["sf:fix"], "web_url": "w", "created_at": "2026-10-20T00:00:00Z",
                "updated_at": "2026-10-20T00:00:00Z"}]),
    );
    assert_eq!(sync::sync_issues(&c, key, "poller").await.unwrap(), 1);
    assert_eq!(
        store.gh_issue(key, 5).await.unwrap().unwrap().labels,
        vec!["sf:fix".to_string()]
    );
    gl.on(
        "GET",
        &format!("{p}/labels?per_page=100"),
        200,
        json!([{"name": "sf:fix", "color": "#00e0c9"}]),
    );
    gl.on(
        "POST",
        &format!("{p}/labels"),
        409,
        json!({"message": "Label already exists"}),
    );
    let ls = sync::sync_labels(&c, key, true, "user").await.unwrap();
    assert_eq!((ls.created.len(), ls.existing), (0, 34));
    assert!(ls.error.is_empty(), "{}", ls.error);

    // Draft: the title gets the prefix through PUT.
    gl.on("PUT", &format!("{p}/merge_requests/3"), 200, mr(3, "h3"));
    sync::pull_action(&c, key, 3, &PullAction::Draft, "user")
        .await
        .unwrap();
    let put = gl.calls().into_iter().find(|c| c.method == "PUT").unwrap();
    assert!(put.stdin.unwrap().contains("Draft: MR 3"));
    assert!(
        put.args
            .windows(2)
            .any(|w| w == ["--hostname", "gitlab.acme.dev"])
    );
    // Team reviewers are refused on GitLab, with the capability named.
    let e = sync::pull_action(
        &c,
        key,
        3,
        &PullAction::RequestReviewers(vec!["grp/ops".into()]),
        "user",
    )
    .await
    .unwrap_err();
    assert!(format!("{e:#}").contains("team_reviewers"), "{e:#}");
}

#[tokio::test]
async fn tea_reads_head_from_stderr_counts_calls_and_labels_by_id() {
    let (core, _d) = core().await;
    let conn = add(&core, ForgeKind::Forgejo, "codeberg.org").await;
    let tea = FakeGh::with_cli(Route::Local, CliKind::Tea);
    let key = "codeberg.org/ana/app";
    core.store()
        .gh_upsert_repo(&GhRepo {
            full_name: key.into(),
            owner: "codeberg.org/ana".into(),
            name: "app".into(),
            default_branch: "main".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    tea.on("GET", "/repos/ana/app", 200, fixture("gitea/repo"));
    let c = Client::with_conn(tea.clone(), core.clone(), conn.clone());
    let r = sync::refresh_repo(&c, key, "user").await.unwrap();
    assert!(r.can_push);
    let call = &tea.calls()[0];
    assert_eq!(
        &call.args[..3],
        &["api".to_string(), "-l".into(), "codeberg.org".into()]
    );
    // No rate headers: the budget is counted against forge.calls_per_hour.
    let q = c.quota().await.unwrap().unwrap();
    assert_eq!((q.limit, q.used), (5000, 1));
    // Labels go by id; a missing one is created first.
    tea.on(
        "GET",
        "/repos/ana/app/labels?limit=50",
        200,
        fixture("gitea/labels"),
    );
    tea.on(
        "POST",
        "/repos/ana/app/labels",
        201,
        json!({"id": 12, "name": "new"}),
    );
    tea.on("POST", "/repos/ana/app/issues/4/labels", 200, json!([]));
    c.api()
        .add_labels(&c, key, 4, &["sf:fix".into(), "new".into()], "user")
        .await
        .unwrap();
    let post = tea
        .calls()
        .into_iter()
        .find(|c| c.path == "/repos/ana/app/issues/4/labels")
        .unwrap();
    let body: Value = serde_json::from_str(post.stdin.as_deref().unwrap()).unwrap();
    assert_eq!(body["labels"], json!([11, 12]));
}

#[tokio::test]
async fn routes_per_connection_and_provenance() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    gh.set_tea_hosts(&["codeberg.org"]);
    let svc = Service::new(
        core.clone(),
        LocalGh::new(Arc::new(FakeExec {
            auth: Some(true),
            gh: gh.clone(),
        })),
    );
    let gl = svc
        .add_connection(ForgeKind::Gitlab, "https://GitLab.acme.dev/", "", "user")
        .await
        .unwrap();
    assert_eq!(gl.host, "gitlab.acme.dev");
    assert_eq!(gl.origin, "user");
    // Adding the same host again returns it.
    let again = svc
        .add_connection(ForgeKind::Gitlab, "gitlab.acme.dev", "", "cli")
        .await
        .unwrap();
    assert_eq!(again.id, gl.id);
    let cb = svc
        .add_connection(ForgeKind::Forgejo, "codeberg.org", "Codeberg", "cli")
        .await
        .unwrap();
    assert_eq!(cb.origin, "cli");
    assert_eq!(svc.connection(&cb.id).await.unwrap().label, "Codeberg");
    let views = svc.connection_views().await.unwrap();
    assert_eq!(views.len(), 3);
    assert_eq!(views[0].conn.id, "github.com");
    for v in &views {
        assert!(v.auth.signed_in(), "{:?}", v.auth);
        assert_eq!(v.route.route, Route::Local, "{}", v.conn.host);
    }
    assert_eq!(
        views[1].route.label(),
        format!("via local {}", views[1].conn.cli().name())
    );
    // A key resolves to its connection.
    assert_eq!(
        svc.connection_of("codeberg.org/ana/app").await.unwrap().id,
        cb.id
    );
    assert_eq!(
        svc.connection_of("acme/api").await.unwrap().id,
        "github.com"
    );
    // Disabled: no route. Removed: gone.
    svc.set_connection_enabled(&gl.id, false, "user")
        .await
        .unwrap();
    let c = svc.connection(&gl.id).await.unwrap();
    assert!(!svc.recheck_conn(&c).await.route.available());
    svc.remove_connection(&gl.id, "user").await.unwrap();
    assert!(svc.connection(&gl.id).await.is_err());
    assert!(svc.remove_connection("github.com", "user").await.is_err());
    let acts = audit_actions(&core).await;
    for a in ["connection_add", "connection_disable", "connection_remove"] {
        assert!(acts.iter().any(|x| x.0 == a), "{a}: {acts:?}");
    }
    assert!(
        acts.iter()
            .any(|x| x.0 == "connection_remove" && x.2.starts_with("error"))
    );
}

#[tokio::test]
async fn kill_switch_refuses_automation_and_audits() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    gh.on(
        "POST",
        "/repos/acme/api/issues/1/comments",
        201,
        json!({"id": 5}),
    );
    let c = Client::new(gh.clone(), core.clone());
    core.set_factory_stopped(true, "incident drill")
        .await
        .unwrap();
    let mut rx = core.subscribe();
    let e = c
        .request(
            "POST",
            "/repos/acme/api/issues/1/comments",
            Some(&json!({"body": "x"})),
            "factory:wf1",
        )
        .await
        .unwrap_err();
    assert!(
        matches!(e.downcast_ref::<GhError>(), Some(GhError::Stopped(_))),
        "{e:#}"
    );
    assert!(gh.calls().is_empty(), "nothing reached the forge");
    let evs = drain(&mut rx);
    assert!(
        evs.iter()
            .any(|e| e.kind == kinds::REFUSED && e.data_json.contains("factory_stopped"))
    );
    // A person's click still works, and is audited with the person as actor.
    c.request(
        "POST",
        "/repos/acme/api/issues/1/comments",
        Some(&json!({"body": "x"})),
        "user",
    )
    .await
    .unwrap();
    // The poller polls nothing.
    let svc = Service::new(
        core.clone(),
        LocalGh::new(Arc::new(FakeExec {
            auth: Some(true),
            gh: gh.clone(),
        })),
    );
    gh.clear_calls();
    let rep = svc.poll_once().await.unwrap();
    assert!(rep.refused.unwrap().contains("stopped"));
    assert!(gh.calls().is_empty());
    let acts = audit_actions(&core).await;
    assert!(
        acts.iter()
            .any(|a| a.1 == "workflow wf1" && a.2 == "refused"),
        "{acts:?}"
    );
    assert!(
        acts.iter().any(|a| a.1 == "owner" && a.2 == "ok"),
        "{acts:?}"
    );
    assert!(
        acts.iter()
            .any(|a| a.0 == "poll" && a.1 == "poller" && a.2 == "refused"),
        "{acts:?}"
    );
    let v = core.audit_verify().await.unwrap();
    assert!(v.factory().is_some_and(|c| c.ok), "{v:?}");
}

#[tokio::test]
async fn the_factory_never_merges_and_observe_repos_refuse() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    gh.on(
        "PUT",
        "/repos/acme/api/pulls/5/merge",
        200,
        json!({"merged": true}),
    );
    let c = Client::new(gh.clone(), core.clone());
    let e = sync::pull_action(
        &c,
        "acme/api",
        5,
        &PullAction::Merge("squash".into()),
        "factory:x",
    )
    .await
    .unwrap_err();
    assert!(format!("{e}").contains("never merges"));
    core.store()
        .gh_upsert_repo(&GhRepo {
            full_name: "acme/api".into(),
            owner: "acme".into(),
            name: "api".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let e = sync::pull_action(
        &c,
        "acme/api",
        5,
        &PullAction::Merge("squash".into()),
        "user",
    )
    .await
    .unwrap_err();
    assert!(format!("{e}").contains("observe"), "{e}");
    assert!(gh.calls().iter().all(|c| !c.path.ends_with("/merge")));
    let acts = audit_actions(&core).await;
    assert_eq!(
        acts.iter()
            .filter(|a| a.0 == "merge" && a.2 == "refused")
            .count(),
        2
    );
}

#[tokio::test]
async fn poller_synthesises_push_ci_release_and_alert_events_once() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let s = core.store();
    s.gh_upsert_org(&GhOrg {
        login: "acme".into(),
        kind: "Organization".into(),
        managed: true,
        enabled: true,
        ..Default::default()
    })
    .await
    .unwrap();
    s.gh_upsert_repo(&GhRepo {
        full_name: "acme/api".into(),
        owner: "acme".into(),
        name: "api".into(),
        default_branch: "main".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    s.gh_set_repo_managed("acme/api", true).await.unwrap();
    s.gh_set_repo_mirroring(
        "acme/api",
        Mirroring {
            pulls: false,
            issues: false,
            labels: false,
        },
    )
    .await
    .unwrap();
    let commits = "/repos/acme/api/commits?sha=main&per_page=20";
    let runs = "/repos/acme/api/actions/runs?status=completed&per_page=20";
    let rels = "/repos/acme/api/releases?per_page=10";
    let dep = "/repos/acme/api/dependabot/alerts?state=open&per_page=50";
    gh.on(
        "GET",
        commits,
        200,
        json!([{"sha": "c1", "commit": {"message": "one"}}]),
    );
    gh.on(
        "GET",
        runs,
        200,
        json!({"workflow_runs": [{"id": 10, "name": "ci", "conclusion": "success"}]}),
    );
    gh.on("GET", rels, 200, json!([{"id": 1, "tag_name": "v1"}]));
    gh.on("GET", dep, 200, json!([{"number": 1, "html_url": "a1"}]));
    let svc = Service::new(
        core.clone(),
        LocalGh::new(Arc::new(FakeExec {
            auth: Some(true),
            gh: gh.clone(),
        })),
    );
    let mut rx = core.subscribe();
    let rep = svc.poll_once().await.unwrap();
    assert!(rep.errors.is_empty(), "{rep:?}");
    let kinds_of = |evs: Vec<kls_store::Event>| -> Vec<String> {
        evs.into_iter()
            .map(|e| e.kind)
            .filter(|k| {
                [
                    kinds::PUSH,
                    kinds::WORKFLOW_RUN,
                    kinds::RELEASE,
                    kinds::ALERT,
                ]
                .contains(&k.as_str())
            })
            .collect()
    };
    assert!(
        kinds_of(drain(&mut rx)).is_empty(),
        "first pass only sets cursors"
    );
    // Nothing new (304s on GitHub): nothing emitted.
    svc.poll_once().await.unwrap();
    assert!(kinds_of(drain(&mut rx)).is_empty());
    // New commit, failed run, release and alert: one event each.
    gh.on("GET", commits, 200, json!([{"sha": "c2", "commit": {"message": "two"}}, {"sha": "c1", "commit": {"message": "one"}}]));
    gh.on(
        "GET",
        "/repos/acme/api/compare/c1...c2",
        200,
        json!({"files": [{"filename": "src/a.rs"}]}),
    );
    gh.on(
        "GET",
        runs,
        200,
        json!({"workflow_runs": [{"id": 11, "name": "ci", "conclusion": "failure"}, {"id": 10}]}),
    );
    gh.on(
        "GET",
        rels,
        200,
        json!([{"id": 2, "tag_name": "v2"}, {"id": 1}]),
    );
    gh.on("GET", dep, 200, json!([{"number": 2, "html_url": "a2", "security_advisory": {"severity": "high"}}, {"number": 1}]));
    let rep = svc.poll_once().await.unwrap();
    assert_eq!(rep.events, 4, "{rep:?}");
    let evs = drain(&mut rx);
    let push = evs.iter().find(|e| e.kind == kinds::PUSH).unwrap();
    let d: Value = serde_json::from_str(&push.data_json).unwrap();
    assert_eq!(
        (
            d["before"].as_str(),
            d["after"].as_str(),
            d["commits"].as_i64()
        ),
        (Some("c1"), Some("c2"), Some(1))
    );
    assert_eq!(d["changedFiles"], json!(["src/a.rs"]));
    let run = evs.iter().find(|e| e.kind == kinds::WORKFLOW_RUN).unwrap();
    assert!(run.data_json.contains("\"failed\":true"));
    let alert = evs.iter().find(|e| e.kind == kinds::ALERT).unwrap();
    assert!(alert.data_json.contains("dependabot") && alert.data_json.contains("high"));
    assert_eq!(kinds_of(evs).len(), 4);
    svc.poll_once().await.unwrap();
    assert!(kinds_of(drain(&mut rx)).is_empty(), "each event once");
}

#[tokio::test]
async fn conflict_detected_once_when_a_pull_turns_conflicted() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let pull = |m: Value, st: &str| {
        json!({
            "number": 9, "title": "T", "state": "open", "user": {"login": "ana"},
            "head": {"ref": "f", "sha": "h", "repo": {"full_name": "acme/api"}},
            "base": {"ref": "main", "sha": "b"}, "mergeable": m, "mergeable_state": st,
            "html_url": "u", "updated_at": "2026-10-20T00:00:00Z"
        })
    };
    gh.on(
        "GET",
        "/repos/acme/api/pulls/9",
        200,
        pull(json!(true), "clean"),
    );
    gh.on(
        "GET",
        "/repos/acme/api/pulls/9/reviews?per_page=100",
        200,
        json!([]),
    );
    gh.on(
        "GET",
        "/repos/acme/api/commits/h/check-runs?per_page=100",
        200,
        json!({"check_runs": []}),
    );
    let c = Client::new(gh.clone(), core.clone());
    let mut rx = core.subscribe();
    sync::refresh_pull(&c, "acme/api", 9, "poller")
        .await
        .unwrap();
    gh.on(
        "GET",
        "/repos/acme/api/pulls/9",
        200,
        pull(json!(false), "dirty"),
    );
    sync::refresh_pull(&c, "acme/api", 9, "poller")
        .await
        .unwrap();
    sync::refresh_pull(&c, "acme/api", 9, "poller")
        .await
        .unwrap();
    let n = drain(&mut rx)
        .iter()
        .filter(|e| e.kind == kinds::CONFLICT_DETECTED)
        .count();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn glab_and_tea_route_to_a_node_that_advertises_the_login() {
    use kls_store::{Capabilities, ForgeLogin, HostInfo};
    let (core, _d) = core().await;
    let conn = add(&core, ForgeKind::Gitlab, "gitlab.acme.dev").await;
    let host = |forges: Vec<ForgeLogin>| HostInfo {
        hostname: "h".into(),
        capabilities: Capabilities {
            forges,
            job_kinds: vec!["github".into(), "forge".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let login = |cli: &str, h: &str, ok: bool| ForgeLogin {
        cli: cli.into(),
        host: h.into(),
        kind: "gitlab".into(),
        login: "ana".into(),
        signed_in: ok,
    };
    let s = core.store();
    s.insert_node(
        "n1",
        "a-other",
        &host(vec![login("glab", "gitlab.com", true)]),
        "s1",
        "t",
    )
    .await
    .unwrap();
    s.insert_node(
        "n2",
        "b-stale",
        &host(vec![login("glab", "gitlab.acme.dev", false)]),
        "s2",
        "t",
    )
    .await
    .unwrap();
    s.insert_node(
        "n3",
        "c-good",
        &host(vec![login("glab", "gitlab.acme.dev", true)]),
        "s3",
        "t",
    )
    .await
    .unwrap();
    let remote = FakeGh::with_cli(Route::Local, CliKind::Glab);
    remote.on("GET", "user", 200, json!({"username": "ana"}));
    let gw = crate::fake::FakeGateway::new(&["n1", "n2", "n3"], remote.clone());
    core.set_gateway(gw.clone());
    // Signed out locally: glab is missing from this host.
    let sel = Selector::new(
        core.clone(),
        LocalGh::new(Arc::new(FakeExec {
            auth: None,
            gh: FakeGh::new(),
        })),
    );
    let (st, cli) = sel.select_conn(&conn).await;
    assert_eq!(
        st.route,
        Route::Node {
            id: "n3".into(),
            name: "c-good".into()
        }
    );
    assert_eq!(st.label(), "via node c-good");
    let c = Client::with_conn(cli.unwrap(), core.clone(), conn.clone());
    assert_eq!(c.api().viewer(&c, "user").await.unwrap()["login"], "ana");
    let jobs = gw.jobs.lock().unwrap().clone();
    let (node, kind, payload, needs) = &jobs[0];
    assert_eq!((node.as_deref(), kind.as_str()), (Some("n3"), "forge"));
    assert_eq!(needs.forge, Some(("glab".into(), "gitlab.acme.dev".into())));
    let p: Value = serde_json::from_str(payload).unwrap();
    assert_eq!(
        (p["cli"].as_str(), p["host"].as_str()),
        (Some("glab"), Some("gitlab.acme.dev"))
    );
    // No node with a login for the host: unavailable, and says why.
    let other = add(&core, ForgeKind::Forgejo, "codeberg.org").await;
    let (st, cli) = sel.select_conn(&other).await;
    assert!(cli.is_none());
    assert!(
        st.label()
            .contains("no connected node has a tea login for codeberg.org"),
        "{}",
        st.label()
    );
}

#[tokio::test]
async fn pull_events_carry_files_once_per_head() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let pull = |sha: &str| {
        json!({
            "number": 3, "title": "T", "state": "open", "user": {"login": "ana"},
            "head": {"ref": "f", "sha": sha, "repo": {"full_name": "acme/api"}},
            "base": {"ref": "main", "sha": "b"}, "mergeable": true, "mergeable_state": "clean",
            "html_url": "u", "updated_at": "2026-10-20T00:00:00Z"
        })
    };
    gh.on("GET", "/repos/acme/api/pulls/3", 200, pull("h1"));
    gh.on(
        "GET",
        "/repos/acme/api/pulls/3/files?per_page=100",
        200,
        json!([{"filename": "src/a.rs"}]),
    );
    gh.on(
        "GET",
        "/repos/acme/api/pulls/3/reviews?per_page=100",
        200,
        json!([]),
    );
    gh.on(
        "GET",
        "/repos/acme/api/commits/h1/check-runs?per_page=100",
        200,
        json!({"check_runs": []}),
    );
    let c = Client::new(gh.clone(), core.clone());
    let mut rx = core.subscribe();
    sync::refresh_pull(&c, "acme/api", 3, "poller")
        .await
        .unwrap();
    let ev = drain(&mut rx)
        .into_iter()
        .find(|e| e.kind == kinds::PULL_OPENED)
        .unwrap();
    let d: Value = serde_json::from_str(&ev.data_json).unwrap();
    assert_eq!(d["files"], json!(["src/a.rs"]));
    assert_eq!(d["base"], "main");
    assert_eq!(d["fork"], false);
    // Same head, a label added: files from the cache, no second call.
    let mut v = pull("h1");
    v["labels"] = json!([{"name": "sf:fix"}]);
    gh.on("GET", "/repos/acme/api/pulls/3", 200, v);
    sync::refresh_pull(&c, "acme/api", 3, "poller")
        .await
        .unwrap();
    let ev = drain(&mut rx)
        .into_iter()
        .find(|e| e.kind == kinds::PULL_UPDATED)
        .unwrap();
    assert!(ev.data_json.contains("src/a.rs"));
    assert_eq!(
        gh.count("GET", "/repos/acme/api/pulls/3/files?per_page=100"),
        1
    );
}

#[tokio::test]
async fn delivery_people_get_an_association_off_github() {
    let (core, _d) = core().await;
    let conn = add(&core, ForgeKind::Forgejo, "codeberg.org").await;
    let tea = FakeGh::with_cli(Route::Local, CliKind::Tea);
    tea.on(
        "GET",
        "/repos/ana/app/collaborators/bo/permission",
        200,
        json!({"permission": "write"}),
    );
    tea.on(
        "GET",
        "/repos/ana/app/collaborators/cy/permission",
        403,
        json!({"message": "forbidden"}),
    );
    tea.on("GET", "/orgs/ana/members/cy", 204, Value::Null);
    let c = Client::with_conn(tea.clone(), core.clone(), conn);
    let api = c.api();
    let key = "codeberg.org/ana/app";
    assert_eq!(
        api.permission(&c, key, "bo", "user").await.unwrap(),
        "COLLABORATOR"
    );
    assert_eq!(
        api.permission(&c, key, "zed", "user").await.unwrap(),
        "NONE"
    );
    assert_eq!(
        api.permission(&c, key, "cy", "user").await.unwrap(),
        "MEMBER"
    );
    assert_eq!(parse::is_internal("COLLABORATOR"), Some(true));
    let members =
        json!([{"username": "ana", "access_level": 40}, {"username": "eve", "access_level": 20}]);
    assert_eq!(
        crate::forge::gitlab::gitlab_association(&members, "ana"),
        "MEMBER"
    );
    assert_eq!(
        crate::forge::gitlab::gitlab_association(&members, "eve"),
        "CONTRIBUTOR"
    );
    assert_eq!(
        crate::forge::gitlab::gitlab_association(&members, "x"),
        "NONE"
    );
}

#[tokio::test]
async fn label_events_say_who_and_whether_the_factory_added_them() {
    let (core, _d) = core().await;
    let gh = FakeGh::new();
    let issues = "/repos/acme/api/issues?state=all&sort=updated&direction=asc&per_page=100";
    let issue = |labels: Value, updated: &str| {
        json!([{"number": 5, "title": "Bug", "state": "open", "user": {"login": "cy"},
                "labels": labels, "html_url": "u", "created_at": "2026-10-20T00:00:00Z",
                "updated_at": updated}])
    };
    gh.on("GET", issues, 200, issue(json!([]), "2026-10-20T00:00:00Z"));
    let c = Client::new(gh.clone(), core.clone());
    sync::sync_issues(&c, "acme/api", "poller").await.unwrap();
    // A person adds sf:fix.
    let since = "/repos/acme/api/issues?state=all&sort=updated&direction=asc&per_page=100&since=2026-10-20T00:00:00Z";
    gh.on(
        "GET",
        since,
        200,
        issue(json!([{"name": "sf:fix"}]), "2026-10-20T01:00:00Z"),
    );
    gh.on(
        "GET",
        "/repos/acme/api/issues/5/events?per_page=100",
        200,
        json!([{"event": "labeled", "label": {"name": "sf:fix"}, "actor": {"login": "bo"}}]),
    );
    let mut rx = core.subscribe();
    sync::sync_issues(&c, "acme/api", "poller").await.unwrap();
    let ev = drain(&mut rx)
        .into_iter()
        .find(|e| e.kind == kinds::ISSUE_UPDATED)
        .unwrap();
    let d: Value = serde_json::from_str(&ev.data_json).unwrap();
    assert_eq!(d["labeledBy"], json!({"sf:fix": "bo"}));
    assert_eq!(d["fromFactory"], false);
    // The factory adds sf:triage: the event says so.
    sync::note_labels_added(&c, "acme/api", 5, &["sf:triage".into()], "factory:wf").await;
    let since2 = "/repos/acme/api/issues?state=all&sort=updated&direction=asc&per_page=100&since=2026-10-20T01:00:00Z";
    gh.on(
        "GET",
        since2,
        200,
        issue(
            json!([{"name": "sf:fix"}, {"name": "sf:triage"}]),
            "2026-10-20T02:00:00Z",
        ),
    );
    sync::sync_issues(&c, "acme/api", "poller").await.unwrap();
    let ev = drain(&mut rx)
        .into_iter()
        .find(|e| e.kind == kinds::ISSUE_UPDATED)
        .unwrap();
    let d: Value = serde_json::from_str(&ev.data_json).unwrap();
    assert_eq!(d["labelsAdded"], json!(["sf:triage"]));
    assert_eq!(d["fromFactory"], true);
}

#[tokio::test]
async fn comments_are_mirrored_with_association_and_sf_command() {
    let (core, _d) = core().await;
    let conn = add(&core, ForgeKind::Forgejo, "codeberg.org").await;
    let tea = FakeGh::with_cli(Route::Local, CliKind::Tea);
    let c = Client::with_conn(tea.clone(), core.clone(), conn);
    let key = "codeberg.org/ana/app";
    let repo = GhRepo {
        full_name: key.into(),
        owner: "codeberg.org/ana".into(),
        name: "app".into(),
        default_branch: "main".into(),
        ..Default::default()
    };
    // First pass: only the cursor.
    let mut rx = core.subscribe();
    crate::events::poll_repo(&c, &repo, "poller").await.unwrap();
    assert!(
        drain(&mut rx)
            .iter()
            .all(|e| e.kind != kinds::ISSUE_COMMENT)
    );
    let at = 1_792_661_531_000_i64; // 2026-10-22T09:32:11Z
    core.store()
        .gh_put_etag(
            &format!("cursor|{}|comments|{key}", c.conn().id),
            "cursor",
            "",
            &at.to_string(),
        )
        .await
        .unwrap();
    tea.on(
        "GET",
        "/repos/ana/app/issues/comments?limit=50&since=2026-10-22T09:32:11Z",
        200,
        json!([{"id": 7, "issue_url": "https://codeberg.org/api/v1/repos/ana/app/issues/4",
                "body": "looks bad\n/sf fix please", "user": {"login": "bo"},
                "html_url": "h", "created_at": "2026-10-22T10:00:00Z"}]),
    );
    tea.on(
        "GET",
        "/repos/ana/app/collaborators/bo/permission",
        200,
        json!({"permission": "write"}),
    );
    let rep = crate::events::poll_repo(&c, &repo, "poller").await.unwrap();
    assert_eq!(rep.comments, 1);
    let ev = drain(&mut rx)
        .into_iter()
        .find(|e| e.kind == kinds::ISSUE_COMMENT)
        .unwrap();
    let d: Value = serde_json::from_str(&ev.data_json).unwrap();
    assert_eq!(
        (d["number"].as_i64(), d["command"].as_str()),
        (Some(4), Some("fix"))
    );
    assert_eq!(
        (d["authorAssociation"].as_str(), d["internal"].as_bool()),
        (Some("COLLABORATOR"), Some(true))
    );
    // Seen once.
    let rep = crate::events::poll_repo(&c, &repo, "poller").await.unwrap();
    assert_eq!(rep.comments, 0);
}

#[tokio::test]
async fn conflict_commit_off_github_runs_on_a_node_as_forge_push() {
    use crate::conflicts::{ConflictFile, ConflictSet};
    use crate::merge3::Merge3;
    let (core, _d) = core().await;
    let conn = add(&core, ForgeKind::Gitlab, "gitlab.acme.dev").await;
    let remote = FakeGh::with_cli(Route::Local, CliKind::Glab);
    let gw = crate::fake::FakeGateway::new(&["n3"], remote);
    core.set_gateway(gw.clone());
    let node = crate::node::NodeCli::new(
        core.clone(),
        "n3",
        "c-good",
        CliKind::Glab,
        "gitlab.acme.dev",
    );
    let c = Client::with_conn(Arc::new(node), core.clone(), conn.clone());
    let key = "gitlab.acme.dev/grp/app";
    let set = ConflictSet {
        repo: key.into(),
        number: 3,
        title: "T".into(),
        head_repo: key.into(),
        head_ref: "feat".into(),
        head_sha: "a".repeat(40),
        base_ref: "main".into(),
        base_sha: "b".repeat(40),
        merge_base: "c".repeat(40),
        files: vec![ConflictFile {
            path: "app.txt".into(),
            previous: None,
            base: Some("a\n".into()),
            ours: Some("feat\n".into()),
            theirs: Some("main\n".into()),
            binary: false,
            merge: Merge3::new("a\n", "feat\n", "main\n"),
            conflicting: true,
        }],
        base_only: vec![],
        forge: ForgeKind::Gitlab,
    };
    let resolved = [("app.txt".to_string(), Some("both\n".to_string()))].into();
    // Stopped: refused before any job.
    core.set_factory_stopped(true, "drill").await.unwrap();
    assert!(
        crate::conflicts::commit(&c, &set, &resolved, "", "factory:wf")
            .await
            .is_err()
    );
    assert!(gw.jobs.lock().unwrap().is_empty());
    core.set_factory_stopped(false, "done").await.unwrap();
    let res = crate::conflicts::commit(&c, &set, &resolved, "", "user")
        .await
        .unwrap();
    assert_eq!(
        (res.commit_sha.as_str(), res.signing.as_str()),
        ("c0ffee", "unsigned_push")
    );
    let jobs = gw.jobs.lock().unwrap().clone();
    let (_, kind, payload, needs) = jobs.iter().find(|j| j.1 == "forge_push").unwrap();
    assert_eq!(kind, "forge_push");
    assert_eq!(needs.forge, Some(("glab".into(), "gitlab.acme.dev".into())));
    let p: Value = serde_json::from_str(payload).unwrap();
    assert_eq!(
        (p["repo"].as_str(), p["head_ref"].as_str()),
        (Some("grp/app"), Some("feat"))
    );
    assert_eq!(p["files"]["app.txt"], "both\n");
    assert!(
        p.get("url").is_none() && p.get("helper").is_none(),
        "the node forces both"
    );
    let acts = audit_actions(&core).await;
    assert!(
        acts.iter().any(|a| a.0 == "conflict_commit" && a.2 == "ok"),
        "{acts:?}"
    );
    assert!(
        acts.iter()
            .any(|a| a.0 == "conflict_commit" && a.2 == "refused"),
        "{acts:?}"
    );
}

#[tokio::test]
async fn gitlab_inline_comments_become_positioned_discussions() {
    use crate::forge::api::InlineComment;
    let (core, _d) = core().await;
    let conn = add(&core, ForgeKind::Gitlab, "gitlab.acme.dev").await;
    let gl = FakeGh::with_cli(Route::Local, CliKind::Glab);
    let c = Client::with_conn(gl.clone(), core.clone(), conn);
    let key = "gitlab.acme.dev/grp/app";
    let p = "projects/grp%2Fapp";
    let inline = [
        InlineComment {
            path: "a.rs".into(),
            line: 3,
            body: "here".into(),
        },
        InlineComment {
            path: "b.rs".into(),
            line: 900,
            body: "outside".into(),
        },
    ];
    gl.on("GET", &format!("{p}/merge_requests/3"), 200, mr(3, "h3"));
    gl.on(
        "POST",
        &format!("{p}/merge_requests/3/notes"),
        201,
        json!({"id": 9}),
    );
    gl.on(
        "POST",
        &format!("{p}/merge_requests/3/discussions"),
        201,
        fixture("gitlab/discussion"),
    );
    let api = c.api();
    let v = api
        .review_inline(
            &c,
            key,
            3,
            "COMMENT",
            "body",
            &inline[..1],
            "h3",
            "factory:wf",
        )
        .await
        .unwrap();
    assert_eq!(v["folded"], 0);
    let calls = gl.calls();
    let disc = calls
        .iter()
        .find(|x| x.path.ends_with("/discussions"))
        .unwrap();
    let b: Value = serde_json::from_str(disc.stdin.as_deref().unwrap()).unwrap();
    assert_eq!(
        b["position"],
        json!({"position_type": "text", "base_sha": "b0", "start_sha": "b1", "head_sha": "h3",
               "new_path": "a.rs", "old_path": "a.rs", "new_line": 3})
    );
    let note = calls.last().unwrap().stdin.clone().unwrap();
    assert!(!note.contains("a.rs:3"), "{note}");

    // A line GitLab refuses (outside the diff) folds into the note.
    gl.on(
        "POST",
        &format!("{p}/merge_requests/3/discussions"),
        400,
        json!({"message": "400 Bad request - Note {:line_code=>[\"can't be blank\"]}"}),
    );
    let v = api
        .review_inline(
            &c,
            key,
            3,
            "COMMENT",
            "body",
            &inline[1..],
            "h3",
            "factory:wf",
        )
        .await
        .unwrap();
    assert_eq!(v["folded"], 1);
    let note = gl.calls().last().unwrap().stdin.clone().unwrap();
    assert!(note.contains("`b.rs:900` outside"), "{note}");

    // A review of an older head folds too (lines would land elsewhere).
    let before = gl
        .calls()
        .iter()
        .filter(|x| x.path.ends_with("/discussions"))
        .count();
    let v = api
        .review_inline(
            &c,
            key,
            3,
            "COMMENT",
            "body",
            &inline[..1],
            "old",
            "factory:wf",
        )
        .await
        .unwrap();
    assert_eq!(v["folded"], 1);
    assert_eq!(
        gl.calls()
            .iter()
            .filter(|x| x.path.ends_with("/discussions"))
            .count(),
        before
    );

    // Audited under the workflow; refused while the factory is stopped.
    assert!(
        audit_actions(&core)
            .await
            .iter()
            .any(|(_, actor, o)| actor == "workflow wf" && o == "ok")
    );
    kls_core::control::stop(&core, "test").await.unwrap();
    let posts = || gl.calls().iter().filter(|x| x.method == "POST").count();
    let n = posts();
    assert!(
        api.review_inline(&c, key, 3, "COMMENT", "b", &inline[..1], "h3", "factory:wf")
            .await
            .is_err()
    );
    assert_eq!(posts(), n, "no write while stopped");
    assert!(
        audit_actions(&core)
            .await
            .iter()
            .any(|(_, _, o)| o == "refused"),
    );
}

#[tokio::test]
async fn issue_edits_reviews_and_approvals_on_every_forge() {
    use crate::forge::api::{InlineComment, IssueEdit};
    let (core, _d) = core().await;
    let edit = IssueEdit {
        body: Some("new body".into()),
        state: Some("open".into()),
        labels: Some(vec!["sf:fix".into()]),
        ..Default::default()
    };
    let inline = [InlineComment {
        path: "a.rs".into(),
        line: 3,
        body: "here".into(),
    }];

    // GitLab: state_event, comma labels; approve with the head sha; inline
    // comments fold into the note when the diff refs are unknown.
    let conn = add(&core, ForgeKind::Gitlab, "gitlab.acme.dev").await;
    let gl = FakeGh::with_cli(Route::Local, CliKind::Glab);
    let c = Client::with_conn(gl.clone(), core.clone(), conn);
    let key = "gitlab.acme.dev/grp/app";
    let p = "projects/grp%2Fapp";
    gl.on("PUT", &format!("{p}/issues/4"), 200, json!({"iid": 4}));
    gl.on(
        "POST",
        &format!("{p}/merge_requests/3/notes"),
        201,
        json!({"id": 9}),
    );
    gl.on(
        "POST",
        &format!("{p}/merge_requests/3/approve"),
        201,
        json!({}),
    );
    let api = c.api();
    api.update_issue(&c, key, 4, &edit, "factory:wf")
        .await
        .unwrap();
    let put: Value =
        serde_json::from_str(gl.calls().last().unwrap().stdin.as_deref().unwrap()).unwrap();
    assert_eq!(
        put,
        json!({"description": "new body", "state_event": "reopen", "labels": "sf:fix"})
    );
    api.approve(&c, key, 3, "ok", "h3", "factory:wf")
        .await
        .unwrap();
    let calls = gl.calls();
    let ap = calls
        .iter()
        .rfind(|x| x.path.ends_with("/approve"))
        .unwrap();
    assert!(ap.stdin.as_deref().unwrap().contains("\"sha\":\"h3\""));
    api.review_inline(&c, key, 3, "COMMENT", "body", &inline, "h3", "factory:wf")
        .await
        .unwrap();
    let note = gl.calls().last().unwrap().stdin.clone().unwrap();
    assert!(note.contains("`a.rs:3` here"), "{note}");
    assert_eq!(
        api.branch_protected(&c, key, "main", "factory:wf")
            .await
            .unwrap(),
        None
    );

    // Gitea / Forgejo: PATCH the fields, PUT labels by id, APPROVED review
    // with inline comments by new_position.
    let conn = add(&core, ForgeKind::Forgejo, "codeberg.org").await;
    let tea = FakeGh::with_cli(Route::Local, CliKind::Tea);
    let c = Client::with_conn(tea.clone(), core.clone(), conn);
    let key = "codeberg.org/ana/app";
    tea.on(
        "PATCH",
        "/repos/ana/app/issues/4",
        201,
        json!({"number": 4}),
    );
    tea.on(
        "GET",
        "/repos/ana/app/labels?limit=50",
        200,
        fixture("gitea/labels"),
    );
    tea.on("PUT", "/repos/ana/app/issues/4/labels", 200, json!([]));
    tea.on(
        "POST",
        "/repos/ana/app/pulls/3/reviews",
        200,
        json!({"id": 5}),
    );
    tea.on(
        "GET",
        "/repos/ana/app/branches/main",
        200,
        json!({"name": "main", "protected": true}),
    );
    let api = c.api();
    api.update_issue(&c, key, 4, &edit, "factory:wf")
        .await
        .unwrap();
    let calls = tea.calls();
    let patch = calls.iter().find(|x| x.method == "PATCH").unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(patch.stdin.as_deref().unwrap()).unwrap(),
        json!({"body": "new body", "state": "open"})
    );
    let put = calls.iter().find(|x| x.method == "PUT").unwrap();
    assert!(put.stdin.as_deref().unwrap().contains("[11]"));
    api.approve(&c, key, 3, "ok", "h3", "factory:wf")
        .await
        .unwrap();
    api.review_inline(
        &c,
        key,
        3,
        "REQUEST_CHANGES",
        "b",
        &inline,
        "h3",
        "factory:wf",
    )
    .await
    .unwrap();
    let posts: Vec<Value> = tea
        .calls()
        .iter()
        .filter(|x| x.method == "POST" && x.path == "/repos/ana/app/pulls/3/reviews")
        .map(|x| serde_json::from_str(x.stdin.as_deref().unwrap()).unwrap())
        .collect();
    assert_eq!(posts[0]["event"], "APPROVED");
    assert_eq!(posts[1]["comments"][0]["new_position"], 3);
    assert_eq!(
        api.branch_protected(&c, key, "main", "factory:wf")
            .await
            .unwrap(),
        Some(true)
    );

    // Every write was audited under the workflow, and none is made while stopped.
    let audit = audit_actions(&core).await;
    assert!(
        audit
            .iter()
            .filter(|(_, actor, o)| actor == "workflow wf" && o == "ok")
            .count()
            >= 6,
        "{audit:?}"
    );
    kls_core::control::stop(&core, "test").await.unwrap();
    assert!(
        api.update_issue(&c, key, 4, &edit, "factory:wf")
            .await
            .is_err()
    );
}
