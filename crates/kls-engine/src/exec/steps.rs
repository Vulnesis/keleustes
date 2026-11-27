//! Steps that act inside Keleustes, without a forge call (web
//! factory_engine_node_output.go, factory_cli_result.go):
//!
//! - the CLI scan's result read from its card (closed shape, exit codes, the
//!   `<job>:exit-1` harness finding, `failOn`), the rescan branch, the
//!   autonomy a mutating CLI job needs and the TEA switch `tea-release` needs;
//! - `card` with `action: ensure`, the web's `kanban_card` state: make, or
//!   find, the open card the workflow keeps for `ref`;
//! - the `vex` output: an OpenVEX document of the run's findings, kept on
//!   the step and handed to a later `vex-apply` job.
//!
//! Each holds under the kill switch first and writes a ledger entry with
//! its origin.

use super::*;
use crate::forge::outputs;

/// A state card was made or found. Data: `{runId, stepId, cardId, ref, created}`.
pub const RUN_STATE_CARD: &str = "run.state_card";
/// An OpenVEX document was written. Data: `{runId, stepId, statements, status}`.
pub const RUN_VEX: &str = "run.vex";
/// A scan step read its result. Data: `{runId, stepId, job, total, critical, high}`.
pub const RUN_SCAN: &str = "run.scan";

/// Most findings a result may carry (web `factoryCliCleanResult`).
const MAX_FINDINGS: usize = 500;

/// A card result (`belai.agent.cliResult`) as the scan's result, or the
/// step error (web `factoryCliScanSummary`). A missing `job` is taken as
/// the node's (older harnesses leave it out); another job's is a mismatch.
/// Exit 0 and 1 are findings for the next node, 3 is not evaluable (an
/// error unless `report`), anything else fails. A job that exits 1 with no
/// finding and no count yields one harness finding `<job>:exit-1`.
pub fn cli_result(job: &str, doc: &Value, report_not_evaluable: bool) -> Result<Value, String> {
    let Some(r) = doc.as_object() else {
        return Err(if doc.is_null() {
            "cli_no_result".into()
        } else {
            "cli_result_invalid".into()
        });
    };
    let got_job = match r.get("job") {
        None | Some(Value::Null) => job.to_string(),
        Some(Value::String(s)) => s.trim().to_string(),
        Some(_) => return Err("cli_result_invalid".into()),
    };
    if got_job != job {
        return Err("cli_result_mismatch".into());
    }
    let exit = match r.get("exitCode") {
        None | Some(Value::Null) => 0,
        Some(v) => v.as_i64().ok_or("cli_result_invalid")?,
    };
    let counts = match r.get("counts") {
        None | Some(Value::Null) => json!({}),
        Some(Value::Object(c)) => Value::Object(c.clone()),
        Some(_) => return Err("cli_result_invalid".into()),
    };
    let mut findings = Vec::new();
    match r.get("findings") {
        None | Some(Value::Null) => {}
        Some(Value::Array(a)) => {
            if a.len() > MAX_FINDINGS {
                return Err("cli_result_invalid".into());
            }
            for f in a {
                let ok = f.as_object().is_some_and(|o| {
                    o.get("id").and_then(Value::as_str).is_some()
                        && ["kind", "path", "severity", "ref"]
                            .iter()
                            .all(|k| o.get(*k).is_none_or(|v| v.is_string() || v.is_null()))
                });
                if !ok {
                    return Err("cli_result_invalid".into());
                }
                findings.push(f.clone());
            }
        }
        Some(_) => return Err("cli_result_invalid".into()),
    }
    match exit {
        0 | 1 => {}
        3 if !report_not_evaluable => return Err("cli_not_evaluable".into()),
        3 => {}
        n => return Err(format!("cli_exit_{n}")),
    }
    let n = |k: &str| counts.get(k).and_then(Value::as_i64).unwrap_or(0).max(0);
    let mut counts = json!({
        "critical": n("critical"), "high": n("high"), "medium": n("medium"), "low": n("low"),
    });
    let total = ["critical", "high", "medium", "low"]
        .iter()
        .map(|k| n(k))
        .sum::<i64>();
    if exit == 1 && findings.is_empty() && total == 0 {
        findings.push(json!({
            "id": format!("{job}:exit-1"), "kind": "check", "severity": "high", "ref": "exit-1",
        }));
        counts["high"] = json!(1);
    }
    let mut out = r.clone();
    out.insert("job".into(), json!(job));
    out.insert("exitCode".into(), json!(exit));
    out.insert("counts".into(), counts);
    out.insert("findings".into(), Value::Array(findings));
    if exit == 3 {
        out.insert("notEvaluable".into(), json!(true));
    }
    Ok(Value::Object(out))
}

