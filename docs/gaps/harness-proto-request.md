# Request: protocol and integration points for harness sessions

From the harness-sessions workstream (docs/gaps/harnesses.md) to the
coordinator and the belai-counterparts owner. Everything below is additive
and should pass `buf breaking`. Field numbers are the next free ones in
`node.proto` as of 2026-11-21 (HostAdvert 11, Capabilities 10, Dispatch 45,
SessionUpsert 25); renumber if something lands first.

Built and tested on our side, waiting for these hooks:

- belai: `internal/harness` (launch profiles), `internal/harnessdetect`
  (detection and `harnessdetect.Default` catalog), `internal/harnesstail`,
  `internal/harnessrun` (PTY runner, prompts, keys, stop, kill switch, key
  modes, A2A bridge, MCP server, hooks), `cmd/belai/rcharness.go`
  (`belai rc-harness`, `belai harness-hook`, `belai a2a-mcp`),
  `internal/rc/factoryharness.go` (`Daemon.startHarness`, not yet called).
- Keleustes: `kls-core/src/harness.rs` (`HarnessInfo`, `KeyPolicy`, `plan`,
  `Core::dispatch_harness`, `harness_key`, `send_owner_a2a`, `a2a_peers`,
  `screen_attached`, transcript labels), `kls-ui/src/screens/harness_picker.rs`,
  `harness_screen.rs`, `harness_a2a.rs`.

## 1. Proto (node.proto)

```proto
message HostAdvert {
  // ... 1-11 unchanged
  // Third-party harnesses detected on the host (belai registry ids).
  repeated Harness harnesses = 12;
}

message Harness {
  string id = 1;              // registry id, "claude-code"
  string name = 2;
  string version = 3;         // first line of --version, sanitized, <= 64
  // "launch", "unverified", "broken", "gui", "configured"
  string state = 4;
  string detail = 5;          // <= 200
  bool resume = 6;
  bool a2a = 7;               // the profile can wire the belai A2A bridge
  bool logged_in = 8;         // native login present (no values)
  repeated string grant_env = 9;   // env names a vault key may be handed to
  repeated string modes = 10;      // "default", "plan", "accept_edits", ...
  string transcript = 11;          // dialect, "" = screen only
  repeated string allow_keys = 12; // key names the Screen view may send
  string path = 13;                // resolved binary (provenance)
  string version_hash = 14;        // sha256 of the probe output
  int64 probed_at_ms = 15;
  string conf = 16;                // "H" host-checked, "M" vendor docs
}

message Capabilities {
  // ... 1-10 unchanged
  bool harness_sessions = 11;  // false on Windows
}

message Dispatch {
  // ... 1-45 unchanged
  string harness = 46;          // registry id; "" = belai (kind "session" only)
  string harness_mode = 47;     // one of Harness.modes; "" = default
  string harness_resume = 48;   // native session id
  // Key mode, resolved by Keleustes: "own_login" (default), "handoff",
  // "always". With own_login the node drops any vault grant unread.
  string harness_keys = 49;
  // Origin of the dispatch for the node's audit lines:
  // {"actor","runId","cardId","keyId","modeSetBy"}.
  string origin_json = 50;
}

message SessionUpsert {
  // ... 1-25 unchanged; kind gains "harness"
  string harness = 26;
  string harness_version = 27;
  string harness_session_id = 28;
  string transcript_source = 29;   // "native" or "screen"
}

service Board {
  // Live sessions any node may message, newest first.
  rpc A2APeers(A2APeersRequest) returns (A2APeerList);
}
message A2APeersRequest { string repo = 1; uint32 limit = 2; }
message A2APeer {
  string agent_id = 1; string node_name = 2; string title = 3; string repo = 4;
  string branch = 5; string harness = 6; string agent = 7; int64 updated_at_ms = 8;
}
message A2APeerList { repeated A2APeer peers = 1; }

// The live terminal of a harness session. Output only: keys go through the
// inbox as Command{name:"key"}.
service Screen {
  rpc Stream(stream ScreenUp) returns (stream ScreenDown);
}
message ScreenUp {
  string session_id = 1;
  uint64 offset = 2;
  oneof body {
    ScreenOpen open = 10;
    bytes output = 11;      // <= 32 KiB, already scrubbed on the node
    ScreenSize size = 12;
    ScreenExit exit = 13;
  }
}
message ScreenOpen { ScreenSize size = 1; bytes replay = 2; }
message ScreenSize { uint32 cols = 1; uint32 rows = 2; }
message ScreenExit { int32 code = 1; string signal = 2; }
message ScreenDown {
  oneof body { ScreenSize resize = 10; bool pause = 11; }
}
```

