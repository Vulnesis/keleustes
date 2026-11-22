//! `keleustes contract`: the machine-readable contract, built from the
//! binary itself (the clap tree, the code table, the schemas), so it cannot
//! drift from what the verbs do. When it and the docs disagree, it wins.

use super::{CODES, V};
use kls_core::sessions::axes::{AGENT, DELIVERY, VERIFIED};
use serde_json::{Value, json};

/// The operator skill, shipped in the binary (`keleustes contract --skill`).
pub const SKILL: &str = include_str!("../../../../docs/skills/keleustes-operator/SKILL.md");

/// Schema names `--schema` takes.
pub const SCHEMAS: [&str; 5] = ["envelope", "handle", "entry", "session", "finding"];

/// The rules an agent needs before its first call.
pub const NOTES: [&str; 14] = [
    "Pass --json to any verb: it prints exactly one envelope as the LAST stdout line, on success and on failure. Parse only that line (and the handle and entry lines before it).",
    "stderr is not part of the contract. --help is human text and the one exemption.",
    "The exit code comes from the envelope: 0 when ok (an open ask is ok), else the code's exit. A command never exits 0 with ok:false.",
    "ok:false always has code (closed enum, errorCodes) and error.message plus error.remediation. Follow the remediation; do not retry a refused_* command unchanged.",
    "next[] holds literal follow-up commands. '<text>' and '<json>' mark content you write; everything else is copied as is.",
    "sessions dispatch prints a handle line ({kind:handle, dispatchId}) as soon as the dispatch is queued, before any agent work: keep that id. Every session verb takes the dispatch id in place of the session id.",
    "A session's status has three separate axes: delivery (did the node get it), agent (is the agent running, asking, idle or done) and verified (host gate evidence only; unknown without it). agent completed does not mean the work is correct.",
    "--timeout is one deadline over delivery to the node, the session start and the turn. Durations: 500ms, 30s, 10m, 1h or bare seconds. On expiry the work goes on (exit 5, code timeout).",
    "--wait-for/--for: started (the node bound a session), ask (an open ask, a finished turn or the end: whenever the session needs you), done (the turn finished or the session ended; an open ask keeps waiting), ended (the session ended).",
    "Resume a session: when agent is idle, sessions prompt <id> '<text>' then wait again. An ended belai session is not reopened by the CLI; dispatch a new one (a harness session resumes with --harness --resume, given in next[]).",
    "Every action is audited with the surface cli and checked against the factory kill switch and the A2A blackout; refusals are exit 6 (kill switch) or 7 (policy).",
    "Decided: the exit codes are the same with and without --json (human output uses them too).",
    "Decided: --timeout only reports (exit 5, a stop command in next[]); it never stops the session. Stop it yourself if you mean to.",
    "Decided: next[] may hold the placeholders '<text>' and '<json>'; replace them, copy everything else. Under the kill switch (pause or stop) prompts and every command but stop, interrupt and cancel are refused (refused_paused / refused_stopped) on every path, UI included.",
];

pub fn envelope_schema() -> Value {
    let codes: Vec<&str> = CODES.iter().map(|c| c.name).collect();
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "keleustes/v1/envelope",
        "title": "The last stdout line of every verb under --json",
        "type": "object",
        "required": ["v", "kind", "ok", "next", "warnings"],
        "additionalProperties": false,
        "properties": {
            "v": {"const": V},
            "kind": {"type": "string", "description": "The verb, dotted: sessions.dispatch, nodes.list, contract …"},
            "ok": {"type": "boolean"},
            "code": {"enum": codes},
            "error": {
                "type": "object", "required": ["message", "remediation"], "additionalProperties": false,
                "properties": {"message": {"type": "string"}, "remediation": {"type": "string"}}
            },
            "data": {"description": "The verb's result (see each verb's `data` in the contract)."},
            "session": {"$ref": "#/$defs/session"},
            "next": {"type": "array", "items": {"type": "string"}},
            "warnings": {"type": "array", "items": {"type": "string"}}
        },
        "if": {"properties": {"ok": {"const": false}}},
        "then": {"required": ["code", "error"]},
        "else": {"not": {"anyOf": [{"required": ["code"]}, {"required": ["error"]}]}},
        "$defs": {"session": session_schema()}
    })
}

