//! Rules 1-4 for library search (WORKSTREAMS rule 5), against a fake
//! source over a `FakeFetcher`: no network.

use super::*;
use kls_libsearch::fetch::FakeFetcher;
use kls_libsearch::{Licence, Mode, Refresh, Trust, Upstream};
use kls_store::audit::LedgerQuery;

const SHA: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
const SHA2: &str = "b7e0000000000000000000000000000000000001";

/// One fake item: its type, ecosystem and files (fetched from raw at the
/// current commit).
#[derive(Clone)]
struct Item {
    name: &'static str,
    item_type: ItemType,
    eco: Ecosystem,
    primary: &'static str,
    files: Vec<&'static str>,
}

struct FakeSource {
    items: Vec<Item>,
    commit: Mutex<String>,
}

impl FakeSource {
    fn hit(&self, it: &Item) -> Hit {
        Hit {
            source: "fake".into(),
            locator: Locator::GitHub {
                owner: "o".into(),
                repo: "r".into(),
                git_ref: None,
                path: it.name.into(),
            },
            item_type: it.item_type,
            ecosystem: it.eco,
            name: it.name.into(),
            title: it.name.into(),
            description: format!("{} for tests", it.name),
            tags: vec![],
            licence: Licence::from_id("MIT", "index"),
            trust: Trust::Trusted,
            popularity: Some(5),
            updated_at: None,
            flags: vec![],
        }
    }
}

#[async_trait::async_trait]
impl Source for FakeSource {
    fn id(&self) -> &str {
        "fake"
    }
    fn label(&self) -> &str {
        "Fake"
    }
    fn types(&self) -> &[ItemType] {
        &ItemType::ALL
    }
    fn ecosystems(&self) -> &[Ecosystem] {
        &[Ecosystem::ClaudeCode]
    }
    fn mode(&self) -> Mode {
        Mode::Both
    }
    async fn refresh(&self, cx: &Ctx, _cursor: Option<&str>) -> Result<Refresh> {
        // One request, so the kill-switch test sees it (or not).
        cx.get("https://geminicli.com/extensions.json", fetch::FILE_MAX)
            .await?;
        Ok(Refresh {
            hits: self.items.iter().map(|i| self.hit(i)).collect(),
            ..Default::default()
        })
    }
    async fn search(&self, cx: &Ctx, q: &Query) -> Result<Vec<Hit>> {
        cx.get("https://clawhub.ai/api/v1/search?q=x", fetch::FILE_MAX)
            .await?;
        Ok(self
            .items
            .iter()
            .filter(|i| i.name.contains(q.text.trim()))
            .map(|i| self.hit(i))
            .collect())
    }
    async fn fetch(&self, cx: &Ctx, hit: &Hit) -> Result<Fetched> {
        let it = self
            .items
            .iter()
            .find(|i| {
                i.name == hit.name
                    || matches!(&hit.locator, Locator::GitHub { path, .. } if path == i.name)
            })
            .ok_or_else(|| anyhow!("no item"))?;
        let sha = self.commit.lock().unwrap().clone();
        let mut f = Fetched::default();
        for p in &it.files {
            let (url, body) = cx.raw("o", "r", &sha, &format!("{}/{p}", it.name)).await?;
            f.files.insert(p.to_string(), body);
            f.file_urls.insert(p.to_string(), url.clone());
            f.upstream.urls.push(url);
        }
        f.upstream.commit = Some(sha);
        f.upstream.sha256 = f.files_sha256();
        f.upstream.fetched_at = cx.now_ms;
        f.primary = Some(it.primary.into());
        f.name_hint = it.name.into();
        Ok(f)
    }
    async fn latest(&self, _cx: &Ctx, _loc: &Locator) -> Result<Option<Upstream>> {
        Ok(Some(Upstream {
            commit: Some(self.commit.lock().unwrap().clone()),
            ..Default::default()
        }))
    }
}

