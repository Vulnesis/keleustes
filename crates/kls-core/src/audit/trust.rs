//! Trust and provenance: read-only aggregates over facts other areas
//! record. Nothing here calls GitHub or a node.
//!
//! - **SLSA-style level** per repository, from what Keleustes can show:
//!   factory work recorded (sessions, runs), provenance linking session →
//!   run → card, and commits signed (M4's delivery commits and conflict
//!   resolutions). Keleustes runs no isolated builders, so level 3 is out
//!   of reach and the page says so.
//! - **Scanner matrix** from belai `gate.verified` audit lines (the gate's
//!   name and exit code) and failing GitHub checks whose names match a
//!   scanner; a scanner with neither reads `not_run`.
//! - **Guardrails** each node reported through its audit lines: hazard
//!   asks and denials, egress refusals, A2A messages withheld, run caps and
//!   faults. A node that reports none shows zeros.

use crate::Core;
use anyhow::Result;
use kls_store::now_ms;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The scanner columns, in display order.
pub const SCANNERS: [&str; 11] = [
    "sca",
    "sast",
    "secrets",
    "iac",
    "containers",
    "license",
    "malscan",
    "eol",
    "cbom",
    "aibom",
    "scorecard",
];

/// Window of scanner results and guardrail counts.
pub const WINDOW_MS: i64 = 30 * 86_400_000;

/// Host audit line kinds the guardrail summary counts.
pub const GUARDRAIL_KINDS: [&str; 7] = [
    "hazard.asked",
    "hazard.denied",
    "egress.denied",
    "a2a.withheld",
    "run_cap",
    "worker.fault",
    "gate.verified",
];

