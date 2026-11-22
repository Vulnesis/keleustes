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
    assert_eq!(auth.login(), "ana");
    let c = svc.client_conn(&conn).await.unwrap();

    // Discovery.
    let r = sync::refresh_orgs(&c, "user", false).await.unwrap();
    assert!(r.orgs >= 1);
    let me = core
        .store()
        .gh_org(&conn.org_key("ana"))
        .await
        .unwrap()
        .unwrap();
    let rr = sync::refresh_org_repos(&c, &me, "user").await.unwrap();
    let key = conn.repo_key("ana/app");
    assert!(rr.repos >= 1, "{rr:?}");
    // Pulls, checks as statuses, labels by id, issues.
    sync::sync_pulls(&c, &key, "user").await.unwrap();
    let pulls = core
        .store()
        .gh_pulls(Some(&key), Some("open"))
        .await
        .unwrap();
    let pr = pulls.first().expect("the seeded pull request").clone();
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
