# OP-1 (CLI agent usability): hooks in files owned elsewhere, and requests

OP-1 owns `crates/kls-app/src/{cli.rs,main.rs (CLI parts)}`,
`crates/kls-app/src/ax/`, `crates/kls-app/tests/cold_agent.rs` and
`docs/skills/`. The contract is `keleustes contract` (built from the
binary); the operator skill is `docs/skills/keleustes-operator/SKILL.md`,
embedded in the binary (`keleustes contract --skill`).

## Minimal hooks made outside OP-1's files

| File (owner) | Change | Why |
|---|---|---|
| `kls-core/src/sessions.rs` (M1) | `pub mod axes;` (new file `kls-core/src/sessions/axes.rs`); doc on `session_end`; test `session_end_is_first_writer_wins` | O-061 status axes shared by CLI and UI; O-073 first-writer-wins asserted. No behaviour change: `Store::end_session` already settled once (`WHERE ended_at IS NULL`). |
| `kls-ui/src/screens/sessions.rs` (M1) | `Detail.axes` and `Detail.label`, shown as two facts (`SEAL_CHECK` delivery · agent · verified, `TAG` label) | O-061 and O-008 in the UI |
| `kls-app/Cargo.toml` | dev-dependencies for the cold-agent test (kls-proto, tonic, rcgen, base64, pem, tokio-stream) | O-079 fake node over mTLS |

`kls-node` was not touched.

## Requests

1. **Runs screen (M3):** show `kls_core::sessions::axes::run_axes(run,
   steps, dispatches)` (delivery over dispatching steps, agent from the run
   status, verified from `gate` steps) next to the run status. The function
   and its test exist; nothing renders it yet.
2. **`forge` and `host` verbs (forge/host owners):** under `--json` they are
   wrapped generically (`kind: forge.<verb>`/`host.<verb>`, `ok` from the
   exit code, code `failed` otherwise), and their human output stays above
   the envelope. Returning structured data (`ax::Done`) and closed codes
   from `forge_cmd::run` / `host_cmd::run` would make them first-class.
3. **Kill switch on queued prompts and commands: done in core (coordinator
   decision).** `Core::queue_prompt`/`queue_command` (UI, engine) and the new
   `queue_prompt_from`/`queue_command_from` (CLI `owner:cli`, cross-host
   `node:<id>` in `belai/fleet.rs::queue_peer_prompt`) call
   `control::check` first; `sessions::WIND_DOWN` (`stop`, `interrupt`,
   `cancel`) stays allowed. Refusals emit the audited `factory.refused`.
   Test: `kls-core sessions::kill_switch_refuses_prompts_and_commands`.
   Hooks: `kls-core/src/sessions.rs` (M1), one call in
   `kls-core/src/belai/fleet.rs`.
4. **Typed errors from core (M1, M3):** a few refusals reach the CLI as
   prose and are matched on Keleustes' own fixed strings:
   `fleet_decide` ("unknown request", "the request is …"),
   `queue_ask_answer` under stop ("the factory is stopped …"), and the
   node's refusal stored as a failed dispatch with the prefix "refused".
   A `Refusal` variant (or a `Blocked`) for each would remove the matching.
5. **`--label` for harness dispatches:** `HarnessRequest` carries no
   trigger, so the label is dropped with a warning. A `subject` (or
   `trigger`) field on `HarnessRequest` would carry it.
6. **Label without a proto change:** the label rides in
   `Trigger.subject` (source `console`), which belai already shows in its
   provenance line. No `node.proto` change was needed.
7. **Usage errors are not audited:** clap rejects the line before the data
   directory opens. `analyze-usage` therefore sees `interface_misread` only
   for calls that parsed (not_found, invalid_state, ambiguous).