/// The `failOn` kind (`malware`, `kev`, `secrets`) a result hits, if any.
pub fn blocking(result: &Value, fail_on: &[String]) -> Option<String> {
    let kinds: Vec<&str> = result["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["kind"].as_str())
        .collect();
    let has = |set: &[&str]| kinds.iter().any(|k| set.contains(k));
    fail_on
        .iter()
        .find(|f| match f.as_str() {
            "malware" => has(&["malware", "malscan"]),
            "kev" => has(&["kev"]),
            "secrets" => has(&["secret", "secrets"]),
            _ => false,
        })
        .cloned()
}

/// Whether `b` is a branch name a rescan may ask a host to check out.
pub fn branch_ok(b: &str) -> bool {
    !b.is_empty()
        && b.len() <= 200
        && !b.contains("..")
        && !b.starts_with(['-', '/', '.'])
        && !b.ends_with(['/', '.'])
        && !b.ends_with(".lock")
        && b.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.'))
}

/// The OpenVEX document of `f` for `repo` (web `outputVEX`): one statement
/// per finding that is not a secret, `fixed` after a gate, else `affected`.
pub fn vex_doc(run_id: &str, repo: &str, f: &outputs::Findings, fixed: bool, at: &str) -> Value {
    let status = if fixed { "fixed" } else { "affected" };
    let statements: Vec<Value> = f
        .items
        .iter()
        .filter(|i| i.kind != "secret" && i.kind != "secrets")
        .map(|i| {
            json!({
                "vulnerability": {"name": i.id},
                "products": [{"@id": format!("pkg:github/{repo}")}],
                "status": status,
            })
        })
        .collect();
    json!({
        "@context": "https://openvex.dev/ns/v0.2.0",
        "@id": format!("urn:uuid:{run_id}"),
        "author": "Keleustes Software Factory",
        "timestamp": at,
        "version": 1,
        "statements": statements,
    })
}

/// The meta key of a state card: one open card per workflow, repository
/// and `ref`.
pub fn state_key(workflow_id: &str, repo: &str, r: &str) -> String {
    format!(
        "state:{workflow_id}:{}:{}",
        repo.to_lowercase(),
        outputs::safe_ref(r)
    )
}

impl Engine {
    fn origin_of(run: &Run, st: &RunStep, node: &Node) -> Origin {
        Origin::engine(json!({
            "runId": run.id, "stepId": st.id, "workflowId": run.workflow_id, "node": node.id,
        }))
    }

    /// Readies a `scan` node for dispatch: refuses an unknown job, skips a
    /// mutating job below `draft_pr` (audited refusal), points a read-only
    /// rescan at the branch the run's crew left, and hands a `vex-apply`
    /// job the run's OpenVEX document. `Err(())` when the step was settled.
    pub(super) async fn prepare_scan(
        &self,
        run: &Run,
        node: &Node,
        st: &RunStep,
        steps: &[RunStep],
        prior: &Value,
    ) -> Result<std::result::Result<Node, ()>> {
        let job = node.str("job").to_string();
        if !crate::catalogue::cli_jobs().contains(&job.as_str()) {
            self.fail(st, "cli_job_invalid", json!({ "job": job }))
                .await?;
            return Ok(Err(()));
        }
        let mutates = crate::catalogue::cli_write_job(&job);
        let first = prior.get("dispatchId").is_none();
        let mut n = node.clone();
        if mutates && first {
            let have = guard::effective_autonomy(self.core(), &run.repo).await;
            if guard::autonomy_rank(&have) < guard::autonomy_rank("draft_pr") {
                guard::refuse(
                    self.core(),
                    "scan",
                    &format!("autonomy:{have}"),
                    json!({ "runId": run.id, "stepId": st.id, "repo": run.repo, "job": job }),
                )
                .await?;
                self.succeed(
                    st,
                    "",
                    json!({ "skipped": "autonomy", "required": "draft_pr", "autonomy": have, "job": job }),
                )
                .await?;
                return Ok(Err(()));
            }
        }
        // TEA publishing needs the repository's TEA switch (web `tea_publish`:
        // "when the repository has TEA enabled").
        if job == "tea-release" && first {
            let on = self
                .core()
                .store()
                .gh_repo(&run.repo)
                .await?
                .is_some_and(|r| r.settings.tea_publish);
            if !on {
                guard::refuse(
                    self.core(),
                    "scan",
                    "tea_disabled",
                    json!({ "runId": run.id, "stepId": st.id, "repo": run.repo, "job": job }),
                )
                .await?;
                self.succeed(st, "", json!({ "skipped": "tea_disabled", "job": job }))
                    .await?;
                return Ok(Err(()));
            }
        }
        if !mutates && first && n.str("branch").is_empty() {
            // A rescan after the run's crew judges the change, not the
            // default branch the fix has not reached.
            if let Some(b) = self.fix_branch(steps, st.seq).await {
                n.config.insert("branch".into(), json!(b));
            }
        }
        if job == "vex-apply"
            && let Some(doc) = steps
                .iter()
                .filter(|s| s.node_type == "github" && s.seq < st.seq && s.status == "succeeded")
                .filter_map(|s| {
                    serde_json::from_str::<Value>(&s.details_json)
                        .ok()?
                        .get("vex")
                        .cloned()
                })
                .next_back()
        {
            n.config.insert("vexDoc".into(), doc);
        }
        Ok(Ok(n))
    }

    /// The branch on the card of the newest crew step that finished before
    /// `seq`, when it passes [`branch_ok`].
    async fn fix_branch(&self, steps: &[RunStep], seq: i64) -> Option<String> {
        let crew = steps
            .iter()
            .filter(|s| s.node_type == "crew" && s.seq < seq && s.status == "succeeded")
            .max_by_key(|s| s.seq)?;
        let d: Value = serde_json::from_str(&crew.details_json).ok()?;
        let card_id = d.get("routeCardId").and_then(Value::as_str)?;
        let card = self.core().card(card_id).await.ok()??;
        branch_ok(&card.branch).then_some(card.branch)
    }

    /// A scan's card reached review or done: read its result.
    pub(super) async fn settle_scan(
        &self,
        st: &RunStep,
        card: &kls_store::board::Card,
        mut details: Value,
    ) -> Result<()> {
        let s = self.core().store();
        let Some(run) = s.run(&st.run_id).await? else {
            return Ok(());
        };
        let g = self.load_graph(&run).await?;
        let node = g.node(&st.node_id).cloned().unwrap_or_default();
        let meta: Value = serde_json::from_str(&card.meta_json).unwrap_or(Value::Null);
        let doc = meta
            .pointer("/belai/agent/cliResult")
            .cloned()
            .unwrap_or(Value::Null);
        details["cardStatus"] = json!(card.status);
        let report = node.str("notEvaluable") == "report";
        let result = match cli_result(node.str("job"), &doc, report) {
            Ok(r) => r,
            Err(why) => {
                details["result"] = doc;
                return self.fail(st, &why, details).await;
            }
        };
        if result["notEvaluable"].as_bool() == Some(true) {
            details["notEvaluable"] = json!(true);
        }
        let fail_on: Vec<String> = node
            .config
            .get("failOn")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let c = |k: &str| result["counts"][k].as_i64().unwrap_or(0);
        self.core()
            .emit(
                RUN_SCAN,
                &run.id,
                &json!({ "runId": run.id, "stepId": st.id, "job": result["job"],
                         "total": c("critical") + c("high") + c("medium") + c("low"),
                         "critical": c("critical"), "high": c("high"),
                         "exitCode": result["exitCode"] }),
            )
            .await?;
        let hit = blocking(&result, &fail_on);
        details["result"] = result;
        match hit {
            Some(f) => self.fail(st, &format!("scan_blocking: {f}"), details).await,
            None => self.succeed(st, "", details).await,
        }
    }

    /// `card` with `action: ensure`: the open card this workflow keeps for
    /// `ref` on the run's repository, made in backlog when there is none.
    pub(super) async fn ensure_state_card(
        &self,
        run: &Run,
        node: &Node,
        st: &RunStep,
        prior: Value,
    ) -> Result<()> {
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
        let r = node.str("ref").trim().to_string();
        if r.is_empty() {
            return self.fail(st, "no_ref", json!({})).await;
        }
        let key = state_key(&run.workflow_id, &run.repo, &r);
        let open = core.cards().await?.into_iter().find(|c| {
            let m: Value = serde_json::from_str(&c.meta_json).unwrap_or(Value::Null);
            !c.deleted
                && c.status != "done"
                && m.pointer("/keleustes/stateKey").and_then(Value::as_str) == Some(key.as_str())
        });
        let origin = Self::origin_of(run, st, node);
        let (card, created) = match open {
            Some(c) => (c, false),
            None => {
                let w = core.store().workflow(&run.workflow_id).await?;
                let wf = w.as_ref().map(|w| w.name.clone()).unwrap_or_default();
                let slug = w.as_ref().map(|w| w.slug.clone()).unwrap_or_default();
                let mut labels = vec!["factory".to_string()];
                if !slug.is_empty() {
                    labels.push(format!("wf-{slug}"));
                }
                let d = kls_core::board::CardDraft {
                    title: format!("{wf}: {r}")
                        .chars()
                        .take(kls_core::board::TITLE_MAX)
                        .collect(),
                    body: format!(
                        "Kept by the Keleustes workflow \"{wf}\" (run {}).",
                        short_id(&run.id)
                    ),
                    status: "backlog".into(),
                    labels,
                    repo: run.repo.clone(),
                    meta_json: json!({ "keleustes": {
                        "stateKey": key, "ref": r, "runId": run.id, "stepId": st.id,
                        "workflowId": run.workflow_id, "nodeId": node.id,
                    }})
                    .to_string(),
                    ..Default::default()
                };
                match core.create_card(d, "engine").await {
                    Ok(c) => (c, true),
                    Err(e) => {
                        guard::audit(
                            core,
                            "card",
                            "state.card",
                            json!({ "ref": r, "repo": run.repo }),
                            &origin,
                            &format!("failed: {e:#}"),
                            json!({ "nodeId": node.id }),
                        )
                        .await?;
                        return self
                            .fail(st, "card_error", json!({ "error": format!("{e:#}") }))
                            .await;
                    }
                }
            }
        };
        core.emit(
            RUN_STATE_CARD,
            &run.id,
            &json!({ "runId": run.id, "stepId": st.id, "cardId": card.id, "ref": r, "created": created }),
        )
        .await?;
        guard::audit(
            core,
            "card",
            "state.card",
            json!({ "cardId": card.id, "key": card.key, "ref": r, "repo": run.repo }),
            &origin,
            if created { "created" } else { "found" },
            json!({ "nodeId": node.id }),
        )
        .await?;
        self.succeed(
            st,
            "",
            json!({ "cardId": card.id, "key": card.key, "ref": r, "created": created }),
        )
        .await
    }

    /// The `vex` output: the run's findings as an OpenVEX document on the
    /// step (`vex`), skipped with no findings. Writes nothing to the forge.
    pub(super) async fn write_vex(
        &self,
        run: &Run,
        node: &Node,
        st: &RunStep,
        steps: &[RunStep],
        prior: Value,
    ) -> Result<()> {
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
        let Some(f) = outputs::findings(steps, st.seq) else {
            return self
                .succeed(st, "", json!({ "skipped": "no_findings" }))
                .await;
        };
        let fixed = steps
            .iter()
            .any(|s| s.node_type == "gate" && s.seq < st.seq && s.status == "succeeded");
        let doc = vex_doc(
            &run.id,
            &run.repo,
            &f,
            fixed,
            &kls_core::github::rfc3339(now_ms()),
        );
        let n = doc["statements"].as_array().map(Vec::len).unwrap_or(0);
        let status = if fixed { "fixed" } else { "affected" };
        core.emit(
            RUN_VEX,
            &run.id,
            &json!({ "runId": run.id, "stepId": st.id, "statements": n, "status": status }),
        )
        .await?;
        guard::audit(
            core,
            "vex",
            "vex.written",
            json!({ "runId": run.id, "repo": run.repo }),
            &Self::origin_of(run, st, node),
            status,
            json!({ "nodeId": node.id, "statements": n }),
        )
        .await?;
        self.succeed(
            st,
            "",
            json!({ "vex": doc, "statements": n, "status": status }),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_result_follows_the_web_grammar() {
        let ok = json!({"job":"sca","exitCode":0,"counts":{"high":1},
            "findings":[{"id":"CVE-1","kind":"vuln","severity":"high","path":"go.mod","ref":"x"}]});
        let r = cli_result("sca", &ok, false).unwrap();
        assert_eq!(r["counts"]["high"], 1);
        assert_eq!(r["findings"].as_array().unwrap().len(), 1);
        assert_eq!(
            cli_result("sca", &Value::Null, false).unwrap_err(),
            "cli_no_result"
        );
        assert_eq!(
            cli_result("sast", &ok, false).unwrap_err(),
            "cli_result_mismatch"
        );
        assert_eq!(
            cli_result("sca", &json!({"findings":[{"kind":"x"}]}), false).unwrap_err(),
            "cli_result_invalid"
        );
        assert_eq!(
            cli_result("sca", &json!("text"), false).unwrap_err(),
            "cli_result_invalid"
        );
        assert_eq!(
            cli_result("sca", &json!({"exitCode":3}), false).unwrap_err(),
            "cli_not_evaluable"
        );
        let r = cli_result("sca", &json!({"exitCode":3}), true).unwrap();
        assert_eq!(r["notEvaluable"], true);
        assert_eq!(
            cli_result("sca", &json!({"exitCode":2}), true).unwrap_err(),
            "cli_exit_2"
        );
        // A job that keeps no findings and exits 1 leaves one harness finding.
        let r = cli_result("attest-verify", &json!({"exitCode":1}), false).unwrap();
        assert_eq!(r["findings"][0]["id"], "attest-verify:exit-1");
        assert_eq!(r["findings"][0]["kind"], "check");
        assert_eq!(r["counts"]["high"], 1);
        // A missing job is the node's.
        assert_eq!(
            cli_result("scout", &json!({"signals":["crypto"]}), false).unwrap()["job"],
            "scout"
        );
    }

    #[test]
    fn fail_on_and_branches() {
        let r = json!({"findings":[{"id":"a","kind":"malscan"}]});
        assert_eq!(
            blocking(&r, &["malware".into()]).as_deref(),
            Some("malware")
        );
        assert_eq!(blocking(&r, &["kev".into(), "secrets".into()]), None);
        assert!(branch_ok("keleustes/K-1a2b3c"));
        assert!(branch_ok("fix/deps-2026"));
        for b in ["", "-x", "a..b", "a/", "x.lock", "a b", "a;rm"] {
            assert!(!branch_ok(b), "{b}");
        }
    }

    #[test]
    fn vex_skips_secrets_and_marks_fixed_after_a_gate() {
        let f = outputs::Findings {
            items: vec![
                outputs::Finding {
                    id: "CVE-1".into(),
                    kind: "vuln".into(),
                    ..Default::default()
                },
                outputs::Finding {
                    id: "aws-key".into(),
                    kind: "secret".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let d = vex_doc("r1", "acme/api", &f, true, "2026-11-28T00:00:00Z");
        assert_eq!(d["@context"], "https://openvex.dev/ns/v0.2.0");
        let s = d["statements"].as_array().unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0]["status"], "fixed");
        assert_eq!(s[0]["products"][0]["@id"], "pkg:github/acme/api");
        assert_eq!(
            state_key("w", "Acme/API", "malware response"),
            "state:w:acme/api:malware-response"
        );
    }
}
