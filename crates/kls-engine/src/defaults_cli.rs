//! The 34 Vulnetix CLI defaults (vdb-site `factory_defaults_cli.go`
//! `fdcSpecs` and `fdcBuild`), ported as data. Each runs the CLI through a
//! `scan` step (a `cli-job:<id>` card for `belai:cli-runner` /
//! `belai:cli-runner-write`), decides with SystemOne whether the findings
//! are worth filing, and files issues, check runs, comments or a draft PR.
//! Every trigger carries the profile condition the web factory sets, so a
//! repository the repo scout has not profiled matches none of them.

use crate::graph::Graph;
use serde_json::{Map, Value, json};

/// A trigger of a CLI default.
pub struct T {
    pub kind: &'static str,
    pub title: &'static str,
    pub cron: &'static str,
    pub event: &'static str,
    pub cond: &'static str,
    pub paths: &'static [&'static str],
    pub debounce: i64,
    pub per_hour: i64,
    pub label: &'static str,
    pub to: &'static str,
}

const fn t() -> T {
    T {
        kind: "",
        title: "",
        cron: "",
        event: "",
        cond: "",
        paths: &[],
        debounce: 0,
        per_hour: 0,
        label: "",
        to: "",
    }
}

/// A stage of a CLI default.
pub struct S {
    pub name: &'static str,
    pub kind: &'static str,
    pub title: &'static str,
    pub after: &'static str,
    pub job: &'static str,
    pub agent: &'static str,
    pub brief: &'static str,
    pub tokens: i64,
    pub cfg: &'static str,
    pub loop_to: &'static str,
}

const fn s() -> S {
    S {
        name: "",
        kind: "",
        title: "",
        after: "",
        job: "",
        agent: "",
        brief: "",
        tokens: 0,
        cfg: "",
        loop_to: "",
    }
}

pub struct Spec {
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub min_autonomy: &'static str,
    /// Profile condition (JSON).
    pub matches: &'static str,
    pub threshold: &'static str,
    pub triggers: &'static [T],
    pub stages: &'static [S],
}

const MANIFESTS: &[&str] = &[
    "**/package.json",
    "**/package-lock.json",
    "**/npm-shrinkwrap.json",
    "**/pnpm-lock.yaml",
    "**/yarn.lock",
    "**/bun.lockb",
    "**/pyproject.toml",
    "**/poetry.lock",
    "**/uv.lock",
    "**/Pipfile.lock",
    "**/requirements*.txt",
    "**/go.mod",
    "**/pom.xml",
    "**/build.gradle",
    "**/build.gradle.kts",
    "**/Cargo.toml",
    "**/Gemfile.lock",
    "**/composer.lock",
    "**/packages.lock.json",
    "**/Package.resolved",
    "**/Dockerfile*",
    "**/Containerfile*",
    "**/docker-compose*.y*ml",
    "**/compose.y*ml",
    "**/Chart.yaml",
    "**/*.tf",
    ".github/workflows/*.y*ml",
    "**/openapi.*",
    "**/swagger.*",
    ".vulnetix/config*",
];

pub const PROFILE_PATHS: &[&str] = MANIFESTS;

const PM: &str = r#"{"packageManagers":["bun","bundler","cargo","composer","go","gradle","maven","npm","nuget","pip","pipenv","pnpm","poetry","swiftpm","uv","yarn"]}"#;
const LANGS: &str = r#"{"languages":["c","c++","dart","elixir","go","java","javascript","kotlin","php","python","ruby","rust","scala","shell","swift","typescript"]}"#;
const SCOUTED: &str = r#"{"scouted":true}"#;
const PR_COND: &str =
    r#"{"actions":["opened","synchronize","reopened"],"drafts":"exclude","origin":"same_repo"}"#;

const BRIEF_DEPS: &str = "The findings to fix are in the issue named in the Trigger event line of this card. This card runs the Vulnetix CLI fix job on the repository: the harness runs the command, so run nothing else. Leave the changed files on the card's branch.";
const BRIEF_SAST: &str = concat!(
    "The findings to fix are in the issue named in the Trigger event line of this card (the run started when its label was added). The issue lists each finding by id, severity, kind, repository path and reference. Fix those findings with the smallest change that removes them, and run the project's tests before you finish. Never weaken a test, a lint rule or a scanner setting to make a finding go away. If a finding needs a larger decision than a small change, leave it and say so in the card's note.",
    " Fix the cause in the code, not the symptom, and keep the behaviour the tests check."
);
const BRIEF_IAC: &str = concat!(
    "The findings to fix are in the issue named in the Trigger event line of this card (the run started when its label was added). The issue lists each finding by id, severity, kind, repository path and reference. Fix those findings with the smallest change that removes them, and run the project's tests before you finish. Never weaken a test, a lint rule or a scanner setting to make a finding go away. If a finding needs a larger decision than a small change, leave it and say so in the card's note.",
    " Change the infrastructure files the findings name only, and keep every resource's name and address so no plan destroys and recreates it."
);
const BRIEF_CONT: &str = concat!(
    "The findings to fix are in the issue named in the Trigger event line of this card (the run started when its label was added). The issue lists each finding by id, severity, kind, repository path and reference. Fix those findings with the smallest change that removes them, and run the project's tests before you finish. Never weaken a test, a lint rule or a scanner setting to make a finding go away. If a finding needs a larger decision than a small change, leave it and say so in the card's note.",
    " For a container finding, pin the base image to a digest, run as a non-root user and remove what the image does not need."
);
const BRIEF_PQC: &str = concat!(
    "The findings to fix are in the issue named in the Trigger event line of this card (the run started when its label was added). The issue lists each finding by id, severity, kind, repository path and reference. Fix those findings with the smallest change that removes them, and run the project's tests before you finish. Never weaken a test, a lint rule or a scanner setting to make a finding go away. If a finding needs a larger decision than a small change, leave it and say so in the card's note.",
    " Each finding names a weak or quantum-vulnerable algorithm. Move it to the algorithm the project's own policy or the standard library recommends, and keep data written with the old algorithm readable."
);
const BRIEF_GHA: &str = concat!(
    "The findings to fix are in the issue named in the Trigger event line of this card (the run started when its label was added). The issue lists each finding by id, severity, kind, repository path and reference. Fix those findings with the smallest change that removes them, and run the project's tests before you finish. Never weaken a test, a lint rule or a scanner setting to make a finding go away. If a finding needs a larger decision than a small change, leave it and say so in the card's note.",
    " For a workflow finding, pin actions to a full commit SHA, give each job the least token permissions it needs and keep untrusted input out of shell scripts."
);
const BRIEF_LIC: &str = "The license findings are in the open issue titled \"Vulnetix license conflicts\" in this repository, which the previous step of this run filed. For each one decide, from the dependency's use in this repository, whether it needs an exception or a change to the license policy, and write that to the repository's license policy files as your profile describes. Never remove a conflict by editing a dependency's own license text.";
const BRIEF_SECR: &str = "The secret findings are in the open issue labelled sf:secret and titled \"Vulnetix secret findings\" in this repository, which the previous step of this run filed. Remove the copies of each secret from the working tree and replace them with a read from the environment. Never print, copy or commit a secret's value. Rotating a credential and rewriting history are for a person: write the rotation checklist for each secret into the pull request description instead of acting on it.";
const BRIEF_DEFEND: &str = "The findings that have no fix are in the issue named in the Trigger event line of this card (the run started when its label was added). For each advisory the issue names, run the Vulnetix CLI defend command and write the block rules or detections it generates into the repository where its documentation says they belong, and change no application code.";

