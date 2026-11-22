-- Keleustes M4: the GitHub plane (orgs, repos, pulls, issues mirror, labels,
-- ETag cache, rate budget, conflict resolutions) and delivery facts.

CREATE TABLE gh_orgs (
    login       TEXT PRIMARY KEY,
    -- "Organization" or "User" (the signed-in account itself).
    kind        TEXT NOT NULL DEFAULT 'Organization',
    managed     BIGINT NOT NULL DEFAULT 0,
    enabled     BIGINT NOT NULL DEFAULT 1,
    repo_count  BIGINT NOT NULL DEFAULT 0,
    avatar_url  TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    synced_at   BIGINT NOT NULL DEFAULT 0
);

CREATE TABLE gh_repos (
    full_name      TEXT PRIMARY KEY,
    owner          TEXT NOT NULL,
    name           TEXT NOT NULL,
    visibility     TEXT NOT NULL DEFAULT 'private',
    default_branch TEXT NOT NULL DEFAULT 'main',
    archived       BIGINT NOT NULL DEFAULT 0,
    fork           BIGINT NOT NULL DEFAULT 0,
    html_url       TEXT NOT NULL DEFAULT '',
    description    TEXT NOT NULL DEFAULT '',
    can_push       BIGINT NOT NULL DEFAULT 0,
    managed        BIGINT NOT NULL DEFAULT 0,
    mirror_pulls   BIGINT NOT NULL DEFAULT 1,
    mirror_issues  BIGINT NOT NULL DEFAULT 0,
    sync_labels    BIGINT NOT NULL DEFAULT 0,
    -- Per-repo defaults and autonomy (RepoSettings as JSON).
    settings_json  TEXT NOT NULL DEFAULT '{}',
    revision       BIGINT NOT NULL DEFAULT 0,
    pushed_at      BIGINT,
    synced_at      BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX gh_repos_owner ON gh_repos (owner);

CREATE TABLE gh_pulls (
    repo                    TEXT NOT NULL,
    number                  BIGINT NOT NULL,
    title                   TEXT NOT NULL DEFAULT '',
    state                   TEXT NOT NULL DEFAULT 'open',
    draft                   BIGINT NOT NULL DEFAULT 0,
    merged                  BIGINT NOT NULL DEFAULT 0,
    author                  TEXT NOT NULL DEFAULT '',
    head_ref                TEXT NOT NULL DEFAULT '',
    head_sha                TEXT NOT NULL DEFAULT '',
    head_repo               TEXT NOT NULL DEFAULT '',
    base_ref                TEXT NOT NULL DEFAULT '',
    base_sha                TEXT NOT NULL DEFAULT '',
    -- "clean", "conflict", "unknown" (GitHub computes it lazily).
    mergeable               TEXT NOT NULL DEFAULT 'unknown',
    mergeable_state         TEXT NOT NULL DEFAULT 'unknown',
    review_decision         TEXT NOT NULL DEFAULT '',
    requested_reviewers_json TEXT NOT NULL DEFAULT '[]',
    labels_json             TEXT NOT NULL DEFAULT '[]',
    -- "success", "failure", "pending", "none".
    checks_state            TEXT NOT NULL DEFAULT 'none',
    checks_json             TEXT NOT NULL DEFAULT '{}',
    html_url                TEXT NOT NULL DEFAULT '',
    additions               BIGINT NOT NULL DEFAULT 0,
    deletions               BIGINT NOT NULL DEFAULT 0,
    changed_files           BIGINT NOT NULL DEFAULT 0,
    created_at              BIGINT NOT NULL DEFAULT 0,
    updated_at              BIGINT NOT NULL DEFAULT 0,
    merged_at               BIGINT,
    closed_at               BIGINT,
    synced_at               BIGINT NOT NULL DEFAULT 0,
    delivery_captured       BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (repo, number)
);
CREATE INDEX gh_pulls_state ON gh_pulls (state, updated_at);

CREATE TABLE gh_issues (
    repo               TEXT NOT NULL,
    number             BIGINT NOT NULL,
    title              TEXT NOT NULL DEFAULT '',
    state              TEXT NOT NULL DEFAULT 'open',
    state_reason       TEXT NOT NULL DEFAULT '',
    author             TEXT NOT NULL DEFAULT '',
    author_association TEXT NOT NULL DEFAULT '',
    labels_json        TEXT NOT NULL DEFAULT '[]',
    assignees_json     TEXT NOT NULL DEFAULT '[]',
    body               TEXT NOT NULL DEFAULT '',
    html_url           TEXT NOT NULL DEFAULT '',
    comments           BIGINT NOT NULL DEFAULT 0,
    created_at         BIGINT NOT NULL DEFAULT 0,
    updated_at         BIGINT NOT NULL DEFAULT 0,
    closed_at          BIGINT,
    -- The board card mirroring this issue, when one exists.
    card_id            TEXT,
    synced_at          BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (repo, number)
);
CREATE INDEX gh_issues_updated ON gh_issues (repo, updated_at);

CREATE TABLE gh_labels (
    repo        TEXT NOT NULL,
    name        TEXT NOT NULL,
    color       TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    synced_at   BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (repo, name)
);

-- Conditional request cache: the ETag and body of the last 200 per route and path.
CREATE TABLE gh_etags (
    cache_key  TEXT PRIMARY KEY,
    etag       TEXT NOT NULL,
    link       TEXT NOT NULL DEFAULT '',
    body       TEXT NOT NULL,
    fetched_at BIGINT NOT NULL
);

-- Rate limit as last observed, per route ("local", "node:<id>") and resource.
CREATE TABLE gh_quota (
    route           TEXT NOT NULL,
    resource        TEXT NOT NULL,
    rate_limit      BIGINT NOT NULL DEFAULT 0,
    remaining       BIGINT NOT NULL DEFAULT 0,
    used            BIGINT NOT NULL DEFAULT 0,
    reset_at        BIGINT NOT NULL DEFAULT 0,
    observed_at     BIGINT NOT NULL DEFAULT 0,
    secondary_until BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (route, resource)
);

-- Calls made per route, caller and UTC hour.
CREATE TABLE gh_calls (
    route        TEXT NOT NULL,
    caller       TEXT NOT NULL,
    hour_bucket  BIGINT NOT NULL,
    calls        BIGINT NOT NULL DEFAULT 0,
    not_modified BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (route, caller, hour_bucket)
);

CREATE TABLE gh_resolutions (
    id                  TEXT PRIMARY KEY,
    repo                TEXT NOT NULL,
    number              BIGINT NOT NULL,
    branch              TEXT NOT NULL,
    commit_sha          TEXT NOT NULL,
    parent_shas_json    TEXT NOT NULL DEFAULT '[]',
    files_json          TEXT NOT NULL DEFAULT '[]',
    -- "github_signs": the commit was made through the API and GitHub signs it.
    signing             TEXT NOT NULL DEFAULT 'github_signs',
    verified            BIGINT,
    verification_reason TEXT NOT NULL DEFAULT '',
    route               TEXT NOT NULL DEFAULT '',
    created_at          BIGINT NOT NULL
);
CREATE INDEX gh_resolutions_pr ON gh_resolutions (repo, number);

-- Delivery facts. Keys: pr:<owner/repo>#<n>, issue:<owner/repo>#<n>, card:<id>, run:<id>, session:<id>.
CREATE TABLE delivery_outcomes (
    outcome_key    TEXT PRIMARY KEY,
    kind           TEXT NOT NULL,
    title          TEXT NOT NULL DEFAULT '',
    repo           TEXT NOT NULL DEFAULT '',
    trigger_kind   TEXT NOT NULL DEFAULT '',
    trigger_ref    TEXT NOT NULL DEFAULT '',
    author         TEXT NOT NULL DEFAULT '',
    opened_at      BIGINT,
    started_at     BIGINT,
    resolved_at    BIGINT,
    result         TEXT NOT NULL DEFAULT '',
    role_key       TEXT NOT NULL DEFAULT '',
    active_ms      BIGINT,
    wait_ms        BIGINT,
    wall_ms        BIGINT,
    -- Open to merge (or close), and open to first review.
    cycle_ms       BIGINT,
    first_review_ms BIGINT,
    cost_micro_usd BIGINT,
    decisions_json TEXT,
    escalated      BIGINT,
    reviewers_json TEXT NOT NULL DEFAULT '[]',
    commits        BIGINT,
    additions      BIGINT,
    deletions      BIGINT,
    pr_ref         TEXT NOT NULL DEFAULT '',
    card_id        TEXT NOT NULL DEFAULT '',
    created_at     BIGINT NOT NULL,
    updated_at     BIGINT NOT NULL
);
CREATE INDEX delivery_outcomes_resolved ON delivery_outcomes (resolved_at);

CREATE TABLE delivery_actions (
    source         TEXT NOT NULL,
    ref_id         TEXT NOT NULL,
    action_kind    TEXT NOT NULL DEFAULT '',
    actor_key      TEXT NOT NULL,
    actor_login    TEXT NOT NULL DEFAULT '',
    is_bot         BIGINT NOT NULL DEFAULT 0,
    bot_kind       TEXT NOT NULL DEFAULT '',
    outcome_key    TEXT,
    repo           TEXT,
    occurred_at    BIGINT NOT NULL,
    raw_units_json TEXT NOT NULL DEFAULT '{}',
    basis          TEXT NOT NULL DEFAULT 'derived',
    PRIMARY KEY (source, ref_id)
);
CREATE INDEX delivery_actions_at ON delivery_actions (occurred_at);
CREATE INDEX delivery_actions_actor ON delivery_actions (actor_key, occurred_at);

CREATE TABLE delivery_people (
    person_key    TEXT PRIMARY KEY,
    login         TEXT NOT NULL DEFAULT '',
    display_name  TEXT NOT NULL DEFAULT '',
    avatar_url    TEXT NOT NULL DEFAULT '',
    emails_json   TEXT NOT NULL DEFAULT '[]',
    is_internal   BIGINT,
    association   TEXT NOT NULL DEFAULT '',
    is_bot        BIGINT NOT NULL DEFAULT 0,
    bot_kind      TEXT NOT NULL DEFAULT '',
    first_seen_at BIGINT,
    last_seen_at  BIGINT
);

CREATE TABLE delivery_commits (
    repo         TEXT NOT NULL,
    sha          TEXT NOT NULL,
    author_name  TEXT NOT NULL DEFAULT '',
    author_email TEXT NOT NULL DEFAULT '',
    author_login TEXT NOT NULL DEFAULT '',
    author_key   TEXT NOT NULL DEFAULT '',
    is_bot       BIGINT NOT NULL DEFAULT 0,
    trailers_json TEXT NOT NULL DEFAULT '[]',
    signed       BIGINT,
    pr_number    BIGINT,
    outcome_key  TEXT,
    committed_at BIGINT,
    PRIMARY KEY (repo, sha)
);
CREATE INDEX delivery_commits_at ON delivery_commits (committed_at);

-- Decisions rolled up per UTC day, kind and model (any area may record them).
CREATE TABLE delivery_decision_days (
    day_start         BIGINT NOT NULL,
    kind              TEXT NOT NULL,
    model             TEXT NOT NULL DEFAULT '',
    total             BIGINT NOT NULL DEFAULT 0,
    proceeded         BIGINT NOT NULL DEFAULT 0,
    rule_handled      BIGINT NOT NULL DEFAULT 0,
    escalated         BIGINT NOT NULL DEFAULT 0,
    conf_buckets_json TEXT NOT NULL DEFAULT '[0,0,0,0,0,0,0,0,0,0]',
    latency_sum_ms    BIGINT NOT NULL DEFAULT 0,
    latency_n         BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (day_start, kind, model)
);