pub fn session_schema() -> Value {
    json!({
        "$id": "keleustes/v1/session",
        "title": "A session, or a dispatch not yet bound to one (id empty)",
        "type": "object",
        "required": ["id", "dispatchId", "nodeId", "label", "state", "nextIndex", "delivery", "agent", "verified"],
        "additionalProperties": false,
        "properties": {
            "id": {"type": "string", "description": "Session id; empty until the node starts it"},
            "dispatchId": {"type": "string"},
            "nodeId": {"type": "string"},
            "label": {"type": "string", "description": "--label given at dispatch"},
            "title": {"type": "string"},
            "cwd": {"type": "string"},
            "state": {"type": "string", "description": "pending (no session yet), live, idle, waiting or ended; a host may report others"},
            "endReason": {"type": "string"},
            "dispatchError": {"type": "string"},
            "nextIndex": {"type": "integer", "description": "Index the next transcript entry gets"},
            "ask": {
                "type": ["object", "null"],
                "additionalProperties": false,
                "properties": {
                    "id": {"type": "string"}, "kind": {"type": "string"}, "title": {"type": "string"},
                    "tool": {"type": "string"}, "questionsJson": {"type": "string"},
                    "answerQueued": {"type": "boolean"},
                    "options": {"type": "array", "items": {
                        "type": "object", "required": ["id", "label"],
                        "properties": {"id": {"type": "string"}, "label": {"type": "string"}}
                    }}
                }
            },
            "delivery": {"enum": DELIVERY},
            "agent": {"enum": AGENT},
            "verified": {"enum": VERIFIED},
            "harness": {"type": "string"},
            "harnessSessionId": {"type": "string"}
        }
    })
}

pub fn handle_schema() -> Value {
    json!({
        "$id": "keleustes/v1/handle",
        "title": "First stdout line of sessions dispatch --json, printed once the dispatch is queued",
        "type": "object",
        "required": ["v", "kind", "dispatchId", "nodeId"],
        "additionalProperties": false,
        "properties": {
            "v": {"const": V}, "kind": {"const": "handle"},
            "dispatchId": {"type": "string"}, "nodeId": {"type": "string"}, "label": {"type": "string"}
        }
    })
}

pub fn entry_schema() -> Value {
    json!({
        "$id": "keleustes/v1/entry",
        "title": "A transcript entry line (sessions transcript --json), before the envelope",
        "type": "object",
        "required": ["v", "kind", "sessionId", "idx", "role", "text"],
        "additionalProperties": false,
        "properties": {
            "v": {"const": V}, "kind": {"const": "entry"},
            "sessionId": {"type": "string"}, "idx": {"type": "integer"},
            "role": {"type": "string"}, "text": {"type": "string"}, "at": {"type": "integer"}
        }
    })
}

pub fn finding_schema() -> Value {
    json!({
        "$id": "keleustes/v1/finding",
        "title": "One usage-friction finding (analyze-usage); a JSONL fix-queue record",
        "type": "object",
        "required": ["findingId", "kind", "axDefect", "severity", "surface", "at", "subject", "detail", "pointer"],
        "additionalProperties": false,
        "properties": {
            "findingId": {"type": "string", "description": "kind:subject id; the dedup key"},
            "kind": {"enum": super::analyze::KINDS.iter().map(|k| k.0).collect::<Vec<_>>()},
            "axDefect": {"enum": [true, false, "review"]},
            "severity": {"enum": ["high", "medium", "low"]},
            "surface": {"type": "string"},
            "at": {"type": "integer"},
            "subject": {"type": "object"},
            "detail": {"type": "string"},
            "pointer": {"type": "string", "description": "A command that shows the evidence"}
        }
    })
}

pub fn schema(name: &str) -> Option<Value> {
    Some(match name {
        "envelope" => envelope_schema(),
        "handle" => handle_schema(),
        "entry" => entry_schema(),
        "session" => session_schema(),
        "finding" => finding_schema(),
        _ => return None,
    })
}

fn arg_json(a: &clap::Arg) -> Value {
    let values: Vec<String> = a
        .get_possible_values()
        .iter()
        .map(|v| v.get_name().to_string())
        .collect();
    let takes = a.get_action().takes_values();
    let defaults: Vec<String> = a
        .get_default_values()
        .iter()
        .map(|v| v.to_string_lossy().to_string())
        .collect();
    json!({
        "name": a.get_id().as_str(),
        "long": a.get_long().map(|l| format!("--{l}")),
        "positional": a.is_positional(),
        "required": a.is_required_set(),
        "takesValue": takes,
        "multiple": matches!(a.get_action(), clap::ArgAction::Append),
        "default": defaults.first(),
        "values": values,
        "help": a.get_help().map(|h| h.to_string()).unwrap_or_default(),
    })
}