const fn cron(title: &'static str, c: &'static str) -> T {
    T {
        kind: "cron",
        title,
        cron: c,
        ..t()
    }
}
const fn push(
    title: &'static str,
    paths: &'static [&'static str],
    debounce: i64,
    per_hour: i64,
) -> T {
    T {
        kind: "push",
        title,
        paths,
        debounce,
        per_hour,
        ..t()
    }
}
const fn ev(
    title: &'static str,
    event: &'static str,
    cond: &'static str,
    debounce: i64,
    per_hour: i64,
) -> T {
    T {
        kind: "event",
        title,
        event,
        cond,
        debounce,
        per_hour,
        ..t()
    }
}
const fn label(title: &'static str, l: &'static str) -> T {
    T {
        kind: "label",
        title,
        label: l,
        ..t()
    }
}
const fn scan(name: &'static str, title: &'static str, job: &'static str) -> S {
    S {
        name,
        kind: "scan",
        title,
        job,
        ..s()
    }
}
const fn scan_after(
    name: &'static str,
    title: &'static str,
    job: &'static str,
    after: &'static str,
) -> S {
    S {
        name,
        kind: "scan",
        title,
        job,
        after,
        ..s()
    }
}
const fn not_eval(name: &'static str, title: &'static str, job: &'static str) -> S {
    S {
        name,
        kind: "scan",
        title,
        job,
        cfg: r#"{"notEvaluable":"report"}"#,
        ..s()
    }
}
const fn not_eval_after(
    name: &'static str,
    title: &'static str,
    job: &'static str,
    after: &'static str,
) -> S {
    S {
        name,
        kind: "scan",
        title,
        job,
        after,
        cfg: r#"{"notEvaluable":"report"}"#,
        ..s()
    }
}
const fn decide(name: &'static str, title: &'static str) -> S {
    S {
        name,
        kind: "decide",
        title,
        ..s()
    }
}
const fn ask(name: &'static str, title: &'static str, q: &'static str) -> S {
    S {
        name,
        kind: "decide",
        title,
        brief: q,
        ..s()
    }
}
const fn issues(name: &'static str, title: &'static str, cfg: &'static str) -> S {
    S {
        name,
        kind: "issues",
        title,
        cfg,
        ..s()
    }
}
const fn crew(
    name: &'static str,
    title: &'static str,
    agent: &'static str,
    brief: &'static str,
    tokens: i64,
) -> S {
    S {
        name,
        kind: "crew",
        title,
        agent,
        brief,
        tokens,
        ..s()
    }
}
const fn gate(name: &'static str, loop_to: &'static str) -> S {
    S {
        name,
        kind: "gate",
        title: "Quality gate",
        loop_to,
        ..s()
    }
}
const fn pr(name: &'static str) -> S {
    S {
        name,
        kind: "pr",
        title: "Draft pull request",
        ..s()
    }
}
const fn human(name: &'static str, title: &'static str) -> S {
    S {
        name,
        kind: "human",
        title,
        ..s()
    }
}
const fn other(
    name: &'static str,
    kind: &'static str,
    title: &'static str,
    cfg: &'static str,
) -> S {
    S {
        name,
        kind,
        title,
        cfg,
        ..s()
    }
}

macro_rules! fix {
    ($agent:expr, $brief:expr, $rescan:expr, $tokens:expr) => {
        [
            crew("fix", "Fix the findings", $agent, $brief, $tokens),
            scan("rescan", "Scan again", $rescan),
            gate("gate", "fix"),
            pr("pr"),
        ]
    };
}

const NIGHTLY: &str = "0 2 * * *";

