use super::*;
use crate::audit::config::keys;
use kls_store::audit::LedgerQuery;

async fn core() -> (Core, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (Core::open(dir.path(), None).await.unwrap(), dir)
}

fn binding(sync: Direction) -> Binding {
    Binding {
        provider: "linear".into(),
        board: "ENG".into(),
        sync,
        lists: parse_lists(&[
            "triage=Triage".into(),
            "todo=Todo".into(),
            "doing=In Progress".into(),
            "done=Done".into(),
        ]),
    }
}

/// An outside board in memory.
#[derive(Default)]
struct FakeRemote {
    items: Mutex<Vec<Item>>,
    calls: Mutex<Vec<String>>,
    fail_updates: Mutex<bool>,
}

impl FakeRemote {
    fn add(&self, id: &str, title: &str, column: &str) {
        self.items.lock().unwrap().push(Item {
            id: id.into(),
            url: format!("https://board.example/{id}"),
            title: title.into(),
            body: String::new(),
            column: column.into(),
            on_board: true,
            ..Default::default()
        });
    }
    fn get(&self, id: &str) -> Item {
        self.items
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.id == id)
            .cloned()
            .unwrap()
    }
    fn set(&self, id: &str, f: impl FnOnce(&mut Item)) {
        let mut v = self.items.lock().unwrap();
        f(v.iter_mut().find(|i| i.id == id).unwrap());
    }
    fn writes(&self) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| !c.starts_with("list"))
            .count()
    }
}

#[async_trait::async_trait]
impl Remote for FakeRemote {
    async fn list(&self) -> Result<Vec<Item>> {
        self.calls.lock().unwrap().push("list".into());
        Ok(self.items.lock().unwrap().clone())
    }
    async fn create(&self, title: &str, body: &str, column: Option<&str>) -> Result<Item> {
        let mut v = self.items.lock().unwrap();
        let it = Item {
            id: format!("new-{}", v.len()),
            url: format!("https://board.example/new-{}", v.len()),
            title: title.into(),
            body: body.into(),
            column: column.unwrap_or("Backlog").into(),
            on_board: true,
            ..Default::default()
        };
        v.push(it.clone());
        self.calls.lock().unwrap().push(format!("create {title}"));
        Ok(it)
    }
    async fn update(&self, item: &Item, ch: &Change) -> Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("update {} {:?}", item.id, ch.fields()));
        if *self.fail_updates.lock().unwrap() {
            bail!("board down");
        }
        let mut v = self.items.lock().unwrap();
        let i = v.iter_mut().find(|i| i.id == item.id).unwrap();
        if let Some(t) = &ch.title {
            i.title = t.clone();
        }
        if let Some(b) = &ch.body {
            i.body = b.clone();
        }
        if let Some(c) = &ch.column {
            i.column = c.clone();
        }
        Ok(())
    }
}

async fn ledger(c: &Core, action: &str) -> Vec<kls_store::audit::LedgerEntry> {
    c.audit_entries(&LedgerQuery {
        kinds: vec!["kanban".into()],
        limit: 1000,
        ..Default::default()
    })
    .await
    .unwrap()
    .into_iter()
    .filter(|e| e.subject["action"] == action)
    .collect()
}

async fn link_of(c: &Core, card: &str) -> KanbanLink {
    c.store()
        .kanban_links_of_card(card)
        .await
        .unwrap()
        .remove(0)
}

#[test]
fn lists_and_binding_from_settings() {
    let l = parse_lists(&[
        "triage=Triage".into(),
        "bogus=X".into(),
        "todo=".into(),
        " doing = In Progress ".into(),
    ]);
    assert_eq!(
        l,
        vec![
            ("triage".into(), "Triage".into()),
            ("doing".into(), "In Progress".into())
        ]
    );
    let mut e = Effective::resolve(|_| None);
    assert!(Binding::from_settings(&e).is_none(), "off by default");
    e.kanban_provider = "jira".into();
    assert!(Binding::from_settings(&e).is_none(), "no board");
    e.kanban_board = "https://acme.atlassian.net/browse/ENG".into();
    let b = Binding::from_settings(&e).unwrap();
    assert_eq!(b.sync, Direction::Push);
    assert_eq!(b.column_of("doing"), Some("In Progress"));
    assert_eq!(b.status_of("in progress"), Some("doing"));
    assert_eq!(b.status_of("Nowhere"), None);
    assert_eq!(b.actor(), "kanban:jira");
}

