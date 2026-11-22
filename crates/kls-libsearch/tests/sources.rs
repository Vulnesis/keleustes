//! Every source through `FakeFetcher`: no network. Fixtures are trimmed
//! real bodies captured 2026-11-21 (`tests/fixtures/<source>/`); repo
//! tarballs are built here from `tests/fixtures/repos/<name>/`.

use kls_libsearch::catalogue::Tap;
use kls_libsearch::fetch::{Class, Ctx, FakeFetcher, FixedGate, GhApi, HttpFetcher, is_stopped};
use kls_libsearch::git::build_tarball;
use kls_libsearch::sources::{self, Config, find};
use kls_libsearch::{Flag, Hit, Index, ItemType, Locator, Query, Source};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SHA: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
const SHA2: &str = "b2b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fx(p: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(p)).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(root, &p, out);
        } else {
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, std::fs::read(&p).unwrap()));
        }
    }
}

/// A codeload-shaped tarball of `fixtures/repos/<name>`.
fn repo_tarball(name: &str, top: &str) -> Vec<u8> {
    let root = fixtures().join("repos").join(name);
    let mut files = vec![];
    walk(&root, &root, &mut files);
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, d)| (p.as_str(), d.as_slice()))
        .collect();
    build_tarball(top, &refs)
}

fn pkt(s: &str) -> String {
    format!("{:04x}{s}", s.len() + 4)
}

/// A smart-HTTP ref advertisement.
fn refs_body(head: &str, extra: &[(&str, &str)]) -> String {
    let mut b = pkt("# service=git-upload-pack\n") + "0000";
    b += &pkt(&format!(
        "{head} HEAD\0multi_ack symref=HEAD:refs/heads/main\n"
    ));
    b += &pkt(&format!("{head} refs/heads/main\n"));
    for (r, s) in extra {
        b += &pkt(&format!("{s} {r}\n"));
    }
    b + "0000"
}

fn refs_url(o: &str, r: &str) -> String {
    format!("https://github.com/{o}/{r}.git/info/refs?service=git-upload-pack")
}

fn tar_url(o: &str, r: &str, sha: &str) -> String {
    format!("https://codeload.github.com/{o}/{r}/tar.gz/{sha}")
}

fn serve_repo(f: &FakeFetcher, o: &str, r: &str, name: &str, sha: &str) {
    f.serve(&refs_url(o, r), 200, refs_body(sha, &[]));
    f.serve(
        &tar_url(o, r, sha),
        200,
        repo_tarball(name, &format!("{r}-{sha}")),
    );
}

fn all() -> Vec<Arc<dyn Source>> {
    sources::all(&Config::default())
}

fn src(id: &str) -> Arc<dyn Source> {
    find(&all(), id).unwrap_or_else(|| panic!("no source {id}"))
}

fn cx(f: &Arc<FakeFetcher>) -> Ctx {
    Ctx::new(f.clone())
}

// ---- curated repos, marketplaces, taps -------------------------------------------

#[tokio::test]
async fn curated_repo_refresh_then_unchanged_commit_stops_at_refs() {
    let f = FakeFetcher::new();
    serve_repo(&f, "anthropics", "skills", "skills", SHA);
    let s = src("gh:anthropics/skills");
    let r = s.refresh(&cx(&f), None).await.unwrap();
    assert_eq!(r.cursor.as_deref(), Some(SHA));
    let names: Vec<_> = r.hits.iter().map(|h| h.name.as_str()).collect();
    assert!(
        names.contains(&"pdf") && names.contains(&"xlsx"),
        "{names:?}"
    );
    let pdf = r.hits.iter().find(|h| h.name == "pdf").unwrap();
    assert_eq!(pdf.item_type, ItemType::Skill);
    assert_eq!(pdf.description, "Read and write PDF files");
    assert_eq!(pdf.licence.spdx, "Proprietary");
    assert_eq!(
        pdf.locator,
        Locator::GitHub {
            owner: "anthropics".into(),
            repo: "skills".into(),
            git_ref: Some(SHA.into()),
            path: "skills/pdf".into()
        }
    );
    assert_eq!(f.requests().len(), 2, "one refs check, one tarball");
    f.clear_log();
    let r = s.refresh(&cx(&f), Some(SHA)).await.unwrap();
    assert!(r.not_modified && r.hits.is_empty());
    assert_eq!(
        f.requests(),
        vec![refs_url("anthropics", "skills")],
        "no tarball"
    );
}

