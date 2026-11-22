//! hook → belai Hook bundle (§3.4): `{name, description, hooks:{Event:
//! [{matcher?, hooks:[{type:"command", command, timeout?}]}]}}` plus the
//! script files the commands name.
//!
//! Stored only: a hook is never enabled or installed by import.

use super::canonical::{self, Json, one_line};
use super::validate::{self, Draft, HOOK_EVENTS};
use super::{BundleFile, Note, NoteKind, Opts, Target, primary};
use crate::scan::Flag;
use crate::source::{Ecosystem, Fetched};
use anyhow::{Result, anyhow, bail};
use std::collections::BTreeMap;

/// The belai event for a dialect's event, and the matcher it implies.
fn event_for(eco: Ecosystem, ev: &str) -> Option<(&'static str, Option<&'static str>)> {
    let direct = HOOK_EVENTS.iter().find(|e| **e == ev).copied();
    match eco {
        Ecosystem::GeminiCli => match ev {
            "BeforeTool" => Some(("PreToolUse", None)),
            "AfterTool" => Some(("PostToolUse", None)),
            "BeforeAgent" => Some(("UserPromptSubmit", None)),
            "AfterAgent" => Some(("Stop", None)),
            "SessionStart" => Some(("SessionStart", None)),
            "SessionEnd" => Some(("SessionEnd", None)),
            "PreCompress" => Some(("PreCompact", None)),
            "Notification" => Some(("Notification", None)),
            _ => None,
        },
        Ecosystem::Copilot => match ev {
            "sessionStart" => Some(("SessionStart", None)),
            "sessionEnd" => Some(("SessionEnd", None)),
            "userPromptSubmitted" => Some(("UserPromptSubmit", None)),
            "preToolUse" => Some(("PreToolUse", None)),
            "postToolUse" => Some(("PostToolUse", None)),
            _ => direct.map(|e| (e, None)),
        },
        Ecosystem::Cursor => match ev {
            "beforeShellExecution" => Some(("PreToolUse", Some("Bash"))),
            "beforeReadFile" => Some(("PreToolUse", Some("Read"))),
            "afterFileEdit" => Some(("PostToolUse", Some("Edit|Write"))),
            "beforeSubmitPrompt" => Some(("UserPromptSubmit", None)),
            "stop" => Some(("Stop", None)),
            _ => None,
        },
        _ => direct.map(|e| (e, None)),
    }
}

/// Root prefixes a command may name files under, stripped to bundle paths.
const ROOTS: [&str; 10] = [
    "\"${CLAUDE_PLUGIN_ROOT}\"/",
    "${CLAUDE_PLUGIN_ROOT}/",
    "$CLAUDE_PLUGIN_ROOT/",
    "\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/",
    "\"${CLAUDE_PROJECT_DIR}\"/.claude/hooks/",
    "${CLAUDE_PROJECT_DIR}/.claude/hooks/",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/",
    "${extensionPath}/",
    "$extensionPath/",
    "./",
];

struct Conv<'a> {
    src: &'a Fetched,
    /// The folder of the hooks file (relative scripts resolve here).
    dir: String,
    files: BTreeMap<String, BundleFile>,
    notes: Vec<Note>,
    flags: Vec<Flag>,
    dropped: std::collections::BTreeSet<String>,
}

