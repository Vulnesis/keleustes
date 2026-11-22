# Request: `library` job kind for agent conversion on a node (WP9)

From: library search workstream (`docs/gaps/library-search.md` §4.4, WP9).
To: the coordinator (owner of `proto/keleustes/node/v1/node.proto`) and the
belai team.

Keleustes converts most foreign definitions itself (`kls-libsearch::convert`).
Hermes profiles, Claws/OpenClaw `CLAW.md` bundles, Nemoclaw and mini-SWE
configs carry sidecars that only belai's `internal/agentimport` adapters
understand. We ask for a job that runs that adapter on a node. Until it
exists, `kls_core::libsearch::node_convert` refuses with a clear error and
the Rust fallback converts `SOUL.md` / `CLAW.md` bodies only (with a note).

## 1. Protocol change (coordinator)

`node.proto`, message `Job`: the comment on `kind` lists the job kinds. Add:

```proto
// kind: ... | "library"   (payload: LibraryJob JSON, below)
```

No new message types are needed: the payload travels as the existing JSON
`payload` bytes and the result as the existing JSON `result`. The host
advertises support by listing `library` in `HostAdvert.capabilities.job_kinds`
(Keleustes checks `Capabilities.job_kinds` before offering "Convert on node").

## 2. Payload and result (belai)

Request (`payload`):

```json
{
  "task": "convert",
  "kind": "agent",
  "format": "hermes | claws | nemoclaw | mini-swe",
  "name": "suggested-name",
  "files": { "SOUL.md": "<base64>", "profile.yaml": "<base64>", "skills/x/SKILL.md": "<base64>" }
}
```

Limits (same as `agentimport`): 1000 files, 1 MiB each, 8 MiB total, depth 6,
no links or specials, no credential file names. The host stages the files in a
fresh temp dir, runs `agentimport.ImportItem`, deletes the dir, and never
executes anything from it.

Result (`result`):

```json
{
  "doc": "<profile markdown, canonical>",
  "sha256": "<belai libitem hash of doc>",
  "notes": [{"kind": "mapped|metadata|dropped|warning", "field": "...", "text": "..."}],
  "skills": [{"name": "...", "doc": "...", "sha256": "..."}]
}
```

## 3. Rules the job keeps

- Kill switch: Keleustes calls `libsearch::guard(Action::NodeConvert)` and the
  gateway's own `control::check(.., "job", ..)` before the frame goes out; a
  refusal is audited (`library.refused`, `kill_switch`).
- A2A: the job is a factory → node Job. It carries no agent identity and the
  converted profile is single-mode and supervised, with no A2A, peer, worker or
  schedule settings (belai `agentimport`: "no worker blocks, no schedule").
- Provenance: Keleustes records `converted_by = "node:<id>"` in
  `library_provenance`, and the ledger entry's `origin` is `host:<node>`.
- Nothing is installed: the result is reviewed and imported like any other
  preview.

## 4. Keleustes side once the proto lands

`kls_core::libsearch::node_convert` builds the payload from `Preview.fetched`,
sends it with `core.gateway()?.job(..)` and turns the result into a
`Converted` (target agent) whose notes come from belai. No other change.