## 2. Keleustes integration points

- `kls-store/src/lib.rs`: `HostInfo.harnesses: Vec<HarnessInfo>` with the
  fields of `kls_core::harness::HarnessInfo` (move the struct to kls-store and
  re-export it from `kls_core::harness`; it is serde camelCase with
  `#[serde(default)]`). Fill it in `kls-node/src/server.rs` `host_info`.
  `capabilities.harness_sessions` likewise.
- `kls-core/src/gateway.rs`: `DispatchSpec` gains `harness`, `harness_mode`,
  `harness_resume`, `harness_keys`, `origin_json` (copy from
  `kls_core::harness::HarnessFields`). Implement
  `kls_core::harness::HarnessDispatcher` for the kls-node `Hub`: set the
  fields and call the normal dispatch path (the vault grant goes through
  `grant_for_purpose(.., "harness")` only when `harness_keys != "own_login"`).
- `kls-node`: `Board.A2APeers` calls `Core::a2a_peers(repo, limit)`.
  `Screen.Stream` server: keep the last `ScreenOpen` + replay per session,
  fan out `output` to UI subscribers, send `pause` when none; call
  `Core::screen_attached(session_id, "owner")` when the UI attaches. Expose
  to the UI as `hub.screen(session_id) -> (replay, offset, rx)`.
- `kls-store` sessions migration: `harness`, `harness_version`,
  `harness_session_id`, `transcript_source` on `sessions`. Until then a harness
  session is `kind == "harness"` with `provider` = harness id (belai sets that
  today), which `kls_core::harness::session_harness` already reads.
- `kls-core/src/audit.rs` `ledger_kind_for_event`: the harness events already
  write ledger entries directly (`Core::audit_record`), so nothing is needed
  unless the fold should also pick up `harness.*` events; if so map
  `harness.*`, `a2a.owner.*`, `screen.attached` to ("harness", "user").
- `kls-app/src/cli.rs`: `keleustes sessions dispatch --harness <id> --mode
  <m> [--key <vault key id>]` calling `Core::dispatch_harness`, and
  `keleustes nodes harnesses <node>`.

### UI hooks (sessions.rs, fleet.rs, settings.rs)

- `sessions.rs` New session dialog: add a field
  `agent: screens::harness_picker::AgentPicker` to `NewSession`; draw
  `agent.agent_ui(ui, &node.host.belai_version, &node.host.harnesses)` as the
  first row after Node; when `agent.is_harness()`, hide Provider, Crew and
  Profile and draw `agent.options_ui(ui, &harnesses, &policy, &keys,
  &resumable)` (policy = `core.harness_key_policy()`, keys =
  `kls_core::vault::list_secrets`, resumable = ended sessions on that node
  with the same harness and cwd: `(harness_session_id, title)`). On Start,
  when `agent.request(..)` is Some, call
  `core.dispatch_harness(&req, &harnesses, &keys, &*hub)` instead of
  `gateway.dispatch`.
- `sessions.rs` transcript: in `role_title`, try
  `kls_core::harness::harness_role_title(e)` first. Session row chip
  `🤖 <harness name>` when `kls_core::harness::session_harness(s)` is Some.
- `sessions.rs` session detail tabs for a harness session: Transcript (as
  now), Screen (`screens::harness_screen::ScreenView` fed from
  `hub.screen(..)`, plus `harness_screen::keys_ui(ui, model, sid,
  &harness.allow_keys)`), and A2A (`screens::harness_a2a::A2aPanel::new(model,
  &format!("{node}:{sid}"))`) for every session, belai ones too.
