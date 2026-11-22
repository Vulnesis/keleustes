//! Executor policy shared by dispatch, job and decision steps: which model a
//! step uses (the step's own, else the routing rules and the per-agent model
//! map), the budget gate before any model work (with the fast tier a node
//! offers and an approval for `escalate_human`), the per-card agent limits a
//! dispatch carries, per-member crew models, the `conflict_cards` output and
//! the System One decision record (daily cap, retention).

use super::*;
use kls_core::vault::{BudgetGate, Spend, SpendScope};

/// Settings key: the most System One decisions in a UTC day (default
/// [`DEFAULT_DECISION_CAP`]); past it a decision goes to a person.
pub const DECISION_CAP: &str = "systemone.daily_cap";
pub const DEFAULT_DECISION_CAP: i64 = 500;

/// The agent limits a dispatch carries (`Dispatch.budget`): Settings >
/// Agents (`agents.*`), overridden by the workflow's limits; the step's
/// token budget as `max_tokens`.
pub fn worker_budget(
    eff: Option<&kls_core::audit::config::Effective>,
    l: &crate::graph::Limits,
    tokens: i64,
) -> kls_core::gateway::WorkerBudget {
    let (mut cost, mut fin, mut att, mut cont, mut passes, mut wall_ms, mut iters) =
        (5.0, 85, 3, 2, 3, 120 * 60_000, 60);
    if let Some(e) = eff {
        cost = e.card_max_cost_usd;
        fin = e.card_finalize_at_pct;
        att = e.card_max_attempts;
        cont = e.card_max_continuations;
        passes = e.card_max_passes;
        wall_ms = e.card_max_wall_ms;
        iters = e.card_max_iterations;
    }
    let u = |v: i64| v.clamp(0, u32::MAX as i64) as u32;
    kls_core::gateway::WorkerBudget {
        max_cost_usd: l.max_cost_usd_per_card.unwrap_or(cost).max(0.0),
        finalize_at: l.finalize_at_pct.unwrap_or(fin).clamp(0, 100) as f64 / 100.0,
        max_continuations: u(l.max_continuations.unwrap_or(cont)),
        max_attempts: u(l.max_attempts_per_card.unwrap_or(att)),
        max_wall: format!("{}m", l.max_wall_minutes.unwrap_or(wall_ms / 60_000).max(0)),
        max_passes: u(l.max_passes.unwrap_or(passes)),
        max_tokens: u(tokens),
        max_iterations: u(l.max_iterations.unwrap_or(iters)),
        ..Default::default()
    }
}

/// The step's `provider/model`, or the step's `provider` + `model`.
fn own_model(node: &Node) -> (String, String) {
    match node.str("model").split_once('/') {
        Some((p, m)) => (p.to_string(), m.to_string()),
        None => (
            node.str("provider").to_string(),
            node.str("model").to_string(),
        ),
    }
}

impl Engine {
    /// The provider and model a step uses, and where they came from:
    /// `step` (its own), else `kls_core::vault::route_for` (routing rules,
    /// the per-agent model map `models.agents`, the profile, the default).
    pub(super) async fn step_model(&self, run: &Run, node: &Node) -> (String, String, String) {
        let (p, m) = own_model(node);
        if !m.is_empty() {
            return (p, m, "step".into());
        }
        let q = kls_core::vault::RouteQuery {
            repo: run.repo.clone(),
            agent: node.str("agent").to_string(),
            ..Default::default()
        };
        match kls_core::vault::route_for(self.core(), &q).await {
            Ok(Some(r)) if !r.model.is_empty() => (r.provider, r.model, r.source),
            _ => (p, m, String::new()),
        }
    }