#[tokio::test]
async fn skill_fetch_is_pinned_rooted_and_hashed() {
    let f = FakeFetcher::new();
    serve_repo(&f, "anthropics", "skills", "skills", SHA);
    let s = src("gh:anthropics/skills");
    let hits = s.refresh(&cx(&f), None).await.unwrap().hits;
    let pdf = hits.into_iter().find(|h| h.name == "pdf").unwrap();
    f.clear_log();
    let got = s.fetch(&cx(&f), &pdf).await.unwrap();
    // Pinned: no refs lookup, the tarball at the SHA only.
    assert_eq!(f.requests(), vec![tar_url("anthropics", "skills", SHA)]);
    let keys: Vec<_> = got.files.keys().cloned().collect();
    assert_eq!(keys, vec!["LICENSE.txt", "SKILL.md", "scripts/fill.py"]);
    assert_eq!(got.primary.as_deref(), Some("SKILL.md"));
    assert_eq!(got.upstream.commit.as_deref(), Some(SHA));
    assert_eq!(
        got.upstream.urls,
        vec![tar_url("anthropics", "skills", SHA)]
    );
    assert_eq!(got.upstream.sha256, got.files_sha256());
    assert_eq!(
        got.file_urls["SKILL.md"],
        format!("https://raw.githubusercontent.com/anthropics/skills/{SHA}/skills/pdf/SKILL.md")
    );
    assert!(
        got.licence_files
            .iter()
            .any(|(p, _)| p == "skills/pdf/LICENSE.txt")
    );
}

#[tokio::test]
async fn marketplace_entries_command_refused_and_moved_ref_refused() {
    let f = FakeFetcher::new();
    serve_repo(&f, "anthropics", "claude-plugins-official", "plugins", SHA);
    let s = src("gh:anthropics/claude-plugins-official");
    let hits = s.refresh(&cx(&f), None).await.unwrap().hits;
    let by = |n: &str| {
        hits.iter()
            .find(|h| h.name == n)
            .unwrap_or_else(|| panic!("no {n}: {hits:#?}"))
    };
    // Components found by the plugin globs.
    assert_eq!(by("sdk-verifier").item_type, ItemType::Agent);
    assert_eq!(by("new-sdk-app").item_type, ItemType::Command);
    assert_eq!(by("sub:deep").item_type, ItemType::Command);
    assert_eq!(by("build").item_type, ItemType::Skill);
    assert_eq!(by("sdk").item_type, ItemType::Hook);
    assert_eq!(by("sdk-docs").item_type, ItemType::Mcp);
    assert_eq!(by("other").item_type, ItemType::Mcp);
    // `source: "command"` is refused and never fetched.
    let x = by("xcode");
    assert!(x.flags.iter().any(|f| matches!(f, Flag::Refused(_))));
    f.clear_log();
    let got = s.fetch(&cx(&f), x).await.unwrap();
    assert!(
        got.flags
            .iter()
            .any(|f| matches!(f, Flag::Refused(r) if r.contains("runs a program")))
    );
    assert!(f.requests().is_empty());
    // A git-subdir entry pinned to a sha whose ref has moved: refused.
    let sub = by("42crunch-api-security-testing");
    let pinned = "faf5305385de8afed9468904e8639be737aff39e";
    assert!(
        matches!(&sub.locator, Locator::GitHub { git_ref: Some(r), path, .. } if r == pinned && path == "plugins/api-security-testing")
    );
    f.serve(
        &refs_url("42Crunch-AI", "claude-plugins"),
        200,
        refs_body(SHA2, &[("refs/tags/v1.5.5", SHA2)]),
    );
    f.serve(
        &tar_url("42Crunch-AI", "claude-plugins", pinned),
        200,
        repo_tarball("subdir", &format!("claude-plugins-{pinned}")),
    );
    let got = s.fetch(&cx(&f), sub).await.unwrap();
    assert_eq!(got.upstream.commit.as_deref(), Some(pinned));
    assert_eq!(got.primary.as_deref(), Some("skills/audit/SKILL.md"));
    assert!(
        got.flags
            .iter()
            .any(|f| matches!(f, Flag::Refused(r) if r.contains("v1.5.5")))
    );
    // The ref still at the pin: no flag.
    f.serve(
        &refs_url("42Crunch-AI", "claude-plugins"),
        200,
        refs_body(SHA2, &[("refs/tags/v1.5.5", pinned)]),
    );
    let got = s.fetch(&cx(&f), sub).await.unwrap();
    assert!(got.flags.is_empty(), "{:?}", got.flags);
    // A hook bundle carries its scripts, rooted at the plugin.
    let hook = s.fetch(&cx(&f), by("sdk")).await.unwrap();
    assert_eq!(hook.primary.as_deref(), Some("hooks/hooks.json"));
    assert!(hook.files.contains_key("hooks/check.sh"));
    assert!(hook.licence_files.iter().any(|(p, _)| p == "LICENSE"));
}