/// The facts one repository's level is computed from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoFacts {
    pub repo: String,
    pub managed: bool,
    pub sessions: i64,
    /// Sessions linked to a run or a card.
    pub linked_sessions: i64,
    pub runs: i64,
    /// Runs linked to a card.
    pub linked_runs: i64,
    pub commits: i64,
    pub signed_commits: i64,
    pub unsigned_commits: i64,
    pub resolutions: i64,
    pub verified_resolutions: i64,
    pub unverified_resolutions: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoTrust {
    pub facts: RepoFacts,
    pub level: u8,
    /// "no_factory_build", "unlinked_provenance", "unsigned_commits",
    /// "hosted_signed".
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LevelBucket {
    pub level: u8,
    pub count: i64,
    pub repos: Vec<String>,
}

/// A scanner cell: "clean", "findings" or "not_run", with when it was seen.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cell {
    pub state: String,
    pub at: i64,
    /// "gate" (a host's gate.verified) or "check" (a GitHub check).
    pub source: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScannerRow {
    pub repo: String,
    pub cells: BTreeMap<String, Cell>,
    pub level: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeGuardrails {
    pub node_id: String,
    pub name: String,
    pub online: bool,
    pub revoked: bool,
    /// Line kind → count in the window.
    pub counts: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub sessions: i64,
    pub linked_sessions: i64,
    pub runs: i64,
    pub linked_runs: i64,
    pub commits: i64,
    pub signed_commits: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustReport {
    pub as_of: i64,
    pub levels: Vec<LevelBucket>,
    pub repos: Vec<RepoTrust>,
    pub scanners: Vec<ScannerRow>,
    pub guardrails: Vec<NodeGuardrails>,
    pub provenance: Provenance,
    /// The factory guardrail default and minimum level, from settings.
    pub settings: BTreeMap<String, String>,
}

/// The level and its reason for a repository's facts.
pub fn slsa_level(f: &RepoFacts) -> (u8, &'static str) {
    if f.sessions == 0 && f.runs == 0 {
        return (0, "no_factory_build");
    }
    if f.linked_sessions == 0 && f.linked_runs == 0 {
        return (1, "unlinked_provenance");
    }
    let signed = f.signed_commits + f.verified_resolutions;
    let unsigned = f.unsigned_commits + f.unverified_resolutions;
    if signed == 0 || unsigned > 0 {
        return (1, "unsigned_commits");
    }
    (2, "hosted_signed")
}

/// What a level's reason means, in words.
pub fn reason_words(reason: &str) -> &'static str {
    match reason {
        "no_factory_build" => "No factory session or run has touched it.",
        "unlinked_provenance" => {
            "Factory work is recorded, but no session or run links back to a run or card."
        }
        "unsigned_commits" => "Provenance links up, but commits are unsigned or not recorded.",
        "hosted_signed" => {
            "Provenance links session to run to card, and every recorded commit is signed."
        }
        _ => "",
    }
}

/// The scanner a gate or check name belongs to.
pub fn scanner_for(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    Some(if has(&["secret", "gitleaks", "trufflehog"]) {
        "secrets"
    } else if has(&["malscan", "malware"]) {
        "malscan"
    } else if has(&["sca", "osv", "dependency", "dependabot", "audit", "vuln"]) {
        "sca"
    } else if has(&["sast", "codeql", "semgrep", "gosec", "bandit"]) {
        "sast"
    } else if has(&["iac", "checkov", "tfsec", "kics", "terraform"]) {
        "iac"
    } else if has(&["container", "trivy", "grype", "docker", "image"]) {
        "containers"
    } else if has(&["license", "licence"]) {
        "license"
    } else if has(&["eol", "end-of-life", "endoflife"]) {
        "eol"
    } else if has(&["cbom", "crypto"]) {
        "cbom"
    } else if has(&["aibom"]) {
        "aibom"
    } else if has(&["scorecard"]) {
        "scorecard"
    } else {
        return None;
    })
}

/// A gate.verified line's (gate name, passed), if it names a gate.
pub fn gate_result(json: &str) -> Option<(String, bool)> {
    let v: Value = serde_json::from_str(json).ok()?;
    let data = v.get("data").unwrap_or(&v);
    let gate = ["gate", "suite", "id"]
        .iter()
        .find_map(|k| data.get(*k).and_then(|x| x.as_str()))?
        .to_string();
    let passed = if let Some(exit) = data.get("exit").and_then(|x| x.as_i64()) {
        exit == 0
    } else {
        let outcome = ["outcome", "status", "verdict"]
            .iter()
            .find_map(|k| data.get(*k).or_else(|| v.get(*k)).and_then(|x| x.as_str()))
            .unwrap_or("");
        matches!(
            outcome,
            "pass" | "passed" | "met" | "ok" | "clean" | "success"
        )
    };
    Some((gate, passed))
}

/// "github.com/acme/api.git", "https://github.com/acme/api" → "acme/api".
pub fn repo_key(r: &str) -> String {
    let r = r.trim().trim_end_matches('/').trim_end_matches(".git");
    let r = r
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("git@");
    let r = r
        .strip_prefix("github.com/")
        .or_else(|| r.strip_prefix("github.com:"))
        .unwrap_or(r);
    r.to_string()
}

impl Core {
    /// The Trust page's report.
    pub async fn trust(&self) -> Result<TrustReport> {
        let s = self.store();
        let now = now_ms();
        let since = now - WINDOW_MS;
        let mut facts: BTreeMap<String, RepoFacts> = BTreeMap::new();
        let entry = |m: &mut BTreeMap<String, RepoFacts>, repo: &str| -> String {
            let k = repo_key(repo);
            m.entry(k.clone()).or_insert_with(|| RepoFacts {
                repo: k.clone(),
                ..Default::default()
            });
            k
        };
        for (repo, _archived) in s.trust_managed_repos().await? {
            let k = entry(&mut facts, &repo);
            facts.get_mut(&k).expect("inserted").managed = true;
        }
        for c in s.trust_session_counts().await? {
            let k = entry(&mut facts, &c.repo);
            let f = facts.get_mut(&k).expect("inserted");
            f.sessions += c.total;
            f.linked_sessions += c.good;
        }
        for c in s.trust_run_counts().await? {
            let k = entry(&mut facts, &c.repo);
            let f = facts.get_mut(&k).expect("inserted");
            f.runs += c.total;
            f.linked_runs += c.good;
        }
        for c in s.trust_commit_counts().await? {
            let k = entry(&mut facts, &c.repo);
            let f = facts.get_mut(&k).expect("inserted");
            f.commits += c.total;
            f.signed_commits += c.good;
            f.unsigned_commits += c.bad;
        }
        for c in s.trust_resolution_counts().await? {
            let k = entry(&mut facts, &c.repo);
            let f = facts.get_mut(&k).expect("inserted");
            f.resolutions += c.total;
            f.verified_resolutions += c.good;
            f.unverified_resolutions += c.bad;
        }

        let mut report = TrustReport {
            as_of: now,
            ..Default::default()
        };
        let mut by_level: BTreeMap<u8, Vec<String>> = BTreeMap::new();
        for f in facts.values() {
            let (level, reason) = slsa_level(f);
            by_level.entry(level).or_default().push(f.repo.clone());
            let p = &mut report.provenance;
            p.sessions += f.sessions;
            p.linked_sessions += f.linked_sessions;
            p.runs += f.runs;
            p.linked_runs += f.linked_runs;
            p.commits += f.commits;
            p.signed_commits += f.signed_commits;
            report.repos.push(RepoTrust {
                facts: f.clone(),
                level,
                reason: reason.into(),
            });
        }
        report.levels = (0..=3u8)
            .map(|level| {
                let repos = by_level.remove(&level).unwrap_or_default();
                LevelBucket {
                    level,
                    count: repos.len() as i64,
                    repos,
                }
            })
            .collect();

        // Scanner matrix: managed repos plus any repo with results.
        let mut cells: BTreeMap<String, BTreeMap<String, Cell>> = BTreeMap::new();
        for l in s.trust_repo_lines(&["gate.verified"], since).await? {
            let Some((gate, passed)) = gate_result(&l.json) else {
                continue;
            };
            let Some(col) = scanner_for(&gate) else {
                continue;
            };
            let row = cells.entry(repo_key(&l.repo)).or_default();
            // Lines come oldest first: the latest result wins.
            row.insert(
                col.into(),
                Cell {
                    state: if passed { "clean" } else { "findings" }.into(),
                    at: l.at,
                    source: "gate".into(),
                },
            );
        }
        for (repo, _state, checks_json, at) in s.trust_pull_checks(since).await? {
            let v: Value = serde_json::from_str(&checks_json).unwrap_or(Value::Null);
            let failing: Vec<String> = v
                .get("failing")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let row = cells.entry(repo_key(&repo)).or_default();
            for name in failing {
                if let Some(col) = scanner_for(&name)
                    && row.get(col).is_none_or(|c| c.at <= at)
                {
                    row.insert(
                        col.into(),
                        Cell {
                            state: "findings".into(),
                            at,
                            source: "check".into(),
                        },
                    );
                }
            }
        }
        let mut rows: BTreeSet<String> = facts
            .values()
            .filter(|f| f.managed)
            .map(|f| f.repo.clone())
            .collect();
        rows.extend(
            cells
                .iter()
                .filter(|(_, c)| !c.is_empty())
                .map(|(r, _)| r.clone()),
        );
        let level_of: BTreeMap<&str, u8> = report
            .repos
            .iter()
            .map(|r| (r.facts.repo.as_str(), r.level))
            .collect();
        for repo in rows {
            let mut row = cells.remove(&repo).unwrap_or_default();
            for col in SCANNERS {
                row.entry(col.into()).or_insert_with(|| Cell {
                    state: "not_run".into(),
                    ..Default::default()
                });
            }
            report.scanners.push(ScannerRow {
                level: level_of.get(repo.as_str()).copied().unwrap_or(0),
                repo,
                cells: row,
            });
        }

        // Guardrails each node reported.
        let online: BTreeSet<String> = match self.gateway() {
            Ok(g) => g.online().await.into_iter().collect(),
            Err(_) => BTreeSet::new(),
        };
        let mut counts: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
        for (node, kind, n) in s.audit_line_counts(&GUARDRAIL_KINDS, since).await? {
            counts.entry(node).or_default().insert(kind, n);
        }
        for n in s.nodes().await? {
            report.guardrails.push(NodeGuardrails {
                online: online.contains(&n.id),
                revoked: n.revoked_at.is_some(),
                counts: counts.remove(&n.id).unwrap_or_default(),
                node_id: n.id,
                name: n.name,
            });
        }
        for key in ["belai.guardrails", "security.slsa_min", "signing.mode"] {
            if let Some(v) = s.setting(key).await? {
                report.settings.insert(key.into(), v);
            }
        }
        Ok(report)
    }
}

// ---- matrix totals, egress hosts, evidence index -------------------------------

/// Counts over the scanner matrix.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatrixTotals {
    pub repos: usize,
    pub clean: usize,
    pub findings: usize,
    pub not_run: usize,
    /// Repositories with at least one finding.
    pub repos_with_findings: usize,
}

pub fn matrix_totals(rows: &[ScannerRow]) -> MatrixTotals {
    let mut t = MatrixTotals {
        repos: rows.len(),
        ..Default::default()
    };
    for r in rows {
        let mut any = false;
        for c in r.cells.values() {
            match c.state.as_str() {
                "clean" => t.clean += 1,
                "findings" => {
                    t.findings += 1;
                    any = true;
                }
                _ => t.not_run += 1,
            }
        }
        if any {
            t.repos_with_findings += 1;
        }
    }
    t
}

/// One host a node's sandbox refused to reach.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressHost {
    pub host: String,
    pub count: i64,
    pub nodes: Vec<String>,
    pub last_at: i64,
}

