//! End-to-end driver for belai's `e2e/keleustes_features_test.go`.
//!
//! It opens the same data directory as a running `keleustes --headless`
//! and calls the library functions the screens call (workflows, runs,
//! defaults, budgets, vault, library search, project overrides). Events it
//! emits reach the running app through its event tail, so the app's engine,
//! gateway and audit fold do the work, exactly as for a screen action.
//! Every command prints one JSON value on stdout; a failure exits 1 with the
//! error on stderr.
//!
//! `cargo build -p kls-app --example kls_e2e`

use anyhow::{Context, Result, anyhow, bail};
use kls_core::Core;
use kls_core::libsearch::{self as ls, kls_libsearch as lsx};
use kls_engine::{Graph, ops};
use kls_store::audit::LedgerQuery;
use serde_json::{Value, json};
use std::io::Write;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    match rt.block_on(run(args)) {
        Ok(v) => println!("{v}"),
        Err(e) => {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
    }
}

fn arg(a: &[String], i: usize) -> Result<&str> {
    a.get(i)
        .map(String::as_str)
        .ok_or_else(|| anyhow!("missing argument {i}"))
}

fn js(s: &str) -> Result<Value> {
    serde_json::from_str(s).with_context(|| format!("not JSON: {s}"))
}

/// `T::default()` with the fields of the JSON object `s` over it.
macro_rules! over_default {
    ($t:ty, $s:expr) => {{
        let mut base = serde_json::to_value(<$t>::default())?;
        if let (Some(b), Value::Object(o)) = (base.as_object_mut(), js($s)?) {
            b.extend(o);
        }
        serde_json::from_value::<$t>(base)?
    }};
}