#[tokio::test]
async fn tap_uses_the_harness_detector() {
    let f = FakeFetcher::new();
    serve_repo(&f, "someone", "dots", "tap", SHA);
    let cfg = Config {
        taps: vec![Tap::parse("someone/dots", &[]).unwrap()],
        ..Default::default()
    };
    let s = find(&sources::all(&cfg), "gh:someone/dots").unwrap();
    let hits = s.refresh(&cx(&f), None).await.unwrap().hits;
    let mut got: Vec<_> = hits
        .iter()
        .map(|h| (h.name.as_str(), h.item_type, h.ecosystem.as_str()))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("architect", ItemType::Agent, "roo"),
            ("coder", ItemType::Agent, "roo"),
            ("dots-agents", ItemType::Prompt, "generic"),
            ("gcs:sync", ItemType::Command, "gemini-cli"),
            ("rev", ItemType::Agent, "claude-code"),
            ("rust", ItemType::Prompt, "cursor"),
        ]
    );
    // A tap with globs keeps to them.
    let cfg = Config {
        taps: vec![Tap::parse("someone/dots", &[".cursor/rules/*.mdc".into()]).unwrap()],
        ..Default::default()
    };
    let s = find(&sources::all(&cfg), "gh:someone/dots").unwrap();
    let hits = s.refresh(&cx(&f), None).await.unwrap().hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].item_type, ItemType::Prompt);
}

#[tokio::test]
async fn huge_repo_is_never_tarballed() {
    let f = FakeFetcher::new();
    let s = src("gh:NousResearch/hermes-agent");
    let r = s.refresh(&cx(&f), None).await.unwrap();
    assert!(r.not_modified && r.hits.is_empty());
    assert!(f.requests().is_empty());
    // Its items come raw by path (through the Hermes index).
    let h = src("hermes-index");
    let hit = Hit {
        source: "hermes-index".into(),
        locator: Locator::GitHub {
            owner: "NousResearch".into(),
            repo: "hermes-agent".into(),
            git_ref: Some(SHA.into()),
            path: "optional-skills/security/1password".into(),
        },
        item_type: ItemType::Skill,
        ecosystem: kls_libsearch::Ecosystem::Hermes,
        name: "1password".into(),
        title: "1password".into(),
        description: String::new(),
        tags: vec![],
        licence: Default::default(),
        trust: kls_libsearch::Trust::Official,
        popularity: None,
        updated_at: None,
        flags: vec![],
    };
    let raw = format!(
        "https://raw.githubusercontent.com/NousResearch/hermes-agent/{SHA}/optional-skills/security/1password/SKILL.md"
    );
    f.serve(&raw, 200, "---\nname: 1password\ndescription: op\n---\nx\n");
    let got = h.fetch(&cx(&f), &hit).await.unwrap();
    assert_eq!(f.requests(), vec![raw.clone()]);
    assert!(!f.requests().iter().any(|u| u.contains("codeload")));
    assert_eq!(got.upstream.urls, vec![raw]);
    assert_eq!(got.licence_hint.as_deref(), Some("MIT"));
}

#[tokio::test]
async fn closed_gate_sends_nothing_from_any_source() {
    let f = FakeFetcher::new();
    serve_repo(&f, "anthropics", "skills", "skills", SHA);
    let mut c = cx(&f);
    c.gate = Some(Arc::new(FixedGate(false)));
    for id in [
        "gh:anthropics/skills",
        "mcp-registry",
        "gemini-gallery",
        "aitmpl",
        "lobehub",
        "cline-mcp",
        "awesome-claude-code",
        "hermes-index",
    ] {
        let e = src(id).refresh(&c, None).await.unwrap_err();
        assert!(is_stopped(&e), "{id}: {e}");
    }
    for id in ["clawhub", "mcp-registry", "skills-sh"] {
        let e = src(id).search(&c, &Query::text("pdf")).await.unwrap_err();
        assert!(is_stopped(&e), "{id}: {e}");
    }
    assert!(f.requests().is_empty());
}

// ---- registries and indexes --------------------------------------------------------

const MCP: &str = "https://registry.modelcontextprotocol.io/v0.1";

