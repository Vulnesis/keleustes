//! Routing cards (vdb-site `factory_engine_route.go`, `feClaimRoute`,
//! `feCrewEntry`, `feCardLabels`; card body after `feCardBody`).
//!
//! belai workers ignore `Dispatch.card_id`: they claim any card in one of
//! their profile's lists that carries every claim label. A crew or worker
//! step therefore files (or routes) a card the worker will claim, then
//! dispatches the worker. The route comes from the agent's profile: a
//! saved profile's `meta_json` (belai format) first, else the bundled
//! builtin catalogue.

use crate::catalogue::{self, ProfileSpec};
use kls_store::Node;
use serde_json::{Value, json};

/// Where a worker looks for work.
#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    /// The worker that will claim the card.
    pub agent: String,
    /// The belai list (`backlog`, `review`, `in_progress`).
    pub list: String,
    /// The Keleustes status that list is stored as.
    pub status: String,
    /// Every label the worker claims by.
    pub labels: Vec<String>,
}

/// The Keleustes status a belai list is stored as (belai
/// `factorylink.statusFor`; `blocked` is `todo` with the belai part saying so).
pub fn status_for_list(list: &str) -> &'static str {
    match list {
        "backlog" => "backlog",
        "in_progress" => "doing",
        "review" => "review",
        "done" => "done",
        "blocked" => "todo",
        _ => "triage",
    }
}

/// The route of a worker profile; `cli_job` adds the `cli-job:<id>` label a
/// CLI runner claims by.
pub fn route_of(p: &ProfileSpec, cli_job: Option<&str>) -> Option<Route> {
    let k = p.kanban.as_ref().filter(|_| p.mode == "worker")?;
    let list = k.lists.first().cloned().unwrap_or_else(|| "backlog".into());
    let mut labels = k.labels.clone();
    if let Some(job) = cli_job.filter(|j| !j.is_empty()) {
        if let Some(cli) = &k.cli
            && !cli.jobs.iter().any(|j| j == job)
        {
            return None;
        }
        labels.push(format!("cli-job:{job}"));
    }
    if labels.is_empty() {
        return None;
    }
    Some(Route {
        agent: p.name.clone(),
        status: status_for_list(&list).into(),
        list,
        labels,
    })
}

/// The route for a step: a worker (`agent`) or a crew's entry member.
/// `saved` resolves a profile saved in Keleustes (its belai `meta_json`).
pub fn route_for(
    dispatch: &str,
    agent: &str,
    crew: &str,
    cli_job: Option<&str>,
    saved: &dyn Fn(&str) -> Option<ProfileSpec>,
) -> Option<Route> {
    let resolve = |name: &str| -> Option<ProfileSpec> {
        saved(name)
            .filter(|p| p.kanban.is_some())
            .or_else(|| catalogue::profile(name).cloned())
    };
    match dispatch {
        "crew" => {
            let entry = catalogue::crew(crew)?.entry()?.to_string();
            route_of(&resolve(&entry)?, cli_job)
        }
        "session" => None,
        _ => route_of(&resolve(agent)?, cli_job),
    }
}

/// The routing card's body: the brief, then where it came from.
pub fn card_body(
    brief: &str,
    workflow: &str,
    run_id: &str,
    step_title: &str,
    repo: &str,
) -> String {
    let mut b = String::new();
    if !brief.trim().is_empty() {
        b.push_str(brief.trim());
        b.push_str("\n\n");
    }
    b.push_str("## Origin\n\n");
    b.push_str(&format!("- Workflow: {workflow}\n"));
    b.push_str(&format!("- Run: {run_id}\n"));
    b.push_str(&format!("- Step: {step_title}\n"));
    if !repo.is_empty() {
        b.push_str(&format!("- Repository: {repo}\n"));
    }
    b
}

/// The card meta a worker reads (belai's part under `belai`) and the
/// provenance Keleustes keeps (under `keleustes`).
pub fn card_meta(r: &Route, run_id: &str, step_id: &str, workflow_id: &str, prior: &str) -> String {
    let mut meta: Value = serde_json::from_str(prior).unwrap_or_else(|_| json!({}));
    if !meta.is_object() {
        meta = json!({});
    }
    if !meta.get("belai").is_some_and(Value::is_object) {
        meta["belai"] = json!({});
    }
    meta["belai"]["list"] = json!(r.list);
    if !meta["belai"].get("agent").is_some_and(Value::is_object) {
        meta["belai"]["agent"] = json!({});
    }
    meta["belai"]["agent"]["labels"] = json!(r.labels);
    meta["keleustes"] = json!({
        "origin": "route", "agent": r.agent, "runId": run_id, "stepId": step_id,
        "workflowId": workflow_id,
    });
    meta.to_string()
}

