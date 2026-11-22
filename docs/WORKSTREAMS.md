# Workstreams: who owns what

Keleustes is being built by several agents in parallel in one working tree.
These rules keep them from colliding. Read the whole file before editing.

## The goal

Feature parity with the web Software Factory, minus sandboxes, as a native
desktop app. The behaviour specs (read them; never import code from them):

- `../vdb-site/docs/factory*.md`: API contract, engine, events, lifecycle,
  operations, validation, SystemOne, delivery
- `../website/.repo/software-factory.md`, `factory-*.md`, `belai-console.md`,
  `a2a-messaging.md`, `forge-connections.md`, `commit-signing.md`
- `../aws-postgres/prisma/models/factory.prisma` and the belai models in
  that folder (the shape of the data)

Single tenant: drop org and tenant columns. There is no login, and the
person running the app is the owner. Timestamps are Unix ms as `i64`.

## Non-negotiable rules (every feature, every agent)

1. **Provenance.** Every state change, dispatch, job, import, provision
   step, schedule run and message records its origin: the person, agent
   profile, workflow or run, node, or external source (URL plus content hash
   for imports). Show the origin in the UI where the item appears.
2. **Audit.** Every such action writes a hash-chained audit entry
   (`kls-core` audit ledger): actor, action, target, origin and outcome,
   including refusals.
3. **Kill switches.** Every dispatch, job, trigger, schedule, workflow
   step, provision action and outbound node frame checks the factory kill
   switch (pause and stop) first. When it blocks, refuse, emit an event and
   audit the refusal.
4. **A2A.** Every agent-to-agent path enforces the A2A blackout and policy
   strictly, with no bypass for built-ins, crews or host-to-host dispatch,
   and keeps belai's classifier kinds (KindPeer and the others).
5. **Tests.** Each feature has tests proving rules 1 to 4: an audit entry is
   written, provenance is stored, and the action is refused under the kill
   switch and the A2A blackout.

## Shared contracts (coordinator only; ask before changing)

- `proto/keleustes/node/v1/node.proto`: the node wire protocol. If your
  area needs a protocol change, stop and report it instead of editing.
- `crates/kls-core/src/lib.rs`, `crates/kls-core/src/gateway.rs` (the
  `NodeGateway` trait the engine and UI use to reach nodes)
- `crates/kls-store/src/lib.rs` (M0 tables and helpers)
- `crates/kls-node/src/server.rs` (Join, Link, auth, Hub)
- `crates/kls-ui/src/lib.rs`, `model.rs`, `loader.rs`, `fleet.rs`, `events.rs`
- `Cargo.toml` (workspace): adding a dependency to `[workspace.dependencies]`
  is allowed; report it in your summary.

## Owned files

| Workstream | Owns (create or edit freely) |
|---|---|
| **M1 sessions** | `kls-store/src/sessions.rs`, migration `0002_sessions.sql`; `kls-core/src/sessions.rs`; `kls-node/src/sessions.rs`, `kls-node/src/jobs.rs`, a new `kls-node/src/gateway.rs` (the `NodeGateway` impl on `Hub`, plus job and dispatch routing); `kls-ui/src/screens/sessions.rs` |
| **M2 coordination** | `kls-store/src/board.rs`, migration `0003_board.sql`; `kls-core/src/board.rs`; `kls-node/src/board.rs`; screens `cockpit.rs`, `board.rs`, `memory.rs` |
| **M3 engine** | new crate `crates/kls-engine/` (workflow graph, validate, dry run, executor, cron, templates); `kls-store/src/engine.rs`, migration `0004_engine.sql`; `kls-core/src/engine.rs`; screens `workflows.rs`, `templates.rs`, `runs.rs`, `checkpoints.rs` |
| **M4 GitHub** | new crate `crates/kls-github/`; `kls-store/src/github.rs`, migration `0006_github.sql`; `kls-core/src/github.rs`; screens `repos.rs`, `conflicts.rs`, `delivery.rs` |
| **M5 vault and workforce** | new crate `crates/kls-vault/`; `kls-store/src/vault.rs`, migration `0005_vault.sql`; `kls-core/src/vault.rs`; screens `budgets.rs`, `workforce.rs`, `library.rs` |
| **M6 finish** | `kls-store/src/audit.rs`, migration `0007_audit.sql`; `kls-core/src/audit.rs`; `kls-node/src/teleport.rs`; screens `trust.rs`, `audit.rs`, `settings.rs`; packaging |

Wiring a new crate into `kls-app/src/main.rs` (for example starting the
engine's scheduler) is a few lines. Add them, keep them minimal, and report
them in your summary.

## Conventions

- **Store.** Write SQL once with `?` placeholders through `self.sql(..)`.
  Stick to types both dialects share: `BIGINT` for i64, timestamps and
  booleans as 0/1; `TEXT` for strings and JSON; `DOUBLE PRECISION` for
  f64; bytes as `BLOB` in SQLite and `BYTEA` in Postgres. Add the same
  migration to `migrations/sqlite/` and `migrations/postgres/`. Ids are
  uuid v7 text. Every new store function gets a test in that module's
  `#[cfg(test)]` block, and the test runs on SQLite (Postgres runs too when
  `KLS_TEST_POSTGRES_URL` is set).
- **Events.** Call `core.emit(kind, subject, &json)` after every change a
  screen shows. Kinds are `<area>.<what>` (`session.updated`, `card.moved`,
  `run.step`), declared as constants in your core module (`pub mod kinds`).
  Screens reload through `kls_ui::loader::Loader` with kind prefixes. Never
  poll.
- **Reaching nodes.** Use `core.gateway()?` (`NodeGateway`: `dispatch`,
  `job`, `online`). Only M1 implements it. Until it exists, build against
  the trait and test with a fake gateway.
- **UI.** Each screen is a `Screen { .. }` with `new(&Model)`,
  `focus(&mut self, id)` and `ui(&mut self, ui, &Model)`. Set
  `pub const READY: bool = true` when the screen works. Run actions with
  `model.act(..)`. Navigate with `model.go(crate::Screen::X, Some(id))`.
  Use egui 0.37 APIs: `egui::Panel::left(..).show(ui, ..)`,
  `CentralPanel::default().show(ui, ..)`, `egui_extras::TableBuilder` for
  tables, `ui.close()` to close menus.
- **Builds.** Use your own target directory so parallel builds do not wait
  on each other: `CARGO_TARGET_DIR=target/<workstream> cargo …`.
- **Done means:** `cargo fmt --all`;
  `cargo clippy --workspace --all-targets -- -D warnings` is clean for
  your crates; tests pass; your screens set `READY`.
- **Git.** Do not commit, stash, reset or check out. Other agents' changes
  are in the same tree. Leave the index alone.
- **Scope.** If a build breaks in a file you do not own, wait a minute and
  retry: someone else is mid-edit. If it stays broken, report it; do not fix it.