/// The host a belai `egress.denied` line names.
pub fn egress_host(json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    let d = v.get("data").unwrap_or(&v);
    ["host", "domain", "target", "url"]
        .iter()
        .find_map(|k| d.get(*k).and_then(Value::as_str))
        .map(|s| {
            let s = s.split("://").last().unwrap_or(s);
            s.split(['/', ':']).next().unwrap_or(s).to_lowercase()
        })
        .filter(|s| !s.is_empty())
}

impl Core {
    /// Hosts egress refusals named since `since`, most refused first.
    pub async fn egress_hosts(&self, since: i64) -> Result<Vec<EgressHost>> {
        let mut by: BTreeMap<String, EgressHost> = BTreeMap::new();
        let mut after: Option<kls_store::sessions::AuditLine> = None;
        loop {
            let page = self
                .store()
                .audit_lines_received_since(since, after.as_ref(), 2000)
                .await?;
            for l in &page {
                if l.kind != "egress.denied" {
                    continue;
                }
                let host = egress_host(&l.json).unwrap_or_else(|| "(unknown)".into());
                let e = by.entry(host.clone()).or_insert_with(|| EgressHost {
                    host,
                    ..Default::default()
                });
                e.count += 1;
                e.last_at = e.last_at.max(l.at);
                if !e.nodes.contains(&l.node_id) {
                    e.nodes.push(l.node_id.clone());
                }
            }
            if page.len() < 2000 {
                break;
            }
            after = page.last().cloned();
        }
        let mut out: Vec<EgressHost> = by.into_values().collect();
        out.sort_by(|a, b| b.count.cmp(&a.count).then(a.host.cmp(&b.host)));
        Ok(out)
    }

