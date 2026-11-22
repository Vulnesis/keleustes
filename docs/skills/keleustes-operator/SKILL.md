---
name: keleustes-operator
description: Drive the Keleustes desktop Software Factory from a shell. Use when you need to start agent sessions on belai nodes, wait for them, answer their asks, resume them, or read their transcripts with the `keleustes` CLI.
---

# Keleustes operator

Keleustes runs agent sessions on a fleet of belai nodes. The `keleustes`
CLI works against the data directory of the running app: what you queue
here reaches the node through the app. You act as the owner; every call is
audited with the surface `cli` and checked against the factory kill switch
and the A2A blackout.

## Start here

1. `keleustes contract --json`: verbs, flags, exit codes, error codes,
   status values and JSON Schemas. It is built from the binary: when it and
   this file disagree, trust the contract.
2. `keleustes nodes list --json`: the nodes you can dispatch to.

Always pass `--json`. Then:

- The **last stdout line** is one envelope:
  `{"v":1,"kind":…,"ok":…,"data":…,"session":…,"next":[…],"warnings":[…]}`.
  Parse that line only (dispatch prints a handle line and transcript prints
  entry lines before it). stderr is not part of the contract.
- **Exit codes:** 0 ok (an open ask is ok), 1 failed/internal, 2 usage,
  3 node or agent failed, 4 not found or wrong state, 5 timeout (the work
  goes on), 6 refused by the kill switch or a maintenance window, 7 refused
  by policy (A2A blackout, node, Keleustes policy). Never 0 with `ok:false`.
- `ok:false` carries `code` and `error.remediation`. Follow the remediation.
  Never retry a `refused_*` call unchanged: a person has to lift the switch.
- `next[]` holds literal commands for the next step. Copy them. `'<text>'`
  and `'<json>'` mark the content you write.

## Status: three axes, never one

`session` in the envelope has `delivery` (queued, delivered, started,
refused, failed), `agent` (pending, running, asking, idle, completed,
stopped, failed, lost, ended) and `verified` (unknown, pass, fail).
`agent: completed` means the agent stopped, not that the work is right.
`verified` comes from host gate evidence only and is usually `unknown`:
check the result yourself (tests, diff) before you report success.

## Dispatch

```sh
keleustes sessions dispatch --node <node> --cwd <dir> --prompt '<text>' \
  --label <short-name> --wait-for ask --timeout 10m --json
```

- The first stdout line is the handle, `{"kind":"handle","dispatchId":…}`,
  printed as soon as the dispatch is queued. Keep the id: every session
  verb takes the dispatch id in place of the session id, so a crash or a
  timeout never loses the session.
- `--timeout` is one deadline over delivery, session start and the turn.
- `--wait-for`: `started` (the node started it), `ask` (the session needs
  you: an open ask, a finished turn or the end), `done` (the turn finished
  or it ended; an open ask keeps waiting for someone else to answer),
  `ended`. Without it dispatch returns once queued.
- Pick the node by id when names repeat (`ambiguous`, exit 2).

## Wait, read, answer, resume

```sh
keleustes sessions wait <id> --for ask --timeout 10m --json
keleustes sessions transcript <id> --since <idx> --json   # entries with idx > <idx>
keleustes sessions show <id> --json
keleustes sessions answer <id> <ask-id> <option-id> --json
keleustes sessions answer <id> <ask-id> --payload '<json>' --json   # clarify asks
keleustes sessions prompt <id> '<text>' --json                      # resume an idle session
keleustes sessions command <id> stop --json
```

- `agent: asking`: `session.ask` lists the options; `next[]` has one answer
  command per option. After answering, wait again: the same ask is not
  reported twice (`ask.answerQueued`).
- `agent: idle`: the turn is over and the session waits for a prompt. Resume
  it with `sessions prompt`, then `sessions wait … --for ask`.
- Ended sessions are not reopened from the CLI: dispatch a new one. A
  harness session (`session.harness`) resumes natively; `next[]` gives the
  `--harness … --resume …` dispatch.
- On `timeout` (exit 5) nothing was cancelled: `--timeout` only reports and
  never stops the session. Wait again with `next[0]`, or stop it yourself
  with the `sessions command <id> stop` in `next[]`.

## Decided behaviour

- The exit codes above are the same with and without `--json`.
- `--timeout` reports only (exit 5 plus a stop command in `next[]`).
- `next[]` may hold the placeholders `'<text>'` and `'<json>'`; replace
  them and copy everything else as is.
- Under the kill switch (pause or stop), prompts and every command except
  `stop`, `interrupt` and `cancel` are refused (`refused_paused`,
  `refused_stopped`, exit 6) on every path, the UI included. Do not retry;
  wait for the owner to run `keleustes host resume`.

## Route

- Route by node: `nodes list` shows each node's hostname and belai version;
  `sessions harnesses <node> --json` lists the third-party harnesses a node
  advertises (`--harness <id>` on dispatch).
- Cross-host requests nodes make wait in `sessions requests --json`; the
  owner approves or declines them.
- One write-capable session per working directory.

## Hygiene

- Check `sessions list --open --json` before you assume what is running.
- Report friction instead of working around it: `keleustes analyze-usage
  --json` lists refusals, lost sessions, stops before output and unanswered
  asks; a confusing envelope is a contract bug.