/// The refusal when every online node that offers the agent lists its
/// trusted dirs, none holds the repository and none can clone it.
pub const NO_HOST_FOR_REPO: &str = "no_host_for_repo";

/// A node to run on: online, offering the agent (or crew) when the node
/// says what it offers, and holding the repository in a trusted directory
/// when it lists any. Best match first; `Err` says why none qualifies.
pub fn pick_node(
    nodes: &[Node],
    online: &[String],
    agent: &str,
    crew: &str,
    repo: &str,
) -> Result<String, String> {
    let repo_tail = repo.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
    let mut best: Vec<(i32, String)> = Vec::new();
    let mut offered_nowhere = true;
    // Whether some qualifying node may hold the repository: it has it in a
    // trusted dir, lists no dirs (it does not say), can clone, or is unknown.
    let mut repo_somewhere = repo_tail.is_empty();
    for id in online {
        let Some(n) = nodes.iter().find(|n| &n.id == id) else {
            best.push((0, id.clone()));
            offered_nowhere = false;
            repo_somewhere = true;
            continue;
        };
        // A worker profile is advertised as a route (`offers_agent`).
        let (want, any, has) = if !crew.is_empty() {
            (
                crew,
                !n.host.crews.is_empty(),
                n.host.crews.iter().any(|c| c == crew),
            )
        } else {
            (
                agent,
                kls_core::belai::advertises_agents(&n.host),
                kls_core::belai::offers_agent(&n.host, agent),
            )
        };
        let offers = if want.is_empty() || !any {
            Some(false)
        } else if has {
            Some(true)
        } else {
            None
        };
        let Some(offers) = offers else { continue };
        offered_nowhere = false;
        let has_repo = !repo_tail.is_empty()
            && n.host.trusted_dirs.iter().any(|d| {
                d.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .is_some_and(|t| t.eq_ignore_ascii_case(&repo_tail))
            });
        if has_repo || n.host.trusted_dirs.is_empty() || n.host.capabilities.can_clone {
            repo_somewhere = true;
        }
        let score = i32::from(offers) * 2 + i32::from(has_repo);
        best.push((score, id.clone()));
    }
    if online.is_empty() {
        return Err("no node is online".into());
    }
    if offered_nowhere {
        let what = if crew.is_empty() { agent } else { crew };
        return Err(format!("no online node offers {what}"));
    }
    if !repo_somewhere {
        return Err(format!(
            "{NO_HOST_FOR_REPO}: no online node holds {repo} or can clone it"
        ));
    }
    best.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    best.into_iter()
        .next()
        .map(|(_, id)| id)
        .ok_or_else(|| "no node qualifies".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kls_store::HostInfo;

    fn none(_: &str) -> Option<ProfileSpec> {
        None
    }

    #[test]
    fn every_builtin_worker_routes_to_its_claim() {
        for p in catalogue::profiles().iter().filter(|p| p.is_worker()) {
            let k = p.kanban.as_ref().unwrap();
            let job = k
                .cli
                .as_ref()
                .and_then(|c| c.jobs.first())
                .map(String::as_str);
            let r = route_for("worker", &p.name, "", job, &none)
                .unwrap_or_else(|| panic!("{} has no route", p.name));
            assert_eq!(&r.list, k.lists.first().unwrap(), "{}", p.name);
            for l in &k.labels {
                assert!(r.labels.contains(l), "{} lacks {l}", p.name);
            }
            assert!(crate::graph::CARD_STATUSES.contains(&r.status.as_str()));
        }
        // Background agents never route.
        assert!(route_for("worker", "belai:deps-go", "", None, &none).is_none());
        // A crew routes to its first member.
        let r = route_for("crew", "", "belai:bug", None, &none).unwrap();
        assert_eq!(
            (r.agent.as_str(), r.labels.clone()),
            ("belai:repro", vec!["repro".to_string()])
        );
        let r = route_for("crew", "", "maintenance", None, &none).unwrap();
        assert_eq!(r.agent, "belai:test-fixer");
        // The reviewer claims from review.
        let r = route_for("worker", "belai:reviewer", "", None, &none).unwrap();
        assert_eq!((r.list.as_str(), r.status.as_str()), ("review", "review"));
        // A CLI runner job outside its list does not route.
        assert!(route_for("worker", "belai:cli-runner", "", Some("fix"), &none).is_none());
        let r = route_for("worker", "belai:cli-runner", "", Some("sca"), &none).unwrap();
        assert_eq!(r.labels, ["cli-job:sca"]);
    }

    #[test]
    fn saved_profiles_win_and_meta_keeps_unknown_keys() {
        let custom = |n: &str| {
            (n == "acme:fixer").then(|| ProfileSpec {
                name: "acme:fixer".into(),
                mode: "worker".into(),
                kanban: Some(catalogue::KanbanSpec {
                    lists: vec!["backlog".into()],
                    labels: vec!["acme-fix".into()],
                    ..Default::default()
                }),
                ..Default::default()
            })
        };
        let r = route_for("worker", "acme:fixer", "", None, &custom).unwrap();
        assert_eq!(r.labels, ["acme-fix"]);
        let m = card_meta(&r, "r1", "s1", "w1", r#"{"belai":{"hostId":"h"},"x":1}"#);
        let v: Value = serde_json::from_str(&m).unwrap();
        assert_eq!(v["x"], 1);
        assert_eq!(v["belai"]["hostId"], "h");
        assert_eq!(v["belai"]["list"], "backlog");
        assert_eq!(v["belai"]["agent"]["labels"][0], "acme-fix");
        assert_eq!(v["keleustes"]["runId"], "r1");
        assert!(card_body("Do it", "WF", "r1", "Build", "acme/api").contains("- Run: r1"));
    }

    #[test]
    fn node_choice_prefers_offer_and_repo() {
        let node = |id: &str, agents: &[&str], dirs: &[&str]| Node {
            id: id.into(),
            name: id.into(),
            host: HostInfo {
                agents: agents.iter().map(|s| s.to_string()).collect(),
                trusted_dirs: dirs.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            cert_serial: String::new(),
            created_at: 0,
            last_seen_at: None,
            revoked_at: None,
            revoked_reason: None,
            node_seq: 0,
            server_acked: 0,
        };
        let nodes = vec![
            node("a", &[], &[]),
            node("b", &["belai:builder"], &["/src/other"]),
            node("c", &["belai:builder"], &["/src/api/"]),
            node("d", &["belai:planner"], &["/src/api"]),
        ];
        let on: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            pick_node(&nodes, &on, "belai:builder", "", "acme/api").unwrap(),
            "c"
        );
        assert_eq!(
            pick_node(&nodes, &on[..1], "belai:builder", "", "").unwrap(),
            "a"
        );
        assert_eq!(
            pick_node(&nodes, &on[3..], "belai:builder", "", "").unwrap_err(),
            "no online node offers belai:builder"
        );
        assert_eq!(
            pick_node(&nodes, &[], "x", "", "").unwrap_err(),
            "no node is online"
        );
        // Every node that offers the agent lists its dirs and none holds the
        // repository: refused, not ranked.
        let held = vec![
            node("b", &["belai:builder"], &["/src/other"]),
            node("c", &["belai:builder"], &["/src/api"]),
        ];
        let bc: Vec<String> = ["b", "c"].iter().map(|s| s.to_string()).collect();
        let err = pick_node(&held, &bc[..1], "belai:builder", "", "acme/api").unwrap_err();
        assert!(err.starts_with(NO_HOST_FOR_REPO), "{err}");
        // A node that can clone may take it.
        let mut cl = held.clone();
        cl[0].host.capabilities.can_clone = true;
        assert_eq!(
            pick_node(&cl, &bc[..1], "belai:builder", "", "acme/api").unwrap(),
            "b"
        );
        assert_eq!(
            pick_node(&held, &bc, "belai:builder", "", "acme/api").unwrap(),
            "c"
        );
        // A worker profile is advertised as a route, not a session agent
        // (belai's HostAdvert.agent_routes): the node offers it.
        let mut w = node("w", &["belai:debug"], &[]);
        w.host.capabilities.agent_routes = vec![kls_store::AgentRoute {
            name: "belai:scout".into(),
            mode: "worker".into(),
            lists: vec!["backlog".into()],
            labels: vec!["scout".into()],
            read_only: true,
        }];
        let nodes = vec![node("d", &["belai:planner"], &[]), w];
        let on: Vec<String> = ["d", "w"].iter().map(|s| s.to_string()).collect();
        assert_eq!(pick_node(&nodes, &on, "belai:scout", "", "").unwrap(), "w");
        assert_eq!(
            pick_node(&nodes, &on[..1], "belai:scout", "", "").unwrap_err(),
            "no online node offers belai:scout"
        );
    }
}