#[tokio::test]
async fn mcp_registry_pages_then_deltas_and_fetches_a_version() {
    let f = FakeFetcher::new();
    f.serve(
        &format!("{MCP}/servers?version=latest&limit=100"),
        200,
        fx("mcp_registry/page1.json"),
    );
    f.serve(
        &format!(
            "{MCP}/servers?version=latest&limit=100&cursor=ai.smithery%2FHint-Services-obsidian-github-mcp%3A0.4.0"
        ),
        200,
        fx("mcp_registry/page2.json"),
    );
    let s = src("mcp-registry");
    let r = s.refresh(&cx(&f), None).await.unwrap();
    assert_eq!(r.hits.len(), 4);
    assert!(!r.partial);
    let cur = r.cursor.clone().unwrap();
    assert!(cur.starts_with("since:2026-11-20T16:05:47"), "{cur}");
    let h = &r.hits[2];
    assert_eq!(h.name, "Hint-Services-obsidian-github-mcp");
    assert_eq!(
        h.locator,
        Locator::Mcp {
            name: "ai.smithery/Hint-Services-obsidian-github-mcp".into(),
            version: "0.4.0".into()
        }
    );
    // A delta call: updated_since, never retires.
    let since = cur.trim_start_matches("since:");
    f.serve(
        &format!(
            "{MCP}/servers?version=latest&limit=100&updated_since={}",
            kls_libsearch::fetch::urlencode(since)
        ),
        200,
        r#"{"servers":[],"metadata":{"count":0}}"#,
    );
    let r2 = s.refresh(&cx(&f), Some(&cur)).await.unwrap();
    assert!(r2.partial && r2.hits.is_empty());
    // Fetch at the exact version.
    let url =
        format!("{MCP}/servers/ai.smithery%2FHint-Services-obsidian-github-mcp/versions/0.4.0");
    let server = String::from_utf8(fx("mcp_registry/server.json")).unwrap();
    f.serve(&url, 200, format!("{{\"server\":{server}}}"));
    let got = s.fetch(&cx(&f), h).await.unwrap();
    assert_eq!(got.upstream.version.as_deref(), Some("0.4.0"));
    assert_eq!(got.upstream.urls, vec![url]);
    assert_eq!(got.primary.as_deref(), Some("server.json"));
    // Remote search.
    f.serve(
        &format!("{MCP}/servers?search=github&version=latest&limit=20"),
        200,
        fx("mcp_registry/page1.json"),
    );
    let mut q = Query::text("github");
    q.limit = 20;
    assert_eq!(s.search(&cx(&f), &q).await.unwrap().len(), 3);
}

#[tokio::test]
async fn etag_round_trip_is_not_modified() {
    let d = tempfile::tempdir().unwrap();
    let ix = Index::open(&d.path().join("libsearch.db")).await.unwrap();
    let fake = FakeFetcher::new();
    fake.serve_with(
        "https://geminicli.com/extensions.json",
        200,
        vec![("etag", "\"abc\"")],
        fx("gemini_gallery/extensions.json"),
    );
    let http: Arc<HttpFetcher> =
        Arc::new(HttpFetcher::with_transport(fake.clone(), Some(ix.cache())));
    let c = Ctx::new(http);
    let s = src("gemini-gallery");
    let r = s.refresh(&c, None).await.unwrap();
    assert!(!r.not_modified);
    let sp = r
        .hits
        .iter()
        .find(|h| h.name == "superpowers" && h.item_type == ItemType::Skill)
        .unwrap();
    assert_eq!(sp.licence.spdx, "MIT");
    assert!(r.hits.iter().any(|h| h.item_type == ItemType::Hook));
    let r = s.refresh(&c, None).await.unwrap();
    assert!(r.not_modified && r.hits.is_empty());
    assert_eq!(fake.requests().len(), 2);
}

#[tokio::test]
async fn gemini_extension_crawled_on_demand() {
    let f = FakeFetcher::new();
    f.serve(
        "https://geminicli.com/extensions.json",
        200,
        fx("gemini_gallery/extensions.json"),
    );
    let s = src("gemini-gallery");
    let hits = s.refresh(&cx(&f), None).await.unwrap().hits;
    let mut h = hits
        .iter()
        .find(|h| h.item_type == ItemType::Skill)
        .unwrap()
        .clone();
    // Point it at a fixture repo.
    h.locator = Locator::GitHub {
        owner: "anthropics".into(),
        repo: "skills".into(),
        git_ref: None,
        path: String::new(),
    };
    h.name = "xlsx".into();
    serve_repo(&f, "anthropics", "skills", "skills", SHA);
    let got = s.fetch(&cx(&f), &h).await.unwrap();
    assert_eq!(got.name_hint, "xlsx");
    assert_eq!(got.upstream.commit.as_deref(), Some(SHA));
}

