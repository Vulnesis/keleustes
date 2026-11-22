//! GitHub REST JSON into store rows. Tolerant: a missing field is a default.

use kls_store::github::{ChecksSummary, GhIssue, GhLabel, GhOrg, GhPull, GhRepo};
use serde_json::Value;

/// RFC 3339 to Unix ms.
pub fn ts(v: &Value) -> Option<i64> {
    v.as_str()?
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|t| t.as_millisecond())
}

/// Unix ms to the RFC 3339 `since` GitHub accepts.
pub fn iso(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| t.strftime("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}

fn bo(v: &Value, k: &str) -> bool {
    v.get(k).and_then(|x| x.as_bool()).unwrap_or(false)
}

fn login(v: &Value) -> String {
    v.get("login")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

pub fn org(v: &Value, kind: &str, now: i64) -> GhOrg {
    GhOrg {
        login: login(v),
        kind: kind.into(),
        managed: false,
        enabled: true,
        repo_count: 0,
        avatar_url: s(v, "avatar_url"),
        description: s(v, "description"),
        synced_at: now,
    }
}

pub fn repo(v: &Value, now: i64) -> GhRepo {
    let full = s(v, "full_name");
    let (owner, name) = full.rsplit_once('/').unwrap_or(("", ""));
    let perms = v.get("permissions");
    GhRepo {
        full_name: full.clone(),
        owner: owner.into(),
        name: name.into(),
        visibility: v
            .get("visibility")
            .and_then(|x| x.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                if bo(v, "private") {
                    "private"
                } else {
                    "public"
                }
                .into()
            }),
        default_branch: Some(s(v, "default_branch"))
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "main".into()),
        archived: bo(v, "archived"),
        fork: bo(v, "fork"),
        html_url: s(v, "html_url"),
        description: s(v, "description"),
        can_push: perms
            .map(|p| bo(p, "push") || bo(p, "admin") || bo(p, "maintain"))
            .unwrap_or(false),
        managed: false,
        mirror_pulls: true,
        mirror_issues: false,
        sync_labels: false,
        settings: Default::default(),
        revision: 0,
        pushed_at: ts(&v["pushed_at"]),
        synced_at: now,
    }
}

/// "clean", "conflict" or "unknown" from `mergeable` and `mergeable_state`.
pub fn mergeable(v: &Value) -> (String, String) {
    let state = Some(s(v, "mergeable_state"))
        .filter(|x| !x.is_empty())
        .unwrap_or_else(|| "unknown".into());
    let m = match (v.get("mergeable").and_then(|x| x.as_bool()), state.as_str()) {
        (_, "dirty") | (Some(false), _) => "conflict",
        (Some(true), _) => "clean",
        _ => "unknown",
    };
    (m.into(), state)
}

/// A pull from a list item or the detail. List items carry no mergeability
/// or size; those keep `prev`'s values (and the checks and review state,
/// which come from their own endpoints).
pub fn pull(repo: &str, v: &Value, prev: Option<&GhPull>, now: i64) -> GhPull {
    let detail = v.get("mergeable").is_some();
    let (m, ms) = if detail {
        mergeable(v)
    } else {
        prev.map(|p| (p.mergeable.clone(), p.mergeable_state.clone()))
            .unwrap_or_else(|| ("unknown".into(), "unknown".into()))
    };
    let head = &v["head"];
    let base = &v["base"];
    let head_sha = s(head, "sha");
    // A new head commit makes the old mergeability and checks stale.
    let same_head = prev.map(|p| p.head_sha == head_sha).unwrap_or(false);
    let (m, ms) = if !detail && !same_head {
        ("unknown".into(), "unknown".into())
    } else {
        (m, ms)
    };
    let mut reviewers: Vec<String> = v["requested_reviewers"]
        .as_array()
        .map(|a| a.iter().map(login).collect())
        .unwrap_or_default();
    if let Some(teams) = v["requested_teams"].as_array() {
        reviewers.extend(teams.iter().map(|t| format!("team:{}", s(t, "slug"))));
    }
    let merged_at = ts(&v["merged_at"]);
    let keep = |f: fn(&GhPull) -> i64, k: &str| {
        if v.get(k).is_some() {
            i(v, k)
        } else {
            prev.map(f).unwrap_or(0)
        }
    };
    GhPull {
        repo: repo.into(),
        number: i(v, "number"),
        title: s(v, "title"),
        state: s(v, "state"),
        draft: bo(v, "draft"),
        merged: bo(v, "merged") || merged_at.is_some(),
        author: login(&v["user"]),
        head_ref: s(head, "ref"),
        head_sha,
        head_repo: head["repo"]
            .get("full_name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        base_ref: s(base, "ref"),
        base_sha: s(base, "sha"),
        mergeable: m,
        mergeable_state: ms,
        review_decision: prev
            .filter(|_| same_head || detail)
            .map(|p| p.review_decision.clone())
            .unwrap_or_default(),
        requested_reviewers: reviewers,
        labels: v["labels"]
            .as_array()
            .map(|a| a.iter().map(|l| s(l, "name")).collect())
            .unwrap_or_default(),
        checks_state: prev
            .filter(|_| same_head)
            .map(|p| p.checks_state.clone())
            .unwrap_or_else(|| "none".into()),
        checks: prev
            .filter(|_| same_head)
            .map(|p| p.checks.clone())
            .unwrap_or_default(),
        html_url: s(v, "html_url"),
        additions: keep(|p| p.additions, "additions"),
        deletions: keep(|p| p.deletions, "deletions"),
        changed_files: keep(|p| p.changed_files, "changed_files"),
        created_at: ts(&v["created_at"]).unwrap_or(0),
        updated_at: ts(&v["updated_at"]).unwrap_or(0),
        merged_at,
        closed_at: ts(&v["closed_at"]),
        synced_at: now,
        delivery_captured: prev.map(|p| p.delivery_captured).unwrap_or(false),
    }
}

/// Check runs of a commit into a summary.
pub fn checks(runs: &[Value]) -> ChecksSummary {
    let mut c = ChecksSummary::default();
    for r in runs {
        c.total += 1;
        let status = s(r, "status");
        let concl = s(r, "conclusion");
        if status != "completed" {
            c.pending += 1;
            continue;
        }
        match concl.as_str() {
            "success" | "neutral" => c.success += 1,
            "skipped" => c.skipped += 1,
            _ => {
                c.failure += 1;
                if c.failing.len() < 10 {
                    c.failing.push(s(r, "name"));
                }
            }
        }
    }
    c
}

/// Review decision from the reviews list: the latest state per reviewer
/// (comments do not override a verdict).
pub fn review_decision(reviews: &[Value], author: &str) -> String {
    let mut latest: std::collections::BTreeMap<String, String> = Default::default();
    for r in reviews {
        let who = login(&r["user"]);
        if who.is_empty() || who == author {
            continue;
        }
        let st = s(r, "state");
        match st.as_str() {
            "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED" => {
                latest.insert(who, st);
            }
            _ => {}
        }
    }
    if latest.values().any(|s| s == "CHANGES_REQUESTED") {
        "CHANGES_REQUESTED".into()
    } else if latest.values().any(|s| s == "APPROVED") {
        "APPROVED".into()
    } else {
        "REVIEW_REQUIRED".into()
    }
}

pub fn issue(repo: &str, v: &Value, now: i64) -> GhIssue {
    GhIssue {
        repo: repo.into(),
        number: i(v, "number"),
        title: s(v, "title"),
        state: s(v, "state"),
        state_reason: s(v, "state_reason"),
        author: login(&v["user"]),
        author_association: s(v, "author_association"),
        labels: v["labels"]
            .as_array()
            .map(|a| a.iter().map(|l| s(l, "name")).collect())
            .unwrap_or_default(),
        assignees: v["assignees"]
            .as_array()
            .map(|a| a.iter().map(login).collect())
            .unwrap_or_default(),
        body: s(v, "body"),
        html_url: s(v, "html_url"),
        comments: i(v, "comments"),
        created_at: ts(&v["created_at"]).unwrap_or(0),
        updated_at: ts(&v["updated_at"]).unwrap_or(0),
        closed_at: ts(&v["closed_at"]),
        card_id: None,
        synced_at: now,
    }
}

pub fn label(repo: &str, v: &Value) -> GhLabel {
    GhLabel {
        repo: repo.into(),
        name: s(v, "name"),
        color: s(v, "color"),
        description: s(v, "description"),
    }
}

/// A login is a bot when GitHub says so or it ends in `[bot]`.
pub fn is_bot(user: &Value) -> bool {
    s(user, "type") == "Bot" || login(user).ends_with("[bot]")
}

/// Internal when the association is OWNER, MEMBER or COLLABORATOR.
pub fn is_internal(association: &str) -> Option<bool> {
    match association {
        "OWNER" | "MEMBER" | "COLLABORATOR" => Some(true),
        "" => None,
        _ => Some(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn times() {
        assert_eq!(ts(&json!("2026-10-22T09:32:11Z")), Some(1_792_661_531_000));
        assert_eq!(ts(&json!(null)), None);
        assert_eq!(iso(1_792_661_531_000), "2026-10-22T09:32:11Z");
    }

    #[test]
    fn pull_list_then_detail() {
        let item = json!({
            "number": 8, "title": "T", "state": "open", "draft": true,
            "user": {"login": "ana"}, "html_url": "u",
            "head": {"ref": "feat", "sha": "h1", "repo": {"full_name": "ana/api"}},
            "base": {"ref": "main", "sha": "b1"},
            "requested_reviewers": [{"login": "bo"}], "requested_teams": [{"slug": "ops"}],
            "labels": [{"name": "sf:fix"}],
            "created_at": "2026-10-22T09:32:11Z", "updated_at": "2026-10-22T09:32:11Z"
        });
        let p = pull("acme/api", &item, None, 1);
        assert_eq!(p.mergeable, "unknown");
        assert!(p.is_fork());
        assert_eq!(
            p.requested_reviewers,
            vec!["bo".to_string(), "team:ops".to_string()]
        );
        let mut detail = item.clone();
        detail["mergeable"] = json!(false);
        detail["mergeable_state"] = json!("dirty");
        detail["additions"] = json!(5);
        let d = pull("acme/api", &detail, Some(&p), 2);
        assert_eq!((d.mergeable.as_str(), d.additions), ("conflict", 5));
        // A later list read keeps the detail-only fields while the head is the same.
        let again = pull("acme/api", &item, Some(&d), 3);
        assert_eq!((again.mergeable.as_str(), again.additions), ("conflict", 5));
        let mut moved = item.clone();
        moved["head"]["sha"] = json!("h2");
        assert_eq!(pull("acme/api", &moved, Some(&d), 4).mergeable, "unknown");
    }

    #[test]
    fn checks_and_reviews() {
        let c = checks(&[
            json!({"name": "a", "status": "completed", "conclusion": "success"}),
            json!({"name": "b", "status": "completed", "conclusion": "failure"}),
            json!({"name": "c", "status": "in_progress"}),
            json!({"name": "d", "status": "completed", "conclusion": "skipped"}),
        ]);
        assert_eq!(
            (c.total, c.success, c.failure, c.pending, c.skipped),
            (4, 1, 1, 1, 1)
        );
        assert_eq!(c.failing, vec!["b".to_string()]);
        assert_eq!(c.state(), "failure");
        let r = |who: &str, st: &str| json!({"user": {"login": who}, "state": st});
        assert_eq!(review_decision(&[r("bo", "APPROVED")], "ana"), "APPROVED");
        assert_eq!(
            review_decision(
                &[
                    r("bo", "CHANGES_REQUESTED"),
                    r("bo", "COMMENTED"),
                    r("cy", "APPROVED")
                ],
                "ana"
            ),
            "CHANGES_REQUESTED"
        );
        assert_eq!(
            review_decision(&[r("bo", "CHANGES_REQUESTED"), r("bo", "APPROVED")], "ana"),
            "APPROVED"
        );
        assert_eq!(
            review_decision(&[r("ana", "APPROVED")], "ana"),
            "REVIEW_REQUIRED"
        );
        assert!(is_bot(&json!({"login": "dependabot[bot]"})));
        assert_eq!(is_internal("MEMBER"), Some(true));
        assert_eq!(is_internal("CONTRIBUTOR"), Some(false));
    }

    #[test]
    fn repo_fields() {
        let r = repo(
            &json!({"full_name": "acme/api", "private": true, "default_branch": "trunk",
                    "permissions": {"push": true}, "pushed_at": "2026-10-22T09:32:11Z"}),
            1,
        );
        assert_eq!((r.owner.as_str(), r.name.as_str()), ("acme", "api"));
        assert_eq!(r.visibility, "private");
        assert!(r.can_push);
        assert_eq!(r.default_branch, "trunk");
    }
}