#[test]
fn untrusted_text_is_cleaned() {
    assert_eq!(clean_title(" a\u{202e}b\n\tc\u{200b} "), "ab c");
    assert_eq!(clean_body("x\r\ny\u{1b}[31m\u{7}\n\tz "), "x\ny[31m\n\tz");
    assert_eq!(clean_title(&"t".repeat(400)).chars().count(), TITLE_MAX);
}

#[test]
fn plan_merges_three_ways() {
    let b = binding(Direction::Both);
    let card = Card {
        title: "T".into(),
        body: "B".into(),
        status: "todo".into(),
        ..Default::default()
    };
    let base = BTreeMap::from([
        ("title".to_string(), "T".to_string()),
        ("body".to_string(), "B".to_string()),
        ("column".to_string(), "Todo".to_string()),
    ]);
    let item = Item {
        title: "T".into(),
        body: "B".into(),
        column: "todo".into(),
        ..Default::default()
    };
    let p = plan(&b, &card, &item, &base);
    assert!(
        p.push.is_empty() && p.take.is_empty() && p.held.is_empty(),
        "{p:?}"
    );
    // Only the board changed: pushed.
    let c2 = Card {
        title: "T2".into(),
        ..card.clone()
    };
    let p = plan(&b, &c2, &item, &base);
    assert_eq!(p.push.title.as_deref(), Some("T2"));
    // Only the outside changed: taken.
    let i2 = Item {
        column: "In Progress".into(),
        ..item.clone()
    };
    let p = plan(&b, &card, &i2, &base);
    assert_eq!(p.take.column.as_deref(), Some("In Progress"));
    // Both changed: held, nothing written either way.
    let i3 = Item {
        title: "Theirs".into(),
        ..item.clone()
    };
    let p = plan(&b, &c2, &i3, &base);
    assert!(p.push.title.is_none() && p.take.title.is_none());
    assert_eq!(p.held["title"].board, "T2");
    assert_eq!(p.held["title"].remote, "Theirs");
    // Push: the factory wins both ways.
    let p = plan(&binding(Direction::Push), &c2, &i3, &base);
    assert_eq!(p.push.title.as_deref(), Some("T2"));
    let p = plan(&binding(Direction::Push), &card, &i3, &base);
    assert_eq!(p.push.title.as_deref(), Some("T"));
    // Pull: a local edit is kept, not pushed and not overwritten.
    let p = plan(&binding(Direction::Pull), &c2, &item, &base);
    assert!(p.push.is_empty() && p.take.is_empty() && p.held.is_empty());
    let p = plan(&binding(Direction::Pull), &c2, &i3, &base);
    assert!(p.held.contains_key("title"));
    // A column outside the mapping is never taken; a locked item keeps its text.
    let i4 = Item {
        column: "Icebox".into(),
        title: "X".into(),
        locked: true,
        ..item.clone()
    };
    let p = plan(&b, &card, &i4, &base);
    assert!(p.take.is_empty() && p.push.is_empty());
}

