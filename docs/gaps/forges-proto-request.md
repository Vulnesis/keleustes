# Request: node protocol for forge logins (forges.md WP-9)

From: M4 (forges). To: coordinator (owns `proto/` and `kls-node`).
Status: landed in node.proto (Capabilities.forges = 10, Job kind `forge`, JobNeeds.forge); Keleustes routes glab/tea through `NodeCli` (2026-11-21). Original request: Keleustes-side code is in place behind
`kls_forge::node::NodeCli` and compiles against the current proto; until
this lands, nodes are used for github.com only (the `github` job and
`Capabilities.gh_authenticated`), and the route to any other forge is local
only (the strip says "nodes cannot reach <host> until they advertise forge
logins").

## 1. `node.proto` additions (append only; no renumbering)

```proto
message Capabilities {
  bool gh_authenticated = 1;           // kept: github.com via gh (old belai)
  bool terminal = 2;
  bool remote_control = 3;
  repeated string providers = 4;
  uint32 max_sessions = 5;
  // Every forge CLI login on the host (gh: all hosts; glab; tea).
  repeated ForgeLogin forges = 6;
}

message ForgeLogin {
  string cli = 1;        // "gh" | "glab" | "tea"
  string host = 2;       // lower-case, no scheme, may carry :port
  string kind = 3;       // "github" | "ghes" | "gitlab" | "gitea" | "forgejo" | "" (unknown)
  string login = 4;      // the account
  bool signed_in = 5;    // the CLI's status check passed this minute
}
```

`kls-store` `Capabilities` gains `forges: Vec<ForgeLogin>` (stored in the
nodes row's capabilities JSON, no migration needed if capabilities are JSON;
otherwise `ALTER TABLE nodes ADD COLUMN forges_json TEXT NOT NULL DEFAULT '[]'`).

## 2. Job kinds (belai `docs/keleustes.md` "Jobs")

`forge` (new), payload JSON:

```json
{"cli": "glab", "host": "gitlab.example.com", "args": ["api", "..."], "stdin": "..." }
```

- Refused unless the host's advert lists `{cli, host, signed_in: true}`
  (checked again when the job runs).
- `gh`: the existing `github` allowlist; `--hostname <host>` forced.
- `glab`: `args[0] == "api"` only; `--hostname <host>` forced; refuses
  `--form`, `--input <file>` other than `-`, `-F @file`, path arguments.
- `tea`: `args[0] == "api"` only; `-l <tea login for host>` forced (belai
  resolves the name; Keleustes sends `-l` but belai overrides it);
  `-d`/`-F` take only `@-`; `-o` refused.
- Answer `output_json`: `{"stdout","stderr","exit","truncated"}` (as `github`).
  For tea the status line and headers stay on stderr; Keleustes joins them.

`forge_push` (later, for conflict commits off GitHub on a node):
`{host, repo_url, bundle_b64, ref, expect_sha}`, pushed with belai's
per-host credential helper and `--force-with-lease=<ref>:<expect_sha>`.

## 3. `kls-core` `JobNeeds` (coordinator-owned `gateway.rs`)

```rust
pub struct JobNeeds {
    pub gh: bool,
    pub provider: Option<String>,
    /// (cli, host) the node must advertise as signed in.
    pub forge: Option<(String, String)>,
}
```

`kls-node/src/gateway.rs` routes a job with `forge: Some((cli, host))` only
to a node whose `Capabilities.forges` holds `{cli, host, signed_in: true}`.

## 4. What Keleustes does once it lands (M4, small)

- `Selector::pick_node` (kls-forge `select.rs`): accept a node whose
  `forges` lists `{cli, host, signed_in}` for the connection; github.com
  still accepts the old `gh_authenticated` bool.
- `NodeCli` (today an alias of `NodeGh`): send `forge` jobs with
  `JobNeeds { forge: Some((cli, host)), .. }` once the node advertises
  `forges`; `github` jobs for github.com before that.
- Kill switch and audit already apply: every write through a node goes
  through `Client::request`, which checks `factory_control` and writes the
  ledger entry before and after the job.

## 5. `forge_push` job (new, additive; no message changes)

Conflict commits off GitHub on a node. Only the `Job.kind` string and its
payload are new; `JobResult` carries the answer in `output_json` as for
every job. Comment to add above `Job.kind` in `node.proto`:

```proto
  // "forge_push" (payload: {"cli","host","repo","head_ref","head_sha",
  //   "base_ref","base_sha","files","renamed_from","base_only","message",
  //   "author_name","author_email","author_forced"}; forges.md 3.6):
  //   build the merge commit and push it with the host's CLI as git
  //   credential helper. Answer {"commit","pushed"}.
```

Payload (JSON, unknown fields refused):

| field | type | rule |
|---|---|---|
| `cli` | `"gh"`, `"glab"`, `"tea"` | must match a `Capabilities.forges` entry with `signed_in` for `host` |
| `host` | string | lower-case host[:port]; the URL is **forced** to `https://<host>/<repo>.git` |
| `repo` | string | `owner/name`, nested groups allowed (2 to 20 segments, `[A-Za-z0-9_.-]`) |
| `head_ref`, `base_ref` | string | git branch names (no `..`, `@{`, leading `-` or `/`, `.lock`) |
| `head_sha`, `base_sha` | hex | 40 or 64 chars |
| `files` | `{path: text \| null}` | resolved files (`null` deletes); paths relative, no `.`/`..`/`.git` segments; at most 2,000 entries and 20 MB of text |
| `renamed_from` | `[path]` | removed from the tree |
| `base_only` | `[{path, removed, previous}]` | taken from `base_sha`'s tree |
| `message` | string | ≤ 8,000 chars |
| `author_name`, `author_email`, `author_forced` | string, string, bool | used when git has none, or always when forced (Settings > Signing email) |

What the node runs (nothing else): `git init --bare` in a temp dir;
`git -c credential.helper= -c credential.https://<host>.helper=<helper>
-c protocol.allow=never -c protocol.https.allow=always fetch <url>
+refs/heads/<head_ref>:refs/kls/head +refs/heads/<base_ref>:refs/kls/base`;
refuse when `refs/kls/head != head_sha` ("the head branch moved");
`read-tree`, `hash-object -w --stdin`, `update-index`, `write-tree`,
`commit-tree -p head_sha -p base_sha`; `git push <url> <commit>:refs/heads/<head_ref>
--force-with-lease=refs/heads/<head_ref>:<head_sha>`. Helpers:
`!gh auth git-credential`, `!glab auth git-credential`, `!tea login helper`.
The kill switch refuses the job like every other (`factory.stopped`).

Answer: `{"commit": "<sha>", "pushed": true}`; a moved branch is a
`refused` result with that reason.

Status: Keleustes side done (`NodeCli::push`, `JobNeeds.forge = (cli, host)`,
`conflicts::push_payload`). belai handler done in a new package
`belai/internal/forgejob` (`ParsePush`, `Push`, tests against a `file://`
remote); **belai agent: wire `case forgejob.KindPush` into
`internal/rc/factoryjobs.go` `handleFactoryJob` (the snippet is in the
package doc) and add `"forge_push"` to `Capabilities.job_kinds`.**

## Landed by belai counterparts (WP-H)

`Core::forge_file` stores each worker's request (`forge_requests`, event `forge.request`), and
`Core::forge_steer(node, request_id, "took_over"|"waiting"|"failed", pr, pr_url, until_ms, reason)` answers it with
a `steer` dispatch. The forge coordinator (kls-forge) can listen for `forge.request`, open the PR or sync the issue
through a gh/glab/tea job on a signed-in node, then call `forge_steer`. Bundles are not uploaded yet: a request with
kind `push` needs the origin host to push itself or a later bundle path.