const SKILL: &str = "---\nname: pdf-tools\ndescription: Work with PDF files.\nlicense: MIT\n---\n\n# PDF\n\nRead and split PDFs.\n";
const HOOKS: &str = r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/guard.sh","timeout":10}]}]}}"#;
const GUARD: &str = "#!/bin/sh\necho checked\n";
const AGENT: &str = "---\nname: reviewer\ndescription: Reviews pull requests.\ntools: Read, Grep, Bash\nmodel: sonnet\na2a: true\npeers: [other]\nlicense: MIT\n---\n\nYou review code carefully.\n";

fn serve_all(fake: &FakeFetcher, sha: &str) {
    fake.serve("https://geminicli.com/extensions.json", 200, "[]");
    fake.serve("https://clawhub.ai/api/v1/search?q=x", 200, "{}");
    let raw = |p: &str| format!("https://raw.githubusercontent.com/o/r/{sha}/{p}");
    fake.serve(&raw("pdf-tools/SKILL.md"), 200, SKILL);
    fake.serve(&raw("guard/hooks/hooks.json"), 200, HOOKS);
    fake.serve(&raw("guard/guard.sh"), 200, GUARD);
    fake.serve(&raw("reviewer/reviewer.md"), 200, AGENT);
}

async fn setup() -> (Core, Arc<FakeFetcher>, Arc<FakeSource>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    let fake = FakeFetcher::new();
    serve_all(&fake, SHA);
    install(&core, fake.clone()).await.unwrap();
    let src = Arc::new(FakeSource {
        items: vec![
            Item {
                name: "pdf-tools",
                item_type: ItemType::Skill,
                eco: Ecosystem::ClaudeCode,
                primary: "SKILL.md",
                files: vec!["SKILL.md"],
            },
            Item {
                name: "guard",
                item_type: ItemType::Hook,
                eco: Ecosystem::ClaudeCode,
                primary: "hooks/hooks.json",
                files: vec!["hooks/hooks.json", "guard.sh"],
            },
            Item {
                name: "reviewer",
                item_type: ItemType::Agent,
                eco: Ecosystem::ClaudeCode,
                primary: "reviewer.md",
                files: vec!["reviewer.md"],
            },
        ],
        commit: Mutex::new(SHA.into()),
    });
    add_source(&core, src.clone()).await.unwrap();
    (core, fake, src, dir)
}

fn full_ack(p: &Preview) -> Ack {
    Ack {
        flags: p.converted.review_flags().into_iter().cloned().collect(),
        licence: true,
        rename: None,
    }
}

async fn ledger_kinds(core: &Core) -> Vec<kls_store::audit::LedgerEntry> {
    core.audit_entries(&LedgerQuery::default()).await.unwrap()
}

async fn hit_named(core: &Core, name: &str) -> Hit {
    search(core, &Query::text(name))
        .await
        .unwrap()
        .hits
        .into_iter()
        .find(|h| h.name == name)
        .unwrap()
}

#[tokio::test]
async fn import_writes_item_provenance_and_ledger_entry() {
    let (core, _fake, _src, _d) = setup().await;
    let r = refresh(&core, None).await.unwrap();
    assert!(r.refreshed >= 1, "{r:?}");
    let hit = hit_named(&core, "pdf-tools").await;
    let p = preview(&core, &hit).await.unwrap();
    assert_eq!(p.converted.name, "pdf-tools");
    let (id, v) = import(&core, &p, &full_ack(&p)).await.unwrap();
    assert_eq!(v, 1);
    let prov = core
        .store()
        .latest_provenance("skill", &id)
        .await
        .unwrap()
        .unwrap();
    assert!(prov.source_urls_json.contains(&format!(
        "https://raw.githubusercontent.com/o/r/{SHA}/pdf-tools/SKILL.md"
    )));
    assert_eq!(prov.upstream_sha256, p.fetched.files_sha256());
    assert_eq!(prov.doc_sha256, p.converted.sha256);
    assert_eq!(prov.upstream_commit, SHA);
    assert_eq!(prov.actor, "user");
    assert!(prov.files_json.contains("SKILL.md"));
    let entries = ledger_kinds(&core).await;
    let e = entries
        .iter()
        .find(|e| e.kind == "library.imported")
        .unwrap();
    assert_eq!(e.seq, prov.audit_seq);
    assert_eq!(e.payload["outcome"], "created");
    assert!(core.audit_verify().await.unwrap().factory().unwrap().ok);
    let item = core.store().library_item(&id).await.unwrap().unwrap();
    assert_eq!(item.kind, "skill");
    let ver = core.store().library_latest(&id).await.unwrap().unwrap();
    assert!(
        ver.note
            .starts_with("imported from https://raw.githubusercontent.com/")
    );
    // The origin chip and the event.
    assert!(origin(&core, "skill", &id).await.unwrap().is_some());
    let evs = core.store().recent_events(50).await.unwrap();
    assert!(evs.iter().any(|e| e.kind == kinds::LIBRARY_IMPORTED_FROM));
    // A second import of the same bytes is `unchanged`.
    let p2 = preview(&core, &hit).await.unwrap();
    assert_eq!(p2.existing.as_ref().map(|x| x.1), Some(1));
    import(&core, &p2, &full_ack(&p2)).await.unwrap();
}