- `fleet.rs` node card: a `🤖 n` chip, n = launchable harnesses, hover lists
  name and version.
- `settings.rs`: embed `screens::harness_picker::KeyPolicyEditor` (global,
  per-harness, per-key key modes; saving is audited).

## 3. belai integration points

- `internal/factorylink/host.go`: `HostAdvert.harnesses` from
  `harnessdetect.Default.All()` (map field for field; `grant_env` from the
  registry profile `harness.ByID(id).Launch.GrantEnv`, `allow_keys` from
  `AllowKeys`), `Capabilities.harness_sessions = harnessrun.Supported`.
  Re-advert when the catalog changes.
- `internal/rc/daemon.go` `Run`: start
  `go harnessdetect.Watch(ctx, harnessdetect.Options{Home: userHome, Path:
  os.Getenv("PATH")}, 10*time.Minute, kick, func(h []harnessdetect.Harness)
  { harnessdetect.Default.Set(h); /* re-advert */ })`, kick on SIGHUP.
- `internal/rc/factory.go` `factoryStart`, kind "session", before
  `d.startGrant`:

  ```go
  if h := fd.GetHarness(); h != "" {
      sid, why = d.startHarness(r, HarnessStart{ID: h, Mode: fd.GetHarnessMode(),
          Resume: fd.GetHarnessResume(), Keys: fd.GetHarnessKeys()}, grant)
      return sid, "", why, strings.HasPrefix(why, "could not start Belai")
  }
  ```
- Kill switch: harness sessions are in `d.sessions`, so the existing stop path
  reaches them; `rc-harness` also polls `factorylink.FactoryStopped` and stops
  its harness by itself.
- A2A directory: once `Board.A2APeers` exists, add
  `(*factorylink.A2ARelay).Peers(ctx)` and feed it to the bridge's `peers`
  tool (`harnessrun.LinkA2A.Peers`) and to a new mailbox entry point so
  `A2AMessage` may address any listed peer, not only one whose broadcast was
  seen.
- `SessionUpsert` harness fields: `factorylink.upsertFor` maps them from
  `sessionsync.SessionMeta` (add `Harness`, `HarnessVersion`,
  `HarnessSessionID`, `TranscriptSource` there; `harnessrun.Session.meta`
  will set them).

## 4. Manual end-to-end checklist (Claude Code)

Run after sections 1 to 3 land. Host: this desktop, `claude` at
`~/.local/bin/claude` (checked: 2.1.330).

1. `belai connect <token>`, then `belai rc --web-controls --dir ~/GitHub/Vulnesis/keleustes`.
2. Fleet: the node shows `🤖 n`; hover lists Claude Code 2.1.330 with `✓ login`.
3. Sessions → New session → Agent Claude Code, the keleustes dir, Mode plan,
   Keys Own login, prompt "List the crates in this workspace" → Start.
4. Audit: `harness dispatched` with actor owner, node, harness, version, mode,
   key mode `own_login`. `ps -o pid,pgid,args -C claude` shows
   `claude --session-id <uuid> --permission-mode plan --mcp-config … --settings …`
   and no `-p`.
5. Screen tab: Claude's TUI with the prompt pasted once. File
   `~/.claude/projects/-home-chris-GitHub-Vulnesis-keleustes/<uuid>.jsonl`
   exists; the transcript shows `harness`, `you`, `tool Bash`, assistant rows.
6. Composer: a second prompt appears typed in the TUI; its inbox item acks
   accepted with the entry index. A prompt `/exit` is refused.
7. Ask for an edit with Mode default: the session shows `waiting`; press `[1]`
   on the Screen tab; the edit proceeds; Audit shows `harness key sent`.
8. Interrupt mid-turn: Esc stops the turn. Stop: the session ends "stopped by
   the owner" and `pgrep -f 'claude --session-id'` is empty.
9. Resume: New session with Resume = the ended session; argv is
   `claude --resume <uuid>` and the transcript continues.