#[tokio::test]
async fn clawhub_search_fetch_zip_and_409() {
    let f = FakeFetcher::new();
    let api = "https://clawhub.ai/api/v1";
    f.serve(
        &format!("{api}/search?q=pdf&limit=20&nonSuspiciousOnly=true"),
        200,
        fx("clawhub/search.json"),
    );
    let s = src("clawhub");
    let mut q = Query::text("pdf");
    q.limit = 20;
    let hits = s.search(&cx(&f), &q).await.unwrap();
    let h = &hits[0];
    assert_eq!(
        h.locator,
        Locator::ClawHub {
            owner: "awspace".into(),
            slug: "pdf".into(),
            version: None
        }
    );
    f.serve(
        &format!("{api}/skills/pdf?owner=awspace"),
        200,
        fx("clawhub/detail.json"),
    );
    // A ZIP with a licence, a path escape and a credential file.
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        for (p, d) in [
            ("SKILL.md", fx("clawhub/SKILL.md")),
            ("LICENSE", b"MIT License".to_vec()),
            ("../evil.md", b"x".to_vec()),
            (".env", b"TOKEN=1".to_vec()),
        ] {
            z.start_file(p, o).unwrap();
            std::io::Write::write_all(&mut z, &d).unwrap();
        }
        z.finish().unwrap();
    }
    let dl = format!("{api}/download?slug=pdf&version=0.1.0&owner=awspace");
    f.serve(&dl, 200, buf.into_inner());
    let got = s.fetch(&cx(&f), h).await.unwrap();
    let keys: Vec<_> = got.files.keys().cloned().collect();
    assert_eq!(keys, vec!["LICENSE", "SKILL.md"]);
    assert_eq!(got.upstream.version.as_deref(), Some("0.1.0"));
    assert_eq!(got.upstream.urls, vec![dl]);
    assert_eq!(got.licence_files.len(), 1);
    // A bare slug: 409 with five owners is not guessed.
    f.serve(
        &format!("{api}/skills/pdf"),
        409,
        fx("clawhub/ambiguous.json"),
    );
    let e = kls_libsearch::sources::clawhub::detail(&cx(&f), "", "pdf")
        .await
        .unwrap_err();
    assert!(e.to_string().contains("awspace"), "{e}");
    // One owner in the matches: resolved to it.
    f.serve(
        &format!("{api}/skills/solo"),
        409,
        r#"{"code":"AMBIGUOUS_SKILL_SLUG","matches":[{"ownerHandle":"awspace","slug":"solo"}]}"#,
    );
    f.serve(
        &format!("{api}/skills/solo?owner=awspace"),
        200,
        fx("clawhub/detail.json"),
    );
    let (o, _) = kls_libsearch::sources::clawhub::detail(&cx(&f), "", "solo")
        .await
        .unwrap();
    assert_eq!(o, "awspace");
    // Upstream suspicion is a flag.
    let mut v: serde_json::Value = serde_json::from_slice(&fx("clawhub/search.json")).unwrap();
    v["results"][0]["native"]["skill"]["isSuspicious"] = true.into();
    f.serve(
        &format!("{api}/search?q=pdf&limit=20&nonSuspiciousOnly=true"),
        200,
        serde_json::to_vec(&v).unwrap(),
    );
    let hits = s.search(&cx(&f), &q).await.unwrap();
    assert!(hits[0].flags.iter().any(|f| matches!(f, Flag::Upstream(_))));
}

#[tokio::test]
async fn skills_sh_search_and_dir_probe() {
    let f = FakeFetcher::new();
    f.serve(
        "https://skills.sh/api/search?q=pdf&limit=20",
        200,
        fx("skills_sh/search.json"),
    );
    let s = src("skills-sh");
    let mut q = Query::text("pdf");
    q.limit = 20;
    let hits = s.search(&cx(&f), &q).await.unwrap();
    assert_eq!(hits.len(), 3);
    f.serve(&refs_url("anthropics", "skills"), 200, refs_body(SHA, &[]));
    let raw = |p: &str| format!("https://raw.githubusercontent.com/anthropics/skills/{SHA}/{p}");
    f.serve(&raw("skills/pdf/SKILL.md"), 404, "");
    f.serve(
        &raw(".agents/skills/pdf/SKILL.md"),
        200,
        "---\nname: pdf\n---\n",
    );
    let got = s.fetch(&cx(&f), &hits[0]).await.unwrap();
    assert_eq!(got.upstream.urls, vec![raw(".agents/skills/pdf/SKILL.md")]);
    // The documented API's 401 means turn it off.
    f.serve("https://skills.sh/api/search?q=x&limit=20", 401, "");
    let mut q = Query::text("x");
    q.limit = 20;
    assert!(
        s.search(&cx(&f), &q)
            .await
            .unwrap_err()
            .to_string()
            .contains("401")
    );
}