#[tokio::test]
async fn each_refusal_is_audited() {
    let (core, fake, _src, _d) = setup().await;
    refresh(&core, None).await.unwrap();
    // Hooks always need review: an empty ack is refused.
    let hit = hit_named(&core, "guard").await;
    let p = preview(&core, &hit).await.unwrap();
    assert!(!p.converted.review_flags().is_empty());
    let e = import(&core, &p, &Ack::default()).await.unwrap_err();
    assert!(e.to_string().contains("review"), "{e}");
    // A refused flag never imports, whatever the ack.
    let mut refused = p.clone();
    refused
        .converted
        .flags
        .push(Flag::Refused("marketplace source runs a program".into()));
    assert!(import(&core, &refused, &full_ack(&refused)).await.is_err());
    // Licence not acknowledged (unknown licence on a skill).
    let skill = hit_named(&core, "pdf-tools").await;
    let mut ps = preview(&core, &skill).await.unwrap();
    ps.converted.licence = Licence::unknown();
    let mut ack = full_ack(&ps);
    ack.licence = false;
    assert!(import(&core, &ps, &ack).await.is_err());
    // Upstream changed between preview and import.
    let ps = preview(&core, &skill).await.unwrap();
    fake.serve(
        &format!("https://raw.githubusercontent.com/o/r/{SHA}/pdf-tools/SKILL.md"),
        200,
        SKILL.replace("split", "merge"),
    );
    let e = import(&core, &ps, &full_ack(&ps)).await.unwrap_err();
    assert!(e.to_string().contains("changed since the preview"), "{e}");
    let outcomes: Vec<String> = ledger_kinds(&core)
        .await
        .into_iter()
        .filter(|e| e.kind == "library.refused")
        .map(|e| {
            e.payload["outcome"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert!(outcomes.iter().any(|o| o == "review"));
    assert!(outcomes.iter().any(|o| o.starts_with("refused:")));
    assert!(outcomes.iter().any(|o| o == "hash_mismatch"));
    let evs = core.store().recent_events(100).await.unwrap();
    assert!(
        evs.iter()
            .filter(|e| e.kind == kinds::LIBRARY_REFUSED)
            .count()
            >= 4
    );
}

#[tokio::test]
async fn kill_switch_refuses_everything_and_sends_nothing() {
    let (core, fake, _src, _d) = setup().await;
    refresh(&core, None).await.unwrap();
    let hit = hit_named(&core, "pdf-tools").await;
    let p = preview(&core, &hit).await.unwrap();
    let (id, _) = import(&core, &p, &full_ack(&p)).await.unwrap();
    core.set_factory_stopped(true, "incident").await.unwrap();
    fake.clear_log();

    assert!(refresh(&core, None).await.is_err());
    // Search still answers from the local index, marked stopped.
    let r = search(&core, &Query::text("pdf")).await.unwrap();
    assert!(r.stopped);
    assert_eq!(r.hits.len(), 1);
    assert!(preview(&core, &hit).await.is_err());
    assert!(import(&core, &p, &full_ack(&p)).await.is_err());
    assert!(check_updates(&core).await.is_err());
    assert!(update_preview(&core, "skill", &id).await.is_err());
    assert!(node_convert(&core, &p, "hermes").await.is_err());
    assert_eq!(
        refresh_stale(&core).await.unwrap(),
        RefreshReport::default()
    );
    assert!(fake.requests().is_empty(), "{:?}", fake.requests());

    let refused: Vec<_> = ledger_kinds(&core)
        .await
        .into_iter()
        .filter(|e| e.kind == "library.refused" && e.payload["outcome"] == "kill_switch")
        .map(|e| e.payload["action"].as_str().unwrap_or_default().to_string())
        .collect();
    for a in [
        "refresh",
        "remote_search",
        "preview",
        "import",
        "check_updates",
        "update",
        "node_convert",
    ] {
        assert!(
            refused.iter().any(|x| x == a),
            "{a} not audited: {refused:?}"
        );
    }
    // Lifting the switch resumes nothing by itself; the person presses again.
    core.set_factory_stopped(false, "").await.unwrap();
    assert!(fake.requests().is_empty());
    refresh(&core, None).await.unwrap();
}

#[tokio::test]
async fn agent_and_run_actors_are_refused() {
    let (core, _fake, _src, _d) = setup().await;
    let loc = Locator::Url {
        url: "https://x/y".into(),
    };
    for actor in [
        Actor::Agent("reviewer".into()),
        Actor::Run("r1".into()),
        Actor::Node("n1".into()),
    ] {
        let e = guard_as(&core, &actor, Action::Import(&loc))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("people only"));
    }
    assert!(
        guard_as(&core, &Actor::System, Action::Preview(&loc))
            .await
            .is_err()
    );
    guard_as(&core, &Actor::System, Action::Refresh("all"))
        .await
        .unwrap();
    let n = ledger_kinds(&core)
        .await
        .into_iter()
        .filter(|e| e.kind == "library.refused" && e.payload["outcome"] == "refused:agent_actor")
        .count();
    assert_eq!(n, 4);
}

#[tokio::test]
async fn hook_import_stores_bundle_and_installs_nothing() {
    let (core, _fake, _src, _d) = setup().await;
    refresh(&core, None).await.unwrap();
    let hit = hit_named(&core, "guard").await;
    let p = preview(&core, &hit).await.unwrap();
    assert!(p.converted.flags.contains(&Flag::RunsCode));
    let (id, v) = import(&core, &p, &full_ack(&p)).await.unwrap();
    let files = core.store().bundle_files(&id, v).await.unwrap();
    assert!(files.iter().any(|f| f.path == "guard.sh"), "{files:?}");
    let blob = core
        .store()
        .library_blob(&files.iter().find(|f| f.path == "guard.sh").unwrap().sha256)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(blob, GUARD.as_bytes());
    // Nothing dispatched or sent to a node: no gateway, no job events.
    assert!(core.gateway().is_err());
    let evs = core.store().recent_events(200).await.unwrap();
    assert!(
        !evs.iter()
            .any(|e| e.kind.starts_with("dispatch.") || e.kind.starts_with("job."))
    );
}

#[tokio::test]
async fn imported_agents_carry_no_a2a_settings() {
    let (core, _fake, _src, _d) = setup().await;
    refresh(&core, None).await.unwrap();
    let hit = hit_named(&core, "reviewer").await;
    let p = preview(&core, &hit).await.unwrap();
    let (id, _) = import(&core, &p, &full_ack(&p)).await.unwrap();
    let a = core.store().agent_profile(&id).await.unwrap().unwrap();
    for k in A2A_KEYS {
        assert!(
            !a.meta_json.contains(&format!("\"{k}\"")),
            "{k} in {}",
            a.meta_json
        );
    }
    assert!(!a.prompt.contains("peers"));
    assert!(
        p.converted
            .notes
            .iter()
            .any(|n| n.field == "a2a" || n.field == "peers")
    );
    let (m, dropped) = strip_a2a(r#"{"a2a":true,"peers":["x"],"claims":["a"]}"#);
    assert_eq!(dropped, vec!["a2a", "peers"]);
    assert_eq!(m, r#"{"claims":["a"]}"#);
}

#[tokio::test]
async fn update_check_flips_behind_and_update_saves_a_new_version() {
    let (core, fake, src, _d) = setup().await;
    refresh(&core, None).await.unwrap();
    let hit = hit_named(&core, "pdf-tools").await;
    let p = preview(&core, &hit).await.unwrap();
    let (id, _) = import(&core, &p, &full_ack(&p)).await.unwrap();
    assert_eq!(check_updates(&core).await.unwrap(), 0);
    // Upstream moves.
    *src.commit.lock().unwrap() = SHA2.into();
    serve_all(&fake, SHA2);
    fake.serve(
        &format!("https://raw.githubusercontent.com/o/r/{SHA2}/pdf-tools/SKILL.md"),
        200,
        SKILL.replace("split", "merge"),
    );
    assert_eq!(check_updates(&core).await.unwrap(), 1);
    let u = core
        .store()
        .update_state("skill", &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(u.state, "behind");
    assert_eq!(u.latest_commit, SHA2);
    let evs = core.store().recent_events(100).await.unwrap();
    assert!(evs.iter().any(|e| e.kind == kinds::LIBRARY_UPDATE));
    assert!(
        ledger_kinds(&core)
            .await
            .iter()
            .any(|e| e.kind == "library.update_checked" && e.payload["outcome"] == "behind")
    );
    // The update: diff base is v1's body; the save is v2 "updated from".
    let up = update_preview(&core, "skill", &id).await.unwrap();
    assert!(up.diff_base().unwrap().contains("split"));
    assert!(up.converted.doc_text().contains("merge"));
    let (id2, v2) = import(&core, &up, &full_ack(&up)).await.unwrap();
    assert_eq!((id2.as_str(), v2), (id.as_str(), 2));
    let ver = core.store().library_latest(&id).await.unwrap().unwrap();
    assert!(ver.note.starts_with("updated from a1b2c3d"), "{}", ver.note);
    assert!(
        ledger_kinds(&core)
            .await
            .iter()
            .any(|e| e.kind == "library.updated")
    );
    let prov = core
        .store()
        .latest_provenance("skill", &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prov.version, 2);
    assert_eq!(prov.upstream_commit, SHA2);
    assert_eq!(
        core.store()
            .update_state("skill", &id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "current"
    );
}

#[tokio::test]
async fn source_changes_are_audited() {
    let (core, _fake, _src, _d) = setup().await;
    let id = add_tap(
        &core,
        "o/r@main",
        &["agents/*.md".into()],
        Some(ItemType::Agent),
        None,
    )
    .await
    .unwrap();
    assert_eq!(id, "gh:o/r");
    set_source(&core, "hermes-index", true).await.unwrap();
    set_setting(&core, "gh", true).await.unwrap();
    let cfg = config(&core).await.unwrap();
    assert!(cfg.gh && cfg.on.contains("hermes-index"));
    assert_eq!(cfg.taps.len(), 1);
    assert!(remove_source(&core, "gh:o/r").await.unwrap());
    let n = ledger_kinds(&core)
        .await
        .iter()
        .filter(|e| e.kind == "libsearch.source_changed")
        .count();
    assert_eq!(n, 4);
}

/// Rule 2: an edit that no longer validates, and a rename that is not a
/// library name, are refused as `invalid:<detail>` and chained.
#[tokio::test]
async fn invalid_refusals_are_audited() {
    let (core, _fake, _src, _d) = setup().await;
    refresh(&core, None).await.unwrap();
    let skill = hit_named(&core, "pdf-tools").await;
    let p = preview(&core, &skill).await.unwrap();

    let mut edited = p.clone();
    edited.converted.doc = b"no front matter and no name".to_vec();
    let e = import(&core, &edited, &full_ack(&edited))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("invalid:"), "{e}");

    let mut ack = full_ack(&p);
    ack.rename = Some("../not a name".into());
    let e = import(&core, &p, &ack).await.unwrap_err();
    assert!(e.to_string().contains("invalid:name"), "{e}");

    let outcomes: Vec<String> = ledger_kinds(&core)
        .await
        .into_iter()
        .filter(|e| e.kind == "library.refused")
        .map(|e| e.payload["outcome"].as_str().unwrap_or_default().into())
        .collect();
    assert!(
        outcomes
            .iter()
            .any(|o| o.starts_with("invalid:") && o != "invalid:name"),
        "{outcomes:?}"
    );
    assert!(outcomes.iter().any(|o| o == "invalid:name"), "{outcomes:?}");
    assert!(core.audit_verify().await.unwrap().ok);
}

/// While stopped, remote search is refused and chained once per stop
/// window, however many queries are typed; a new window chains again.
#[tokio::test]
async fn stopped_search_refusal_once_per_window() {
    let (core, fake, _src, _d) = setup().await;
    refresh(&core, None).await.unwrap();
    let count = |core: Core| async move {
        ledger_kinds(&core)
            .await
            .into_iter()
            .filter(|e| e.kind == "library.refused" && e.payload["action"] == "remote_search")
            .count()
    };
    core.set_factory_stopped(true, "incident").await.unwrap();
    fake.clear_log();
    for q in ["pdf", "pdf-tools", "guard", "anything"] {
        assert!(search(&core, &Query::text(q)).await.unwrap().stopped);
    }
    assert_eq!(count(core.clone()).await, 1);
    assert!(fake.requests().is_empty());

    core.set_factory_stopped(false, "").await.unwrap();
    // stopped_since has millisecond resolution.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    core.set_factory_stopped(true, "again").await.unwrap();
    for q in ["pdf", "guard"] {
        assert!(search(&core, &Query::text(q)).await.unwrap().stopped);
    }
    assert_eq!(count(core.clone()).await, 2, "one more for the new window");
    assert!(core.audit_verify().await.unwrap().ok);
}

/// A belai node with the `library` job (WP9): records the payload and
/// answers with `reply`.
struct LibraryNode {
    payloads: Mutex<Vec<String>>,
    reply: Mutex<String>,
}

#[async_trait::async_trait]
impl crate::NodeGateway for LibraryNode {
    async fn online(&self) -> Vec<String> {
        vec!["n-lib".into(), "n-old".into()]
    }
    async fn dispatch(&self, _: &str, _: crate::DispatchSpec) -> Result<String> {
        bail!("no dispatches here")
    }
    async fn job(
        &self,
        node: Option<&str>,
        kind: &str,
        payload: &str,
        _: crate::JobNeeds,
        _: i64,
    ) -> Result<crate::JobOutcome> {
        assert_eq!(kind, LIBRARY_JOB_KIND);
        assert_eq!(node, Some("n-lib"), "only the node advertising the job");
        self.payloads.lock().unwrap().push(payload.into());
        Ok(crate::JobOutcome {
            status: "ok".into(),
            output_json: self.reply.lock().unwrap().clone(),
            node_id: "n-lib".into(),
            ..Default::default()
        })
    }
}

async fn library_nodes(core: &Core, reply: String) -> Arc<LibraryNode> {
    let mut host = kls_store::HostInfo::default();
    host.capabilities.job_kinds = vec![LIBRARY_JOB_KIND.into(), "github".into()];
    core.store()
        .insert_node("n-lib", "zz-lib", &host, "s1", "t1")
        .await
        .unwrap();
    let mut old = kls_store::HostInfo::default();
    old.capabilities.job_kinds = vec!["github".into()];
    core.store()
        .insert_node("n-old", "aa-old", &old, "s2", "t2")
        .await
        .unwrap();
    let gw = Arc::new(LibraryNode {
        payloads: Mutex::new(vec![]),
        reply: Mutex::new(reply),
    });
    core.set_gateway(gw.clone());
    gw
}

#[tokio::test]
async fn node_convert_runs_on_a_library_node_and_records_provenance() {
    use base64::Engine as _;
    let (core, _fake, _src, _d) = setup().await;
    refresh(&core, None).await.unwrap();
    let hit = hit_named(&core, "reviewer").await;
    let p = preview(&core, &hit).await.unwrap();
    // belai's answer: the same profile with its own body, and its notes.
    let doc = p.converted.doc_text().replace(
        "You review code carefully.",
        "You review code carefully, as belai converted it.",
    );
    let reply = json!({
        "doc": doc, "sha256": "x", "name": "reviewer", "format": "hermes",
        "notes": [{"kind": "dropped", "field": "memories", "text": "memories are not imported"}],
        "skills": [{"name": "notes", "doc": "---\nname: notes\ndescription: Take notes.\n---\n\nWrite short notes.\n", "sha256": "y"}],
    });
    let gw = library_nodes(&core, reply.to_string()).await;

    // The screens offer a node conversion only where one is needed.
    let mut hermes = p.clone();
    hermes.hit.ecosystem = kls_libsearch::Ecosystem::Hermes;
    assert_eq!(node_format(&hermes), Some("hermes"));
    let mut claws = p.clone();
    claws.hit.ecosystem = kls_libsearch::Ecosystem::OpenClaw;
    assert_eq!(node_format(&claws), Some("claws"));
    let mut plain = p.clone();
    plain.hit.ecosystem = kls_libsearch::Ecosystem::ClaudeCode;
    assert_eq!(node_format(&plain), None);
    let np = node_convert(&core, &p, "hermes").await.unwrap();
    assert_eq!(node_format(&np), None, "already converted on a node");
    let sent: Value = serde_json::from_str(&gw.payloads.lock().unwrap()[0]).unwrap();
    assert_eq!(sent["task"], "convert");
    assert_eq!(sent["format"], "hermes");
    let body = base64::engine::general_purpose::STANDARD
        .decode(sent["files"]["reviewer.md"].as_str().unwrap())
        .unwrap();
    assert_eq!(body, AGENT.as_bytes());
    assert!(np.converted.doc_text().contains("as belai converted it"));
    assert!(np.converted.notes.iter().any(|n| n.field == "memories"));
    assert_eq!(np.converted.extra_skills.len(), 1);

    let conv = ledger_kinds(&core)
        .await
        .into_iter()
        .find(|e| e.kind == "library.converted")
        .expect("library.converted chained");
    assert_eq!(conv.origin, "host:n-lib");
    assert_eq!(conv.actor, "node:n-lib");

    let (id, version) = import(&core, &np, &full_ack(&np)).await.unwrap();
    let prov = core
        .store()
        .latest_provenance("agent", &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prov.version, version);
    assert_eq!(prov.converted_by, "node:n-lib");
    assert!(core.audit_verify().await.unwrap().ok);
}

#[tokio::test]
async fn node_convert_refuses_without_a_library_node_or_for_a_non_agent() {
    let (core, _fake, _src, _d) = setup().await;
    refresh(&core, None).await.unwrap();
    let agent = preview(&core, &hit_named(&core, "reviewer").await)
        .await
        .unwrap();
    let skill = preview(&core, &hit_named(&core, "pdf-tools").await)
        .await
        .unwrap();
    let gw = library_nodes(&core, "{}".into()).await;
    assert!(node_convert(&core, &skill, "hermes").await.is_err());
    assert!(node_convert(&core, &agent, "claude-code").await.is_err());
    // No node advertises the job: refused before anything is sent.
    core.store().revoke_node("n-lib", "test").await.unwrap();
    assert!(node_convert(&core, &agent, "hermes").await.is_err());
    assert!(gw.payloads.lock().unwrap().is_empty());
    let outcomes: Vec<String> = ledger_kinds(&core)
        .await
        .into_iter()
        .filter(|e| e.kind == "library.refused" && e.payload["action"] == "node_convert")
        .map(|e| {
            e.payload["outcome"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert!(outcomes.iter().any(|o| o == "invalid:kind"), "{outcomes:?}");
    assert!(
        outcomes.iter().any(|o| o == "invalid:format"),
        "{outcomes:?}"
    );
    assert!(outcomes.iter().any(|o| o == "no_node"), "{outcomes:?}");
}

/// A node with a signed-in `gh` answering `github` jobs.
struct GhNode {
    jobs: Mutex<Vec<(String, String, bool)>>,
}

#[async_trait::async_trait]
impl crate::NodeGateway for GhNode {
    async fn online(&self) -> Vec<String> {
        vec!["n-gh".into()]
    }
    async fn dispatch(&self, _: &str, _: crate::DispatchSpec) -> Result<String> {
        bail!("no dispatches here")
    }
    async fn job(
        &self,
        node: Option<&str>,
        kind: &str,
        payload: &str,
        needs: crate::JobNeeds,
        _: i64,
    ) -> Result<crate::JobOutcome> {
        assert!(node.is_none(), "routed by capability");
        self.jobs
            .lock()
            .unwrap()
            .push((kind.into(), payload.into(), needs.gh));
        let missing = payload.contains("missing.md");
        let out = if missing {
            json!({"stdout": "", "stderr": "gh: Not Found (HTTP 404)", "exit": 1})
        } else {
            json!({"stdout": "---\nname: x\n---\nVia node.\n", "stderr": "", "exit": 0})
        };
        Ok(crate::JobOutcome {
            status: "ok".into(),
            output_json: out.to_string(),
            node_id: "n-gh".into(),
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn node_gh_fallback_is_off_by_default_audited_and_held_by_the_switch() {
    let (core, _fake, _src, _d) = setup().await;
    let gw = Arc::new(GhNode {
        jobs: Mutex::new(vec![]),
    });
    core.set_gateway(gw.clone());
    let e = engine(&core).await.unwrap();
    // Off by default: no node route, the offline error stands, no job.
    assert!(!status(&core).await.unwrap().node_gh);
    let url = "https://raw.githubusercontent.com/acme/x/main/SKILL.md";
    let cx = ctx(&core, &e, false).await;
    assert!(cx.node_gh.is_none());
    assert!(cx.get(url, fetch::FILE_MAX).await.is_err());
    assert!(gw.jobs.lock().unwrap().is_empty());
    // On: the fetch goes to the node's gh, routed by capability.
    set_setting(&core, "node_gh", true).await.unwrap();
    assert!(status(&core).await.unwrap().node_gh);
    let cx = ctx(&core, &e, false).await;
    let r = cx.get(url, fetch::FILE_MAX).await.unwrap();
    assert!(r.text().unwrap().contains("Via node."));
    let missing = cx
        .get_req(
            "https://raw.githubusercontent.com/acme/x/main/missing.md",
            "*/*",
            fetch::FILE_MAX,
        )
        .await
        .unwrap();
    assert_eq!(missing.status, 404);
    {
        let jobs = gw.jobs.lock().unwrap();
        assert_eq!(jobs.len(), 2);
        let (kind, payload, needs_gh) = &jobs[0];
        assert_eq!(kind, NODE_GH_JOB_KIND);
        assert!(*needs_gh);
        let p: Value = serde_json::from_str(payload).unwrap();
        assert_eq!(p["args"][0], "api");
        assert_eq!(
            p["args"].as_array().unwrap().last().unwrap(),
            "repos/acme/x/contents/SKILL.md?ref=main"
        );
    }
    // Audited, with the node as the actor and origin.
    let fetched: Vec<_> = ledger_kinds(&core)
        .await
        .into_iter()
        .filter(|e| e.kind == "libsearch.node_fetch")
        .collect();
    assert_eq!(fetched.len(), 2, "{fetched:?}");
    assert!(fetched.iter().all(|e| e.actor == "node:n-gh"));
    assert!(fetched.iter().any(|e| e.payload["outcome"] == "not_found"));
    // The kill switch holds it: no job goes out.
    control::pause(&core, "drill").await.unwrap();
    assert!(cx.get(url, fetch::FILE_MAX).await.is_err());
    assert_eq!(gw.jobs.lock().unwrap().len(), 2);
}
