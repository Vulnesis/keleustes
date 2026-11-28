//! The default `sf:` label set (factory.md 12.9): one colour, a
//! description that says what will run. A label runs nothing until a
//! workflow has a label trigger for it.

pub const COLOR: &str = "00e0c9";

pub const DEFAULT_LABELS: [(&str, &str); 34] = [
    ("sf:triage", "Software Factory: triage this issue"),
    ("sf:ticket", "Software Factory: write this up as a ticket"),
    ("sf:repro", "Software Factory: reproduce the problem"),
    ("sf:fix", "Software Factory: fix this"),
    ("sf:plan", "Software Factory: plan the work"),
    ("sf:build", "Software Factory: build it"),
    ("sf:split", "Software Factory: split into smaller tickets"),
    ("sf:fix-tests", "Software Factory: fix the failing tests"),
    ("sf:write-tests", "Software Factory: write tests"),
    ("sf:review", "Software Factory: review this pull request"),
    ("sf:security-review", "Software Factory: security review"),
    (
        "sf:explain",
        "Software Factory: explain this code or change",
    ),
    ("sf:rebase", "Software Factory: rebase onto the base branch"),
    ("sf:split-pr", "Software Factory: split this pull request"),
    (
        "sf:describe",
        "Software Factory: write the pull request description",
    ),
    ("sf:backport", "Software Factory: backport this change"),
    ("sf:dep-check", "Software Factory: check the dependencies"),
    ("sf:quarantine", "Software Factory: quarantine a flaky test"),
    ("sf:docs", "Software Factory: write the docs"),
    ("sf:sbom", "Software Factory: produce an SBOM"),
    (
        "sf:rotate-secret",
        "Software Factory: rotate a leaked secret",
    ),
    ("sf:scout", "Software Factory: scout the repository"),
    ("sf:profile", "Software Factory: profile the repository"),
    ("sf:pqc", "Software Factory: post-quantum crypto check"),
    ("sf:ai", "Software Factory: AI usage check"),
    ("sf:secret", "Software Factory: secrets scan"),
    ("sf:malware", "Software Factory: malware scan"),
    ("sf:no-fix", "Software Factory: report without fixing"),
    (
        "sf:fix-deps",
        "Software Factory: fix vulnerable dependencies",
    ),
    ("sf:fix-code", "Software Factory: fix code findings"),
    (
        "sf:fix-iac",
        "Software Factory: fix infrastructure-as-code findings",
    ),
    (
        "sf:fix-container",
        "Software Factory: fix container findings",
    ),
    (
        "sf:fix-workflow",
        "Software Factory: fix CI workflow findings",
    ),
    ("sf:stop", "Software Factory: stop work on this item"),
];