#[tokio::test]
async fn aitmpl_lobehub_cline_well_known() {
    let f = FakeFetcher::new();
    // aitmpl
    f.serve(
        "https://www.aitmpl.com/components.json",
        200,
        fx("aitmpl/components.json"),
    );
    let a = src("aitmpl");
    let hits = a.refresh(&cx(&f), None).await.unwrap().hits;
    for t in [
        ItemType::Agent,
        ItemType::Command,
        ItemType::Hook,
        ItemType::Mcp,
        ItemType::Skill,
    ] {
        assert!(hits.iter().any(|h| h.item_type == t), "{t:?}");
    }
    let caution = hits.iter().find(|h| h.name == "agent-management").unwrap();
    assert!(
        caution
            .flags
            .iter()
            .any(|f| matches!(f, Flag::Upstream(s) if s.contains("CAUTION")))
    );
    f.serve(
        &refs_url("davila7", "claude-code-templates"),
        200,
        refs_body(SHA, &[]),
    );
    let raw = format!(
        "https://raw.githubusercontent.com/davila7/claude-code-templates/{SHA}/cli-tool/components/skills/ai-maestro/agent-management/SKILL.md"
    );
    f.serve(&raw, 200, "---\nname: agent-management\n---\n");
    let got = a.fetch(&cx(&f), caution).await.unwrap();
    assert_eq!(got.upstream.urls, vec![raw]);
    assert!(got.flags.iter().any(|f| matches!(f, Flag::Upstream(_))));
    // LobeHub
    f.serve(
        "https://chat-agents.lobehub.com/index.json",
        200,
        fx("lobehub/index.json"),
    );
    let l = src("lobehub");
    let hits = l.refresh(&cx(&f), None).await.unwrap().hits;
    assert_eq!(hits.len(), 5);
    let h = hits
        .iter()
        .find(|h| h.name == "lateral-thinking-puzzle")
        .unwrap();
    assert_eq!(h.item_type, ItemType::Agent);
    f.serve(
        "https://chat-agents.lobehub.com/lateral-thinking-puzzle.json",
        200,
        fx("lobehub/detail.json"),
    );
    let got = l.fetch(&cx(&f), h).await.unwrap();
    assert_eq!(got.primary.as_deref(), Some("lateral-thinking-puzzle.json"));
    // Cline: discovery only, README at a commit.
    f.serve(
        "https://api.cline.bot/v1/mcp/marketplace",
        200,
        fx("cline_mcp/marketplace.json"),
    );
    let c = src("cline-mcp");
    let hits = c.refresh(&cx(&f), None).await.unwrap().hits;
    let h = &hits[0];
    let Locator::GitHub { owner, repo, .. } = &h.locator else {
        panic!()
    };
    f.serve(&refs_url(owner, repo), 200, refs_body(SHA, &[]));
    f.serve(
        &format!("https://raw.githubusercontent.com/{owner}/{repo}/{SHA}/README.md"),
        200,
        "# Setup\n",
    );
    let got = c.fetch(&cx(&f), h).await.unwrap();
    assert!(got.files.contains_key("README.md"));
    assert_eq!(got.primary.as_deref(), Some(sources::cline_mcp::ENTRY_FILE));
    // Well-known: the host joins the allowlist when the person adds it.
    let d = tempfile::tempdir().unwrap();
    let ix = Index::open(&d.path().join("i.db")).await.unwrap();
    let fake = FakeFetcher::new();
    let base = "https://www.mintlify.com/docs/.well-known/skills";
    fake.serve(
        &format!("{base}/index.json"),
        200,
        fx("well_known/index.json"),
    );
    fake.serve(
        &format!("{base}/mintlify/SKILL.md"),
        200,
        "---\nname: mintlify\n---\n",
    );
    let http = Arc::new(HttpFetcher::with_transport(fake.clone(), Some(ix.cache())));
    let cfg = Config {
        well_known: vec!["https://www.mintlify.com/docs".into()],
        ..Default::default()
    };
    let w = find(&sources::all(&cfg), "well-known:www.mintlify.com/docs").unwrap();
    let c = Ctx::new(http);
    let hits = w.refresh(&c, None).await.unwrap().hits;
    assert_eq!(hits[0].name, "mintlify");
    let got = w.fetch(&c, &hits[0]).await.unwrap();
    assert_eq!(got.files.len(), 1);
}