    /// The evidence index: the trust report, matrix totals, the ledger head
    /// and the audit signing key, written to `<data>/exports/` and audited.
    pub async fn export_evidence_index(&self) -> Result<std::path::PathBuf> {
        let report = self.trust().await?;
        let head = self.store().ledger_head().await?;
        let index = serde_json::json!({
            "type": "evidence_index", "asOf": now_ms(), "server": self.server_id().await?,
            "matrixTotals": matrix_totals(&report.scanners), "report": report,
            "ledgerHead": {"seq": head.seq, "hash": head.hash},
            "auditPublicKey": self.audit_public_key().await.unwrap_or_default(),
            "egress": self.egress_hosts(now_ms() - 30 * 86_400_000).await?,
        });
        let dir = self.data_dir().join("exports");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("evidence-{}.json", now_ms()));
        let text = serde_json::to_string_pretty(&index)?;
        std::fs::write(&path, &text)?;
        use sha2::Digest;
        self.emit(
            super::kinds::AUDIT_EXPORTED,
            &path.display().to_string(),
            &serde_json::json!({"what": "evidence_index", "sha256": hex::encode(sha2::Sha256::digest(text.as_bytes())), "by": "owner"}),
        )
        .await?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kls_store::sessions::{AuditLine, Session};

    #[test]
    fn matrix_totals_and_egress_hosts() {
        let mut cells = BTreeMap::new();
        cells.insert(
            "sca".to_string(),
            Cell {
                state: "clean".into(),
                ..Default::default()
            },
        );
        cells.insert(
            "sast".to_string(),
            Cell {
                state: "findings".into(),
                ..Default::default()
            },
        );
        let rows = vec![
            ScannerRow {
                repo: "a/b".into(),
                cells,
                level: 1,
            },
            ScannerRow::default(),
        ];
        let t = matrix_totals(&rows);
        assert_eq!(
            (t.repos, t.clean, t.findings, t.repos_with_findings),
            (2, 1, 1, 1)
        );
        assert_eq!(
            egress_host(r#"{"data":{"url":"https://Evil.example:443/x"}}"#).as_deref(),
            Some("evil.example")
        );
        assert_eq!(egress_host("{}"), None);
    }

    #[tokio::test]
    async fn evidence_index_is_written_and_audited() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        crate::vault::install_vault(
            &c,
            crate::vault::KeyBackend::File(dir.path().join("vault.key")),
        )
        .await
        .unwrap();
        let p = c.export_evidence_index().await.unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        assert_eq!(v["type"], "evidence_index");
        assert!(v["auditPublicKey"].as_str().unwrap().len() == 64);
        assert!(c.egress_hosts(0).await.unwrap().is_empty());
    }