async fn run(args: Vec<String>) -> Result<Value> {
    if args.len() < 3 || args[0] != "--data" {
        bail!("usage: kls_e2e --data DIR COMMAND ...");
    }
    let core = Core::open(std::path::Path::new(&args[1]), None).await?;
    // The node gateway (frames go through the outbox the app sweeps).
    let _hub = kls_node::hub(core.clone()).await?;
    let a = &args[3..];
    let s = core.store();
    match args[2].as_str() {
        "catalogue" => {
            let p = kls_engine::catalogue::profiles();
            let c = kls_engine::catalogue::crews();
            Ok(json!({
                "profiles": p.len(), "crews": c.len(),
                "profileNames": p.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
                "crewNames": kls_engine::catalogue::crew_names(),
                "workers": kls_engine::catalogue::worker_names(),
            }))
        }
        "node" => {
            let name = arg(a, 0)?;
            let n = s
                .nodes()
                .await?
                .into_iter()
                .find(|n| n.name == name || n.id == name)
                .ok_or_else(|| anyhow!("no node {name}"))?;
            let routes: Vec<_> = n
                .host
                .capabilities
                .agent_routes
                .iter()
                .map(|r| &r.name)
                .collect();
            Ok(
                json!({"id": n.id, "agents": n.host.agents, "routes": routes,
                "crews": n.host.crews, "lastSeen": n.last_seen_at,
                "jobKinds": n.host.capabilities.job_kinds,
                "forges": n.host.capabilities.forges.iter()
                    .map(|f| format!("{}@{}:{}", f.cli, f.host, f.signed_in)).collect::<Vec<_>>()}),
            )
        }
        "workflow" => {
            let g: Graph = serde_json::from_value(js(arg(a, 0)?)?)?;
            let w = ops::create_workflow(
                &core,
                ops::NewWorkflow {
                    name: a.get(1).cloned().unwrap_or_else(|| "e2e flow".into()),
                    graph: Some(g),
                    ..Default::default()
                },
            )
            .await?;
            ops::publish(&core, &w.id, 1, "e2e").await?;
            Ok(json!({"id": w.id}))
        }
        "start" => {
            let out = ops::start_run(
                &core,
                ops::StartRun {
                    workflow_id: arg(a, 0)?.into(),
                    trigger: ops::TriggerInfo {
                        kind: "manual".into(),
                        ..Default::default()
                    },
                    repo: a.get(1).cloned().unwrap_or_default(),
                    ..Default::default()
                },
            )
            .await?;
            let r = out.run().ok_or_else(|| anyhow!("not started: {out:?}"))?;
            Ok(json!({"runId": r.id}))
        }
        "run" => {
            let d = ops::run_detail(&core, arg(a, 0)?)
                .await?
                .ok_or_else(|| anyhow!("no run"))?;
            Ok(json!({
                "status": d.run.status, "reason": d.run.reason, "trigger": d.run.trigger_kind,
                "event": js(&d.run.event_json).unwrap_or_default(),
                "steps": d.steps.iter().map(|st| json!({
                    "nodeId": st.node_id, "type": st.node_type, "status": st.status,
                    "route": st.route, "details": js(&st.details_json).unwrap_or_default(),
                })).collect::<Vec<_>>(),
                "approvals": d.approvals.iter().map(|ap| json!({"id": ap.id, "state": ap.state})).collect::<Vec<_>>(),
            }))
        }
        "runs" => {
            let rs = s
                .workflow_runs_in(
                    arg(a, 0)?,
                    &[
                        "queued",
                        "running",
                        "waiting",
                        "waiting_approval",
                        "paused",
                        "succeeded",
                        "failed",
                        "cancelled",
                    ],
                    None,
                )
                .await?;
            Ok(json!(
                rs.iter()
                    .map(|r| json!({"id": r.id, "status": r.status, "trigger": r.trigger_kind, "reason": r.reason}))
                    .collect::<Vec<_>>()
            ))
        }
        "cards" => {
            let cs = core.cards().await?;
            Ok(json!(cs.iter().map(|c| json!({
                "id": c.id, "key": c.key, "title": c.title, "status": c.status,
                "labels": c.labels, "assignee": c.assignee, "meta": js(&c.meta_json).unwrap_or_default(),
            })).collect::<Vec<_>>()))
        }
        "emit" => {
            let ev = core.emit(arg(a, 0)?, arg(a, 1)?, &js(arg(a, 2)?)?).await?;
            Ok(json!({"id": ev.id}))
        }
        "from-event" => {
            let ev = core.emit(arg(a, 0)?, arg(a, 1)?, &js(arg(a, 2)?)?).await?;
            let w = ops::create_from_event(&core, &ev).await?;
            Ok(json!({"id": w.id, "eventId": ev.id}))
        }
        "publish" => {
            let p = ops::publish(&core, arg(a, 0)?, arg(a, 1)?.parse()?, "e2e").await?;
            Ok(json!({"version": p.version}))
        }
        "managed-repo" => {
            let full = arg(a, 0)?;
            let (owner, name) = full.split_once('/').context("owner/name")?;
            let mut r = kls_store::github::GhRepo {
                full_name: full.into(),
                owner: owner.into(),
                name: name.into(),
                default_branch: "main".into(),
                can_push: true,
                managed: true,
                ..Default::default()
            };
            r.settings.autonomy = a.get(1).cloned().unwrap_or_else(|| "suggest".into());
            s.gh_upsert_repo(&r).await?;
            s.gh_set_repo_managed(full, true).await?;
            let rev = s.gh_repo(full).await?.map(|x| x.revision).unwrap_or(0);
            s.gh_set_repo_settings(full, &r.settings, rev).await?;
            Ok(json!({"repo": full}))
        }
        "setting" => {
            s.set_setting(arg(a, 0)?, arg(a, 1)?).await?;
            Ok(json!({}))
        }
        "seed-defaults" => {
            let r = kls_engine::defaults::seed(&core).await?;
            Ok(serde_json::to_value(r)?)
        }
        "workflows" => {
            let ws = s.workflows(true).await?;
            Ok(json!(
                ws.iter()
                    .map(|w| json!({
                        "id": w.id, "name": w.name, "systemKey": w.system_key, "enabled": w.enabled,
                        "status": w.status, "version": w.published_version,
                    }))
                    .collect::<Vec<_>>()
            ))
        }
        "ledger" => {
            let q = LedgerQuery {
                kinds: a
                    .first()
                    .filter(|k| !k.is_empty())
                    .map(|k| vec![k.clone()])
                    .unwrap_or_default(),
                text: a.get(1).cloned(),
                ..Default::default()
            };
            Ok(serde_json::to_value(core.audit_entries(&q).await?)?)
        }
        "verify" => {
            let v = core.audit_verify().await?;
            Ok(json!({"ok": v.factory().map(|f| f.ok).unwrap_or(false)}))
        }
        "events" => {
            let evs = s
                .events_query(&kls_store::audit::EventQuery {
                    prefixes: vec![arg(a, 0)?.into()],
                    text: a.get(1).cloned(),
                    limit: 500,
                    ..Default::default()
                })
                .await?;
            Ok(json!(
                evs.iter()
                    .map(|e| json!({
                        "id": e.id, "kind": e.kind, "subject": e.subject,
                        "data": js(&e.data_json).unwrap_or_default(),
                    }))
                    .collect::<Vec<_>>()
            ))
        }
        "budget" => {
            let b = over_default!(kls_store::vault::Budget, arg(a, 0)?);
            let b = kls_core::vault::save_budget(&core, &b).await?;
            Ok(json!({"id": b.id}))
        }
        // `kb-search QUERY [SCOPE]`: the Memory › Knowledge search (row 49),
        // fanned out as knowledge_search jobs; answers come through the tail.
        "kb-search" => {
            core.start_event_tail(Duration::from_millis(100));
            let scope = a.get(1).map(String::as_str).unwrap_or("");
            // This process has no links: name the nodes that run searches.
            let nodes: Vec<String> = s
                .nodes()
                .await?
                .into_iter()
                .filter(kls_core::belai::knowledge::searches)
                .map(|n| n.id)
                .collect();
            let r = kls_core::belai::knowledge::search_on(
                &core,
                &nodes,
                arg(a, 0)?,
                scope,
                "",
                10,
                "owner",
            )
            .await?;
            Ok(serde_json::to_value(r)?)
        }
        // `kb-reindex NODE SCOPE KEY`: the Knowledge window's Reindex.
        "kb-reindex" => {
            let id = kls_core::belai::knowledge::reindex(
                &core,
                arg(a, 0)?,
                arg(a, 1)?,
                arg(a, 2)?,
                "owner",
            )
            .await?;
            Ok(json!({"dispatch": id}))
        }
        // `init-draft NODE CWD`: Repositories › Draft AGENTS.md (row 54).
        "init-draft" => {
            core.start_event_tail(Duration::from_millis(100));
            let d = kls_core::belai::initdraft::request(&core, arg(a, 0)?, arg(a, 1)?, "", "owner")
                .await?;
            Ok(serde_json::to_value(d)?)
        }
        // `plan-limits NODE`: the windows Keleustes holds for a node (row 48).
        "plan-limits" => Ok(serde_json::to_value(
            core.node_plan_limits(arg(a, 0)?).await?,
        )?),
        // `node-status NODE`: the node's last HostStatus (workers, sessions).
        "node-status" => {
            let st = core
                .node_statuses()
                .await?
                .into_iter()
                .find(|(n, _, _)| n == arg(a, 0).unwrap_or(""))
                .map(|(_, _, s)| s)
                .unwrap_or_default();
            Ok(serde_json::to_value(st)?)
        }
        "provider" => {
            let p = over_default!(kls_store::vault::Provider, arg(a, 0)?);
            kls_core::vault::save_provider(&core, &p).await?;
            Ok(json!({"id": p.id}))
        }
        "vault-save" => vault_save(&core, a).await,
        "secrets" => {
            let now = kls_store::now_ms();
            let ms = kls_core::vault::list_secrets(&core).await?;
            Ok(json!(
                ms.iter()
                    .map(|m| json!({
                        "id": m.id, "provider": m.provider, "env": m.env,
                        "state": kls_core::vault::probe::effective(m, now).state.as_str(),
                        "status": m.test_status,
                    }))
                    .collect::<Vec<_>>()
            ))
        }
        "lost" => {
            let node = arg(a, 0)?;
            let off = s
                .events_query(&kls_store::audit::EventQuery {
                    prefixes: vec![kls_core::kinds::NODE_OFFLINE.into()],
                    limit: 500,
                    ..Default::default()
                })
                .await?
                .into_iter()
                .find(|e| e.subject == node)
                .ok_or_else(|| anyhow!("no node.offline event for {node}"))?;
            let n = kls_core::sweeps::mark_lost(&core, node, off.id).await?;
            Ok(json!({"marked": n, "offlineEvent": off.id}))
        }
        "libsearch" => {
            let mut q = lsx::Query::text(arg(a, 0)?);
            q.limit = 50;
            let r = ls::search(&core, &q).await?;
            Ok(json!({
                "live": r.live, "offline": r.offline, "stopped": r.stopped,
                "hits": r.hits.iter().map(|h| json!({
                    "source": h.source, "name": h.name, "type": h.item_type,
                    "locator": h.locator.to_json(), "licence": h.licence.spdx,
                })).collect::<Vec<_>>(),
            }))
        }
        "libimport" => lib_import(&core, a).await,
        // `forge-api NODE PATH`: one github.com API GET through NODE, by the
        // selector's own rule (a `forge` job when the node runs them, else
        // the old `github` job). The answer arrives through the event tail.
        "forge-api" => {
            core.start_event_tail(Duration::from_millis(100));
            let node = s
                .node(arg(a, 0)?)
                .await?
                .ok_or_else(|| anyhow!("no node"))?;
            let conn = kls_forge::Connection::github_com();
            let sel = kls_forge::Selector::new(core.clone(), kls_forge::LocalGh::default());
            let (_, cli) = sel
                .node_runner(&conn, &node)
                .ok_or_else(|| anyhow!("node {} cannot reach github.com", node.name))?;
            let job = if kls_forge::select::node_reach(&node.host.capabilities, &conn) == Some(true)
            {
                "forge"
            } else {
                "github"
            };
            let out = cli.run(&["api".into(), arg(a, 1)?.into()], None).await?;
            Ok(json!({"job": job, "exit": out.exit, "stdout": out.stdout, "stderr": out.stderr}))
        }
        "dispatch" => {
            let d = s
                .dispatch(arg(a, 0)?)
                .await?
                .ok_or_else(|| anyhow!("no dispatch"))?;
            Ok(json!({"status": d.status, "error": d.error, "kind": d.kind,
                "sessionId": d.session_id, "nodeId": d.node_id}))
        }
        "libsources" => {
            let st = ls::status(&core).await?;
            Ok(json!({
                "stopped": st.stopped, "skillsSh": st.skills_sh,
                "sources": st.sources.iter().map(|s| json!({
                    "id": s.id, "on": s.on, "remote": s.mode.remote(), "needsGh": s.needs_gh,
                })).collect::<Vec<_>>(),
            }))
        }
        "librefresh" => {
            let r = ls::refresh(&core, a.first().map(String::as_str)).await?;
            Ok(
                json!({"refreshed": r.refreshed, "notModified": r.not_modified,
                "failed": r.failed, "added": r.added}),
            )
        }
        "prefs" => {
            let mut set = std::collections::BTreeMap::new();
            for kv in &a[2..] {
                let (k, v) = kv.split_once('=').context("key=value")?;
                set.insert(k.to_string(), v.to_string());
            }
            let id = kls_core::audit::overrides::set_project_prefs(
                &core,
                arg(a, 0)?,
                arg(a, 1)?,
                &set,
                &[],
                "owner",
            )
            .await?;
            Ok(json!({"dispatchId": id}))
        }
        other => bail!("unknown command {other}"),
    }
}