#[tokio::test]
async fn imports_pushes_takes_and_holds_conflicts() {
    let (c, _d) = core().await;
    let b = binding(Direction::Both);
    let r = FakeRemote::default();
    r.add("i1", "Outside \u{202e}task", "In Progress");
    r.add("old", "Shipped long ago", "Done");
    let local = c
        .create_card(
            CardDraft {
                title: "Local card".into(),
                status: "todo".into(),
                ..Default::default()
            },
            "owner",
        )
        .await
        .unwrap();
    let done = c
        .create_card(
            CardDraft {
                title: "Done card".into(),
                status: "done".into(),
                ..Default::default()
            },
            "owner",
        )
        .await
        .unwrap();

    let rep = sync_with(&c, &b, &r, "tick").await.unwrap();
    assert_eq!((rep.imported, rep.created), (1, 1), "{rep:?}");
    // The outside item: a triage card, cleaned, labelled, origin in meta.
    let cards = c.cards().await.unwrap();
    let imp = cards.iter().find(|x| x.title == "Outside task").unwrap();
    assert_eq!(imp.status, "triage");
    assert!(imp.labels.contains(&"kanban:linear".to_string()));
    let m: Value = serde_json::from_str(&imp.meta_json).unwrap();
    assert_eq!(m["kanban"]["url"], "https://board.example/i1");
    assert_eq!(m["kanban"]["trust"], "external");
    assert_eq!(m["kanban"]["sha256"].as_str().unwrap().len(), 64);
    let act = c.card_activity(&imp.id).await.unwrap();
    assert!(act.iter().any(|a| a.actor == "kanban:linear"));
    // Done items are not imported; done cards are not exported.
    assert!(!cards.iter().any(|x| x.title == "Shipped long ago"));
    assert!(
        c.store()
            .kanban_links_of_card(&done.id)
            .await
            .unwrap()
            .is_empty()
    );
    // The local card got an item in its column.
    let l = link_of(&c, &local.id).await;
    assert_eq!(r.get(&l.remote_id).column, "Todo");
    assert_eq!(ledger(&c, "kanban.import").await.len(), 1);
    assert_eq!(ledger(&c, "kanban.create").await.len(), 1);

    // Next pass: the imported card's triage list goes out; nothing else moves.
    let rep = sync_with(&c, &b, &r, "tick").await.unwrap();
    assert_eq!(rep.pushed, 1, "{rep:?}");
    assert_eq!(r.get("i1").column, "Triage");
    let rep = sync_with(&c, &b, &r, "tick").await.unwrap();
    assert_eq!(
        (rep.pushed, rep.taken, rep.created, rep.imported),
        (0, 0, 0, 0),
        "echo: {rep:?}"
    );

    // Outside edit taken, with provenance.
    let rid = l.remote_id.clone();
    r.set(&rid, |i| {
        i.column = "In Progress".into();
        i.body = "from outside".into();
    });
    let rep = sync_with(&c, &b, &r, "tick").await.unwrap();
    assert_eq!(rep.taken, 1);
    let now = c.card(&local.id).await.unwrap().unwrap();
    assert_eq!(
        (now.status.as_str(), now.body.as_str()),
        ("doing", "from outside")
    );
    let take = ledger(&c, "kanban.take").await;
    assert_eq!(take.len(), 1);
    assert_eq!(take[0].payload["data"]["url"], r.get(&rid).url);
    assert_eq!(take[0].actor, "kanban:linear");

    // Both sides edit the title: held, neither overwritten, until resolved.
    c.update_card(
        &now.id,
        now.version,
        CardPatch {
            title: Some("Mine".into()),
            ..Default::default()
        },
        "owner",
    )
    .await
    .unwrap();
    r.set(&rid, |i| i.title = "Theirs".into());
    let w = r.writes();
    let rep = sync_with(&c, &b, &r, "tick").await.unwrap();
    assert_eq!(rep.conflicts, 1);
    assert_eq!(r.writes(), w, "nothing written");
    assert_eq!(c.card(&local.id).await.unwrap().unwrap().title, "Mine");
    assert_eq!(r.get(&rid).title, "Theirs");
    assert_eq!(ledger(&c, "kanban.conflict").await.len(), 1);
    // Still held on the next pass, not re-announced.
    sync_with(&c, &b, &r, "tick").await.unwrap();
    assert_eq!(ledger(&c, "kanban.conflict").await.len(), 1);
    let rows = conflicts_of(&c, &b).await;
    assert_eq!(rows, vec!["title".to_string()]);
    // The owner keeps the board's title: the next pass pushes it.
    set_binding(&c, &b).await;
    resolve_conflict(&c, &local.id, "title", true)
        .await
        .unwrap();
    let rep = sync_with(&c, &b, &r, "tick").await.unwrap();
    assert_eq!((rep.pushed, rep.conflicts), (1, 0), "{rep:?}");
    assert_eq!(r.get(&rid).title, "Mine");
    assert_eq!(ledger(&c, "kanban.resolve").await[0].actor, "owner");
}