impl Conv<'_> {
    /// Finds a referenced file in the fetched tree.
    fn find(&self, rel: &str, root: &str) -> Option<String> {
        let mut cands = vec![rel.to_string()];
        if root.contains(".claude/hooks/") {
            cands.insert(0, format!(".claude/hooks/{rel}"));
        }
        if !self.dir.is_empty() {
            cands.push(format!("{}/{rel}", self.dir));
        }
        // A plugin's hooks.json sits in hooks/; the root is one up.
        cands.push(format!("hooks/{rel}"));
        cands.into_iter().find(|c| self.src.files.contains_key(c))
    }

    /// Carries one file under `bundle` (its path below the root).
    fn carry(&mut self, found: &str, bundle: &str) -> Option<String> {
        let data = self.src.files.get(found)?.to_vec();
        let path = bundle
            .split('/')
            .map(|s| {
                let mut out: String = s
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                if out.starts_with('.') || out.is_empty() {
                    out = format!("_{}", out.trim_start_matches('.'));
                }
                canonical::clip(&out, 64).to_string()
            })
            .collect::<Vec<_>>()
            .join("/");
        let path = if path == "hooks.json" {
            "script-hooks.json".to_string()
        } else {
            path
        };
        if self.files.contains_key(&path) {
            return Some(path);
        }
        if std::str::from_utf8(&data).is_err() || data.contains(&0) {
            self.flags.push(Flag::Refused(format!(
                "{bundle} is not text (a binary script)"
            )));
            return None;
        }
        if data.is_empty() || data.len() > validate::MAX_BUNDLE_FILE_BYTES {
            self.flags
                .push(Flag::Refused(format!("{bundle} is empty or over 256 KiB")));
            return None;
        }
        if crate::scan_rules::holds_secret(&String::from_utf8_lossy(&data)) {
            self.flags.push(Flag::Refused(format!(
                "{bundle} looks like it holds a secret"
            )));
            return None;
        }
        let sha256 = canonical::hash(&data);
        self.files.insert(
            path.clone(),
            BundleFile {
                path: path.clone(),
                data,
                sha256,
            },
        );
        self.notes.push(Note::mapped(
            "command",
            format!("{bundle} carried as {path}"),
        ));
        Some(path)
    }

    /// Rewrites root-relative file references to bundle paths, carrying them.
    fn command(&mut self, cmd: &str) -> String {
        let cmd = cmd.trim();
        let mut out = Vec::new();
        for (i, tok) in cmd.split(' ').enumerate() {
            let mut t = tok.to_string();
            let mut hit = false;
            for root in ROOTS {
                if let Some(rel) = tok.strip_prefix(root) {
                    let rel = rel.trim_matches('"');
                    if rel.split('/').any(|s| s == "..") || rel.starts_with('/') {
                        self.flags
                            .push(Flag::Refused(format!("{tok} escapes the bundle")));
                        hit = true;
                        break;
                    }
                    let bundle = rel.to_string();
                    match self.find(rel, root) {
                        Some(found) => {
                            if let Some(p) = self.carry(&found, &bundle) {
                                t = p;
                            }
                        }
                        None => self.notes.push(Note::warning(
                            "command",
                            format!("{tok} was not found, so it is not carried"),
                        )),
                    }
                    hit = true;
                    break;
                }
            }
            if i == 0 && !hit {
                // A relative script without a root (Copilot, Cursor).
                if (tok.contains('/') || tok.ends_with(".sh") || tok.ends_with(".py"))
                    && !tok.starts_with('/')
                    && !tok.contains("..")
                    && let Some(found) = self.find(tok.trim_start_matches("./"), "")
                {
                    let base = tok.rsplit('/').next().unwrap_or(tok).to_string();
                    if let Some(p) = self.carry(&found, &base) {
                        t = p;
                        hit = true;
                    }
                }
                if !hit {
                    let word = tok.trim_matches(['"', '\'']);
                    if !word.is_empty() {
                        self.flags.push(Flag::NeedsProgram(word.to_string()));
                        self.notes.push(Note::warning(
                            "command",
                            format!("{word} is not a bundle file; add it to hooks.allowed_programs on the host"),
                        ));
                    }
                }
            }
            out.push(t);
        }
        out.join(" ")
    }

    fn handler(&mut self, h: &Json, copilot: bool) -> Option<Json> {
        let o = h.as_obj()?;
        let typ = o.get("type").and_then(Json::as_str).unwrap_or("command");
        let cmd = if copilot {
            o.get("bash").or(o.get("command")).and_then(Json::as_str)
        } else {
            o.get("command").and_then(Json::as_str)
        };
        if typ != "command" || cmd.is_none() {
            self.dropped.insert(format!("{typ} handler"));
            return None;
        }
        let cmd = one_line(cmd.unwrap_or_default());
        if cmd.is_empty() {
            return None;
        }
        let mut out = Json::obj();
        out.set("type", Json::s("command"));
        out.set("command", Json::s(self.command(&cmd)));
        let to = if copilot {
            o.get("timeoutSec").or(o.get("timeout"))
        } else {
            o.get("timeout")
        };
        if let Some(t) = to {
            match t.as_i64() {
                Some(n) if (1..=validate::MAX_HOOK_TIMEOUT).contains(&n) => {
                    out.set("timeout", Json::Num(n.to_string()))
                }
                Some(n) if n > validate::MAX_HOOK_TIMEOUT => {
                    out.set("timeout", Json::Num(validate::MAX_HOOK_TIMEOUT.to_string()));
                    self.notes
                        .push(Note::warning("timeout", format!("{n} s capped at 600")));
                }
                _ => self.notes.push(Note::warning(
                    "timeout",
                    "not a whole number of 1-600 s; dropped",
                )),
            }
        }
        for k in o.keys() {
            let used = matches!(k.as_str(), "type" | "command" | "timeout")
                || (copilot && matches!(k.as_str(), "bash" | "timeoutSec"));
            if !used {
                if copilot && matches!(k.as_str(), "powershell" | "cwd" | "env") {
                    self.notes
                        .push(Note::warning(k, "dropped (Copilot handler field)"));
                } else {
                    self.dropped.insert(k.clone());
                }
            }
        }
        Some(out)
    }
}

