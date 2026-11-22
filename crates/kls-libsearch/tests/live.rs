//! Live smoke tests: one call per source against the real endpoints,
//! asserting shape only (they catch upstream drift such as skills.sh's v1
//! auth). Ignored by default and skipped unless `KLS_LIVE_NET=1`:
//!
//! `KLS_LIVE_NET=1 cargo test -p kls-libsearch --test live -- --ignored`

use kls_libsearch::fetch::{Ctx, HttpFetcher};
use kls_libsearch::sources::{self, Config, find};
use kls_libsearch::{ItemType, Query, Source};
use std::sync::Arc;
use std::time::Duration;

fn live() -> Option<Ctx> {
    if std::env::var("KLS_LIVE_NET").ok().as_deref() != Some("1") {
        eprintln!("skipped: set KLS_LIVE_NET=1");
        return None;
    }
    let http = Arc::new(HttpFetcher::new(None).expect("http client"));
    Some(Ctx::new(http).with_deadline(Duration::from_secs(120)))
}

fn src(id: &str) -> Arc<dyn Source> {
    let cfg = Config {
        well_known: vec!["https://www.mintlify.com/docs".into()],
        ..Default::default()
    };
    find(&sources::all(&cfg), id).unwrap()
}

fn q(t: &str) -> Query {
    let mut q = Query::text(t);
    q.limit = 5;
    q
}

#[tokio::test]
#[ignore]
async fn s1_mcp_registry() {
    let Some(cx) = live() else { return };
    let s = src("mcp-registry");
    let hits = s.search(&cx, &q("github")).await.unwrap();
    assert!(!hits.is_empty());
    let f = s.fetch(&cx, &hits[0]).await.unwrap();
    assert!(f.files.contains_key("server.json"));
    assert!(f.upstream.version.is_some());
}

#[tokio::test]
#[ignore]
async fn s2_hermes_index() {
    let Some(cx) = live() else { return };
    let r = src("hermes-index").refresh(&cx, None).await.unwrap();
    assert!(r.hits.len() > 100, "{}", r.hits.len());
    assert!(r.cursor.is_some());
    let s = src("hermes-index");
    let h = r
        .hits
        .iter()
        .find(|h| matches!(&h.locator, kls_libsearch::Locator::GitHub { repo, .. } if repo == "hermes-agent"))
        .unwrap();
    let f = s.fetch(&cx, h).await.unwrap();
    assert!(f.files.contains_key("SKILL.md"));
}

#[tokio::test]
#[ignore]
async fn s3_clawhub() {
    let Some(cx) = live() else { return };
    let s = src("clawhub");
    let hits = s.search(&cx, &q("pdf")).await.unwrap();
    assert!(!hits.is_empty());
    let f = s.fetch(&cx, &hits[0]).await.unwrap();
    assert!(f.files.contains_key("SKILL.md"));
    assert!(f.upstream.version.is_some());
}

#[tokio::test]
#[ignore]
async fn s4_skills_sh() {
    let Some(cx) = live() else { return };
    let hits = src("skills-sh").search(&cx, &q("pdf")).await.unwrap();
    assert!(!hits.is_empty());
}

#[tokio::test]
#[ignore]
async fn s5_gemini_gallery() {
    let Some(cx) = live() else { return };
    let r = src("gemini-gallery").refresh(&cx, None).await.unwrap();
    assert!(r.hits.len() > 100);
    assert!(r.hits.iter().any(|h| h.item_type == ItemType::Mcp));
}

#[tokio::test]
#[ignore]
async fn s6_aitmpl() {
    let Some(cx) = live() else { return };
    let s = src("aitmpl");
    let r = s.refresh(&cx, None).await.unwrap();
    assert!(r.hits.len() > 100);
    let agent = r
        .hits
        .iter()
        .find(|h| h.item_type == ItemType::Agent)
        .unwrap();
    let f = s.fetch(&cx, agent).await.unwrap();
    assert_eq!(f.files.len(), 1);
    assert!(f.upstream.commit.is_some());
}

#[tokio::test]
#[ignore]
async fn s7_lobehub() {
    let Some(cx) = live() else { return };
    let s = src("lobehub");
    let r = s.refresh(&cx, None).await.unwrap();
    assert!(r.hits.len() > 100);
    let f = s.fetch(&cx, &r.hits[0]).await.unwrap();
    assert_eq!(f.files.len(), 1);
}

#[tokio::test]
#[ignore]
async fn s8_cline_mcp() {
    let Some(cx) = live() else { return };
    let r = src("cline-mcp").refresh(&cx, None).await.unwrap();
    assert!(r.hits.len() > 10);
}

#[tokio::test]
#[ignore]
async fn s9_well_known() {
    let Some(cx) = live() else { return };
    let s = src("well-known:www.mintlify.com/docs");
    let r = s.refresh(&cx, None).await.unwrap();
    assert!(!r.hits.is_empty());
    let f = s.fetch(&cx, &r.hits[0]).await.unwrap();
    assert!(f.files.contains_key("SKILL.md"));
}

#[tokio::test]
#[ignore]
async fn s10_s11_package_facts() {
    let Some(cx) = live() else { return };
    let n = sources::pkg_facts::npm(&cx, "@modelcontextprotocol/server-github")
        .await
        .unwrap();
    assert!(n.latest.is_some());
    let p = sources::pkg_facts::pypi(&cx, "mcp-server-git")
        .await
        .unwrap();
    assert!(p.latest.is_some());
}

#[tokio::test]
#[ignore]
async fn m2_curated_tarball_anthropics_skills() {
    let Some(cx) = live() else { return };
    let s = src("gh:anthropics/skills");
    let r = s.refresh(&cx, None).await.unwrap();
    assert!(r.hits.iter().any(|h| h.item_type == ItemType::Skill));
    let again = s.refresh(&cx, r.cursor.as_deref()).await.unwrap();
    assert!(again.not_modified);
    let f = s.fetch(&cx, &r.hits[0]).await.unwrap();
    assert!(f.files.contains_key("SKILL.md"));
}

#[tokio::test]
#[ignore]
async fn m1_marketplace_claude_plugins_official() {
    let Some(cx) = live() else { return };
    let r = src("gh:anthropics/claude-plugins-official")
        .refresh(&cx, None)
        .await
        .unwrap();
    assert!(r.hits.len() > 20, "{}", r.hits.len());
    assert!(r.hits.iter().any(|h| h.tags.iter().any(|t| t == "plugin")));
}

#[tokio::test]
#[ignore]
async fn g_curated_refs_only() {
    let Some(cx) = live() else { return };
    // Every curated repo answers ls-refs (cheap; no tarballs).
    for c in kls_libsearch::catalogue::curated() {
        let u = sources::github_repo::latest_commit(&cx, &c.owner, &c.repo, None)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", c.id()));
        assert!(u.and_then(|u| u.commit).is_some(), "{}", c.id());
    }
}

#[tokio::test]
#[ignore]
async fn g10_awesome_csv() {
    let Some(cx) = live() else { return };
    let r = src("awesome-claude-code").refresh(&cx, None).await.unwrap();
    assert!(r.hits.len() > 50);
}

#[tokio::test]
#[ignore]
async fn wp8_gh_code_needs_gh() {
    let Some(cx) = live() else { return };
    // No gh in this context: the source answers nothing and sends nothing.
    assert!(
        src("gh-code")
            .search(&cx, &q("deploy"))
            .await
            .unwrap()
            .is_empty()
    );
}