async fn conflicts_of(c: &Core, b: &Binding) -> Vec<String> {
    c.store()
        .kanban_links(&b.provider, &b.board)
        .await
        .unwrap()
        .iter()
        .flat_map(|l| held_of(l).into_keys())
        .collect()
}

async fn set_binding(c: &Core, b: &Binding) {
    let s = c.store();
    s.set_setting(keys::KANBAN_PROVIDER, &b.provider)
        .await
        .unwrap();
    s.set_setting(keys::KANBAN_BOARD, &b.board).await.unwrap();
    s.set_setting(keys::KANBAN_SYNC, b.sync.as_str())
        .await
        .unwrap();
    let lists: Vec<String> = b.lists.iter().map(|(a, z)| format!("{a}={z}")).collect();
    s.set_setting(keys::KANBAN_LISTS, &lists.join(", "))
        .await
        .unwrap();
}

#[tokio::test]
async fn pull_keeps_local_edits_and_gone_sides_are_left_alone() {
    let (c, _d) = core().await;
    let b = binding(Direction::Pull);
    let r = FakeRemote::default();
    r.add("i1", "One", "Todo");
    r.add("i2", "Two", "Todo");
    sync_with(&c, &b, &r, "tick").await.unwrap();
    let mut cards = c.cards().await.unwrap();
    cards.sort_by(|a, z| a.title.cmp(&z.title));
    let (one, two) = (cards[0].clone(), cards[1].clone());
    // A local edit under pull: kept, nothing pushed, still kept next pass.
    c.update_card(
        &one.id,
        one.version,
        CardPatch {
            body: Some("my notes".into()),
            ..Default::default()
        },
        "owner",
    )
    .await
    .unwrap();
    let rep = sync_with(&c, &b, &r, "tick").await.unwrap();
    assert_eq!((rep.pushed, rep.created), (0, 0));
    assert_eq!(r.writes(), 0);
    assert_eq!(c.card(&one.id).await.unwrap().unwrap().body, "my notes");
    // The outside item goes; the card stays.
    r.items.lock().unwrap().retain(|i| i.id != "i1");
    let rep = sync_with(&c, &b, &r, "tick").await.unwrap();
    assert_eq!(rep.gone, 1);
    assert!(!c.card(&one.id).await.unwrap().unwrap().deleted);
    assert_eq!(link_of(&c, &one.id).await.state, "gone");
    // The card goes; the item stays and is not imported again.
    let two = c.card(&two.id).await.unwrap().unwrap();
    c.delete_card(&two.id, two.version, "owner").await.unwrap();
    let rep = sync_with(&c, &b, &r, "tick").await.unwrap();
    assert_eq!((rep.gone, rep.imported), (1, 0));
    assert_eq!(r.items.lock().unwrap().len(), 1);
    assert_eq!(ledger(&c, "kanban.unlink").await.len(), 2);
}

#[tokio::test]
async fn failed_pushes_keep_the_base_and_are_audited() {
    let (c, _d) = core().await;
    let b = binding(Direction::Push);
    let r = FakeRemote::default();
    let card = c
        .create_card(
            CardDraft {
                title: "A".into(),
                status: "todo".into(),
                ..Default::default()
            },
            "owner",
        )
        .await
        .unwrap();
    sync_with(&c, &b, &r, "tick").await.unwrap();
    let card = c.card(&card.id).await.unwrap().unwrap();
    c.update_card(
        &card.id,
        card.version,
        CardPatch {
            title: Some("B".into()),
            ..Default::default()
        },
        "owner",
    )
    .await
    .unwrap();
    *r.fail_updates.lock().unwrap() = true;
    let rep = sync_with(&c, &b, &r, "tick").await.unwrap();
    assert_eq!(rep.failed, 1);
    let f = ledger(&c, "kanban.push").await;
    assert_eq!(f[0].payload["outcome"], "failed");
    assert_eq!(
        c.store()
            .kanban_run(&b.provider, &b.board)
            .await
            .unwrap()
            .unwrap()
            .outcome,
        "partial"
    );
    *r.fail_updates.lock().unwrap() = false;
    let rep = sync_with(&c, &b, &r, "tick").await.unwrap();
    assert_eq!(rep.pushed, 1, "retried: {rep:?}");
    let id = link_of(&c, &card.id).await.remote_id;
    assert_eq!(r.get(&id).title, "B");
}

