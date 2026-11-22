# Host management: requests to the owners of shared files

From the host-management work (docs/gaps/host-management.md). The host
management wire lives in its own file, `proto/keleustes/node/v1/host.proto`
(service `Host`: `Report`, `Actions`, `Progress`), so `node.proto` needs no
change: capabilities travel in `HostInventory.manage` and actions are
delivered on the node's `Host.Actions` stream (replayed until the node files
its first `HostActionUpdate`). Everything below is small wiring in files
other workstreams own. Each item says what breaks until it lands.

Applied by the host-management agent (not shared contracts, listed for
review): `crates/kls-proto/build.rs` compiles `host.proto` next to
`node.proto`; `justfile` `proto-go` generates both files.

## Keleustes

### R1. Register the Host service (owner: `kls-node/src/server.rs`). Landed.

In `Hub::serve`, next to the other services:

```rust
.add_service(crate::host::service(self.clone()))
```

`crate::host::service(hub) -> HostServer<crate::host::Svc>` exists in the
new owned file `kls-node/src/host.rs`. Until this lands a real belai gets
`Unimplemented` on `Host.*`, so the Host view shows the HostAdvert facts only
and every action stays queued. The kls-node tests serve the service on their
own listener.

### R2. Fleet opens the host (owner: `kls-ui/src/fleet.rs`). Landed.

Two lines in `Fleet`:

1. At the top of `Fleet::ui`, before the page header:
   ```rust
   if crate::screens::host::show(ui, model, snap) { return; }
   ```
2. In the row body (the table already senses clicks), one line per row:
   ```rust
   if row.response().clicked() { crate::screens::host::open(ui.ctx(), &n.id, None); }
   ```
   plus a `CARET_RIGHT` ghost button in the last column calling the same
   function (hover text `Open`).

`host::show` draws the host view when one is open and returns `true`; its
`Fleet` back button closes it. All state lives in `screens/host.rs`.

### R3. Fleet deep links (owner: `kls-ui/src/lib.rs`). Landed.

In `App::ui`, the `take_nav` branch drops focus for `Screen::Fleet`
(`Screens::focus` ignores it). Add:

```rust
if screen == Screen::Fleet { crate::screens::host::open_link(ui.ctx(), &id); }
```

`open_link("n1/products/cli")` splits node, tab and item; an unknown
tab opens `overview`. `parse_link` already yields `(Fleet, "n1/products")`;
tests for it are in `kls-app/src/desktop.rs`. Note: `?item=` is one of
`query_focus`'s keys, so `keleustes://fleet/n1/products?item=cli` yields the
focus `cli` and loses the node. The Host view therefore links items as a
path segment (`fleet/<node>/<tab>/<item>`). If `?item=` must work, make
`parse_link` keep the path for `Screen::Fleet` and append `?item=<v>`.

Also needed in `lib.rs`: `pub mod host;` is in `screens/mod.rs` (owned
there), nothing else.

### R4. Audit deep link with a node filter (owner: `kls-app/src/desktop.rs` `query_focus`, `screens/audit.rs`)

The Host view's Audit tab opens `keleustes://audit?node=<id>`. `query_focus`
should pass `node=<id>` through as the focus `node:<id>` and `Audit::focus`
should filter the ledger by `node_id`. Until then the tab shows the host's
`host_action` ledger entries inline.

### R5. Refuse host actions over Fleet.Request (owner: `kls-node` Fleet service, WP-I)

`Fleet.Request` must refuse every kind that starts with `host_` (and any
kind in `kls_core::hosts::KINDS`) with `PermissionDenied`, audited, even
when the A2A blackout is off. No agent or peer node may manage a host.
`kls_core::hosts::is_host_kind(kind)` is the predicate.

### R6. Retention sweep for action lines (owner: `kls-core/src/sweeps.rs`)

Call `kls_core::hosts::prune_lines(core, 30 days)` next to the teleport
sweeper. Until then `host_action_lines` grows (rows are small and capped at
64 KiB per action).

### R7. Needs you: host hand-offs and belai updates (HM-14; owner: `kls-core/src/needs.rs`, `kls-app` tray). Done by host management: `CheckpointKind::HostHandoff`/`HostUpdate` from `hosts::needs`, `"host."` in `RELOAD_ON`, Checkpoints shows Copy command and Open host, the Cockpit opens the Host view, and `desktop.rs needs_notes` raises an OS notice for a hand-off (the tray menu itself stays Open/Quit).

`kls_core::hosts::needs(core) -> Vec<HostNeed>` lists hand-offs waiting on a
person (`kind:"handoff"`, with the exact `command` and `reason`
`needs_root|needs_browser`) and hosts with a belai update
(`kind:"belai_update"`), each with a `link`
(`keleustes://fleet/<node>/overview|provision`). Needed:

1. `CheckpointKind::HostHandoff` and `CheckpointKind::HostUpdate` in
   `needs.rs`, filled from `hosts::needs`, and `"host."` in `RELOAD_ON`.