#[tokio::test]
async fn awesome_csv_and_hermes_stream() {
    let f = FakeFetcher::new();
    f.serve(
        &refs_url("hesreallyhim", "awesome-claude-code"),
        200,
        refs_body(SHA, &[]),
    );
    f.serve(
        &format!("https://raw.githubusercontent.com/hesreallyhim/awesome-claude-code/{SHA}/THE_RESOURCES_TABLE_NEW.csv"),
        200,
        fx("awesome_csv/resources.csv"),
    );
    let s = src("awesome-claude-code");
    let r = s.refresh(&cx(&f), None).await.unwrap();
    assert_eq!(r.hits.len(), 3);
    assert!(
        r.hits
            .iter()
            .all(|h| h.tags.contains(&"discovery".to_string()))
    );
    let r = s.refresh(&cx(&f), Some(SHA)).await.unwrap();
    assert!(r.not_modified);
    // Hermes: a 2000-row index read element by element.
    let mut rows = vec![];
    for i in 0..2000 {
        let row = match i % 4 {
            0 => {
                serde_json::json!({"name": format!("o{i}"), "description": "d", "source": "official",
                "identifier": "x", "trust_level": "builtin", "repo": "NousResearch/hermes-agent",
                "path": format!("optional-skills/c/o{i}"), "tags": ["t"], "extra": {"a": [1, 2]}})
            }
            1 => {
                serde_json::json!({"name": format!("g{i}"), "source": "github", "trust_level": "trusted",
                "repo": "a/b", "path": format!("skills/g{i}")})
            }
            2 => {
                serde_json::json!({"name": format!("c{i}"), "source": "clawhub", "identifier": "c", "repo": "", "path": ""})
            }
            _ => serde_json::json!({"name": format!("s{i}"), "source": "skills.sh",
                "resolved_github_id": format!("o/r/skills/s{i}"), "repo": "o/r", "path": "x"}),
        };
        rows.push(row);
    }
    let body = serde_json::to_vec(
        &serde_json::json!({"version": 1, "generated_at": "2026-11-20T18:27:18Z",
        "skill_count": 2000, "skills": rows}),
    )
    .unwrap();
    let p = sources::hermes_index::parse(&body).unwrap();
    assert_eq!(p.rows, 2000);
    assert_eq!(p.hits.len(), 1500, "ClawHub rows are left to S3");
    assert_eq!(p.generated_at.as_deref(), Some("2026-11-20T18:27:18Z"));
    let s3 = p.hits.iter().find(|h| h.name == "s3").unwrap();
    assert!(matches!(&s3.locator, Locator::GitHub { path, .. } if path == "skills/s3"));
    f.serve(sources::hermes_index::URL, 200, body);
    let h = src("hermes-index");
    let r = h.refresh(&cx(&f), None).await.unwrap();
    assert_eq!(r.hits.len(), 1500);
    let r = h
        .refresh(&cx(&f), Some("2026-11-20T18:27:18Z"))
        .await
        .unwrap();
    assert!(r.not_modified);
}

#[tokio::test]
async fn package_facts() {
    let f = FakeFetcher::new();
    f.serve(
        "https://registry.npmjs.org/@modelcontextprotocol%2Fserver-github",
        200,
        fx("pkg_facts/npm.json"),
    );
    f.serve(
        "https://pypi.org/pypi/mcp-server-git/json",
        200,
        fx("pkg_facts/pypi.json"),
    );
    let n = sources::pkg_facts::npm(&cx(&f), "@modelcontextprotocol/server-github")
        .await
        .unwrap();
    assert_eq!(
        (n.licence.as_deref(), n.latest.as_deref()),
        (Some("MIT"), Some("2025.4.8"))
    );
    let p = sources::pkg_facts::pypi(&cx(&f), "mcp-server-git")
        .await
        .unwrap();
    assert_eq!(p.licence.as_deref(), Some("MIT"));
    assert!(sources::pkg_facts::npm(&cx(&f), "../x").await.is_err());
}

struct FakeGh(Vec<String>);

#[async_trait::async_trait]
impl GhApi for FakeGh {
    async fn get_json(&self, path: &str, _class: Class) -> anyhow::Result<serde_json::Value> {
        if path.starts_with("search/code") {
            assert!(path.contains("filename%3ASKILL.md"), "{path}");
            return Ok(serde_json::json!({"items": [
                {"name": "SKILL.md", "path": ".claude/skills/deploy/SKILL.md", "repository": {"full_name": "acme/tools"}}
            ]}));
        }
        if path.contains("/git/trees/") {
            return Ok(
                serde_json::json!({"tree": self.0.iter().map(|p| serde_json::json!({"path": p, "type": "blob"})).collect::<Vec<_>>()}),
            );
        }
        if path.ends_with("/license") {
            return Ok(serde_json::json!({"license": {"spdx_id": "MIT"}}));
        }
        anyhow::bail!("unexpected {path}")
    }
}