pub fn convert(eco: Ecosystem, src: &Fetched, opts: &Opts) -> Result<Draft> {
    let path = primary(
        src,
        &[
            "hooks.json",
            "hooks/hooks.json",
            ".claude/settings.json",
            "settings.json",
            ".cursor/hooks.json",
        ],
    )
    .or_else(|| {
        src.files
            .keys()
            .find(|k| k.ends_with(".json"))
            .map(|k| k.as_str())
    })
    .ok_or_else(|| anyhow!("no hooks file in the fetched files"))?
    .to_string();
    let (text, hidden) = super::text(src, &path)?;
    let top = canonical::parse_value(text.as_bytes()).map_err(|e| anyhow!("{path}: {e}"))?;
    let hooks = top
        .get("hooks")
        .and_then(Json::as_obj)
        .ok_or_else(|| anyhow!("{path}: no hooks object"))?;
    let mut cv = Conv {
        src,
        dir: path
            .rsplit_once('/')
            .map(|(d, _)| d.to_string())
            .unwrap_or_default(),
        files: BTreeMap::new(),
        notes: vec![],
        flags: vec![],
        dropped: Default::default(),
    };
    if hidden {
        cv.flags.push(Flag::HiddenText);
    }
    let flat = matches!(eco, Ecosystem::Copilot | Ecosystem::Cursor);
    if matches!(
        eco,
        Ecosystem::GeminiCli | Ecosystem::Copilot | Ecosystem::Cursor
    ) {
        cv.flags.push(Flag::ProtocolMismatch);
        cv.notes.push(Note::warning(
            "protocol",
            format!(
                "{} hook converted: its stdin/stdout protocol differs",
                eco.label()
            ),
        ));
    }
    let mut events: BTreeMap<String, Vec<Json>> = BTreeMap::new();
    for (ev, groups) in hooks {
        if ev.starts_with('_') {
            cv.notes.push(Note::dropped(ev));
            continue;
        }
        let Some((bev, implied)) = event_for(eco, ev) else {
            cv.notes
                .push(Note::new(NoteKind::Dropped, ev, "no belai event"));
            continue;
        };
        let Some(groups) = groups.as_arr() else {
            bail!("{path}: hooks.{ev} is not a list");
        };
        for g in groups {
            let (matcher, handlers): (Option<String>, Vec<Json>) = if flat {
                (implied.map(str::to_string), vec![g.clone()])
            } else {
                let m = g
                    .str_of("matcher")
                    .map(str::to_string)
                    .or(implied.map(str::to_string));
                for k in g.as_obj().into_iter().flat_map(|o| o.keys()) {
                    if k != "matcher" && k != "hooks" {
                        cv.dropped.insert(k.clone());
                    }
                }
                (
                    m,
                    g.get("hooks")
                        .and_then(Json::as_arr)
                        .cloned()
                        .unwrap_or_default(),
                )
            };
            let hs: Vec<Json> = handlers
                .iter()
                .filter_map(|h| cv.handler(h, eco == Ecosystem::Copilot))
                .collect();
            if hs.is_empty() {
                continue;
            }
            let mut group = Json::obj();
            if let Some(m) = matcher.filter(|m| !m.is_empty()) {
                if ["UserPromptSubmit", "Stop", "SubagentStop"].contains(&bev) {
                    cv.notes.push(Note::dropped(&format!("{bev}.matcher")));
                } else {
                    group.set("matcher", Json::s(m));
                }
            }
            group.set("hooks", Json::Arr(hs));
            events.entry(bev.to_string()).or_default().push(group);
        }
    }
    for k in std::mem::take(&mut cv.dropped) {
        cv.notes.push(Note::dropped(&k));
    }
    let mut d = Draft::new(Target::Library { kind: "hook" });
    if events.is_empty() {
        d.flags.push(Flag::Refused(
            "no command handler maps to a belai hook event".into(),
        ));
    }
    let raw_name = if !src.name_hint.is_empty() {
        src.name_hint.clone()
    } else {
        top.str_of("name")
            .map(str::to_string)
            .unwrap_or_else(|| "imported-hooks".into())
    };
    let mut notes = std::mem::take(&mut cv.notes);
    d.name = super::name_for(&raw_name, opts, &mut notes);
    d.description = top
        .str_of("description")
        .map(|s| canonical::clip(&one_line(s), validate::MAX_HOOK_DESCRIPTION).to_string())
        .unwrap_or_default();
    let mut doc = Json::obj();
    doc.set("name", Json::s(d.name.clone()));
    if !d.description.is_empty() {
        doc.set("description", Json::s(d.description.clone()));
    }
    doc.set(
        "hooks",
        Json::Obj(events.into_iter().map(|(k, v)| (k, Json::Arr(v))).collect()),
    );
    d.doc = doc.canonical();
    d.files = cv.files.into_values().collect();
    let n_cmds = d.doc.windows(10).filter(|w| *w == b"\"command\":").count();
    notes.push(Note::mapped(
        "hooks",
        format!(
            "{n_cmds} command(s), {} script(s) carried; stored, not installed",
            d.files.len()
        ),
    ));
    d.notes = notes;
    for f in cv.flags {
        d.flag(f);
    }
    d.flag(Flag::RunsCode);
    Ok(d)
}