10. Key modes: Settings → Harness keys → Claude Code `Hand-off`, saved (Audit
    `key policy set`, set by owner). New session → Keys → the Anthropic key →
    the TUI uses the key (`/status`), Audit `vault key handed to a harness`
    with key id, node, session, mode. Set back to Own login: `cat
    /proc/<claude pid>/environ | tr '\0' '\n' | grep ANTHROPIC` is empty.
11. A2A: start a belai session on a second node. In Claude: "call belai
    peers, then send the other session a hello". It appears in the other
    session as a classified peer message. Reply there; Claude gets the notice
    line (no peer text) and reads the reply with `belai inbox`. The A2A tab
    shows both directions; send one as the owner from the A2A tab.
12. A2A blackout on: `send` refused, inbox empty, owner send refused; all
    audited.
13. Factory stop: the claude process exits within 15 s, the session ends
    "factory stopped", a new dispatch, a key and a hand-off are refused.
    `keleustes audit verify` passes.

## 5. OP-2 harness hardening: proto and mapping (additive)

belai already produces these; they ride in transcript meta until the fields
land. `kls_store::HarnessInfo` already has the Keleustes fields (serde
default) and the picker shows them.

```proto
message Harness {
  // ... 1-16 unchanged
  bool kill = 17;                    // process-group stop (POSIX)
  bool asks = 18;                    // first-run / tool-permission dialogs become asks
  bool named = 19;                   // native session gets a name (kls-<sid8>)
  repeated string continuity = 20;   // "session-id-match"; resume is offered only with one
  repeated HarnessCaveat caveats = 21;
  int64 startup_p50_ms = 22;         // spawn to first output, measured on this node
  int64 startup_p95_ms = 23;
  uint32 startup_samples = 24;
  int64 startup_measured_at_ms = 25;
}
message HarnessCaveat { string code = 1; string note = 2; }

message SessionEnd {
  // ... unchanged
  bool termination_verified = 10;    // the process group is gone (kill -0 ESRCH)
  bool empty_result = 11;            // no reply and no change in the work tree
  bool adapter_error = 12;           // a typed prompt got no final assistant turn
  HarnessTiming timing = 13;
}
message HarnessTiming { int64 startup_ms = 1; int64 ready_ms = 2; int64 first_reply_ms = 3; int64 wall_ms = 4; }
```

Mapping:

- belai `internal/rc/factoryharnessadvert.go` `harnessAdvert`: copy
  `harnessdetect.Harness.{Kill, Asks, Named, Continuity, Caveats,
  StartupP50MS, StartupP95MS, Samples, MeasuredAt}`.
- belai `harnessrun.Session.end`: the values are in the `harness_end` entry's
  meta (`timing`, `terminationVerified`, `emptyResult`, `adapterError`); add
  them to `sessionsync.EndInfo` when the fields exist and map them in
  `factorylink.EndWith`.
- Keleustes `kls-node/src/server.rs` `host_info`: fill the new
  `HarnessInfo` fields (the literal now ends in `..Default::default()`).
  Store the session end fields with the session; show "termination
  verified" and the empty-result / adapter-error flags on the ended session.
- The end reason already says it: `adapter_error: no final assistant turn
  after the last prompt`, `continuity_unverified`, and the end line reads
  `termination verified|unverified`.

> **Section 5 landed (node agent, 2026-11-21).** `Harness` 17–25 + `HarnessCaveat`; `SessionEnd` 10–13 +
> `HarnessTiming`. belai: `harnessAdvert` copies the hardening fields; `harnessrun.Session.end` fills
> `sessionsync.EndInfo.Harness` (Keleustes only, not on the website wire); `factorylink.EndWith` maps it.
> Keleustes: `host_info` fills the new `HarnessInfo` fields; end facts are stored in the session's
> `SessionHarness.end` (no migration); the ended session shows Stopped/Unverified, Empty, Adapter and
> wall-time chips. Tests: `factorylink.TestEndWithCarriesHarnessFacts`, `kls-node tests/belai.rs` session end.