#[tokio::test]
async fn gh_code_search_trees_and_licence() {
    let f = FakeFetcher::new();
    let s = src("gh-code");
    assert!(s.needs_gh());
    // Without gh: nothing, no requests.
    assert!(
        s.search(&cx(&f), &Query::text("deploy"))
            .await
            .unwrap()
            .is_empty()
    );
    let mut c = cx(&f);
    c.gh = Some(Arc::new(FakeGh(vec![
        "optional-skills/a/x/SKILL.md".into(),
        "optional-skills/a/x/ref.md".into(),
        "other/y.md".into(),
    ])));
    let hits = s.search(&c, &Query::text("deploy")).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "deploy");
    assert!(
        matches!(&hits[0].locator, Locator::GitHub { path, .. } if path == ".claude/skills/deploy")
    );
    let t = sources::gh_code::tree(&c, "o", "r", SHA, "optional-skills/a/x")
        .await
        .unwrap();
    assert_eq!(t.len(), 2);
    assert_eq!(
        sources::gh_code::licence(&c, "o", "r")
            .await
            .unwrap()
            .as_deref(),
        Some("MIT")
    );
    // Huge repo with gh: the dir listing drives raw fetches.
    let h = src("hermes-index");
    let mut hit = Hit {
        source: "hermes-index".into(),
        locator: Locator::GitHub {
            owner: "NousResearch".into(),
            repo: "hermes-agent".into(),
            git_ref: Some(SHA.into()),
            path: "optional-skills/a/x".into(),
        },
        item_type: ItemType::Skill,
        ecosystem: kls_libsearch::Ecosystem::Hermes,
        name: "x".into(),
        title: "x".into(),
        description: String::new(),
        tags: vec![],
        licence: Default::default(),
        trust: kls_libsearch::Trust::Official,
        popularity: None,
        updated_at: None,
        flags: vec![],
    };
    let raw =
        |p: &str| format!("https://raw.githubusercontent.com/NousResearch/hermes-agent/{SHA}/{p}");
    f.serve(
        &raw("optional-skills/a/x/SKILL.md"),
        200,
        "---\nname: x\n---\n",
    );
    f.serve(&raw("optional-skills/a/x/ref.md"), 200, "ref");
    let got = h.fetch(&c, &hit).await.unwrap();
    assert_eq!(got.files.len(), 2);
    hit.name = "y".into();
    let _ = hit;
    let _ = c.http.budgets();
}

#[tokio::test]
async fn latest_is_cheap() {
    let f = FakeFetcher::new();
    f.serve(&refs_url("anthropics", "skills"), 200, refs_body(SHA2, &[]));
    let s = src("gh:anthropics/skills");
    let loc = Locator::GitHub {
        owner: "anthropics".into(),
        repo: "skills".into(),
        git_ref: Some(SHA.into()),
        path: "skills/pdf".into(),
    };
    let u = s.latest(&cx(&f), &loc).await.unwrap().unwrap();
    assert_eq!(u.commit.as_deref(), Some(SHA2));
    assert_eq!(f.requests().len(), 1);
    let m = src("mcp-registry");
    f.serve(
        &format!("{MCP}/servers/io.github.x%2Fy/versions/latest"),
        200,
        r#"{"server":{"name":"io.github.x/y","version":"2.0.0"}}"#,
    );
    let u = m
        .latest(
            &cx(&f),
            &Locator::Mcp {
                name: "io.github.x/y".into(),
                version: "1.0.0".into(),
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(u.version.as_deref(), Some("2.0.0"));
}

#[tokio::test]
async fn oversized_ref_advertisement_falls_back_to_one_rest_call() {
    let f = FakeFetcher::new();
    f.serve(
        &refs_url("NousResearch", "hermes-agent"),
        200,
        vec![b'0'; 9 * 1024 * 1024],
    );
    f.serve(
        "https://api.github.com/repos/NousResearch/hermes-agent/commits/HEAD",
        200,
        SHA,
    );
    let sha = sources::github_repo::resolve(&cx(&f), "NousResearch", "hermes-agent", None)
        .await
        .unwrap();
    assert_eq!(sha, SHA);
    assert_eq!(f.requests().len(), 3, "refs, feed (missing), REST");
    // The free commits feed answers first when it is there.
    f.serve(
        "https://github.com/NousResearch/hermes-agent/commits/HEAD.atom",
        200,
        format!("<feed><entry><id>tag:github.com,2008:Grit::Commit/{SHA2}</id></entry></feed>"),
    );
    f.clear_log();
    let sha = sources::github_repo::resolve(&cx(&f), "NousResearch", "hermes-agent", None)
        .await
        .unwrap();
    assert_eq!(sha, SHA2);
    assert_eq!(f.requests().len(), 2);
}