pub static SPECS: &[Spec] = &[
    Spec {
        key: "cli-scan-nightly",
        name: "Default: nightly scan",
        min_autonomy: "observe",
        threshold: "high",
        matches: SCOUTED,
        description: "Runs the Vulnetix CLI scan on a repository the repo scout has profiled, every night and after a manifest change on the default branch, and keeps one digest issue current with the findings at or above high severity. It changes no code.",
        triggers: &[
            cron("Nightly scan", NIGHTLY),
            push("Manifest changed on the default branch", MANIFESTS, 900, 2),
        ],
        stages: &[
            scan("scan", "Scan the repository", "scan"),
            decide("decide", "Findings worth reporting"),
            issues(
                "issues",
                "File the digest issue",
                r#"{"title":"Vulnetix nightly scan","minSeverity":"high"}"#,
            ),
        ],
    },
    Spec {
        key: "cli-sca",
        name: "Default: dependency findings",
        min_autonomy: "observe",
        threshold: "high",
        matches: PM,
        description: "Scans the repository's dependencies with the Vulnetix CLI weekly and after a manifest change on the default branch. Findings at or above high severity become an issue labelled sf:fix-deps, which starts the dependency fix workflow. A person adds sf:no-fix to a finding that has no fixed version to start the defend workflow.",
        triggers: &[
            cron("Weekly dependency scan", "0 3 * * 1"),
            push("Manifest changed on the default branch", MANIFESTS, 900, 2),
        ],
        stages: &[
            scan("scan", "Scan the dependencies", "sca"),
            decide("decide", "Findings worth fixing"),
            issues(
                "issues",
                "File the dependency issue",
                r#"{"label":"sf:fix-deps","title":"Vulnetix dependency findings","minSeverity":"high"}"#,
            ),
        ],
    },
    Spec {
        key: "cli-sca-fix",
        name: "Default: dependency fixes",
        min_autonomy: "draft_pr",
        threshold: "high",
        matches: PM,
        description: "When the factory adds sf:fix-deps to a dependency findings issue (or a team member does), runs the Vulnetix CLI fix job on belai:cli-runner-write, scans again, checks the quality gate and opens a draft pull request. It never merges.",
        triggers: &[label("Label sf:fix-deps", "sf:fix-deps")],
        stages: &[
            S {
                job: "fix",
                ..crew(
                    "fix",
                    "Fix the findings",
                    "belai:cli-runner-write",
                    BRIEF_DEPS,
                    150_000,
                )
            },
            scan("rescan", "Scan again", "sca"),
            gate("gate", "fix"),
            pr("pr"),
        ],
    },
    Spec {
        key: "cli-sast",
        name: "Default: code findings",
        min_autonomy: "observe",
        threshold: "high",
        matches: LANGS,
        description: "Runs the Vulnetix CLI SAST scan weekly and after a push to the default branch, in a repository written in a language the rules cover. Each rule with findings at or above high severity becomes an issue (at most 5) labelled sf:fix-code, which starts the code fix workflow.",
        triggers: &[
            cron("Weekly code scan", "0 3 * * 2"),
            push("Push to the default branch", &[], 3600, 1),
        ],
        stages: &[
            scan("scan", "Scan the code", "sast"),
            decide("decide", "Findings worth fixing"),
            issues(
                "issues",
                "File an issue for each rule",
                r#"{"label":"sf:fix-code","title":"Vulnetix code finding","minSeverity":"high","group":"id","maxIssues":5}"#,
            ),
        ],
    },
    Spec {
        key: "cli-sast-fix",
        name: "Default: code fixes",
        min_autonomy: "draft_pr",
        threshold: "high",
        matches: LANGS,
        description: "When sf:fix-code is added to a code findings issue, belai:patcher fixes the rule's findings, the Vulnetix CLI scans again, the quality gate checks the result and a draft pull request opens. It never merges.",
        triggers: &[label("Label sf:fix-code", "sf:fix-code")],
        stages: &fix!("belai:patcher", BRIEF_SAST, "sast", 200_000),
    },
    Spec {
        key: "cli-secrets",
        name: "Default: secrets",
        min_autonomy: "draft_pr",
        threshold: "low",
        matches: SCOUTED,
        description: "Scans for secrets weekly, after a push to the default branch and when GitHub raises a secret scanning alert. An issue labelled sf:secret names each secret by id and location only. belai:secret-triager removes the copies from the working tree and opens a draft pull request with a rotation checklist. Rotating a credential and rewriting history stay with a person.",
        triggers: &[
            cron("Weekly secrets scan", "0 3 * * 3"),
            push("Push to the default branch", &[], 3600, 1),
            ev(
                "Secret scanning alert raised",
                "secret_scanning_alert.created",
                r#"{"origin":"same_repo"}"#,
                0,
                12,
            ),
        ],
        stages: &[
            scan("scan", "Scan for secrets", "secrets"),
            decide("decide", "Secrets worth acting on"),
            issues(
                "issues",
                "File the secrets issue",
                r#"{"label":"sf:secret","title":"Vulnetix secret findings","minSeverity":"any","kinds":["secrets"]}"#,
            ),
            crew(
                "fix",
                "Fix the findings",
                "belai:secret-triager",
                BRIEF_SECR,
                150_000,
            ),
            scan("rescan", "Scan again", "secrets"),
            gate("gate", "fix"),
            pr("pr"),
        ],
    },
    Spec {
        key: "cli-iac",
        name: "Default: infrastructure findings",
        min_autonomy: "observe",
        threshold: "high",
        matches: r#"{"signals":["terraform","cloudformation","k8s","helm"]}"#,
        description: "Scans Terraform, CloudFormation, Kubernetes and Helm files with the Vulnetix CLI weekly and after a change to them. Findings at or above high severity become an issue labelled sf:fix-iac, which starts the infrastructure fix workflow.",
        triggers: &[
            cron("Weekly infrastructure scan", "0 3 * * 4"),
            push(
                "Infrastructure files changed on the default branch",
                &[
                    "**/*.tf",
                    "**/Chart.yaml",
                    "**/k8s/**",
                    "**/kubernetes/**",
                    "**/manifests/**",
                    "**/template.y*ml",
                    "**/template.json",
                ],
                900,
                2,
            ),
        ],
        stages: &[
            scan("scan", "Scan the infrastructure", "iac"),
            decide("decide", "Findings worth fixing"),
            issues(
                "issues",
                "File the infrastructure issue",
                r#"{"label":"sf:fix-iac","title":"Vulnetix infrastructure findings","minSeverity":"high"}"#,
            ),
        ],
    },
    Spec {
        key: "cli-iac-fix",
        name: "Default: infrastructure fixes",
        min_autonomy: "draft_pr",
        threshold: "high",
        matches: r#"{"signals":["terraform","cloudformation","k8s","helm"]}"#,
        description: "When sf:fix-iac is added to an infrastructure findings issue, belai:iac-fixer changes the files the findings name, the Vulnetix CLI scans again, the quality gate checks the result and a draft pull request opens. It never merges and never applies a plan.",
        triggers: &[label("Label sf:fix-iac", "sf:fix-iac")],
        stages: &fix!("belai:iac-fixer", BRIEF_IAC, "iac", 200_000),
    },
    Spec {
        key: "cli-containers",
        name: "Default: container findings",
        min_autonomy: "observe",
        threshold: "high",
        matches: r#"{"signals":["dockerfile","compose","helm"]}"#,
        description: "Scans Dockerfiles, Compose files and Helm charts with the Vulnetix CLI weekly and after a change to them. Findings at or above high severity become an issue labelled sf:fix-container, which starts the container fix workflow.",
        triggers: &[
            cron("Weekly container scan", "0 3 * * 5"),
            push(
                "Container files changed on the default branch",
                &[
                    "**/Dockerfile*",
                    "**/Containerfile*",
                    "**/docker-compose*.y*ml",
                    "**/compose.y*ml",
                    "**/Chart.yaml",
                ],
                900,
                2,
            ),
        ],
        stages: &[
            scan("scan", "Scan the container files", "containers"),
            decide("decide", "Findings worth fixing"),
            issues(
                "issues",
                "File the container issue",
                r#"{"label":"sf:fix-container","title":"Vulnetix container findings","minSeverity":"high"}"#,
            ),
        ],
    },
    Spec {
        key: "cli-containers-fix",
        name: "Default: container fixes",
        min_autonomy: "draft_pr",
        threshold: "high",
        matches: r#"{"signals":["dockerfile","compose","helm"]}"#,
        description: "When sf:fix-container is added to a container findings issue, belai:container-fixer pins the base image, drops root and trims the image, the Vulnetix CLI scans again, the quality gate checks the result and a draft pull request opens. It never merges.",
        triggers: &[label("Label sf:fix-container", "sf:fix-container")],
        stages: &fix!("belai:container-fixer", BRIEF_CONT, "containers", 200_000),
    },
    Spec {
        key: "cli-image",
        name: "Default: published image inventory",
        min_autonomy: "observe",
        threshold: "high",
        matches: r#"{"signals":["dockerfile","release-artifacts"]}"#,
        description: "When a container image is published to GitHub Packages, runs the Vulnetix CLI image inventory on it and keeps one issue current with the findings at or above high severity. It changes no code.",
        triggers: &[ev(
            "Image published",
            "registry_package",
            r#"{"actions":["published","updated"]}"#,
            300,
            12,
        )],
        stages: &[
            not_eval("scan", "Inventory the image", "image"),
            decide("decide", "Findings worth reporting"),
            issues(
                "issues",
                "File the image issue",
                r#"{"title":"Vulnetix published image","minSeverity":"high"}"#,
            ),
        ],
    },
    Spec {
        key: "cli-cbom-pqc",
        name: "Default: cryptography inventory",
        min_autonomy: "observe",
        threshold: "medium",
        matches: r#"{"signals":["crypto"]}"#,
        description: "Builds a cryptography bill of materials with the Vulnetix CLI weekly and after a manifest change, in a repository the scout found cryptography in. Each weak or quantum-vulnerable algorithm at or above medium severity becomes an issue (at most 5) labelled sf:pqc, which starts the migration workflow.",
        triggers: &[
            cron("Weekly cryptography scan", "0 3 * * 6"),
            push("Manifest changed on the default branch", MANIFESTS, 900, 2),
        ],
        stages: &[
            scan("scan", "Build the cryptography inventory", "cbom"),
            decide("decide", "Algorithms worth migrating"),
            issues(
                "issues",
                "File an issue for each algorithm",
                r#"{"label":"sf:pqc","title":"Vulnetix cryptography finding","minSeverity":"medium","group":"id","maxIssues":5}"#,
            ),
        ],
    },
    Spec {
        key: "cli-cbom-pqc-fix",
        name: "Default: cryptography migration",
        min_autonomy: "draft_pr",
        threshold: "medium",
        matches: r#"{"signals":["crypto"]}"#,
        description: "When sf:pqc is added to a cryptography finding, belai:pqc-fixer moves the code to a safer algorithm, the Vulnetix CLI rebuilds the inventory, the quality gate checks the result and a draft pull request opens. It never merges.",
        triggers: &[label("Label sf:pqc", "sf:pqc")],
        stages: &fix!("belai:pqc-fixer", BRIEF_PQC, "cbom", 200_000),
    },
    Spec {
        key: "cli-aibom",
        name: "Default: AI inventory",
        min_autonomy: "observe",
        threshold: "medium",
        matches: r#"{"signals":["ai-sdk","agent-config"]}"#,
        description: "Builds an AI bill of materials with the Vulnetix CLI weekly and after a manifest change, in a repository that uses an AI library or holds an agent configuration. Findings at or above medium severity become an issue labelled sf:ai for the team to review. It changes no code.",
        triggers: &[
            cron("Weekly AI inventory", "0 3 * * 0"),
            push("Manifest changed on the default branch", MANIFESTS, 900, 2),
        ],
        stages: &[
            scan("scan", "Build the AI inventory", "aibom"),
            decide("decide", "Findings worth reviewing"),
            issues(
                "issues",
                "File the AI inventory issue",
                r#"{"label":"sf:ai","title":"Vulnetix AI inventory","minSeverity":"medium"}"#,
            ),
        ],
    },
    Spec {
        key: "cli-license",
        name: "Default: license policy",
        min_autonomy: "draft_pr",
        threshold: "low",
        matches: PM,
        description: "Checks dependency licenses against the repository's policy with the Vulnetix CLI weekly and after a manifest change. A conflict becomes an issue, and belai:license-fixer adds the exception or policy change in a draft pull request for a person to review. It never merges.",
        triggers: &[
            cron("Weekly license check", "0 4 * * 1"),
            push("Manifest changed on the default branch", MANIFESTS, 900, 2),
        ],
        stages: &[
            scan("scan", "Check the licenses", "license"),
            decide("decide", "Conflicts worth acting on"),
            issues(
                "issues",
                "File the license issue",
                r#"{"title":"Vulnetix license conflicts","minSeverity":"any"}"#,
            ),
            crew(
                "fix",
                "Fix the findings",
                "belai:license-fixer",
                BRIEF_LIC,
                150_000,
            ),
            scan("rescan", "Scan again", "license"),
            gate("gate", "fix"),
            pr("pr"),
        ],
    },
    Spec {
        key: "cli-malscan",
        name: "Default: malware scan",
        min_autonomy: "observe",
        threshold: "low",
        matches: r#"{"signals":["installed-deps"]}"#,
        description: "Scans the installed dependencies for malware with the Vulnetix CLI daily and after a manifest change. A hit becomes an urgent issue labelled sf:malware, a card on the board and a critical notification. Nothing is fixed automatically: a person decides.",
        triggers: &[
            cron("Daily malware scan", "0 1 * * *"),
            push("Manifest changed on the default branch", MANIFESTS, 900, 2),
        ],
        stages: &[
            scan("scan", "Scan for malware", "malscan"),
            decide("decide", "Hits worth raising"),
            issues(
                "issues",
                "File the malware issue",
                r#"{"label":"sf:malware","title":"Vulnetix malware finding","minSeverity":"any","kinds":["malscan"]}"#,
            ),
            other(
                "card",
                "card",
                "Malware response card",
                r#"{"ref":"malware-response"}"#,
            ),
            other(
                "notify",
                "notify",
                "Notify the team",
                r#"{"severity":"critical","message":"A dependency of {repo} was flagged as malware. See the issue labelled sf:malware."}"#,
            ),
        ],
    },
    Spec {
        key: "cli-sbom-publish",
        name: "Default: publish the SBOM",
        min_autonomy: "draft_pr",
        threshold: "",
        matches: PM,
        description: "Generates a CycloneDX SBOM with the Vulnetix CLI when a release is published and weekly, publishes it to TEA when the repository has TEA enabled, and sends the dependency graph and the code scanning results to GitHub. It changes no code.",
        triggers: &[
            ev("Release published", "release.published", "", 0, 6),
            cron("Weekly SBOM", "0 5 * * 2"),
        ],
        stages: &[
            scan("sbom", "Generate the SBOM", "sbom"),
            human("approve", "Approve publishing the SBOM"),
            S {
                after: "approve",
                ..other("tea", "tea", "Publish to TEA", "")
            },
            scan_after(
                "graph",
                "Send the dependency graph",
                "gha-dep-graph",
                "approve",
            ),
            scan_after(
                "code",
                "Send the code scanning results",
                "gha-code-scanning",
                "graph",
            ),
        ],
    },
    Spec {
        key: "cli-bom-diff",
        name: "Default: SBOM change on a pull request",
        min_autonomy: "suggest",
        threshold: "high",
        matches: PM,
        description: "When a pull request from this repository changes, compares its SBOM with the base branch's with the Vulnetix CLI, validates it, and posts a check run and a comment with the delta. The check fails on findings at or above high severity.",
        triggers: &[ev(
            "Pull request opened or updated",
            "pull_request",
            PR_COND,
            60,
            60,
        )],
        stages: &[
            not_eval("diff", "Compare the SBOM with the base", "bom-diff"),
            not_eval("validate", "Validate the SBOM", "bom-validate"),
            decide("decide", "Changes worth flagging"),
            other(
                "check",
                "check",
                "Post the check run",
                r#"{"name":"Vulnetix SBOM change","minSeverity":"high"}"#,
            ),
            S {
                after: "check",
                ..other("comment", "comment", "Comment the delta", "")
            },
        ],
    },
    Spec {
        key: "cli-bom-hygiene",
        name: "Default: SBOM hygiene",
        min_autonomy: "draft_pr",
        threshold: "medium",
        matches: r#"{"signals":["monorepo"]}"#,
        description: "In a repository with several package managers or directories, finds version skew between the SBOMs with the Vulnetix CLI weekly, files an issue for it, and builds one merged, enriched SBOM. It changes no code.",
        triggers: &[
            cron("Weekly SBOM hygiene", "0 5 * * 3"),
            push("Manifest changed on the default branch", MANIFESTS, 900, 2),
        ],
        stages: &[
            scan("skew", "Find version skew", "bom-skew"),
            ask(
                "decide",
                "Skew worth reporting",
                "Does the scan report version skew between the SBOMs that should be filed?",
            ),
            issues(
                "issues",
                "File the skew issue",
                r#"{"title":"Vulnetix version skew","minSeverity":"medium"}"#,
            ),
            not_eval_after("merge", "Merge the SBOMs", "bom-merge", "skew"),
            not_eval_after("enrich", "Enrich the merged SBOM", "bom-enrich", "merge"),
        ],
    },
    Spec {
        key: "cli-vex-triage",
        name: "Default: VEX triage",
        min_autonomy: "draft_pr",
        threshold: "high",
        matches: PM,
        description: "When Dependabot raises a critical or high alert, and weekly, triages the dependency findings with the Vulnetix CLI, writes an OpenVEX document of the verdicts to the run's evidence and uploads it to Vulnetix. It changes no code.",
        triggers: &[
            ev(
                "Dependabot alert raised",
                "dependabot_alert.created",
                r#"{"severity":["critical","high"]}"#,
                0,
                12,
            ),
            cron("Weekly triage", "0 5 * * 4"),
        ],
        stages: &[
            scan("triage", "Triage the findings", "triage"),
            ask(
                "decide",
                "Findings worth a statement",
                "Do the triaged findings need a VEX statement uploaded?",
            ),
            human("approve", "Approve uploading the VEX statements"),
            other("vex", "vex", "Write the OpenVEX document", ""),
            not_eval("apply", "Upload the VEX statements", "vex-apply"),
        ],
    },
    Spec {
        key: "cli-ignore-sync",
        name: "Default: suppression sync",
        min_autonomy: "draft_pr",
        threshold: "",
        matches: r#"{"signals":["vulnetix-config"]}"#,
        description: "When the Vulnetix configuration changes, and weekly, syncs the suppression rules into .vulnetix/memory.yaml with the Vulnetix CLI and opens a draft pull request with the change. A repository already in sync gets none.",
        triggers: &[
            push(
                "Configuration changed on the default branch",
                &[".vulnetix/**"],
                900,
                2,
            ),
            cron("Weekly suppression sync", "0 5 * * 5"),
        ],
        stages: &[
            scan("sync", "Sync the suppression rules", "ignore-sync"),
            pr("pr"),
        ],
    },
    Spec {
        key: "cli-defend",
        name: "Default: rules for findings with no fix",
        min_autonomy: "draft_pr",
        threshold: "high",
        matches: PM,
        description: "When sf:no-fix is added to a findings issue, has belai:defend-writer run the Vulnetix CLI defend command for each advisory the issue names and put the rules where they belong, checks the quality gate and opens a draft pull request. It never merges.",
        triggers: &[label("Label sf:no-fix", "sf:no-fix")],
        stages: &[
            crew(
                "write",
                "Write the rules into the repository",
                "belai:defend-writer",
                BRIEF_DEFEND,
                120_000,
            ),
            gate("gate", "write"),
            pr("pr"),
        ],
    },
    Spec {
        key: "cli-jail-gate",
        name: "Default: jail gate on a pull request",
        min_autonomy: "suggest",
        threshold: "high",
        matches: PM,
        description: "When a pull request from this repository changes, evaluates the organisation's jail policy with the Vulnetix CLI and posts a check run: it fails when the policy is breached and reports not evaluable when the CLI cannot tell. A breach also gets a comment explaining the rule.",
        triggers: &[ev(
            "Pull request opened or updated",
            "pull_request",
            PR_COND,
            60,
            60,
        )],
        stages: &[
            not_eval("jail", "Evaluate the jail policy", "jail"),
            other(
                "check",
                "check",
                "Post the check run",
                r#"{"name":"Vulnetix jail","minSeverity":"any"}"#,
            ),
            S {
                after: "jail",
                brief: "Does the jail evaluation report a breach that needs an explanation?",
                ..decide("decide", "Breach worth explaining")
            },
            scan("explain", "Explain the breach", "jail-explain"),
            other("comment", "comment", "Comment the explanation", ""),
        ],
    },
    Spec {
        key: "cli-gha-posture",
        name: "Default: workflow posture",
        min_autonomy: "observe",
        threshold: "high",
        matches: r#"{"signals":["github-workflows"]}"#,
        description: "Audits the repository's GitHub Actions workflows with the Vulnetix CLI weekly and after a workflow change. Findings at or above high severity become an issue labelled sf:fix-workflow, which starts the workflow fix.",
        triggers: &[
            cron("Weekly workflow audit", "0 5 * * 6"),
            push(
                "Workflow changed on the default branch",
                &[".github/workflows/*.y*ml"],
                900,
                2,
            ),
        ],
        stages: &[
            scan("scan", "Audit the workflows", "gha-audit"),
            decide("decide", "Findings worth fixing"),
            issues(
                "issues",
                "File the workflow issue",
                r#"{"label":"sf:fix-workflow","title":"Vulnetix workflow findings","minSeverity":"high"}"#,
            ),
        ],
    },
    Spec {
        key: "cli-gha-posture-fix",
        name: "Default: workflow fixes",
        min_autonomy: "draft_pr",
        threshold: "high",
        matches: r#"{"signals":["github-workflows"]}"#,
        description: "When sf:fix-workflow is added to a workflow findings issue, belai:workflow-hardener pins actions and tightens token permissions, the Vulnetix CLI audits again, the quality gate checks the result and a draft pull request opens. It never merges.",
        triggers: &[label("Label sf:fix-workflow", "sf:fix-workflow")],
        stages: &fix!("belai:workflow-hardener", BRIEF_GHA, "gha-audit", 150_000),
    },
    Spec {
        key: "cli-gha-onboard",
        name: "Default: onboard to Vulnetix workflows",
        min_autonomy: "draft_pr",
        threshold: "",
        matches: SCOUTED,
        description: "Weekly, and when a person asks for a new scout with sf:scout, in a repository the scout has profiled and once a repository admin approves, adds the managed Vulnetix workflow with the Vulnetix CLI and opens a draft pull request. A repository that already has the workflow gets no pull request.",
        triggers: &[
            cron("Weekly onboarding check", "0 6 * * 0"),
            label("Label sf:scout", "sf:scout"),
        ],
        stages: &[
            human("approve", "Approve adding the Vulnetix workflow"),
            scan("init", "Add the managed workflow", "gha-init"),
            pr("pr"),
        ],
    },
    Spec {
        key: "cli-advisories",
        name: "Default: draft security advisories",
        min_autonomy: "draft_pr",
        threshold: "critical",
        matches: LANGS,
        description: "When code scanning raises a critical or high alert, and weekly, runs the SAST scan with the Vulnetix CLI and, once a repository admin approves, drafts GitHub security advisories for the critical findings. Nothing is published.",
        triggers: &[
            ev(
                "Code scanning alert raised",
                "code_scanning_alert.created",
                r#"{"severity":["critical","high","error"]}"#,
                0,
                12,
            ),
            cron("Weekly advisory check", "0 6 * * 1"),
        ],
        stages: &[
            scan("scan", "Scan the code", "sast"),
            ask(
                "decide",
                "Findings worth an advisory",
                "Does the scan report a critical finding that deserves a draft security advisory?",
            ),
            human("approve", "Approve drafting advisories"),
            scan("draft", "Draft the advisories", "gha-advisories"),
            other(
                "notify",
                "notify",
                "Tell the team the drafts are ready",
                r#"{"severity":"notice","message":"Draft security advisories for {repo} are ready for review on GitHub."}"#,
            ),
        ],
    },
    Spec {
        key: "cli-config-plan",
        name: "Default: Vulnetix configuration plan",
        min_autonomy: "draft_pr",
        threshold: "low",
        matches: r#"{"signals":["vulnetix-config"]}"#,
        description: "When a pull request changes the Vulnetix configuration, runs the Vulnetix CLI configuration plan and comments the plan on the pull request. After a change reaches the default branch, a repository admin approves before the configuration is applied.",
        triggers: &[
            T {
                to: "planpr",
                ..ev(
                    "Pull request opened or updated",
                    "pull_request",
                    PR_COND,
                    60,
                    60,
                )
            },
            T {
                to: "plan",
                ..push(
                    "Configuration changed on the default branch",
                    &[".vulnetix/**"],
                    900,
                    2,
                )
            },
        ],
        stages: &[
            not_eval("planpr", "Plan the change", "config-plan"),
            other("comment", "comment", "Comment the plan", ""),
            S {
                after: "-",
                ..not_eval("plan", "Plan the change", "config-plan")
            },
            ask(
                "decide",
                "Plan worth applying",
                "Does the configuration plan change anything that should be applied?",
            ),
            human("approve", "Approve applying the configuration"),
            scan("apply", "Apply the configuration", "config-apply"),
        ],
    },
    Spec {
        key: "cli-ai-firewall",
        name: "Default: AI firewall policy",
        min_autonomy: "draft_pr",
        threshold: "medium",
        matches: r#"{"signals":["vulnetix-config","agent-config"]}"#,
        description: "Checks the repository's AI firewall policy against the live firewall with the Vulnetix CLI weekly and after the policy file changes. A difference becomes an issue labelled sf:ai, and once a repository admin approves the policy is applied.",
        triggers: &[
            push(
                "Policy changed on the default branch",
                &[".vulnetix/ai-firewall.y*ml"],
                900,
                2,
            ),
            cron("Weekly policy check", "0 6 * * 4"),
        ],
        stages: &[
            scan(
                "status",
                "Compare the policy with the firewall",
                "ai-firewall-status",
            ),
            ask(
                "decide",
                "Differences worth applying",
                "Does the live AI firewall differ from the repository's policy in a way that should be applied?",
            ),
            issues(
                "issues",
                "File the policy issue",
                r#"{"label":"sf:ai","title":"Vulnetix AI firewall policy","minSeverity":"medium"}"#,
            ),
            human("approve", "Approve applying the policy"),
            scan("apply", "Apply the policy", "ai-firewall-apply"),
        ],
    },
    Spec {
        key: "cli-package-firewall",
        name: "Default: package firewall advice",
        min_autonomy: "observe",
        threshold: "",
        matches: PM,
        description: "Keeps one issue current with the exact vulnetix package-firewall command for each package manager the repository uses, weekly and after a manifest change. The factory changes no machine; the issue is the whole recommendation.",
        triggers: &[
            cron("Weekly package firewall advice", "0 6 * * 5"),
            push("Manifest changed on the default branch", MANIFESTS, 900, 2),
        ],
        stages: &[issues(
            "issues",
            "File the package firewall issue",
            r#"{"from":"profile","title":"Vulnetix package firewall"}"#,
        )],
    },
    Spec {
        key: "cli-attest",
        name: "Default: verify release provenance",
        min_autonomy: "observe",
        threshold: "low",
        matches: r#"{"signals":["sigstore","release-artifacts"]}"#,
        description: "When a release is published, verifies its provenance and signatures with the Vulnetix CLI and keeps one issue current with any failure. It changes no code.",
        triggers: &[ev("Release published", "release.published", "", 0, 6)],
        stages: &[
            not_eval("verify", "Verify the release", "attest-verify"),
            ask(
                "decide",
                "Failures worth reporting",
                "Does the verification report a failure that should be filed?",
            ),
            issues(
                "issues",
                "File the provenance issue",
                r#"{"title":"Vulnetix release provenance","minSeverity":"any"}"#,
            ),
        ],
    },
    Spec {
        key: "cli-tea-release",
        name: "Default: release to TEA",
        min_autonomy: "draft_pr",
        threshold: "",
        matches: r#"{"signals":["release-artifacts"]}"#,
        description: "When a release is published in a repository with release artifacts, publishes it to TEA with the Vulnetix CLI and tells the team. It changes no code.",
        triggers: &[ev("Release published", "release.published", "", 0, 6)],
        stages: &[
            human("approve", "Approve publishing the release to TEA"),
            scan("release", "Publish the release to TEA", "tea-release"),
            other(
                "notify",
                "notify",
                "Tell the team",
                r#"{"severity":"info","message":"A release of {repo} was published to TEA."}"#,
            ),
        ],
    },
    Spec {
        key: "cli-api-surface",
        name: "Default: API surface",
        min_autonomy: "draft_pr",
        threshold: "medium",
        matches: r#"{"signals":["openapi"]}"#,
        description: "Finds the OpenAPI descriptions in a repository with the Vulnetix CLI weekly and after one changes, uploads them to Vulnetix, and checks that the routes still answer. Routes that stop answering become an issue.",
        triggers: &[
            cron("Weekly API surface check", "0 6 * * 6"),
            push(
                "API description changed on the default branch",
                &["**/openapi.*", "**/swagger.*"],
                900,
                2,
            ),
        ],
        stages: &[
            scan("discover", "Find the API descriptions", "api-discover"),
            S {
                after: "discover",
                ..human("approve", "Approve uploading the descriptions")
            },
            scan_after("upload", "Upload the descriptions", "api-upload", "approve"),
            scan_after(
                "live",
                "Check the routes answer",
                "surface-liveness",
                "discover",
            ),
            ask(
                "decide",
                "Routes worth reporting",
                "Do any of the API routes fail to answer?",
            ),
            issues(
                "issues",
                "File the API surface issue",
                r#"{"title":"Vulnetix API surface","minSeverity":"medium"}"#,
            ),
        ],
    },
    Spec {
        key: "cli-analyze",
        name: "Default: exposure analysis",
        min_autonomy: "observe",
        threshold: "high",
        matches: SCOUTED,
        description: "Analyses the repository's tech stack, routes and infrastructure exposure with the Vulnetix CLI weekly and after a manifest change, and keeps one issue current with the exposures at or above high severity. It changes no code.",
        triggers: &[
            cron("Weekly exposure analysis", "0 7 * * 0"),
            push("Manifest changed on the default branch", MANIFESTS, 900, 2),
        ],
        stages: &[
            scan("analyze", "Analyse the repository", "analyze"),
            decide("decide", "Exposure worth reporting"),
            issues(
                "issues",
                "File the exposure issue",
                r#"{"title":"Vulnetix exposure analysis","minSeverity":"high"}"#,
            ),
        ],
    },
];