    /// Per-member models of a crew step: the step's `memberModels`
    /// (`{agent: "provider/model"}`), else the crew member's own `model`,
    /// else the per-agent model map.
    pub(super) async fn member_models(
        &self,
        node: &Node,
    ) -> std::collections::BTreeMap<String, String> {
        let crew = node.str("crew");
        let mut members: Vec<(String, String)> = match self
            .core()
            .store()
            .crews()
            .await
            .ok()
            .and_then(|cs| cs.into_iter().find(|c| c.name == crew || c.id == crew))
        {
            Some(c) => c
                .members
                .into_iter()
                .map(|m| (m.agent, m.model.trim().to_string()))
                .collect(),
            None => crate::catalogue::crew(crew)
                .map(|c| {
                    c.members
                        .iter()
                        .map(|m| (m.profile.clone(), String::new()))
                        .collect()
                })
                .unwrap_or_default(),
        };
        members.dedup_by(|a, b| a.0 == b.0);
        let own = node.config.get("memberModels").and_then(Value::as_object);
        let map = self
            .core()
            .store()
            .setting(kls_core::vault::settings::MODELS_AGENTS)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let mut out = std::collections::BTreeMap::new();
        for (a, member_model) in members {
            if let Some(v) = own.and_then(|o| o.get(&a)).and_then(Value::as_str)
                && !v.trim().is_empty()
            {
                out.insert(a, v.trim().to_string());
            } else if !member_model.is_empty() {
                out.insert(a, member_model);
            } else if let Some((p, m)) = kls_core::vault::agent_model(&map, &a) {
                out.insert(a, format!("{p}/{m}"));
            }
        }
        // Step entries for agents the crew list does not name still count.
        if let Some(o) = own {
            for (k, v) in o {
                if let Some(v) = v.as_str().filter(|v| !v.trim().is_empty()) {
                    out.entry(k.clone()).or_insert_with(|| v.trim().to_string());
                }
            }
        }
        out
    }

    /// The fast model to downgrade to on `node_id`: the gate's choice when
    /// the node offers that provider (or says nothing), else the fast model
    /// of the first provider the node advertises that has one.
    pub(super) async fn fast_on_node(
        &self,
        node_id: &str,
        p: String,
        m: String,
    ) -> (String, String) {
        let Ok(nodes) = self.core().store().nodes().await else {
            return (p, m);
        };
        let Some(n) = nodes.iter().find(|n| n.id == node_id) else {
            return (p, m);
        };
        let caps = &n.host.capabilities;
        if caps.providers.is_empty() || caps.providers.iter().any(|x| x.eq_ignore_ascii_case(&p)) {
            return (p, m);
        }
        for ap in &caps.providers {
            if let Ok(Some(pr)) = self.core().store().provider(ap).await
                && !pr.fast_model.is_empty()
            {
                return (ap.clone(), pr.fast_model);
            }
            if let Some(first) = caps
                .models
                .iter()
                .find(|x| x.provider.eq_ignore_ascii_case(ap))
                .and_then(|x| x.models.first())
            {
                return (ap.clone(), first.clone());
            }
        }
        (p, m)
    }