2. The tray and desktop notification (`kls-app/src/desktop.rs`
   `event_allowed`) for `host.action.needs_human`.

Until then the Host view's Overview shows them under Needs you.

### R8. Engine `provision` step (HM-9b; owner: `kls-engine`)

A workflow node `provision {plan_id, node}` that calls
`kls_core::hosts::preview_plan` then `kls_core::hosts::run_plan` with an
origin `Origin { actor: "owner", surface: "workflow:<id>", workflow_id,
run_id, .. }` and waits for `host.plan.finished`. `run_plan` already checks
the kill switch before every step; the step must also follow the engine's
kill-switch path (`exec.rs` pause/stop) and needs a typed confirmation
captured when the workflow is published (the host name), since a running
workflow has no person to type it.

## belai

(Appended by the belai host-management work; see below.)

### B1. Lend the node connection (owner: `belai/internal/factorylink/client.go`). Landed.

`internal/hostmgmt` calls the Host service on the node certificate's
connection. `factorylink.Client` keeps it unexported (`cc()`). Add:

```go
// Conn is the client's connection to Keleustes, dialled with the node
// certificate on first use.
func (c *Client) Conn(ctx context.Context) (*grpc.ClientConn, error) { return c.cc() }
```

`cmd/belai/rchost.go` already checks for this method by interface
assertion. Until it lands, `belai rc` logs that host management is waiting
and sends no inventory and no updates; every action stays queued on the
Keleustes side.

### B2. Parameter names and rules Keleustes must send (owner: `kls-core/src/host.rs`)

The belai kind table (`internal/hostmgmt/actions.go`) takes exactly these
params; any other key is refused:

- read: `inventory_refresh{}`, `product_probe{product}`, `preflight{}`,
  `config_get{scope=global|project, path}`, `log_tail{lines<=1000}`
- config: `config_set{key, value}` plus `HostAction.if_match`;
  `project_prefs_set{path, key, value}` (`value` empty clears the key);
  `agents_set{path, profile, state=on|off|inherit}`; `repo_trust{path}`;
  `repo_untrust{path}`; `worktree_drop{path}`; `session_prune{}`
- provision: `product_install{product, method=brew|scoop|direct, version, profile}`
  (`version` is required for a direct CLI install and a sensor download,
  `profile` for a sensor install); `product_upgrade{product}`;
  `product_uninstall{product}`; `product_configure{product, on, purge,
  remove_credentials, scope, path}`; `product_auth{product=cli}`;
  `belai_upgrade{version}`; `rc_restart{force_after_s}`;
  `repo_clone{forge_host, owner, name, depth}` (`depth` is accepted and not
  applied: rc/clone.go makes a full clone into the worktrees directory, not
  `<workspace_dir>/<name>`)
- shell: `shell_exec{argv_json}`

Every action must carry `class`, equal to the class above. Every provision
and shell action must be preceded by a dry run and carry its
`preview_sha256`; belai refuses one without it. `agents_set` and
`project_prefs_set` need the host's `--web-project-settings`. Per-repo
scanner switches are the pref keys `vulnetix.scans.<subcommand>` (one bool
key per scanner), not a single `vulnetix.scans` list; `vulnetix.dep_watch`,
`vulnetix.sweep` and `agents.builtin.<profile>` are bools.

### B3. Command line for the host e2e (owner: `kls-app/src/main.rs`, `cli.rs`)

`belai/e2e/keleustes_host_test.go` (`just e2e-host`) skips until keleustes
has:

- `keleustes host inventory <node>`: the last `HostInventory` as protojson
  (camelCase names);
- `keleustes host act <node> <kind> [--param k=v]... [--dry-run]
  [--if-match SHA] [--preview SHA] [--wait SECS]`: request the action as the
  owner and print its final `HostActionUpdate` as protojson;
- `keleustes host stop [--reason TEXT]` and `keleustes host resume`;
- `keleustes host verify`: exit 0 when the ledger chain verifies. (Landed in `kls-app/src/host_cmd.rs`; the test uses these.)

### B4. A full argv in the audit (owner: Keleustes ledger)

belai's audit log never holds a command (belai AGENTS.md, audit invariant),
so its `host.action.*` line for `shell_exec` carries the program's base name
(`tool`) and the params digest only. The full argv is in the confirmed
preview; the Keleustes ledger entry should keep it.

### B5. Version check after rc_restart and belai_upgrade rollback. Done: `rc_restart{expect_version}` (the Host view sends the last upgrade's version). The old process writes `rc/hostrestart.json`; the new one settles the restart done on a match, and on a mismatch copies `belai.prev` back, fails the action saying so and re-execs into it, once. Keleustes emits and audits `host.action.version_mismatch` when the reported version differs. A binary that crashes before host management starts cannot roll itself back; that needs a supervisor (systemd).

belai settles `rc_restart` `done` when the new process starts. Keleustes
should compare the new link's `Hello.belai_version` with the upgrade target.
The rollback to `belai.prev` supervised by the old binary's exit hook is not
built on the belai side yet; the backup is written.