pub fn spec(key: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.key == key)
}

/// `fdcQuestion`.
pub fn question(threshold: &str) -> String {
    match threshold {
        "low" | "any" | "" => "Does the scan report anything that should be filed?".into(),
        t => {
            format!("Does the scan report a finding at or above {t} severity that should be filed?")
        }
    }
}

fn obj(s: &str) -> Map<String, Value> {
    serde_json::from_str::<Value>(if s.is_empty() { "{}" } else { s })
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn conditions(sp: &Spec, extra: &str) -> Value {
    let mut c = obj(extra);
    c.insert("profile".into(), Value::Object(obj(sp.matches)));
    Value::Object(c)
}

/// `fdcBuild`: the graph of a CLI default for `repo`.
pub fn build(sp: &Spec, repo: &str) -> Graph {
    let n_trig = sp.triggers.len();
    let id_of = |name: &str| -> Option<String> {
        sp.stages
            .iter()
            .position(|s| s.name == name)
            .map(|i| format!("n{}", n_trig + i + 1))
    };
    let at = |i: usize, row: usize| json!({ "x": 40.0 + 220.0 * i as f64, "y": 80.0 + 120.0 * row as f64 });
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let first = sp.stages[0].name;
    for (i, t) in sp.triggers.iter().enumerate() {
        let id = format!("n{}", i + 1);
        let cfg = match t.kind {
            "cron" => {
                json!({"kind": "cron", "cron": t.cron, "overlap": "skip_if_running",
                "conditions": conditions(sp, "")})
            }
            "push" => {
                let mut extra = obj(r#"{"headBranches":["$default"],"origin":"same_repo"}"#);
                if !t.paths.is_empty() {
                    extra.insert("paths".into(), json!(t.paths));
                }
                json!({"kind": "webhook", "event": "push", "overlap": "skip_if_running",
                    "debounceSeconds": t.debounce, "maxRunsPerHour": t.per_hour.max(1),
                    "conditions": conditions(sp, &Value::Object(extra).to_string())})
            }
            "event" => json!({"kind": "webhook", "event": t.event, "overlap": "queue",
                "debounceSeconds": t.debounce, "maxRunsPerHour": t.per_hour.max(1),
                "conditions": conditions(sp, t.cond)}),
            _ => {
                json!({"kind": "label", "label": t.label, "overlap": "queue", "maxRunsPerHour": 10,
                "fromFactory": true, "conditions": conditions(sp, "")})
            }
        };
        nodes.push(json!({"id": id, "type": "trigger", "title": t.title, "config": cfg, "position": at(0, i)}));
        let to = if t.to.is_empty() { first } else { t.to };
        let label = match t.kind {
            "cron" => "repos in scope",
            "label" => "labelled issue",
            _ => "event",
        };
        if let Some(dst) = id_of(to) {
            edges.push(json!({"from": id, "to": dst, "label": label}));
        }
    }
    for (i, st) in sp.stages.iter().enumerate() {
        let id = id_of(st.name).unwrap_or_default();
        let pos = at(i + 1, 0);
        let mut cfg = obj(st.cfg);
        let node = match st.kind {
            "scan" => {
                cfg.insert("source".into(), json!("cli"));
                cfg.insert("job".into(), json!(st.job));
                json!({"id": id, "type": "scan", "title": st.title, "config": cfg})
            }
            "decide" => {
                let q = if st.brief.is_empty() {
                    question(sp.threshold)
                } else {
                    st.brief.to_string()
                };
                json!({"id": id, "type": "decision", "title": st.title, "config": {
                    "question": q, "outcomes": [{"value": "file"}, {"value": "clean", "end": true}],
                    "threshold": 0.8, "belowThreshold": "route", "onAbstain": "route" }})
            }
            "issues" => {
                cfg.insert("action".into(), json!("file_issues"));
                json!({"id": id, "type": "github", "title": st.title, "config": cfg})
            }
            "check" => {
                cfg.insert("action".into(), json!("check_run"));
                json!({"id": id, "type": "github", "title": st.title, "config": cfg})
            }
            "comment" => {
                json!({"id": id, "type": "github", "title": st.title, "config": {"action": "issue_comment"}})
            }
            "human" => json!({"id": id, "type": "human", "title": st.title, "config": {
                "approvers": ["owner"], "slaHours": 72, "onTimeout": "park_and_renotify"}}),
            "crew" => {
                let mut c = json!({"dispatch": "worker", "agent": st.agent, "await": "session",
                    "maxAttempts": 2, "brief": st.brief});
                if !st.job.is_empty() {
                    c["cliJob"] = json!(st.job);
                }
                let mut n = json!({"id": id, "type": "crew", "title": st.title, "config": c});
                if st.tokens > 0 {
                    n["budget"] = json!({"tokens": st.tokens});
                }
                n
            }
            "gate" => {
                json!({"id": id, "type": "gate", "title": st.title, "config": {"check": "forge", "onFail": "return_to_crew"}})
            }
            "pr" => json!({"id": id, "type": "github", "title": st.title, "config": {
                "action": "draft_pr", "prState": "draft", "reviewers": "codeowners", "attach": "sbom_only", "ifNoBranch": "skip"}}),
            // TEA publishing is the CLI's tea-release job on the write runner.
            "tea" => {
                json!({"id": id, "type": "scan", "title": st.title, "config": {"source": "cli", "job": "tea-release"}})
            }
            // The OpenVEX document rides on the triage result; record it in the run.
            "vex" => json!({"id": id, "type": "notify", "title": st.title, "config": {
                "severity": "info", "message": "{workflow}: OpenVEX statements written for {repo} ({run})"}}),
            "card" => json!({"id": id, "type": "notify", "title": st.title, "config": {
                "severity": "critical", "message": "{workflow}: malware response needed on {repo} ({run})"}}),
            _ => {
                let mut c = cfg.clone();
                c.entry("severity").or_insert(json!("info"));
                json!({"id": id, "type": "notify", "title": st.title, "config": c})
            }
        };
        let mut node = node;
        node["position"] = pos;
        nodes.push(node);
        let from = if !st.after.is_empty() {
            st.after
        } else if i > 0 {
            sp.stages[i - 1].name
        } else {
            ""
        };
        if from.is_empty() || from == "-" {
            continue;
        }
        for f in from.split(',') {
            let Some(src) = id_of(f) else { continue };
            let prev = sp.stages.iter().find(|s| s.name == f);
            if prev.is_some_and(|p| p.kind == "decide") {
                for o in ["file", "$below_threshold", "$unknown"] {
                    edges.push(json!({"from": src, "to": id, "outcome": o}));
                }
                continue;
            }
            let label = match prev.map(|p| p.kind) {
                Some("scan") => "findings.json",
                Some("crew") => "fix branch",
                Some("gate") => "pass",
                Some("issues") => "issue",
                _ => "next",
            };
            edges.push(json!({"from": src, "to": id, "label": label}));
        }
        if st.kind == "gate"
            && !st.loop_to.is_empty()
            && let Some(dst) = id_of(st.loop_to)
        {
            edges.push(json!({"from": id, "to": dst, "outcome": "fail", "loop": true, "label": "return_to_crew"}));
        }
    }
    serde_json::from_value(json!({"nodes": nodes, "edges": edges, "scope": {"repos": [repo]}}))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::{Mode, validate};

    #[test]
    fn all_34_build_and_validate() {
        assert_eq!(SPECS.len(), 34);
        let mut keys = std::collections::HashSet::new();
        for sp in SPECS {
            assert!(keys.insert(sp.key), "{}", sp.key);
            let g = build(sp, "acme/api");
            let issues = validate(&g, Mode::Publish);
            assert!(issues.is_empty(), "{}: {issues:?}", sp.key);
            // Every trigger carries the profile condition (fail closed).
            for t in g.triggers() {
                assert!(
                    t.config
                        .get("conditions")
                        .and_then(|c| c.get("profile"))
                        .is_some(),
                    "{}",
                    sp.key
                );
            }
        }
        assert_eq!(
            question("high"),
            "Does the scan report a finding at or above high severity that should be filed?"
        );
        let g = build(spec("cli-config-plan").unwrap(), "a/b");
        // Two entry points: the pull request plan and the default-branch plan.
        assert_eq!(
            g.edges
                .iter()
                .filter(|e| e.from == "n1" || e.from == "n2")
                .count(),
            2
        );
    }
}
