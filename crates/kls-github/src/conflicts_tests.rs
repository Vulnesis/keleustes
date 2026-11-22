//! Conflicts: lockfiles, carry-over across a moved head, the commit
//! description and the hand-off to belai:conflict-resolver.

use super::*;
use kls_core::Core;
use kls_store::audit::LedgerQuery;

fn file(path: &str, base: &str, ours: &str, theirs: &str) -> ConflictFile {
    let merge = Merge3::new(base, ours, theirs);
    ConflictFile {
        path: path.into(),
        previous: None,
        base: Some(base.into()),
        ours: Some(ours.into()),
        theirs: Some(theirs.into()),
        binary: false,
        conflicting: merge.has_conflicts(),
        merge,
    }
}

fn set(head: &str, files: Vec<ConflictFile>) -> ConflictSet {
    ConflictSet {
        repo: "acme/api".into(),
        number: 12,
        title: "Feature".into(),
        head_repo: "acme/api".into(),
        head_ref: "feat".into(),
        head_sha: head.into(),
        base_ref: "main".into(),
        base_sha: "b".repeat(40),
        merge_base: "m".repeat(40),
        files,
        base_only: vec![BaseChange {
            path: "README.md".into(),
            status: "modified".into(),
            previous: None,
            sha: "s".into(),
        }],
        forge: crate::ForgeKind::Github,
    }
}

#[test]
fn lockfiles_are_classified_with_their_command() {
    for p in [
        "Cargo.lock",
        "web/package-lock.json",
        "yarn.lock",
        "a/b/pnpm-lock.yaml",
        "go.sum",
        "poetry.lock",
        "Gemfile.lock",
        "composer.lock",
        "uv.lock",
    ] {
        assert!(is_lockfile(p), "{p}");
    }
    for p in [
        "Cargo.toml",
        "package.json",
        "src/lock.rs",
        "go.mod",
        "lock",
    ] {
        assert!(!is_lockfile(p), "{p}");
    }
    assert_eq!(
        regenerate_command("x/Cargo.lock"),
        Some("cargo generate-lockfile")
    );
    assert!(file("go.sum", "a\n", "b\n", "c\n").lockfile());
}

#[test]
fn resolutions_carry_over_when_only_other_files_moved() {
    let old = set(
        &"1".repeat(40),
        vec![
            file("src/a.rs", "a\n", "b\n", "c\n"),
            file("src/b.rs", "x\n", "y\n", "z\n"),
        ],
    );
    // The head moved: b.rs changed on ours, a.rs is the same.
    let new = set(
        &"2".repeat(40),
        vec![
            file("src/a.rs", "a\n", "b\n", "c\n"),
            file("src/b.rs", "x\n", "y2\n", "z\n"),
            file("src/c.rs", "p\n", "q\n", "r\n"),
        ],
    );
    let keep = unchanged_files(&old, &new);
    assert_eq!(keep.into_iter().collect::<Vec<_>>(), vec!["src/a.rs"]);
    let mut other = new.clone();
    other.number = 13;
    assert!(unchanged_files(&old, &other).is_empty());
    assert!(head_moved(
        "the head branch moved since the conflicts were loaded; load them again"
    ));
}

#[test]
fn description_lists_files_how_and_lockfiles() {
    let s = set(
        &"1".repeat(40),
        vec![
            file("src/a.rs", "a\n", "b\n", "c\n"),
            file("Cargo.lock", "a\n", "b\n", "c\n"),
            file(
                "src/clean.rs",
                "a\nx\ny\nb\n",
                "A\nx\ny\nb\n",
                "a\nx\ny\nB\n",
            ),
        ],
    );
    let mut how = BTreeMap::new();
    how.insert("src/a.rs".to_string(), How::Hunks);
    how.insert("Cargo.lock".to_string(), How::Theirs);
    let d = describe(&s, &how);
    let want = "Merge main into feat\n\
        \n\
        Resolved 3 files both sides changed:\n\
        - src/a.rs: resolved conflict by conflict (1 conflict)\n\
        - Cargo.lock: kept theirs (lockfile)\n\
        - src/clean.rs: merged cleanly\n\
        \n\
        Regenerate these lockfiles before merging:\n\
        - Cargo.lock: run `cargo generate-lockfile`\n\
        \n\
        Takes 1 file only main changed.\n\
        \n\
        Head 111111111111 · base bbbbbbbbbbbb · merge base mmmmmmmmmmmm\n";
    assert_eq!(d, want);
    assert!(d.lines().count() > 1, "multi-line body");
}

async fn core() -> (Core, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (Core::open(dir.path(), None).await.unwrap(), dir)
}

async fn forge_audit(core: &Core) -> Vec<kls_store::audit::LedgerEntry> {
    core.audit_entries(&LedgerQuery {
        kinds: vec!["forge".into()],
        ..Default::default()
    })
    .await
    .unwrap()
    .into_iter()
    .filter(|e| e.subject["action"] == "conflict_resolver")
    .collect()
}

#[tokio::test]
async fn resolver_card_is_filed_once_and_audited() {
    let (core, _d) = core().await;
    let s = set(
        &"1".repeat(40),
        vec![file("Cargo.lock", "a\n", "b\n", "c\n")],
    );
    let url = "https://github.com/acme/api/pull/12";
    let c = file_resolver_card(&core, &s, url, "user").await.unwrap();
    assert!(!c.existing);
    let card = core.card(&c.card_id).await.unwrap().unwrap();
    assert_eq!(card.labels, [RESOLVER_LABEL]);
    assert_eq!(card.pr_url, url);
    assert!(card.body.contains("```conflict") && card.body.contains("pull: 12"));
    assert!(card.body.contains("Cargo.lock (lockfile"));
    let meta: Value = serde_json::from_str(&card.meta_json).unwrap();
    assert_eq!(meta["keleustes"]["agent"], RESOLVER);
    assert_eq!(meta["keleustes"]["by"], "owner");
    let again = file_resolver_card(&core, &s, url, "user").await.unwrap();
    assert!(again.existing);
    assert_eq!(again.card_id, c.card_id);
    let a = forge_audit(&core).await;
    assert_eq!(a.len(), 2);
    assert!(
        a.iter()
            .all(|e| e.actor == "owner" && e.payload["outcome"] == "ok")
    );
}

#[tokio::test]
async fn resolver_card_refused_under_the_kill_switch() {
    let (core, _d) = core().await;
    let s = set(&"1".repeat(40), vec![file("src/a.rs", "a\n", "b\n", "c\n")]);
    kls_core::control::stop(&core, "test").await.unwrap();
    let e = file_resolver_card(&core, &s, "", "user").await.unwrap_err();
    assert!(kls_core::control::is_blocked(&e), "{e:#}");
    assert!(core.cards().await.unwrap().is_empty());
    let a = forge_audit(&core).await;
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].payload["outcome"], "refused");
}