    #[test]
    fn levels() {
        let mut f = RepoFacts::default();
        assert_eq!(slsa_level(&f), (0, "no_factory_build"));
        f.sessions = 2;
        assert_eq!(slsa_level(&f), (1, "unlinked_provenance"));
        f.linked_sessions = 1;
        assert_eq!(slsa_level(&f), (1, "unsigned_commits"));
        f.signed_commits = 3;
        assert_eq!(slsa_level(&f), (2, "hosted_signed"));
        f.unsigned_commits = 1;
        assert_eq!(slsa_level(&f).0, 1);
        f.unsigned_commits = 0;
        f.unverified_resolutions = 1;
        assert_eq!(slsa_level(&f).0, 1);
        assert!(!reason_words("hosted_signed").is_empty());
    }

    #[test]
    fn names_and_results() {
        assert_eq!(scanner_for("gitleaks"), Some("secrets"));
        assert_eq!(scanner_for("CodeQL / analyze (go)"), Some("sast"));
        assert_eq!(scanner_for("osv-scanner"), Some("sca"));
        assert_eq!(scanner_for("trivy image"), Some("containers"));
        assert_eq!(scanner_for("unit tests"), None);
        assert_eq!(
            gate_result(r#"{"data":{"gate":"sca","exit":1}}"#),
            Some(("sca".into(), false))
        );
        assert_eq!(
            gate_result(r#"{"data":{"gate":"secrets"},"outcome":"met"}"#),
            Some(("secrets".into(), true))
        );
        assert_eq!(gate_result("{}"), None);
        assert_eq!(repo_key("https://github.com/acme/api.git"), "acme/api");
        assert_eq!(repo_key("git@github.com:acme/api"), "acme/api");
        assert_eq!(repo_key("acme/api"), "acme/api");
    }

    #[tokio::test]
    async fn report_from_facts() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        let s = c.store();
        s.insert_node("n1", "box", &Default::default(), "serial", "tok")
            .await
            .unwrap();
        s.upsert_session(&Session {
            id: "s1".into(),
            node_id: "n1".into(),
            repo: "github.com/acme/api".into(),
            card_id: "c1".into(),
            ..Default::default()
        })
        .await
        .unwrap();
        let now = now_ms();
        s.append_audit_lines(
            "n1",
            "s1",
            &[
                AuditLine {
                    idx: 0,
                    at: now - 10,
                    kind: "gate.verified".into(),
                    json: r#"{"data":{"gate":"gitleaks","exit":1}}"#.into(),
                    ..Default::default()
                },
                AuditLine {
                    idx: 1,
                    at: now - 5,
                    kind: "hazard.denied".into(),
                    json: "{}".into(),
                    ..Default::default()
                },
            ],
        )
        .await
        .unwrap();
        let r = c.trust().await.unwrap();
        assert_eq!(r.repos.len(), 1);
        assert_eq!(r.repos[0].facts.repo, "acme/api");
        assert_eq!(
            (r.repos[0].level, r.repos[0].reason.as_str()),
            (1, "unsigned_commits")
        );
        assert_eq!(r.levels[1].count, 1);
        assert_eq!(r.levels.len(), 4);
        assert_eq!(r.scanners.len(), 1);
        assert_eq!(r.scanners[0].cells["secrets"].state, "findings");
        assert_eq!(r.scanners[0].cells["sca"].state, "not_run");
        assert_eq!(r.scanners[0].cells.len(), SCANNERS.len());
        assert_eq!(r.guardrails.len(), 1);
        assert_eq!(r.guardrails[0].counts["hazard.denied"], 1);
        assert_eq!(r.provenance.linked_sessions, 1);
    }
}