#[tokio::test]
async fn pause_and_stop_refuse_the_pass_and_are_audited() {
    let (c, _d) = core().await;
    let b = binding(Direction::Both);
    let r = FakeRemote::default();
    r.add("i1", "One", "Todo");
    crate::control::pause(&c, "drill").await.unwrap();
    let e = sync_with(&c, &b, &r, "tick").await.unwrap_err();
    assert!(crate::control::is_blocked(&e));
    assert!(
        r.calls.lock().unwrap().is_empty(),
        "the board is not called"
    );
    assert!(c.cards().await.unwrap().is_empty());
    let refused = ledger(&c, "kanban.sync").await;
    assert_eq!(refused[0].payload["outcome"], "refused");
    assert_eq!(
        c.store()
            .kanban_run("linear", "ENG")
            .await
            .unwrap()
            .unwrap()
            .outcome,
        "refused"
    );
    crate::control::resume(&c).await.unwrap();
    crate::control::stop(&c, "drill").await.unwrap();
    assert!(crate::control::is_blocked(
        &sync_with(&c, &b, &r, "tick").await.unwrap_err()
    ));
    assert!(r.calls.lock().unwrap().is_empty());
    crate::control::resume(&c).await.unwrap();
    assert_eq!(sync_with(&c, &b, &r, "tick").await.unwrap().imported, 1);
}

struct FakeConnector(Arc<FakeRemote>);

struct Shared(Arc<FakeRemote>);

#[async_trait::async_trait]
impl Remote for Shared {
    async fn list(&self) -> Result<Vec<Item>> {
        self.0.list().await
    }
    async fn create(&self, t: &str, b: &str, c: Option<&str>) -> Result<Item> {
        self.0.create(t, b, c).await
    }
    async fn update(&self, i: &Item, ch: &Change) -> Result<()> {
        self.0.update(i, ch).await
    }
}

#[async_trait::async_trait]
impl Connector for FakeConnector {
    async fn remote(&self, _: &Core, b: &Binding) -> Result<Option<Arc<dyn Remote>>> {
        Ok((b.provider == "github_projects")
            .then(|| Arc::new(Shared(self.0.clone())) as Arc<dyn Remote>))
    }
}

#[tokio::test]
async fn the_tick_reads_settings_and_uses_registered_connectors() {
    let (c, _d) = core().await;
    assert!(tick(&c).await.is_none(), "nothing bound");
    let r = Arc::new(FakeRemote::default());
    r.add("PVTI_1", "From the project", "Todo");
    register_connector(&c, Arc::new(FakeConnector(r.clone())));
    let mut b = binding(Direction::Both);
    b.provider = "github_projects".into();
    b.board = "https://github.com/orgs/acme/projects/3".into();
    set_binding(&c, &b).await;
    let rep = tick(&c).await.unwrap();
    assert_eq!(rep.imported, 1);
    let st = status(&c).await.unwrap();
    assert_eq!(st.binding.unwrap().provider, "github_projects");
    assert_eq!((st.linked, st.token_needed), (1, false));
    assert_eq!(st.run.unwrap().outcome, "ok");
    // Linear with no key sealed: unavailable, recorded.
    c.store()
        .set_setting(keys::KANBAN_PROVIDER, "linear")
        .await
        .unwrap();
    c.store()
        .set_setting(keys::KANBAN_BOARD, "ENG")
        .await
        .unwrap();
    assert!(tick(&c).await.is_none());
    let st = status(&c).await.unwrap();
    assert!(st.token_needed && !st.token_set);
    assert_eq!(st.run.unwrap().outcome, "unavailable");
}