    /// The run's token cap, then the budgets (`kls_core::vault::budget_gate`)
    /// before model work. `Some((provider, model, tier))` to go ahead (tier
    /// `fast` after a downgrade); `None` when the step was failed, set waiting
    /// or handed to a person. `escalate_human` files a `budget` approval; once
    /// approved the step runs past the gate (`budgetApproved`).
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn budget_check(
        &self,
        run: &Run,
        node: &Node,
        st: &RunStep,
        prior: &Value,
        provider: &str,
        model: &str,
        node_id: &str,
        planned: i64,
    ) -> Result<Option<(String, String, String)>> {
        if let Some(cap) = self
            .load_graph(run)
            .await?
            .limits
            .run_budget_tokens
            .filter(|c| *c > 0)
            && run.tokens + planned > cap
        {
            self.fail(
                st,
                "run_budget_exceeded",
                json!({ "tokens": run.tokens, "planned": planned, "cap": cap }),
            )
            .await?;
            return Ok(None);
        }
        if prior
            .get("budgetApproved")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(Some((provider.into(), model.into(), String::new())));
        }
        let spend = Spend {
            provider: provider.into(),
            model: model.into(),
            agent: node.str("agent").to_string(),
            repo: run.repo.clone(),
            tokens: planned,
            usd: 0.0,
        };
        let scope = SpendScope {
            workflow: run.workflow_id.clone(),
            node: node_id.into(),
        };
        let gate = kls_core::vault::budget_gate_scoped(
            self.core(),
            &spend,
            &scope,
            &format!("run:{}", run.id),
        )
        .await;
        match gate {
            Ok(BudgetGate::Allow) => Ok(Some((provider.into(), model.into(), String::new()))),
            Ok(BudgetGate::Downgrade {
                provider: p,
                model: m,
                ..
            }) => {
                let (p, m) = self.fast_on_node(node_id, p, m).await;
                Ok(Some((p, m, "fast".into())))
            }
            Ok(BudgetGate::Block(h)) => {
                self.fail(
                    st,
                    "budget_exceeded",
                    json!({ "budget": h.name, "pct": h.pct, "reason": h.reason }),
                )
                .await?;
                Ok(None)
            }
            Ok(BudgetGate::Escalate(h)) => {
                // A person decides: approve runs the step past the gate.
                let mut d = if prior.is_object() {
                    prior.clone()
                } else {
                    json!({})
                };
                d["budget"] =
                    json!({ "name": h.name, "pct": h.pct, "action": h.action, "reason": h.reason });
                let mut st = st.clone();
                st.details_json = d.to_string();
                let title = format!(
                    "Budget {} at {:.0}%: let {} go ahead?",
                    h.name, h.pct, node.title
                );
                self.ask_person(run, node, &st, "budget", title).await?;
                Ok(None)
            }
            Ok(BudgetGate::Pause(h)) => {
                // The run pauses; a person resumes it (the step tries again).
                self.wait(
                    st,
                    "deferred:budget",
                    json!({ "budget": h.name, "pct": h.pct, "action": h.action, "reason": h.reason }),
                    Some(now_ms() + 60_000),
                )
                .await?;
                let _ = ops::pause_run(self.core(), &run.id).await;
                Ok(None)
            }
            Err(e) => {
                // Fail closed: budgets that cannot be read stop the work.
                self.fail(st, "budget_error", json!({ "error": format!("{e:#}") }))
                    .await?;
                Ok(None)
            }
        }
    }

    /// The `conflict_cards` output: files a merge-conflict card for the
    /// run's pull request, or for each of up to 20 open pull requests of
    /// the repository (after a push or on a schedule), and looks again at
    /// those GitHub has not worked out yet 1, 3 and 10 minutes later.
    pub(super) async fn conflict_cards(
        &self,
        run: &Run,
        node: &Node,
        st: &RunStep,
        prior: Value,
    ) -> Result<()> {
        use crate::conflicts::{MAX_PULLS, RECHECK_MINUTES};
        let core = self.core();
        if let Some(_why) = guard::held(core, "step").await {
            guard::refuse_switch(
                core,
                "step",
                json!({ "runId": run.id, "stepId": st.id, "workflowId": run.workflow_id }),
            )
            .await?;
            return self
                .wait(st, "deferred:kill_switch", prior, Some(now_ms() + 60_000))
                .await;
        }
        if run.repo.is_empty() {
            return self.fail(st, "no_repo", json!({})).await;
        }
        let need = guard::required_autonomy("conflict_cards");
        let have = guard::effective_autonomy(core, &run.repo).await;
        if guard::autonomy_rank(&have) < guard::autonomy_rank(need) {
            let why = format!("autonomy {have} is below {need}");
            guard::refuse(core, "github", &why, json!({ "runId": run.id, "stepId": st.id, "repo": run.repo, "action": "conflict_cards" }))
                .await?;
            return self
                .succeed(
                    st,
                    "",
                    json!({ "skipped": "autonomy", "required": need, "autonomy": have, "action": "conflict_cards" }),
                )
                .await;
        }
        let mut d = if prior.is_object() { prior } else { json!({}) };
        let started = d
            .get("startedAt")
            .and_then(Value::as_i64)
            .unwrap_or_else(now_ms);
        let round = d.get("recheck").and_then(Value::as_u64).unwrap_or(0) as usize;
        d["startedAt"] = json!(started);
        // Which pull requests: those still unknown from the last look, the
        // run's own, or the repository's open ones.
        let event: Value = serde_json::from_str(&run.event_json).unwrap_or(Value::Null);
        let number = event.pointer("/data/number").and_then(Value::as_i64);
        let s = core.store();
        let pulls: Vec<kls_store::github::GhPull> =
            if let Some(list) = d.get("pending").and_then(Value::as_array) {
                let mut out = Vec::new();
                for n in list.iter().filter_map(Value::as_i64) {
                    if let Some(p) = s.gh_pull(&run.repo, n).await? {
                        out.push(p);
                    }
                }
                out
            } else if let Some(n) = number {
                s.gh_pull(&run.repo, n).await?.into_iter().collect()
            } else {
                let mut open = s.gh_pulls(Some(&run.repo), Some("open")).await?;
                open.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
                open.truncate(MAX_PULLS);
                open
            };
        let origin = Origin::engine(json!({
            "runId": run.id, "stepId": st.id, "workflowId": run.workflow_id, "node": node.id,
        }));
        let mut filed: Vec<Value> = d
            .get("filed")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut pending = Vec::new();
        let mut checked = d.get("checked").and_then(Value::as_i64).unwrap_or(0);
        for p in &pulls {
            if p.state != "open" {
                continue;
            }
            checked += 1;
            match p.mergeable.as_str() {
                "conflict" => {
                    let data = crate::conflicts::pull_data(p);
                    if let Some(card) = crate::conflicts::file_card(core, &data, &origin).await? {
                        filed.push(json!({ "number": p.number, "cardId": card }));
                    }
                }
                "clean" => {}
                _ => pending.push(p.number),
            }
        }
        d["filed"] = json!(filed);
        d["checked"] = json!(checked);
        let next = round + 1;
        if !pending.is_empty() && next < RECHECK_MINUTES.len() {
            d["pending"] = json!(pending);
            d["recheck"] = json!(next);
            let at = started + RECHECK_MINUTES[next] * 60_000;
            return self
                .wait(st, "deferred:recheck", d, Some(at.max(now_ms())))
                .await;
        }
        if let Some(o) = d.as_object_mut() {
            o.remove("pending");
        }
        d["unknown"] = json!(pending);
        self.succeed(st, "", d).await
    }

    /// Stores one System One decision (rationale, confidence, model), adds
    /// it to the day's decision stats and drops rows past
    /// `systemone.retention`.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn record_decision(
        &self,
        run: &Run,
        node: &Node,
        st: &RunStep,
        a: &crate::decision::Applied,
        action: &str,
        outcome: &str,
        node_ref: &str,
        latency_ms: i64,
    ) -> Result<()> {
        let s = self.core().store();
        let now = now_ms();
        s.insert_engine_decision(&kls_store::engine::EngineDecision {
            id: new_id(),
            at: now,
            run_id: run.id.clone(),
            step_id: st.id.clone(),
            workflow_id: run.workflow_id.clone(),
            node_id: node.id.clone(),
            question: node.str("question").chars().take(500).collect(),
            choice: a.choice.clone(),
            outcome: outcome.into(),
            action: action.into(),
            why: a.why.into(),
            confidence: a.confidence,
            threshold: a.threshold,
            provider: a.provider.clone(),
            model: a.model.clone(),
            node_ref: node_ref.into(),
            latency_ms,
        })
        .await?;
        let end = match action {
            "route" if a.why == "chosen" => kls_store::github::DecisionEnd::Proceeded,
            "route" => kls_store::github::DecisionEnd::RuleHandled,
            _ => kls_store::github::DecisionEnd::Escalated,
        };
        let model = if a.model.is_empty() {
            "systemone"
        } else {
            a.model.as_str()
        };
        s.record_decision(
            now,
            "systemone",
            model,
            end,
            (a.why != "abstained" && a.why != "daily_cap").then_some(a.confidence),
            Some(latency_ms),
        )
        .await?;
        let days = match kls_core::audit::config::Effective::load(self.core())
            .await
            .map(|e| e.systemone_retention)
            .unwrap_or_default()
            .as_str()
        {
            "30d" => 30,
            "400d" => 400,
            _ => 90,
        };
        s.prune_engine_decisions(now - days * 86_400_000).await?;
        Ok(())
    }

    /// The daily decision cap (`systemone.daily_cap`) and whether today's
    /// decisions reached it.
    pub(super) async fn decision_capped(&self) -> (i64, bool) {
        let cap = self
            .core()
            .store()
            .setting(DECISION_CAP)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.trim().trim_matches('"').parse::<i64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_DECISION_CAP);
        let today = kls_store::github::day_start(now_ms());
        let n = self
            .core()
            .store()
            .count_engine_decisions(today)
            .await
            .unwrap_or(i64::MAX);
        (cap, n >= cap)
    }
}
