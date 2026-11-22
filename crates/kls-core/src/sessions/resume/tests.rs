//! P3 rows 45, 46, 51, 52, 55: resume and fork, console session actions,
//! relay review answers, answers / gates / one-off workers. Each proves
//! provenance, the audit line, the kill switch and (where an agent can ask)
//! the A2A blackout.

use super::*;
use crate::belai::{Accepts, AskOption, Checkpoint, ExtrasUpdate, PendingAsk};
use crate::gateway::{AdhocWorker, JobNeeds, JobOutcome, NodeGateway};
use crate::sessions::Session;
use crate::sessions::start::{self, StartFields};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Gw {
    sent: Mutex<Vec<(String, DispatchSpec)>>,
}

#[async_trait::async_trait]
impl NodeGateway for Gw {
    async fn online(&self) -> Vec<String> {
        vec!["n1".into()]
    }
    async fn dispatch(&self, node_id: &str, spec: DispatchSpec) -> Result<String> {
        let mut s = self.sent.lock().unwrap();
        s.push((node_id.into(), spec));
        Ok(format!("d{}", s.len()))
    }
    async fn job(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: JobNeeds,
        _: i64,
    ) -> Result<JobOutcome> {
        anyhow::bail!("no jobs")
    }
}

async fn setup() -> (Core, Arc<Gw>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    let gw = Arc::new(Gw::default());
    core.set_gateway(gw.clone());
    (core, gw, dir)
}

