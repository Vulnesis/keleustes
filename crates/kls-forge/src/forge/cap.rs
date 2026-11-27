//! The forge capability table (vdb-site `forgecap.Catalogue()`), with the
//! Keleustes desktop mechanism as the note. Pinned to a copy of
//! `forgecap/testdata/forge-capabilities.json` in `testdata/`: the test fails
//! when an id or a level differs. Refresh the copy from vdb-site when
//! forgecap changes (`FORGECAP_UPDATE=1 go test ./internal/forgecap`).

use super::kind::ForgeKind;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Native,
    Emulated,
    Unavailable,
}

/// What a forge can do for one capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Support {
    Native,
    Emulated(&'static str),
    Unavailable(&'static str),
}

use Level::{Emulated as E, Native as N, Unavailable as U};

/// One row: id, levels for github, ghes, gitea, forgejo, gitlab, desktop note.
pub struct Row {
    pub id: &'static str,
    pub levels: [Level; 5],
    pub note: &'static str,
}

/// The catalogue, in forgecap order.
pub const TABLE: &[Row] = &[
    Row {
        id: "connect",
        levels: [N, E, E, E, E],
        note: "the CLI's own sign-in (gh / glab / tea)",
    },
    Row {
        id: "webhooks",
        levels: [N, N, E, E, E],
        note: "not used on desktop: polling on every kind",
    },
    Row {
        id: "repos",
        levels: [N, N, E, E, E],
        note: "orgs or groups and their repositories, on Refresh",
    },
    Row {
        id: "pull_requests",
        levels: [N, N, N, N, E],
        note: "GitLab merge requests by iid",
    },
    Row {
        id: "draft",
        levels: [N, N, E, E, E],
        note: "title prefix: WIP: (Gitea, Forgejo), Draft: (GitLab)",
    },
    Row {
        id: "checks",
        levels: [N, N, E, E, E],
        note: "commit statuses read as checks",
    },
    Row {
        id: "merge_state",
        levels: [N, N, E, E, N],
        note: "mergeable, detailed_merge_status",
    },
    Row {
        id: "review_decision",
        levels: [N, N, E, E, E],
        note: "latest verdict per reviewer; GitLab approvals",
    },
    Row {
        id: "reviews",
        levels: [N, N, N, N, E],
        note: "GitLab: approve, notes, unapprove",
    },
    Row {
        id: "team_reviewers",
        levels: [N, N, N, N, U],
        note: "GitLab requests users only",
    },
    Row {
        id: "update_branch",
        levels: [N, N, E, E, E],
        note: "Gitea merges the base in; GitLab rebases",
    },
    Row {
        id: "signed_commits",
        levels: [N, N, E, E, E],
        note: "off GitHub: an unsigned merge commit pushed over git",
    },
    Row {
        id: "issues",
        levels: [N, N, E, E, N],
        note: "Gitea labels by id",
    },
    Row {
        id: "close_reason",
        levels: [N, N, U, U, U],
        note: "board only",
    },
    Row {
        id: "rate_limit",
        levels: [N, N, E, E, N],
        note: "Gitea: calls counted against forge.calls_per_hour",
    },
    Row {
        id: "bot_identity",
        levels: [N, N, E, E, E],
        note: "desktop: the signed-in person, no bot",
    },
    Row {
        id: "security_alerts",
        levels: [N, N, U, U, U],
        note: "Dependabot, code and secret scanning",
    },
    Row {
        id: "merge_queue",
        levels: [N, N, U, U, U],
        note: "",
    },
    Row {
        id: "app_capabilities",
        levels: [N, N, U, U, U],
        note: "",
    },
    Row {
        id: "remote_clone",
        levels: [N, N, N, N, N],
        note: "belai node",
    },
    Row {
        id: "manifest_curator",
        levels: [N, U, U, U, U],
        note: "web only; not in Keleustes",
    },
    Row {
        id: "pix_sandbox",
        levels: [N, U, U, U, U],
        note: "web only; not in Keleustes",
    },
];

fn col(kind: ForgeKind) -> usize {
    match kind {
        ForgeKind::Github => 0,
        ForgeKind::Ghes => 1,
        ForgeKind::Gitea => 2,
        ForgeKind::Forgejo => 3,
        ForgeKind::Gitlab => 4,
    }
}

/// Support of `id` on `kind`; an unknown id is unavailable.
pub fn support(kind: ForgeKind, id: &str) -> Support {
    let Some(r) = TABLE.iter().find(|r| r.id == id) else {
        return Support::Unavailable("unknown capability");
    };
    match r.levels[col(kind)] {
        N => Support::Native,
        E => Support::Emulated(r.note),
        U => Support::Unavailable(if r.note.is_empty() {
            "not offered by this forge"
        } else {
            r.note
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn table_matches_forgecap_json() {
        let json: Value =
            serde_json::from_str(include_str!("../../testdata/forge-capabilities.json")).unwrap();
        let caps = json["capabilities"].as_array().unwrap();
        let ids: Vec<&str> = caps.iter().map(|c| c["id"].as_str().unwrap()).collect();
        let ours: Vec<&str> = TABLE.iter().map(|r| r.id).collect();
        assert_eq!(ids, ours, "capability ids or order changed in forgecap");
        for c in caps {
            let row = TABLE.iter().find(|r| r.id == c["id"]).unwrap();
            for k in ForgeKind::ALL {
                let want = c["support"][k.id()]["level"].as_str().unwrap();
                let got = match row.levels[col(k)] {
                    N => "native",
                    E => "emulated",
                    U => "unavailable",
                };
                assert_eq!(got, want, "{} on {}", row.id, k.id());
            }
        }
        let kinds: Vec<&str> = json["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k["id"].as_str().unwrap())
            .collect();
        for k in ForgeKind::ALL {
            assert!(kinds.contains(&k.id()));
        }
        assert_eq!(
            support(ForgeKind::Gitlab, "team_reviewers"),
            Support::Unavailable("GitLab requests users only")
        );
        assert_eq!(support(ForgeKind::Github, "draft"), Support::Native);
    }
}