fn usage_of(path: &str, c: &clap::Command) -> String {
    let mut u = format!("keleustes {path}");
    for a in c.get_arguments().filter(|a| !a.is_global_set()) {
        let id = a.get_id().as_str();
        if id == "help" || id == "version" {
            continue;
        }
        let name = id.to_uppercase();
        let piece = match a.get_long() {
            None => {
                let dots = if a.get_num_args().is_some_and(|n| n.max_values() > 1) {
                    "..."
                } else {
                    ""
                };
                format!("<{name}>{dots}")
            }
            Some(l) if a.get_action().takes_values() => format!("--{l} <{name}>"),
            Some(l) => format!("--{l}"),
        };
        if a.is_required_set() {
            u.push_str(&format!(" {piece}"));
        } else {
            u.push_str(&format!(" [{piece}]"));
        }
    }
    u
}

fn walk(prefix: &str, c: &clap::Command, out: &mut Vec<Value>) {
    for s in c.get_subcommands() {
        if s.get_name() == "help" {
            continue;
        }
        let path = if prefix.is_empty() {
            s.get_name().to_string()
        } else {
            format!("{prefix} {}", s.get_name())
        };
        if s.has_subcommands() {
            walk(&path, s, out);
            continue;
        }
        out.push(json!({
            "verb": path,
            "kind": path.replace(' ', "."),
            "about": s.get_about().map(|a| a.to_string()).unwrap_or_default(),
            "usage": usage_of(&path, s),
            "args": s.get_arguments()
                .filter(|a| !a.is_global_set() && !matches!(a.get_id().as_str(), "help" | "version"))
                .map(arg_json).collect::<Vec<_>>(),
            "data": data_doc(&path),
        }));
    }
}

/// What `data` holds for the verbs agents use most.
fn data_doc(verb: &str) -> &'static str {
    match verb {
        "sessions dispatch" => {
            "{dispatchId, nodeId, label}; session carries the status. Prints a handle line first."
        }
        "sessions wait" => "{for, satisfied}; session carries the status",
        "sessions show" => "the session's full record (state, ask, accepts, usage, git, harness)",
        "sessions transcript" => {
            "{count, lastIdx}; entry lines precede the envelope. --since N prints idx > N"
        }
        "sessions answer" | "sessions prompt" | "sessions command" => "{itemId}",
        "sessions list" => "{sessions: [session]}",
        "nodes list" => "{nodes: [{id, name, hostname, belaiVersion, state, lastSeenAt}]}",
        "contract" => "the contract (or one schema with --schema, the skill with --skill)",
        "analyze-usage" => "{findings: [finding], counts, appended}",
        _ => "",
    }
}

/// The whole contract.
pub fn contract(cli: &clap::Command) -> Value {
    let mut verbs = Vec::new();
    walk("", cli, &mut verbs);
    let globals: Vec<Value> = cli
        .get_arguments()
        .filter(|a| a.is_global_set())
        .map(arg_json)
        .collect();
    let mut exits: Vec<Value> = vec![
        json!({"exit": 0, "meaning": "ok (including an open ask or a finished turn)", "codes": []}),
    ];
    let mut seen: Vec<i32> = Vec::new();
    for c in &CODES {
        if seen.contains(&c.exit) {
            continue;
        }
        seen.push(c.exit);
        let codes: Vec<&str> = CODES
            .iter()
            .filter(|x| x.exit == c.exit)
            .map(|x| x.name)
            .collect();
        let meaning = match c.exit {
            1 => "failed or internal error",
            2 => "usage: fix the command line",
            3 => "the node or agent failed",
            4 => "not found or wrong state",
            5 => "timeout: the work goes on",
            6 => "refused by the factory kill switch or a maintenance window",
            7 => "refused by policy (A2A blackout, node, Keleustes policy)",
            _ => "",
        };
        exits.push(json!({"exit": c.exit, "meaning": meaning, "codes": codes}));
    }
    exits.sort_by_key(|e| e["exit"].as_i64());
    json!({
        "contract": format!("keleustes/v{V}"),
        "version": env!("CARGO_PKG_VERSION"),
        "notes": NOTES,
        "globalFlags": globals,
        "verbs": verbs,
        "exitCodes": exits,
        "errorCodes": CODES.iter().map(|c| json!({
            "code": c.name, "exit": c.exit, "meaning": c.meaning, "remediation": c.remediation,
        })).collect::<Vec<_>>(),
        "status": {
            "delivery": DELIVERY, "agent": AGENT, "verified": VERIFIED,
            "waitFor": ["started", "ask", "done", "ended"],
        },
        "schemas": SCHEMAS.iter().map(|s| (s.to_string(), schema(s).unwrap())).collect::<serde_json::Map<_, _>>(),
        "skill": "keleustes contract --skill",
    })
}