#[tokio::test]
async fn the_token_is_sealed_and_never_shown() {
    let (c, _d) = core().await;
    assert!(
        set_token(&c, "jira", SecretString::new("no-colon"))
            .await
            .is_err()
    );
    assert!(
        set_token(&c, "linear", SecretString::new("lin_api_secretvalue123"))
            .await
            .unwrap()
    );
    assert!(token_set(&c).await);
    c.audit_sync().await.unwrap();
    for e in c.store().recent_events(500).await.unwrap() {
        assert!(!e.data_json.contains("secretvalue123"), "{}", e.kind);
    }
    for e in ledger(&c, "kanban.token").await {
        assert!(!format!("{} {}", e.msg, e.payload).contains("secretvalue123"));
    }
    assert!(
        !set_token(&c, "linear", SecretString::new(""))
            .await
            .unwrap()
    );
    assert!(!token_set(&c).await);
}

// ---- adapters ------------------------------------------------------------------

fn gql_has(r: &http::Request, s: &str) -> bool {
    r.body
        .as_ref()
        .and_then(|b| b["query"].as_str())
        .is_some_and(|q| q.contains(s))
}

#[tokio::test]
async fn linear_lists_creates_and_moves_by_state() {
    assert_eq!(
        linear::team_key("https://linear.app/acme/team/eng/active").unwrap(),
        "ENG"
    );
    assert!(linear::team_key("https://x/y").is_err());
    let f = Arc::new(http::Fake::default());
    f.on(|r| {
        gql_has(r, "teams(").then_some(()).and_then(|_| {
            http::ok(json!({ "data": { "teams": { "nodes": [{
                "id": "team-1",
                "states": { "nodes": [{ "id": "s-todo", "name": "Todo" }, { "id": "s-doing", "name": "In Progress" }] },
                "issues": { "nodes": [{ "id": "iss-1", "url": "https://linear.app/i/1", "title": "Hi\u{202e}", "description": "d", "state": { "name": "Todo" } }],
                            "pageInfo": { "hasNextPage": false, "endCursor": null } }
            }] } } }))
        })
    });
    f.on(|r| {
        gql_has(r, "issueCreate").then_some(()).and_then(|_| {
            http::ok(json!({ "data": { "issueCreate": { "success": true, "issue": {
                "id": "iss-2", "url": "https://linear.app/i/2", "title": "New", "description": "", "state": { "name": "In Progress" } } } } }))
        })
    });
    f.on(|r| {
        gql_has(r, "issueUpdate")
            .then_some(())
            .and_then(|_| http::ok(json!({ "data": { "issueUpdate": { "success": true } } })))
    });
    let l = linear::Linear::new(
        f.clone(),
        SecretString::new("lin_api_k"),
        &binding(Direction::Both),
    )
    .unwrap();
    let items = l.list().await.unwrap();
    assert_eq!(items[0].title, "Hi");
    assert_eq!(items[0].column, "Todo");
    let n = l.create("New", "", Some("In Progress")).await.unwrap();
    assert_eq!(n.id, "iss-2");
    l.update(
        &items[0],
        &Change {
            column: Some("in progress".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let seen = f.seen();
    assert!(seen.iter().all(|r| {
        r.url == linear::API
            && r.headers
                .contains(&("authorization".into(), "lin_api_k".into()))
    }));
    let create = seen.iter().find(|r| gql_has(r, "issueCreate")).unwrap();
    assert_eq!(
        create.body.as_ref().unwrap()["variables"]["input"]["stateId"],
        "s-doing"
    );
    let upd = seen.iter().find(|r| gql_has(r, "issueUpdate")).unwrap();
    assert_eq!(
        upd.body.as_ref().unwrap()["variables"]["input"]["stateId"],
        "s-doing"
    );
    // GraphQL errors come back as errors.
    let f2 = Arc::new(http::Fake::default());
    f2.on(|_| http::ok(json!({ "errors": [{ "message": "Authentication required" }] })));
    let l2 = linear::Linear::new(f2, SecretString::new("bad"), &binding(Direction::Both)).unwrap();
    assert!(format!("{:#}", l2.list().await.unwrap_err()).contains("Authentication required"));
}

#[tokio::test]
async fn jira_lists_creates_and_transitions() {
    assert_eq!(
        jira::site_and_key("https://acme.atlassian.net/jira/software/projects/ENG/boards/1")
            .unwrap(),
        ("acme.atlassian.net".into(), "ENG".into())
    );
    assert_eq!(
        jira::site_and_key("acme.atlassian.net/browse/ops-12")
            .unwrap()
            .1,
        "OPS"
    );
    assert!(jira::site_and_key("https://evil.example/browse/ENG").is_err());
    let text = "one\n\nthree";
    assert_eq!(jira::from_adf(&jira::adf(text)), text);

    let f = Arc::new(http::Fake::default());
    f.on(|r| {
        (r.url.ends_with("/rest/api/3/search/jql")).then_some(()).and_then(|_| {
            http::ok(json!({ "issues": [{ "id": "10001", "key": "ENG-1", "fields": {
                "summary": "Fix", "description": jira::adf("a\nb"), "status": { "name": "To Do" } } }], "isLast": true }))
        })
    });
    f.on(|r| {
        (r.method == "POST" && r.url.ends_with("/rest/api/3/issue"))
            .then_some(())
            .and_then(|_| http::ok(json!({ "id": "10002", "key": "ENG-2" })))
    });
    f.on(|r| {
        (r.method == "GET" && r.url.ends_with("/transitions")).then_some(()).and_then(|_| {
            http::ok(json!({ "transitions": [{ "id": "21", "to": { "name": "In Progress" } }, { "id": "31", "to": { "name": "Done" } }] }))
        })
    });
    f.on(|r| {
        ((r.method == "POST" && r.url.ends_with("/transitions")) || r.method == "PUT")
            .then_some(())
            .and_then(|_| http::status(204))
    });
    let mut b = binding(Direction::Both);
    b.provider = "jira".into();
    b.board = "https://acme.atlassian.net/browse/ENG".into();
    assert!(jira::Jira::new(f.clone(), SecretString::new("nocolon"), &b).is_err());
    let j = jira::Jira::new(f.clone(), SecretString::new("me@acme.test:tok"), &b).unwrap();
    let items = j.list().await.unwrap();
    assert_eq!(
        (items[0].title.as_str(), items[0].body.as_str()),
        ("Fix", "a\nb")
    );
    assert_eq!(items[0].url, "https://acme.atlassian.net/browse/ENG-1");
    let n = j.create("New", "body", Some("In Progress")).await.unwrap();
    assert_eq!((n.id.as_str(), n.column.as_str()), ("10002", "In Progress"));
    j.update(
        &items[0],
        &Change {
            title: Some("Fixed".into()),
            column: Some("done".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let seen = f.seen();
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("me@acme.test:tok")
    );
    assert!(
        seen.iter()
            .all(|r| r.url.starts_with("https://acme.atlassian.net/")
                && r.headers.contains(&("authorization".into(), basic.clone())))
    );
    let moves: Vec<&Value> = seen
        .iter()
        .filter(|r| r.method == "POST" && r.url.ends_with("/transitions"))
        .filter_map(|r| r.body.as_ref())
        .collect();
    assert_eq!(moves.len(), 2);
    assert_eq!(moves[1]["transition"]["id"], "31");
    let put = seen.iter().find(|r| r.method == "PUT").unwrap();
    assert_eq!(put.body.as_ref().unwrap()["fields"]["summary"], "Fixed");
    // No transition to the column: an error, not a silent skip.
    assert!(
        j.update(
            &items[0],
            &Change {
                column: Some("Nowhere".into()),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
}

use base64::Engine as _;
