//! Row 53 and agent files: caps, the blob store, the host feature, the
//! kill switch, who may read what, and an audit line for each outcome.

use super::*;
use crate::sessions::Session;
use kls_store::{Capabilities, HostInfo};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest-of-a-png";

async fn setup(features: &[&str]) -> (Core, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    for (id, f) in [("n1", features), ("n2", &[][..])] {
        let h = HostInfo {
            capabilities: Capabilities {
                remote_control: true,
                dispatch_features: f.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            ..Default::default()
        };
        core.store()
            .insert_node(id, id, &h, &format!("s-{id}"), &format!("t-{id}"))
            .await
            .unwrap();
        core.session_upsert(
            id,
            Session {
                id: format!("s-{id}"),
                cwd: "/work".into(),
                state: "idle".into(),
                kind: "rc".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    (core, dir)
}

fn owner() -> Origin {
    Origin {
        actor: "owner".into(),
        surface: "sessions/composer".into(),
        ..Default::default()
    }
}

async fn ledger(core: &Core, kind: &str) -> Vec<kls_store::audit::LedgerEntry> {
    core.audit_entries(&Default::default())
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

#[test]
fn sniff_names_and_caps() {
    assert_eq!(sniff(PNG), Some(MIME_PNG));
    assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(MIME_JPEG));
    assert_eq!(sniff(b"hello"), Some(MIME_TEXT));
    assert_eq!(sniff(b"GIF89a"), Some(MIME_TEXT));
    assert_eq!(sniff(b"a\0b"), None);
    assert_eq!(sniff(&[0xC3, 0x28]), None);
    assert_eq!(clean_name("/tmp/dir/shot.png").unwrap(), "shot.png");
    assert_eq!(clean_name("C:\\x\\a.txt").unwrap(), "a.txt");
    assert!(clean_name("dir/").is_err());
    assert!(clean_name("a\nb").is_err());
    let a = |name: &str, mime: &str, size: u64, sha: &str| AttachmentRef {
        name: name.into(),
        mime: mime.into(),
        sha256: sha.into(),
        size,
    };
    let h = |c: char| c.to_string().repeat(64);
    assert!(check_list(&[a("a.png", MIME_PNG, 10, &h('a'))]).is_ok());
    assert!(check_list(&[a("a.gif", "image/gif", 10, &h('a'))]).is_err());
    assert!(check_list(&[a("a.txt", MIME_TEXT, MAX_TEXT_BYTES + 1, &h('a'))]).is_err());
    assert!(check_list(&[a("a/b.txt", MIME_TEXT, 1, &h('a'))]).is_err());
    assert!(check_list(&[a("a.txt", MIME_TEXT, 1, "nothex")]).is_err());
    let twice = [
        a("a.txt", MIME_TEXT, 1, &h('a')),
        a("b.txt", MIME_TEXT, 1, &h('a')),
    ];
    assert!(check_list(&twice).is_err());
    let nine: Vec<_> = "abcdef012"
        .chars()
        .map(|c| a("x.txt", MIME_TEXT, 1, &h(c)))
        .collect();
    assert!(check_list(&nine).unwrap_err().contains("at most 8"));
    let big: Vec<_> = "abc"
        .chars()
        .map(|c| a("x.png", MIME_PNG, MAX_IMAGE_BYTES, &h(c)))
        .collect();
    assert!(check_list(&big).unwrap_err().contains("total"));
}

#[tokio::test]
async fn stored_content_addressed_and_refused_past_caps() {
    let (core, _d) = setup(&[FEATURE]).await;
    let a = store_bytes(&core, "/x/shot.png", PNG, &owner())
        .await
        .unwrap();
    assert_eq!(a.mime, MIME_PNG);
    assert_eq!(a.name, "shot.png");
    assert_eq!(a.sha256, sha256_hex(PNG));
    assert_eq!(core.blobs().read(&a.sha256).await.unwrap(), PNG);
    // Same bytes, one blob.
    let b = store_bytes(&core, "again.png", PNG, &owner())
        .await
        .unwrap();
    assert_eq!(a.sha256, b.sha256);
    assert!(
        store_bytes(&core, "x.bin", b"\0\x01", &owner())
            .await
            .is_err()
    );
    let long = vec![b'a'; MAX_TEXT_BYTES as usize + 1];
    assert!(store_bytes(&core, "x.txt", &long, &owner()).await.is_err());
    // A file read from disk.
    let p = _d.path().join("notes.md");
    std::fs::write(&p, "# notes\n").unwrap();
    let f = store_file(&core, &p, &owner()).await.unwrap();
    assert_eq!((f.name.as_str(), f.mime.as_str()), ("notes.md", MIME_TEXT));
    assert!(check_stored(&core, &[a.clone(), f]).await.is_ok());
    let ghost = AttachmentRef {
        sha256: "f".repeat(64),
        ..a
    };
    assert!(check_stored(&core, &[ghost]).await.is_err());
}

#[tokio::test]
async fn prompt_carries_references_and_is_audited() {
    let (core, _d) = setup(&[FEATURE]).await;
    let a = store_bytes(&core, "shot.png", PNG, &owner()).await.unwrap();
    let item = queue_prompt(&core, "s-n1", "look at this", vec![a.clone()], "owner")
        .await
        .unwrap();
    // The inbox holds the reference, never the bytes.
    assert!(item.payload_json.contains(&a.sha256));
    assert!(!item.payload_json.contains("rest-of-a-png"));
    match InboxBody::of(&item).unwrap() {
        InboxBody::Prompt { text, attachments } => {
            assert_eq!(text, "look at this");
            assert_eq!(attachments, vec![a.clone()]);
        }
        b => panic!("{b:?}"),
    }
    let l = ledger(&core, "attachment").await;
    let q = l
        .iter()
        .find(|e| e.payload["outcome"] == "queued")
        .expect("queued line");
    assert_eq!(q.payload["attachments"][0]["sha256"], a.sha256);
    assert_eq!(q.session_id, "s-n1");
    // A host without the feature is refused, audited.
    let e = queue_prompt(&core, "s-n2", "look", vec![a.clone()], "owner")
        .await
        .unwrap_err();
    assert!(e.to_string().contains("does not take attachments"), "{e}");
    assert!(
        ledger(&core, "attachment")
            .await
            .iter()
            .any(|e| e.payload["outcome"] == "refused")
    );
    // A plain prompt still goes as before.
    let plain = queue_prompt(&core, "s-n2", "hi", vec![], "owner")
        .await
        .unwrap();
    assert!(!plain.payload_json.contains("attachments"));
}

#[tokio::test]
async fn kill_switch_refuses_prompt_and_read() {
    let (core, _d) = setup(&[FEATURE]).await;
    let a = store_bytes(&core, "shot.png", PNG, &owner()).await.unwrap();
    let item = queue_prompt(&core, "s-n1", "look", vec![a.clone()], "owner")
        .await
        .unwrap();
    for stop in [false, true] {
        if stop {
            control::stop(&core, "t").await.unwrap();
        } else {
            control::pause(&core, "t").await.unwrap();
        }
        let e = queue_prompt(&core, "s-n1", "again", vec![a.clone()], "owner")
            .await
            .unwrap_err();
        assert!(control::is_blocked(&e), "{e}");
        let e = authorize(&core, "n1", &a.sha256, "", "s-n1", &item.id)
            .await
            .unwrap_err();
        assert!(control::is_blocked(&e), "{e}");
        control::resume(&core).await.unwrap();
    }
    assert!(
        ledger(&core, "attachment")
            .await
            .iter()
            .any(|e| e.payload["outcome"] == "refused" && e.node_id == "n1")
    );
}

#[tokio::test]
async fn only_the_listing_node_reads_and_each_read_is_audited() {
    let (core, _d) = setup(&[FEATURE]).await;
    let a = store_bytes(&core, "shot.png", PNG, &owner()).await.unwrap();
    let other = store_bytes(&core, "n.txt", b"other", &owner())
        .await
        .unwrap();
    let item = queue_prompt(&core, "s-n1", "look", vec![a.clone()], "owner")
        .await
        .unwrap();
    let got = authorize(&core, "n1", &a.sha256, "", "s-n1", &item.id)
        .await
        .unwrap();
    assert_eq!(got, a);
    // Another node, another session, a hash the item does not list.
    assert!(
        authorize(&core, "n2", &a.sha256, "", "s-n1", &item.id)
            .await
            .is_err()
    );
    assert!(
        authorize(&core, "n2", &a.sha256, "", "s-n2", &item.id)
            .await
            .is_err()
    );
    assert!(
        authorize(&core, "n1", &other.sha256, "", "s-n1", &item.id)
            .await
            .is_err()
    );
    // A dispatch lists its own.
    let spec = DispatchSpec {
        kind: "session".into(),
        prompt: "go".into(),
        attachments: vec![other.clone()],
        ..Default::default()
    };
    assert!(check_spec(&core, &spec).await.is_ok());
    core.dispatch_queued(&crate::sessions::Dispatch {
        id: "d1".into(),
        node_id: "n1".into(),
        kind: "session".into(),
        spec_json: serde_json::to_string(&spec).unwrap(),
        status: "queued".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    assert!(
        authorize(&core, "n1", &other.sha256, "d1", "", "")
            .await
            .is_ok()
    );
    assert!(
        authorize(&core, "n2", &other.sha256, "d1", "", "")
            .await
            .is_err()
    );
    assert!(
        authorize(&core, "n1", &a.sha256, "d1", "", "")
            .await
            .is_err()
    );
    let served = ledger(&core, "attachment")
        .await
        .into_iter()
        .filter(|e| e.payload["outcome"] == "served")
        .count();
    assert_eq!(served, 2);
    // A worker dispatch takes none.
    let worker = DispatchSpec {
        kind: "worker".into(),
        ..spec
    };
    assert!(check_spec(&core, &worker).await.is_err());
}

#[test]
fn agent_paths() {
    for ok in ["docs/guide.md", "~/notes/a.md", "/etc/x.md", "./a//b.md"] {
        assert!(valid_agent_path(ok), "{ok}");
    }
    for bad in ["", "../a", "a/../b", " a", "a\nb", "/", "~/", ".", "a\\b"] {
        assert!(!valid_agent_path(bad), "{bad:?}");
    }
    assert_eq!(clean_agent_path("./a//b/"), "a/b");
    assert_eq!(clean_agent_path("~/x/./y"), "~/x/y");
}

#[tokio::test]
async fn agent_files_checked_stored_audited_and_published() {
    let (core, _d) = setup(&[]).await;
    let a = crate::vault::save_agent(
        &core,
        &kls_store::vault::AgentProfile {
            name: "docs-bot".into(),
            prompt: "help".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let f = add_agent_file(&core, &a.id, "docs/guide.md", b"# Guide\n", &owner())
        .await
        .unwrap();
    assert_eq!(f.added_by, owner().label());
    assert!(core.blobs().exists(&f.sha256).await);
    for (path, body, why) in [
        ("../x.md", &b"x"[..], "path"),
        ("bin.md", b"\0\x01", "UTF-8"),
        (
            "key.md",
            b"-----BEGIN RSA PRIVATE KEY-----\nabc",
            "private key",
        ),
    ] {
        let e = add_agent_file(&core, &a.id, path, body, &owner())
            .await
            .unwrap_err();
        assert!(e.to_string().contains(why), "{e}");
    }
    let big = vec![b'a'; AGENT_FILE_MAX_BYTES + 1];
    assert!(
        add_agent_file(&core, &a.id, "big.md", &big, &owner())
            .await
            .is_err()
    );
    let l = ledger(&core, "agent").await;
    assert_eq!(
        l.iter().filter(|e| e.payload["outcome"] == "saved").count(),
        1
    );
    assert_eq!(
        l.iter()
            .filter(|e| e.payload["outcome"] == "refused")
            .count(),
        4
    );

    // A publish lists them in the document and saves them with the version.
    let (item, v) = crate::vault::agent_to_library(&core, &a.id, "t")
        .await
        .unwrap();
    let entries = core.store().bundle_files(&item, v).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].sha256, f.sha256);
    let body = core.store().library_versions(&item).await.unwrap()[0]
        .body
        .clone();
    assert!(body.contains(&f.sha256));
    // A removal is a new version without them.
    assert!(
        remove_agent_file(&core, &a.id, "docs/guide.md", &owner())
            .await
            .unwrap()
    );
    let (_, v2) = crate::vault::agent_to_library(&core, &a.id, "t")
        .await
        .unwrap();
    assert!(v2 > v);
    assert!(
        core.store()
            .bundle_files(&item, v2)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a2a_blackout_holds_agent_sent_attachments() {
    let (core, _d) = setup(&[FEATURE]).await;
    let a = store_bytes(&core, "n.txt", b"notes", &owner())
        .await
        .unwrap();
    control::set_a2a_blackout(&core, true, "t", "owner")
        .await
        .unwrap();
    let e = queue_prompt(&core, "s-n1", "look", vec![a.clone()], "run:r1")
        .await
        .unwrap_err();
    assert!(e.to_string().contains("A2A withheld"), "{e}");
    // The owner is not an agent.
    assert!(
        queue_prompt(&core, "s-n1", "look", vec![a.clone()], "owner:cli")
            .await
            .is_ok()
    );
    control::set_a2a_blackout(&core, false, "t", "owner")
        .await
        .unwrap();
    assert!(
        queue_prompt(&core, "s-n1", "look", vec![a], "run:r1")
            .await
            .is_ok()
    );
}