fn owner() -> Origin {
    Origin {
        actor: "owner".into(),
        surface: "sessions/resume".into(),
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

/// A session of node n1 (ended unless `open`), with the given accepts.
async fn session(core: &Core, id: &str, open: bool, allowed: &[&str]) {
    core.session_upsert(
        "n1",
        Session {
            id: id.into(),
            cwd: "/work/api".into(),
            provider: "openai".into(),
            model: "test".into(),
            mode: "agent".into(),
            state: "idle".into(),
            kind: "rc".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    core.session_extras_apply(
        "n1",
        id,
        ExtrasUpdate {
            accepts: Some(Accepts {
                prompts: true,
                answers: true,
                controls: true,
                controls_allowed: allowed.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }),
            checkpoints: Some(vec![Checkpoint {
                id: "cp1".into(),
                label: "write the marker".into(),
                entry_id: "e1".into(),
                files: 1,
                ..Default::default()
            }]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    if !open {
        core.session_end("n1", id, 10, "completed").await.unwrap();
    }
}

#[tokio::test]
async fn resume_and_fork_carry_provenance_and_audit() {
    let (core, gw, _d) = setup().await;
    session(&core, "s1", false, &[]).await;
    let id = resume(
        &core,
        Resume {
            session_id: "s1".into(),
            ..Default::default()
        },
        &owner(),
    )
    .await
    .unwrap();
    let id2 = resume(
        &core,
        Resume {
            session_id: "s1".into(),
            fork_entry: "e1".into(),
            prompt: "try again".into(),
            ..Default::default()
        },
        &owner(),
    )
    .await
    .unwrap();
    let sent = gw.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    let (node, r) = &sent[0];
    assert_eq!(node, "n1");
    assert_eq!(
        (r.kind.as_str(), r.cwd.as_str(), r.resume_session.as_str()),
        ("session", "/work/api", "s1")
    );
    assert_eq!(
        (r.prompt.as_str(), r.model.as_str()),
        (DEFAULT_PROMPT, "test")
    );
    let t = r.trigger.as_ref().unwrap();
    assert_eq!(
        (t.source.as_str(), t.subject.as_str()),
        ("console", "resume:s1")
    );
    assert!(r.origin_json.contains("\"resumeOf\":\"s1\""));
    let f = &sent[1].1;
    assert_eq!(f.fork_entry, "e1");
    assert_eq!(f.trigger.as_ref().unwrap().subject, "fork:s1@e1");
    let lines = ledger(&core, "dispatch").await;
    for (id, what) in [(&id, "resume"), (&id2, "fork")] {
        assert!(
            lines.iter().any(|e| e.payload["outcome"] == "dispatched"
                && e.payload["action"] == what
                && e.subject["dispatch"] == id.as_str()
                && e.payload["origin"]["surface"] == "sessions/resume"),
            "{what} audited"
        );
    }
    assert_eq!(start::features_needed(r), ["resume"]);
    assert_eq!(start::features_needed(f), ["resume", "fork"]);
}

#[tokio::test]
async fn resume_refused_under_the_switch_blackout_and_while_open() {
    let (core, gw, _d) = setup().await;
    session(&core, "s1", false, &[]).await;
    session(&core, "s2", true, &[]).await;
    let r = || Resume {
        session_id: "s1".into(),
        ..Default::default()
    };
    for stop in [true, false] {
        if stop {
            control::stop(&core, "t").await.unwrap();
        } else {
            control::pause(&core, "t").await.unwrap();
        }
        let e = resume(&core, r(), &owner()).await.unwrap_err();
        assert!(e.to_string().contains("kill switch"), "{e}");
        control::resume(&core).await.unwrap();
    }
    // An agent's request is held by the A2A blackout; the owner's is not.
    control::set_a2a_blackout(&core, true, "t", "owner")
        .await
        .unwrap();
    let agent = Origin {
        actor: "run:r1".into(),
        surface: "workflow:w1".into(),
        ..Default::default()
    };
    assert!(resume(&core, r(), &agent).await.is_err());
    control::set_a2a_blackout(&core, false, "t", "owner")
        .await
        .unwrap();
    // A running session is not resumed.
    let e = resume(
        &core,
        Resume {
            session_id: "s2".into(),
            ..Default::default()
        },
        &owner(),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("still running"), "{e}");
    assert!(gw.sent.lock().unwrap().is_empty());
    let refused = ledger(&core, "dispatch")
        .await
        .into_iter()
        .filter(|e| e.payload["outcome"] == "refused")
        .count();
    assert_eq!(refused, 4);
}

#[tokio::test]
async fn execute_plan_needs_a_plan_and_names_it() {
    let (core, gw, _d) = setup().await;
    session(&core, "s1", false, &[]).await;
    assert!(execute_plan(&core, "s1", &owner()).await.is_err());
    let sha = "ab".repeat(32);
    core.session_extras_apply(
        "n1",
        "s1",
        ExtrasUpdate {
            plan_sha256: Some(sha.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    execute_plan(&core, "s1", &owner()).await.unwrap();
    let sent = gw.sent.lock().unwrap().clone();
    let s = &sent[0].1;
    assert_eq!(
        (s.mode.as_str(), s.prompt.as_str()),
        ("agent", EXECUTE_PROMPT)
    );
    assert_eq!(s.trigger.as_ref().unwrap().subject, format!("plan:{sha}"));
}

#[tokio::test]
async fn console_actions_are_checked_queued_and_audited() {
    let (core, _gw, _d) = setup().await;
    session(
        &core,
        "s1",
        true,
        &["compact", "undo", "rewind", "btw", "rename"],
    )
    .await;
    // Listed and well formed: queued, audited with the text's hash only.
    core.queue_command_from(
        "s1",
        "btw",
        &["what is the secret plan".into()],
        "owner:cli",
    )
    .await
    .unwrap();
    core.queue_command_from("s1", "rewind", &["cp1".into()], "owner")
        .await
        .unwrap();
    // Not listed, bad arguments, an unknown checkpoint.
    for (name, args) in [
        ("relay", vec![]),
        ("rename", vec![]),
        ("undo", vec!["x".to_string()]),
        ("rewind", vec!["cp9".to_string()]),
    ] {
        assert!(
            core.queue_action("s1", name, &args, "owner").await.is_err(),
            "{name} {args:?}"
        );
    }
    // The kill switch refuses new work but lets undo and rewind put files back.
    control::stop(&core, "t").await.unwrap();
    assert!(
        core.queue_action("s1", "compact", &[], "owner")
            .await
            .is_err()
    );
    core.queue_action("s1", "undo", &[], "owner").await.unwrap();
    control::resume(&core).await.unwrap();
    // An agent's action is held by the A2A blackout.
    control::set_a2a_blackout(&core, true, "t", "owner")
        .await
        .unwrap();
    assert!(
        core.queue_action("s1", "undo", &[], "run:r1")
            .await
            .is_err()
    );
    control::set_a2a_blackout(&core, false, "t", "owner")
        .await
        .unwrap();

    let lines = ledger(&core, "session").await;
    let queued: Vec<_> = lines
        .iter()
        .filter(|e| e.payload["outcome"] == "queued")
        .collect();
    let refused = lines
        .iter()
        .filter(|e| e.payload["outcome"] == "refused")
        .count();
    assert_eq!(queued.len(), 3);
    assert_eq!(refused, 6);
    let btw = queued
        .iter()
        .find(|e| e.subject["action"] == "btw")
        .unwrap();
    assert_eq!(btw.actor, "owner:cli");
    assert!(btw.payload["args"]["sha256"].is_string());
    assert!(
        !serde_json::to_string(&lines)
            .unwrap()
            .contains("secret plan")
    );
    let inbox = core.store().inbox_unacked_after("s1", 0).await.unwrap();
    assert_eq!(inbox.len(), 3);
}

#[tokio::test]
async fn relay_review_answers() {
    let (core, _gw, _d) = setup().await;
    session(&core, "s1", true, &["relay"]).await;
    let ask = PendingAsk {
        id: "r1".into(),
        kind: "relay_review".into(),
        title: "relay".into(),
        options: ["approve", "edit", "drop"]
            .into_iter()
            .map(|o| AskOption {
                id: o.into(),
                label: o.into(),
            })
            .collect(),
        relay_json: r#"{"text":"the draft\nline two","direction":""}"#.into(),
        ..Default::default()
    };
    core.session_extras_apply(
        "n1",
        "s1",
        ExtrasUpdate {
            pending_ask: Some(ask),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let x = core.store().session_extras("s1").await.unwrap().unwrap();
    assert!(x.pending_ask.unwrap().relay_json.contains("the draft"));
    // An edit carries the note, lines kept; an empty edit is refused.
    assert!(
        core.queue_ask_answer("s1", "r1", "edit", "", " ")
            .await
            .is_err()
    );
    let item = core
        .queue_ask_answer("s1", "r1", "edit", "", "new note\nsecond line")
        .await
        .unwrap();
    assert!(item.payload_json.contains("new note\\nsecond line"));
    // Under stop the relay does not go out; dropping it still does.
    control::stop(&core, "t").await.unwrap();
    assert!(
        core.queue_ask_answer("s1", "r1", "approve", "", "")
            .await
            .is_err()
    );
    core.queue_ask_answer("s1", "r1", "drop", "", "")
        .await
        .unwrap();
}

#[tokio::test]
async fn answers_gates_and_one_off_workers() {
    let (core, gw, _d) = setup().await;
    let o = owner();
    let session = DispatchSpec {
        kind: "session".into(),
        cwd: "/w".into(),
        ..Default::default()
    };
    let mut gates = std::collections::BTreeMap::new();
    gates.insert("finish_review".to_string(), "on".to_string());
    gates.insert("commit_mode".to_string(), "off".to_string());
    let notes = r#"[{"choose":1,"note":"use the staging db"},{"skip":true}]"#;
    let f = StartFields {
        answers_json: notes.into(),
        gates: gates.clone(),
        ..Default::default()
    };
    start::dispatch(&core, "n1", session.clone(), &f, &o)
        .await
        .unwrap();
    // Wrong shapes and loosening values are refused here.
    for bad in [
        StartFields {
            answers_json: r#"[{"pick":1}]"#.into(),
            ..Default::default()
        },
        StartFields {
            gates: [("finish_review".to_string(), "off".to_string())].into(),
            ..Default::default()
        },
    ] {
        assert!(
            start::dispatch(&core, "n1", session.clone(), &bad, &o)
                .await
                .is_err()
        );
    }
    let prompt = "Triage every flaky test; never touch the vault";
    let adhoc = AdhocWorker {
        prompt: prompt.into(),
        tools: vec!["Read".into(), "Grep".into()],
        claim_lists: vec!["backlog".into()],
        ..Default::default()
    };
    let w = StartFields {
        adhoc: Some(adhoc.clone()),
        ..Default::default()
    };
    let worker = DispatchSpec {
        kind: "worker".into(),
        cwd: "/w".into(),
        ..Default::default()
    };
    start::dispatch(&core, "n1", worker.clone(), &w, &o)
        .await
        .unwrap();
    // Not with an agent profile, not on a session.
    let named = DispatchSpec {
        agent: "belai:scout".into(),
        ..worker.clone()
    };
    assert!(start::dispatch(&core, "n1", named, &w, &o).await.is_err());
    assert!(
        start::dispatch(&core, "n1", session.clone(), &w, &o)
            .await
            .is_err()
    );
    // The kill switch refuses all three.
    control::stop(&core, "t").await.unwrap();
    assert!(start::dispatch(&core, "n1", worker, &w, &o).await.is_err());
    control::resume(&core).await.unwrap();

    let sent = gw.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].1.answers_json, notes);
    assert_eq!(sent[0].1.options.get("finish_review").unwrap(), "on");
    assert_eq!(start::features_needed(&sent[0].1), ["answers", "gates"]);
    assert_eq!(sent[1].1.adhoc.as_ref().unwrap(), &adhoc);
    assert_eq!(start::features_needed(&sent[1].1), ["adhoc"]);
    let lines = ledger(&core, "dispatch").await;
    let text = serde_json::to_string(&lines).unwrap();
    assert!(!text.contains("staging db") && !text.contains("flaky test"));
    assert!(text.contains(&start::sha256_hex(prompt)));
    assert!(text.contains(&start::sha256_hex(notes)));
    assert_eq!(
        lines
            .iter()
            .filter(|e| e.payload["outcome"] == "refused")
            .count(),
        5
    );
}