/// Saves a key the way the Vault screen does and reports how long the save
/// took, then (in this process, as the screen's process would) follows the
/// background test's state until it settles: one JSON line per state.
async fn vault_save(core: &Core, a: &[String]) -> Result<Value> {
    let (provider, env, value) = (arg(a, 0)?, arg(a, 1)?, arg(a, 2)?);
    let t0 = Instant::now();
    let m = kls_core::vault::put_secret(
        core,
        provider,
        env,
        "e2e",
        kls_core::vault::SecretString::new(value.to_string()),
    )
    .await?;
    let save_ms = t0.elapsed().as_millis() as i64;
    let first = kls_core::vault::probe::effective(&m, kls_store::now_ms()).state;
    println!(
        "{}",
        json!({"saved": m.id, "saveMs": save_ms, "state": first.as_str()})
    );
    let _ = std::io::stdout().flush();
    let mut states = vec![first.as_str().to_string()];
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let cur = core
            .store()
            .vault_secret_meta(&m.id)
            .await?
            .context("key vanished")?;
        let st = kls_core::vault::probe::effective(&cur, kls_store::now_ms()).state;
        if states.last().map(String::as_str) != Some(st.as_str()) {
            states.push(st.as_str().to_string());
        }
        let settled = !matches!(
            st,
            kls_core::vault::probe::ProbeState::Untested
                | kls_core::vault::probe::ProbeState::Testing
        );
        if settled || Instant::now() > deadline {
            return Ok(
                json!({"id": m.id, "states": states, "status": cur.test_status, "detail": cur.test_detail}),
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Search, review (preview) and import one skill whose name contains the
/// second argument (or the first skill hit). Prints the provenance stored.
async fn lib_import(core: &Core, a: &[String]) -> Result<Value> {
    let mut q = lsx::Query::text(arg(a, 0)?);
    q.types = vec![lsx::ItemType::Skill];
    q.limit = 50;
    let r = ls::search(core, &q).await?;
    let want = a.get(1).map(String::as_str).unwrap_or("");
    let mut tried = Vec::new();
    for hit in r.hits.iter().filter(|h| h.name.contains(want)) {
        let p = match ls::preview(core, hit).await {
            Ok(p) => p,
            Err(e) => {
                tried.push(format!("{} {}: {e:#}", hit.source, hit.name));
                continue;
            }
        };
        if p.converted.refused().is_some() {
            tried.push(format!("{} {}: refused", hit.source, hit.name));
            continue;
        }
        let ack = ls::Ack {
            flags: p.converted.review_flags().into_iter().cloned().collect(),
            licence: true,
            rename: None,
        };
        // The switch goes on between the review and the import button.
        if a.iter().any(|x| x == "--stop-before-import") {
            kls_core::control::stop(core, "e2e drill").await?;
        }
        let (id, version) = ls::import(core, &p, &ack).await?;
        let prov = core
            .store()
            .latest_provenance("skill", &id)
            .await?
            .context("no provenance")?;
        let name = core
            .store()
            .library_item(&id)
            .await?
            .map(|i| i.name)
            .unwrap_or_default();
        return Ok(json!({
            "id": id, "name": name, "version": version, "source": prov.source,
            "urls": js(&prov.source_urls_json).unwrap_or_default(),
            "upstreamSha256": prov.upstream_sha256, "docSha256": prov.doc_sha256,
            "auditSeq": prov.audit_seq, "actor": prov.actor,
        }));
    }
    bail!(
        "no importable skill among {} hits (live {:?}): {tried:?}",
        r.hits.len(),
        r.live
    )
}
