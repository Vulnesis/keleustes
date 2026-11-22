//! O-079: the cold-agent AX test. A scripted agent that knows only the
//! binary and `keleustes contract` drives dispatch → ask → answer → resume
//! against a Keleustes serving a fake belai node over real mTLS. Every
//! command it runs comes from the contract's verb synopses or from a
//! `next[]` array; every stdout line is checked against the contract's
//! schemas. A failure here is a contract bug.
//!
//! The same harness proves the rules for the CLI: an audit entry with the
//! surface `cli`, the dispatch's provenance, and refusals under the kill
//! switch and the A2A blackout (exit 6 and 7).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kls_core::Core;
use kls_node::{Hub, hub, pki, run, token};
use kls_proto::v1::node_client::NodeClient;
use kls_proto::v1::sessions_client::SessionsClient;
use kls_proto::v1::{self as pb, inbox_item, node_frame, server_frame};
use rcgen::{CertificateParams, KeyPair};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};

const BIN: &str = env!("CARGO_BIN_EXE_keleustes");

struct Harness {
    hub: Hub,
    addr: String,
    dir: tempfile::TempDir,
    _stop: tokio::sync::oneshot::Sender<()>,
}

async fn start() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let listen = format!("127.0.0.1:{port}");
    core.store()
        .set_setting(kls_core::settings::LISTEN_ADDR, &listen)
        .await
        .unwrap();
    // As `keleustes --headless` does: pick up what CLI processes write.
    core.start_event_tail(Duration::from_millis(100));
    let hub = hub(core).await.unwrap();
    let (stop, rx) = tokio::sync::oneshot::channel();
    let h2 = hub.clone();
    tokio::spawn(async move {
        run(h2, async {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(&listen).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Harness {
        hub,
        addr: listen,
        dir,
        _stop: stop,
    }
}

async fn channel(addr: &str, ca_pem: &str, identity: Option<Identity>) -> Channel {
    let mut tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(ca_pem))
        .domain_name(pki::TLS_SERVER_NAME);
    if let Some(id) = identity {
        tls = tls.identity(id);
    }
    Channel::from_shared(format!("https://{addr}"))
        .unwrap()
        .tls_config(tls)
        .unwrap()
        .connect()
        .await
        .unwrap()
}

/// Joins a node that runs belai rc; returns its id and mTLS channel.
async fn join(h: &Harness) -> (String, Channel) {
    let ca = h.hub.pki_ca_pem();
    let minted = h.hub.mint_token("", token::DEFAULT_TTL_MS).await.unwrap();
    let p = token::Payload::decode(&minted.token).unwrap();
    let key = KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let csr = CertificateParams::default()
        .serialize_request(&key)
        .unwrap();
    let joined = NodeClient::new(channel(&h.addr, &ca, None).await)
        .join(pb::JoinRequest {
            token_id: p.t,
            secret: URL_SAFE_NO_PAD.decode(p.s).unwrap(),
            csr_der: csr.der().to_vec(),
            host: Some(pb::HostAdvert {
                hostname: "worker".into(),
                capabilities: Some(pb::Capabilities {
                    remote_control: true,
                    ..Default::default()
                }),
                trusted_dirs: vec!["/work".into()],
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner();
    let cert = ::pem::encode(&::pem::Pem::new("CERTIFICATE", joined.client_cert_der));
    let id = Identity::from_pem(cert, key.serialize_pem());
    (joined.node_id, channel(&h.addr, &ca, Some(id)).await)
}

async fn next<T>(s: &mut tonic::Streaming<T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), s.message())
        .await
        .expect("stream timed out")
        .unwrap()
        .expect("stream ended")
}

fn entry(index: u64, role: &str, text: &str) -> pb::Entry {
    pb::Entry {
        index,
        role: role.into(),
        text: text.into(),
        at_ms: kls_store::now_ms(),
        meta_json: String::new(),
    }
}

/// The fake belai: takes the dispatch, asks one question, applies the
/// answer and goes idle, then takes the resume prompt and completes.
async fn fake_node(chan: Channel) {
    let (tx, rx) = mpsc::channel(8);
    tx.send(pb::NodeFrame {
        body: Some(node_frame::Body::Hello(pb::Hello::default())),
        ..Default::default()
    })
    .await
    .unwrap();
    let mut down = NodeClient::new(chan.clone())
        .link(ReceiverStream::new(rx))
        .await
        .unwrap()
        .into_inner();
    let d = loop {
        let f = next(&mut down).await;
        if let Some(server_frame::Body::Dispatch(d)) = f.body {
            break d;
        }
    };
    let mut s = SessionsClient::new(chan);
    let sid = "s-cold".to_string();
    let upsert = |state: &str, ask: bool| pb::SessionUpsert {
        session_id: sid.clone(),
        title: "cold agent".into(),
        cwd: d.cwd.clone(),
        kind: "rc".into(),
        state: state.into(),
        dispatch_id: d.id.clone(),
        started_at_ms: 1,
        updated_at_ms: kls_store::now_ms(),
        pending_question_id: if ask { "q1".into() } else { String::new() },
        pending_question: if ask {
            "Run the migration?".into()
        } else {
            String::new()
        },
        pending_ask: ask.then(|| pb::PendingAsk {
            id: "q1".into(),
            kind: "permission".into(),
            title: "Run the migration?".into(),
            tool: "shell".into(),
            options: vec![
                pb::AskOption {
                    id: "allow_once".into(),
                    label: "Allow once".into(),
                },
                pb::AskOption {
                    id: "deny".into(),
                    label: "Deny".into(),
                },
            ],
            questions_json: String::new(),
            asked_at_ms: kls_store::now_ms(),
            relay_json: String::new(),
        }),
        accepts: Some(pb::SessionAccepts {
            prompts: true,
            answers: true,
            controls: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let append = |entries: Vec<pb::Entry>| pb::SessionEntries {
        session_id: sid.clone(),
        entries,
    };
    s.upsert(upsert("live", false)).await.unwrap();
    s.append(append(vec![
        entry(0, "user", &d.prompt),
        entry(1, "assistant", "The change needs a migration."),
    ]))
    .await
    .unwrap();
    s.upsert(upsert("waiting", true)).await.unwrap();

    let mut inbox = s
        .inbox(pb::InboxRequest {
            session_id: sid.clone(),
            after: 0,
        })
        .await
        .unwrap()
        .into_inner();
    let ack = |id: String| pb::InboxAck {
        session_id: sid.clone(),
        item_id: id,
        status: "done".into(),
        detail: String::new(),
    };
    // The answer.
    let item = next(&mut inbox).await;
    let Some(inbox_item::Body::Answer(a)) = &item.body else {
        panic!("expected an answer, got {item:?}");
    };
    assert_eq!(
        (a.question_id.as_str(), a.decision.as_str()),
        ("q1", "allow_once")
    );
    s.upsert(upsert("live", false)).await.unwrap();
    s.ack(ack(item.id.clone())).await.unwrap();
    s.append(append(vec![entry(2, "assistant", "Migration applied.")]))
        .await
        .unwrap();
    s.upsert(upsert("idle", false)).await.unwrap();
    // The resume.
    let item = next(&mut inbox).await;
    let Some(inbox_item::Body::Prompt(p)) = &item.body else {
        panic!("expected a prompt, got {item:?}");
    };
    s.upsert(upsert("live", false)).await.unwrap();
    s.ack(ack(item.id.clone())).await.unwrap();
    s.append(append(vec![
        entry(3, "user", &p.text),
        entry(4, "assistant", "All done."),
    ]))
    .await
    .unwrap();
    s.end(pb::SessionEnd {
        session_id: sid.clone(),
        ended_at_ms: kls_store::now_ms(),
        reason: "completed".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    drop(tx);
}

/// One CLI run: (exit code, stdout lines).
fn keleustes(data: &Path, args: &[String]) -> (i32, Vec<String>) {
    let out = std::process::Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(args)
        .env("RUST_LOG", "warn")
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    (
        out.status.code().unwrap_or(-1),
        stdout.lines().map(str::to_string).collect(),
    )
}

/// Splits a next[] command the way a shell would (single quotes only).
fn words(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in cmd.chars() {
        match c {
            '\'' => {
                quoted = !quoted;
                any = true;
            }
            ' ' if !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The cold agent: knows the contract and nothing else.
struct Agent {
    data: PathBuf,
    contract: Value,
    /// Every command it ran, for the provenance check.
    ran: Vec<String>,
}

impl Agent {
    fn schema(&self, name: &str) -> &Value {
        &self.contract["schemas"][name]
    }

    fn check(&self, name: &str, line: &str) -> Value {
        let v: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}"));
        validate(self.schema(name), &v).unwrap_or_else(|e| panic!("{name} schema: {e}\n{line}"));
        v
    }

    /// Runs argv and returns the envelope (the last line), checking the
    /// exit code against the contract's table.
    fn run(&mut self, argv: Vec<String>) -> (i32, Vec<String>, Value) {
        self.ran.push(argv.join(" "));
        let (code, lines) = keleustes(&self.data, &argv);
        let last = lines.last().expect("an envelope");
        let env = self.check("envelope", last);
        let want = if env["ok"] == true {
            0
        } else {
            let c = env["code"].as_str().unwrap();
            self.contract["errorCodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|x| x["code"] == c)
                .unwrap()["exit"]
                .as_i64()
                .unwrap() as i32
        };
        assert_eq!(code, want, "exit code per the contract for {env}");
        (code, lines, env)
    }

    /// A verb's flags from the contract; panics on a flag it does not list
    /// (no guessed flags).
    fn verb(&self, verb: &str, values: &[(&str, &str)]) -> Vec<String> {
        let v = self.contract["verbs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["verb"] == verb)
            .unwrap_or_else(|| panic!("contract lists no verb {verb}"));
        let args = v["args"].as_array().unwrap();
        let mut argv: Vec<String> = verb.split(' ').map(str::to_string).collect();
        for a in args.iter().filter(|a| a["required"] == true) {
            let name = a["name"].as_str().unwrap();
            assert!(
                values.iter().any(|(k, _)| *k == name),
                "{verb} requires {name}"
            );
        }
        for (k, val) in values {
            let a = args
                .iter()
                .find(|a| a["name"] == *k)
                .unwrap_or_else(|| panic!("{verb} has no flag {k}"));
            let allowed = a["values"].as_array().unwrap();
            assert!(
                allowed.is_empty() || allowed.iter().any(|x| x == *val),
                "{k}={val} not in {allowed:?}"
            );
            if let Some(l) = a["long"].as_str() {
                argv.push(l.to_string());
            }
            argv.push(val.to_string());
        }
        argv.push("--json".into());
        argv
    }

    /// The argv of a next[] command, with '<text>' filled in.
    fn argv_of_next(&self, cmd: &str, text: &str) -> Vec<String> {
        let w = words(cmd);
        assert_eq!(w[0], "keleustes", "{cmd}");
        assert_eq!(w[1], "--data", "next[] keeps the data directory: {cmd}");
        assert_eq!(Path::new(&w[2]), self.data.as_path());
        w[3..]
            .iter()
            .map(|a| {
                if a == "<text>" {
                    text.to_string()
                } else {
                    a.clone()
                }
            })
            .collect()
    }
}

fn pick<'a>(env: &'a Value, starts: &str) -> &'a str {
    env["next"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .find(|n| n.contains(starts))
        .unwrap_or_else(|| panic!("no next[] with {starts:?} in {env}"))
}

/// The contract's own schemas, checked with a tiny draft-2020 subset (the
/// same keywords the contract uses).
fn validate(s: &Value, v: &Value) -> Result<(), String> {
    fn go(root: &Value, s: &Value, v: &Value) -> Result<(), String> {
        let Some(o) = s.as_object() else {
            return Ok(());
        };
        if let Some(r) = o.get("$ref").and_then(Value::as_str) {
            let name = r.trim_start_matches("#/$defs/");
            go(root, &root["$defs"][name], v)?;
        }
        let ty = |t: &str| match t {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "boolean" => v.is_boolean(),
            "null" => v.is_null(),
            "integer" => v.is_i64() || v.is_u64(),
            _ => v.is_number(),
        };
        match o.get("type") {
            Some(Value::String(t)) if !ty(t) => return Err(format!("{v} is not {t}")),
            Some(Value::Array(ts)) if !ts.iter().any(|t| ty(t.as_str().unwrap())) => {
                return Err(format!("{v} is none of {ts:?}"));
            }
            _ => {}
        }
        if let Some(c) = o.get("const")
            && c != v
        {
            return Err(format!("{v} != {c}"));
        }
        if let Some(Value::Array(e)) = o.get("enum")
            && !e.contains(v)
        {
            return Err(format!("{v} not in enum"));
        }
        if let Some(m) = v.as_object() {
            for r in o
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if !m.contains_key(r.as_str().unwrap()) {
                    return Err(format!("missing {r}"));
                }
            }
            for (k, x) in m {
                match o.get("properties").and_then(|p| p.get(k)) {
                    Some(ps) => go(root, ps, x).map_err(|e| format!("{k}: {e}"))?,
                    None if o.get("additionalProperties") == Some(&Value::Bool(false)) => {
                        return Err(format!("unexpected {k}"));
                    }
                    None => {}
                }
            }
        }
        if let (Some(i), Some(a)) = (o.get("items"), v.as_array()) {
            for x in a {
                go(root, i, x)?;
            }
        }
        if let Some(Value::Array(any)) = o.get("anyOf")
            && !any.iter().any(|x| go(root, x, v).is_ok())
        {
            return Err("anyOf".into());
        }
        if let Some(n) = o.get("not")
            && go(root, n, v).is_ok()
        {
            return Err("not".into());
        }
        if let Some(c) = o.get("if") {
            let b = if go(root, c, v).is_ok() {
                o.get("then")
            } else {
                o.get("else")
            };
            if let Some(b) = b {
                go(root, b, v)?;
            }
        }
        Ok(())
    }
    go(s, s, v)
}

async fn cli(h: &Harness, args: &[&str]) -> (i32, Vec<String>) {
    let data = h.dir.path().to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || keleustes(&data, &args))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn cold_agent_dispatch_ask_answer_resume() {
    let h = start().await;
    let (node_id, chan) = join(&h).await;
    let node = tokio::spawn(fake_node(chan));
    let data = h.dir.path().to_path_buf();

    let out = tokio::task::spawn_blocking(move || {
        // 1. Only the contract.
        let (code, lines) = keleustes(&data, &["contract".into(), "--json".into()]);
        assert_eq!(code, 0);
        let env: Value = serde_json::from_str(lines.last().unwrap()).unwrap();
        let mut agent = Agent {
            data,
            contract: env["data"].clone(),
            ran: vec!["contract --json".into()],
        };
        agent.check("envelope", lines.last().unwrap());

        // 2. A node to dispatch to.
        let argv = agent.verb("nodes list", &[]);
        let (_, _, env) = agent.run(argv);
        let node = env["data"]["nodes"][0]["id"].as_str().unwrap().to_string();

        // 3. Dispatch and wait for the ask; the handle line comes first.
        let argv = agent.verb(
            "sessions dispatch",
            &[
                ("node", &node),
                ("cwd", "/work/api"),
                ("prompt", "add the orders table"),
                ("label", "cold-agent"),
                ("wait_for", "ask"),
                ("timeout", "60s"),
            ],
        );
        let (_, lines, env) = agent.run(argv);
        assert!(lines.len() >= 2, "{lines:?}");
        let handle = agent.check("handle", &lines[0]);
        assert_eq!(env["session"]["dispatchId"], handle["dispatchId"]);
        assert_eq!(env["session"]["label"], "cold-agent");
        assert_eq!(env["session"]["agent"], "asking", "{env}");
        assert_eq!(env["session"]["delivery"], "started");
        assert_eq!(env["session"]["verified"], "unknown");

        // 4. Answer with the first option, from next[].
        let first = env["session"]["ask"]["options"][0]["id"].as_str().unwrap();
        let argv = agent.argv_of_next(pick(&env, &format!(" {first}")), "");
        let (_, _, env) = agent.run(argv);
        assert_eq!(env["session"]["ask"]["answerQueued"], true, "{env}");

        // 5. Wait (from next[]) until the turn is over.
        let argv = agent.argv_of_next(pick(&env, "sessions wait"), "");
        let (_, _, env) = agent.run(argv);
        assert_eq!(env["session"]["agent"], "idle", "{env}");

        // 6. Read what is new, then resume with the prompt from next[].
        let argv = agent.argv_of_next(pick(&env, "sessions transcript"), "");
        let (_, lines, _) = agent.run(argv);
        for l in &lines[..lines.len() - 1] {
            agent.check("entry", l);
        }
        let argv = agent.argv_of_next(pick(&env, "sessions prompt"), "continue");
        let (_, _, env) = agent.run(argv);
        let argv = agent.argv_of_next(pick(&env, "sessions wait"), "");
        let (_, _, env) = agent.run(argv);
        assert_eq!(env["session"]["agent"], "completed", "{env}");
        assert_eq!(env["session"]["state"], "ended");
        agent.ran
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(20), node)
        .await
        .unwrap()
        .unwrap();
    assert!(out.len() >= 8, "{out:?}");

    // Audit: each CLI action is on the ledger with the surface cli.
    let core = h.hub.core();
    let cli_entries = core
        .audit_entries(&kls_store::audit::LedgerQuery {
            kinds: vec!["cli".into()],
            limit: 100,
            ..Default::default()
        })
        .await
        .unwrap();
    let verbs: Vec<&str> = cli_entries
        .iter()
        .filter_map(|e| e.payload["verb"].as_str())
        .collect();
    for v in ["sessions.dispatch", "sessions.answer", "sessions.prompt"] {
        assert!(verbs.contains(&v), "{v} audited: {verbs:?}");
    }
    assert!(cli_entries.iter().all(|e| e.payload["surface"] == "cli"));
    // Provenance: the dispatch says who started it and carries the label.
    let d = core.store().dispatches(Some(&node_id), 10).await.unwrap();
    let spec: kls_core::DispatchSpec = serde_json::from_str(&d[0].spec_json).unwrap();
    let t = spec.trigger.unwrap();
    assert_eq!(
        (t.source.as_str(), t.subject.as_str()),
        ("console", "cold-agent")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn refusals_have_their_own_exit_codes_and_are_audited() {
    let h = start().await;
    let (node_id, _chan) = join(&h).await;
    let core = h.hub.core();

    kls_core::control::pause(core, "drill").await.unwrap();
    let (code, lines) = cli(
        &h,
        &[
            "sessions", "dispatch", "--node", &node_id, "--cwd", "/work", "--prompt", "x", "--json",
        ],
    )
    .await;
    let env: Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(
        (code, env["code"].as_str()),
        (6, Some("refused_paused")),
        "{env}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("\"handle\"")),
        "no handle when refused"
    );
    assert!(core.store().dispatches(None, 10).await.unwrap().is_empty());
    // The refusal is on the ledger: the kill switch's event and the CLI's own entry.
    let led = core
        .audit_entries(&kls_store::audit::LedgerQuery {
            kinds: vec!["cli".into()],
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        led.iter()
            .any(|e| e.payload["outcome"] == "refused" && e.payload["code"] == "refused_paused")
    );
    let refused = core.store().recent_events(50).await.unwrap();
    assert!(refused.iter().any(|e| e.kind == "factory.refused"));
    kls_core::control::resume(core).await.unwrap();

    // The A2A blackout: exit 7.
    core.set_a2a_blackout(true, "drill").await.unwrap();
    let (code, lines) = cli(&h, &["sessions", "a2a-send", "s-any", "hello", "--json"]).await;
    let env: Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(
        (code, env["code"].as_str()),
        (7, Some("refused_a2a")),
        "{env}"
    );

    // A wait that cannot be satisfied: exit 5 with a retry in next[].
    core.set_a2a_blackout(false, "").await.unwrap();
    let (code, lines) = cli(
        &h,
        &[
            "sessions",
            "dispatch",
            "--node",
            &node_id,
            "--cwd",
            "/work",
            "--prompt",
            "x",
            "--timeout",
            "1s",
            "--json",
        ],
    )
    .await;
    let env: Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!((code, env["code"].as_str()), (5, Some("timeout")), "{env}");
    assert!(
        lines[0].contains("\"handle\""),
        "the handle survives a timeout"
    );
    assert!(env["next"][0].as_str().unwrap().contains("sessions wait"));
    assert_eq!(env["session"]["agent"], "pending");

    // Usage errors are envelopes too.
    let (code, lines) = cli(&h, &["sessions", "dispatch", "--json"]).await;
    let env: Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!((code, env["code"].as_str()), (2, Some("usage_error")));
}
