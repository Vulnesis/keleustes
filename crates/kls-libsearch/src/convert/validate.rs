//! belai validators per kind (`internal/libitem`: skill, command, prompt,
//! hook, mcp; `libstore` bundle bounds), the converter dispatch and the
//! finishing step every converter goes through (hash, licence, scan).

use super::canonical::{self, Json};
use super::frontmatter::{self, scalar};
use super::{BundleFile, Converted, Note, Opts, SourceFile, Target};
use crate::licence::{self, LicenceClass};
use crate::scan::Flag;
use crate::source::{Ecosystem, Fetched, ItemType};
use anyhow::{Result, anyhow, bail};
use serde_norway::Value as Yaml;

/// Markdown kinds (skill, prompt, command): 32 KiB.
pub const MAX_MD_BYTES: usize = 32 << 10;
/// JSON kinds (mcp, hook): 16 KiB.
pub const MAX_JSON_BYTES: usize = 16 << 10;
pub const MAX_PROMPT_DESCRIPTION: usize = 300;
pub const MAX_SKILL_DESCRIPTION: usize = 1024;
pub const MAX_LICENSE: usize = 128;
pub const MAX_COMPATIBILITY: usize = 500;
pub const MAX_ALLOWED_TOOLS: usize = 64;
pub const MAX_TOOL: usize = 128;
pub const MAX_ARGUMENT_HINT: usize = 256;
pub const MAX_METADATA_ENTRIES: usize = 32;
pub const MAX_METADATA_KEY: usize = 64;
pub const MAX_METADATA_VALUE: usize = 4096;
pub const MAX_METADATA_TOTAL: usize = 16384;
/// Hook rules.
pub const MAX_HOOK_DESCRIPTION: usize = 300;
pub const MAX_HOOK_GROUPS: usize = 8;
pub const MAX_HOOK_HANDLERS: usize = 8;
pub const MAX_HOOK_MATCHER: usize = 128;
pub const MAX_HOOK_COMMAND: usize = 512;
pub const MAX_HOOK_TIMEOUT: i64 = 600;
/// Bundle bounds (belai libstore).
pub const MAX_BUNDLE_FILES: usize = 32;
pub const MAX_BUNDLE_FILE_BYTES: usize = 256 << 10;
pub const MAX_BUNDLE_BYTES: usize = 2 << 20;

/// The nine events a belai hook can name.
pub const HOOK_EVENTS: [&str; 9] = [
    "PreToolUse",
    "PostToolUse",
    "UserPromptSubmit",
    "SessionStart",
    "SessionEnd",
    "Stop",
    "SubagentStop",
    "Notification",
    "PreCompact",
];
const HOOK_NO_MATCHER: [&str; 3] = ["UserPromptSubmit", "Stop", "SubagentStop"];

const SKILL_FIELDS: [&str; 7] = [
    "name",
    "description",
    "license",
    "compatibility",
    "metadata",
    "allowed-tools",
    "disable-model-invocation",
];

/// belai's item name rule: `^[a-z0-9][a-z0-9._-]{0,63}$`.
pub fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

/// belai's MCP name rule: `^[A-Za-z0-9_-]{1,32}$`.
pub fn valid_mcp_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// `^[A-Za-z_][A-Za-z0-9_]{0,63}$`.
pub fn valid_ref_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// belai `MCPLooksSecret`.
pub fn looks_secret(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "secret",
        "token",
        "password",
        "passwd",
        "credential",
        "private",
        "key",
        "auth",
        "cookie",
    ]
    .iter()
    .any(|w| n.contains(w))
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

fn yaml_text(v: &Yaml, what: &str) -> Result<String> {
    match v {
        Yaml::Null => Ok(String::new()),
        v => scalar(v).ok_or_else(|| anyhow!("field {what:?}: expected a single value")),
    }
}

/// The parsed front matter of a belai Markdown item.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MdItem {
    pub name: String,
    pub description: String,
    pub body: String,
}

fn strict_closing(doc: &str) -> Result<()> {
    let rest = doc.strip_prefix("---\n").unwrap_or(doc);
    for line in rest.split('\n') {
        if line.starts_with("---") {
            if line.trim_end_matches([' ', '\t']) != "---" {
                bail!("malformed front matter line {:?}", line.trim());
            }
            return Ok(());
        }
    }
    bail!("the front matter is not closed by a --- line")
}

/// belai `skills.Split`: front matter text and body.
fn split_doc(doc: &str) -> Result<(&str, &str)> {
    let rest = doc
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow!("missing front-matter delimiter ---"))?;
    let idx = rest
        .find("\n---")
        .ok_or_else(|| anyhow!("unterminated front-matter"))?;
    let after = &rest[idx + 4..];
    let body = match after.find('\n') {
        Some(nl) => &after[nl + 1..],
        None => "",
    };
    Ok((&rest[..idx], body.trim_start_matches(['\r', '\n'])))
}

/// belai `ParseSkill` / `ParseCommand` over a canonical document.
pub fn skill_or_command(doc: &str, command: bool) -> Result<MdItem> {
    if !doc.starts_with("---\n") {
        bail!("the document must open with a --- front matter line");
    }
    strict_closing(doc)?;
    let (front, body) = split_doc(doc)?;
    let fields = frontmatter::strict_fields(front).map_err(|e| anyhow!(e))?;
    let mut m = MdItem::default();
    // belai `textField`: a plain or flow value on the key line is read as
    // the raw text after the colon.
    let raw: std::collections::BTreeMap<String, String> = front
        .lines()
        .filter(|l| !l.starts_with([' ', '\t']))
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let text = |k: &str, v: &Yaml| -> Result<String> {
        match raw.get(k) {
            Some(r)
                if !r.is_empty()
                    && !r.starts_with(['"', '\'', '|', '>'])
                    && !matches!(v, Yaml::Null) =>
            {
                Ok(r.clone())
            }
            _ => yaml_text(v, k),
        }
    };
    let mut tools: Vec<String> = Vec::new();
    for (k, v) in &fields {
        let allowed = SKILL_FIELDS.contains(&k.as_str()) || (command && k == "argument-hint");
        if !allowed {
            bail!("unknown front-matter field {k:?}");
        }
        match k.as_str() {
            "name" => m.name = text(k, v)?,
            "description" => m.description = text(k, v)?,
            "license" => {
                if text(k, v)?.len() > MAX_LICENSE {
                    bail!("license is over {MAX_LICENSE} bytes");
                }
            }
            "compatibility" => {
                if text(k, v)?.len() > MAX_COMPATIBILITY {
                    bail!("compatibility is over {MAX_COMPATIBILITY} bytes");
                }
            }
            "argument-hint" => {
                let h = text(k, v)?;
                if h.len() > MAX_ARGUMENT_HINT {
                    bail!(
                        "argument-hint is {} bytes; the most is {MAX_ARGUMENT_HINT}",
                        h.len()
                    );
                }
            }
            "metadata" => check_metadata(v)?,
            "allowed-tools" => {
                tools = match v {
                    Yaml::Sequence(s) => {
                        let mut out = vec![];
                        for x in s {
                            let t = scalar(x)
                                .ok_or_else(|| anyhow!("expected a list of tool names"))?;
                            if t.is_empty() {
                                bail!("field \"allowed-tools\": empty list item");
                            }
                            out.push(t);
                        }
                        out
                    }
                    v => text(k, v)?
                        .split_whitespace()
                        .map(|t| t.trim_end_matches(',').to_string())
                        .filter(|t| !t.is_empty())
                        .collect(),
                }
            }
            "disable-model-invocation" => {
                let s = text(k, v)?.to_ascii_lowercase();
                if s != "true" && s != "false" {
                    bail!("field \"disable-model-invocation\" must be true or false");
                }
            }
            _ => {}
        }
    }
    if m.name.trim().is_empty() {
        bail!("required front-matter field \"name\" missing or empty");
    }
    if m.description.trim().is_empty() {
        bail!("required front-matter field \"description\" missing or empty");
    }
    if !valid_name(&m.name) {
        bail!("name {:?} is not valid", m.name);
    }
    if !command && m.name.starts_with("belai-") {
        bail!("names starting with \"belai-\" are reserved for Belai's own skills");
    }
    if m.description.len() > MAX_SKILL_DESCRIPTION {
        bail!(
            "description is {} bytes; the most is {MAX_SKILL_DESCRIPTION}",
            m.description.len()
        );
    }
    if tools.len() > MAX_ALLOWED_TOOLS {
        bail!(
            "allowed-tools has {} entries; the most is {MAX_ALLOWED_TOOLS}",
            tools.len()
        );
    }
    if tools.iter().any(|t| t.len() > MAX_TOOL) {
        bail!("an allowed-tools entry is over {MAX_TOOL} bytes");
    }
    if body.trim().is_empty() {
        bail!(
            "the {} has no instructions after the front matter",
            if command { "command" } else { "skill" }
        );
    }
    m.body = body.trim_start_matches('\n').to_string();
    Ok(m)
}

fn check_metadata(v: &Yaml) -> Result<()> {
    let entries: Vec<(String, String)> = match v {
        Yaml::Null => vec![],
        Yaml::Mapping(map) => {
            let mut out = vec![];
            for (k, v) in map {
                let (Some(k), Some(v)) = (
                    scalar(k),
                    scalar(v).or(matches!(v, Yaml::Null).then(String::new)),
                ) else {
                    bail!("field \"metadata\": expected a map of string keys to string values");
                };
                out.push((k, v));
            }
            out
        }
        v => match scalar(v) {
            Some(s) if s.is_empty() => vec![],
            Some(s) => vec![("note".to_string(), s)],
            None => bail!("field \"metadata\": expected a map of string keys to string values"),
        },
    };
    if entries.len() > MAX_METADATA_ENTRIES {
        bail!(
            "metadata has {} entries; the most is {MAX_METADATA_ENTRIES}",
            entries.len()
        );
    }
    let mut total = 0;
    for (k, v) in &entries {
        if k.is_empty() || k.len() > MAX_METADATA_KEY {
            bail!("a metadata key is empty or over {MAX_METADATA_KEY} bytes");
        }
        if v.len() > MAX_METADATA_VALUE {
            bail!("metadata value {k:?} is over {MAX_METADATA_VALUE} bytes");
        }
        if format!("{k}{v}")
            .chars()
            .any(|r| (r < ' ' && r != '\t' && r != '\n') || r == '\u{7f}')
        {
            bail!("metadata holds a control character");
        }
        total += k.len() + v.len();
    }
    if total > MAX_METADATA_TOTAL {
        bail!("metadata is over {MAX_METADATA_TOTAL} bytes");
    }
    Ok(())
}

/// belai `ParsePrompt`: strict `key: value` lines, `name, description,
/// order, enabled`.
pub fn prompt(doc: &str) -> Result<MdItem> {
    let rest = doc
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow!("the document must open with a --- front matter line"))?;
    let mut m = MdItem::default();
    let mut seen: Vec<String> = Vec::new();
    let mut have_name = false;
    let mut rest = rest;
    let body;
    loop {
        let (line, tail, more) = match rest.split_once('\n') {
            Some((l, t)) => (l, t, true),
            None => (rest, "", false),
        };
        if line.trim_end_matches([' ', '\t']) == "---" {
            body = tail;
            break;
        }
        if !more {
            bail!("the front matter is not closed by a --- line");
        }
        rest = tail;
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let Some((k, v)) = l.split_once(':') else {
            bail!("malformed front matter line {l:?}");
        };
        let (k, v) = (k.trim(), v.trim());
        if !matches!(k, "name" | "description" | "order" | "enabled") {
            bail!("unknown front matter field {k:?}");
        }
        if seen.iter().any(|s| s == k) {
            bail!("front matter field {k:?} appears twice");
        }
        seen.push(k.to_string());
        let unq = {
            let s = v;
            if s.len() >= 2
                && ((s.starts_with('"') && s.ends_with('"'))
                    || (s.starts_with('\'') && s.ends_with('\'')))
            {
                &s[1..s.len() - 1]
            } else {
                s
            }
        };
        match k {
            "name" => {
                m.name = unq.to_string();
                have_name = true;
                if m.name.is_empty() {
                    bail!("name is required");
                }
                if !valid_name(&m.name) {
                    bail!("name {:?} is not valid", m.name);
                }
            }
            "description" => {
                m.description = unq.to_string();
                if m.description.len() > MAX_PROMPT_DESCRIPTION {
                    bail!(
                        "description is {} bytes; the most is {MAX_PROMPT_DESCRIPTION}",
                        m.description.len()
                    );
                }
            }
            "order" => {
                if unq.is_empty() || unq.len() > 3 || !unq.bytes().all(|b| b.is_ascii_digit()) {
                    bail!("order must be a whole number from 0 to 999");
                }
            }
            _ => {
                if !matches!(unq.to_ascii_lowercase().as_str(), "true" | "false") {
                    bail!("front matter \"enabled\" must be true or false");
                }
            }
        }
    }
    if !have_name {
        bail!("name is required");
    }
    if body.trim().is_empty() {
        bail!("the prompt has no text after the front matter");
    }
    m.body = body.trim_start_matches('\n').to_string();
    Ok(m)
}

/// A bundle path belai accepts: relative segments of
/// `[A-Za-z0-9_][A-Za-z0-9._-]{0,63}`, never `hooks.json`.
pub fn bundle_path_ok(p: &str) -> bool {
    !p.is_empty()
        && p != "hooks.json"
        && p.split('/').count() <= 4
        && p.split('/').all(|s| {
            let b = s.as_bytes();
            !b.is_empty()
                && b.len() <= 64
                && (b[0].is_ascii_alphanumeric() || b[0] == b'_')
                && b.iter()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        })
}

/// belai `validateHook` plus the bundle bounds. Returns name and description.
pub fn hook(doc: &[u8], files: &[BundleFile]) -> Result<(String, String)> {
    if doc.len() > MAX_JSON_BYTES {
        bail!(
            "the document is {} bytes; the most is {MAX_JSON_BYTES}",
            doc.len()
        );
    }
    let v = canonical::parse_object(doc, canonical::MAX_JSON_DEPTH)?;
    let m = v
        .as_obj()
        .ok_or_else(|| anyhow!("the document must be a JSON object"))?;
    only_keys(m, "hook", &["name", "description", "hooks"])?;
    let name = m
        .get("name")
        .and_then(Json::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("name is required"))?;
    if !valid_name(name) {
        bail!("name {name:?} is not valid");
    }
    let desc = opt_str(m, "description", "hook", MAX_HOOK_DESCRIPTION)?;
    let events = match m.get("hooks") {
        Some(Json::Obj(e)) if !e.is_empty() => e,
        Some(Json::Obj(_)) | None => bail!("hook.hooks must name at least one event"),
        Some(_) => bail!("hook.hooks must be an object"),
    };
    for (ev, groups) in events {
        if !HOOK_EVENTS.contains(&ev.as_str()) {
            bail!("hook.hooks.{ev} is not an event a hook can name");
        }
        let w = format!("hook.hooks.{ev}");
        let groups = match groups {
            Json::Arr(g) if !g.is_empty() => g,
            _ => bail!("{w} must be a list of at least one group"),
        };
        if groups.len() > MAX_HOOK_GROUPS {
            bail!(
                "{w} has {} groups; the most is {MAX_HOOK_GROUPS}",
                groups.len()
            );
        }
        for (i, g) in groups.iter().enumerate() {
            let gw = format!("{w}[{i}]");
            let g = g
                .as_obj()
                .ok_or_else(|| anyhow!("{gw} must be an object"))?;
            only_keys(g, &gw, &["matcher", "hooks"])?;
            let matcher = opt_str(g, "matcher", &gw, MAX_HOOK_MATCHER)?;
            if g.contains_key("matcher")
                && HOOK_NO_MATCHER.contains(&ev.as_str())
                && !matcher.is_empty()
            {
                bail!("{gw}.matcher: {ev} has nothing to match");
            }
            let hs = match g.get("hooks") {
                Some(Json::Arr(h)) if !h.is_empty() => h,
                Some(Json::Arr(_)) | None => bail!("{gw}.hooks must list at least one handler"),
                Some(_) => bail!("{gw}.hooks must be a list"),
            };
            if hs.len() > MAX_HOOK_HANDLERS {
                bail!(
                    "{gw}.hooks has {} entries; the most is {MAX_HOOK_HANDLERS}",
                    hs.len()
                );
            }
            for (j, h) in hs.iter().enumerate() {
                let hw = format!("{gw}.hooks[{j}]");
                let h = h
                    .as_obj()
                    .ok_or_else(|| anyhow!("{hw} must be an object"))?;
                only_keys(h, &hw, &["type", "command", "timeout"])?;
                if h.get("type").and_then(Json::as_str) != Some("command") {
                    bail!("{hw}.type must be command");
                }
                let cmd = h
                    .get("command")
                    .and_then(Json::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow!("{hw}.command is required"))?;
                if cmd.len() > MAX_HOOK_COMMAND {
                    bail!(
                        "{hw}.command is {} bytes; the most is {MAX_HOOK_COMMAND}",
                        cmd.len()
                    );
                }
                if has_control(cmd) {
                    bail!("{hw}.command holds a control character");
                }
                if cmd.trim() != cmd {
                    bail!("{hw}.command must not start or end with a space");
                }
                if let Some(t) = h.get("timeout") {
                    let Json::Num(n) = t else {
                        bail!("{hw}.timeout must be a whole number");
                    };
                    let n: i64 = n
                        .parse()
                        .map_err(|_| anyhow!("{hw}.timeout must be a whole number"))?;
                    if !(1..=MAX_HOOK_TIMEOUT).contains(&n) {
                        bail!("{hw}.timeout must be from 1 to {MAX_HOOK_TIMEOUT}");
                    }
                }
            }
        }
    }
    if files.len() > MAX_BUNDLE_FILES {
        bail!("a hook bundle carries at most {MAX_BUNDLE_FILES} files");
    }
    let mut total = 0;
    let mut seen = std::collections::BTreeSet::new();
    for f in files {
        if !bundle_path_ok(&f.path) {
            bail!("bundle path {:?} is not valid", f.path);
        }
        if !seen.insert(f.path.as_str()) {
            bail!("a file is listed twice");
        }
        if f.data.is_empty()
            || f.data.len() > MAX_BUNDLE_FILE_BYTES
            || std::str::from_utf8(&f.data).is_err()
            || f.data.contains(&0)
        {
            bail!("a file is not bounded text (1 byte to 256 KiB, UTF-8, no NUL)");
        }
        if canonical::hash(&f.data) != f.sha256 {
            bail!("a file does not match its hash");
        }
        if crate::scan_rules::holds_secret(&String::from_utf8_lossy(&f.data)) {
            bail!("script {} looks like it holds a secret", f.path);
        }
        total += f.data.len();
        if total > MAX_BUNDLE_BYTES {
            bail!("the files are over {MAX_BUNDLE_BYTES} bytes");
        }
    }
    Ok((name.to_string(), desc))
}

fn only_keys(
    m: &std::collections::BTreeMap<String, Json>,
    w: &str,
    allowed: &[&str],
) -> Result<()> {
    if let Some(k) = m.keys().find(|k| !allowed.contains(&k.as_str())) {
        bail!("{w}: unknown field {k:?}");
    }
    Ok(())
}

fn opt_str(
    m: &std::collections::BTreeMap<String, Json>,
    k: &str,
    w: &str,
    max: usize,
) -> Result<String> {
    match m.get(k) {
        None => Ok(String::new()),
        Some(Json::Str(s)) => {
            if s.len() > max {
                bail!("{w}.{k} is {} bytes; the most is {max}", s.len());
            }
            if has_control(s) {
                bail!("{w}.{k} holds a control character");
            }
            Ok(s.clone())
        }
        Some(_) => bail!("{w}.{k} must be a string"),
    }
}

/// belai `validateMCP` (strict `ValidateMCPServer`). Returns the name.
pub fn mcp(doc: &[u8]) -> Result<String> {
    if doc.len() > MAX_JSON_BYTES {
        bail!(
            "the document is {} bytes; the most is {MAX_JSON_BYTES}",
            doc.len()
        );
    }
    let v = canonical::parse_object(doc, canonical::MAX_JSON_DEPTH)?;
    let m = v
        .as_obj()
        .ok_or_else(|| anyhow!("the document must be a JSON object"))?;
    only_keys(
        m,
        "mcp",
        &[
            "name",
            "transport",
            "command",
            "args",
            "env",
            "url",
            "headers",
            "tools",
            "disabled",
            "sandbox",
            "timeout_ms",
            "secrets",
        ],
    )?;
    let name = m
        .get("name")
        .and_then(Json::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("name is required"))?;
    if !valid_mcp_name(name) {
        bail!("name {name:?} is not valid: letters, digits, _ or -, 32 characters at most");
    }
    if name.eq_ignore_ascii_case("clef") {
        bail!("\"clef\" is the built-in decision server and cannot be an item");
    }
    let s = |k: &str| -> Result<String> {
        match m.get(k) {
            None => Ok(String::new()),
            Some(Json::Str(s)) => Ok(s.clone()),
            Some(_) => bail!("the document does not match the schema: {k} must be a string"),
        }
    };
    let strs = |k: &str| -> Result<Vec<String>> {
        match m.get(k) {
            None => Ok(vec![]),
            Some(Json::Arr(a)) => a
                .iter()
                .map(|x| {
                    x.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| anyhow!("the document does not match the schema: {k}"))
                })
                .collect(),
            Some(_) => bail!("the document does not match the schema: {k} must be a list"),
        }
    };
    let map = |k: &str| -> Result<Vec<(String, String)>> {
        match m.get(k) {
            None => Ok(vec![]),
            Some(Json::Obj(o)) => o
                .iter()
                .map(|(a, b)| {
                    b.as_str()
                        .map(|b| (a.clone(), b.to_string()))
                        .ok_or_else(|| anyhow!("the document does not match the schema: {k}.{a}"))
                })
                .collect(),
            Some(_) => bail!("the document does not match the schema: {k} must be an object"),
        }
    };
    let transport = s("transport")?;
    let command = s("command")?;
    let url = s("url")?;
    let args = strs("args")?;
    let tools = strs("tools")?;
    let env = map("env")?;
    let headers = map("headers")?;
    let secrets = map("secrets")?;
    for b in ["disabled", "sandbox"] {
        if m.get(b).is_some_and(|v| v.as_bool().is_none()) {
            bail!("the document does not match the schema: {b} must be true or false");
        }
    }
    let sandbox = m.get("sandbox").and_then(Json::as_bool).unwrap_or(false);
    if let Some(t) = m.get("timeout_ms") {
        let n = t
            .as_i64()
            .ok_or_else(|| anyhow!("the document does not match the schema: timeout_ms"))?;
        if !(0..=600_000).contains(&n) {
            bail!("mcp server {name:?}: timeout_ms must be 0 to 600000");
        }
    }
    if tools.len() > 64 || args.len() > 64 || env.len() > 64 || headers.len() > 32 {
        bail!("mcp server {name:?}: too many tools, args, env entries or headers");
    }
    if tools.iter().any(|t| t.trim().is_empty()) {
        bail!("mcp server {name:?}: tools holds an empty name");
    }
    match transport.as_str() {
        "" | "stdio" => {
            if command.trim().is_empty() {
                bail!("mcp server {name:?}: a stdio server needs a command");
            }
            if !url.is_empty() || !headers.is_empty() {
                bail!("mcp server {name:?}: url and headers belong to an http server");
            }
        }
        "http" => {
            check_url(&url).map_err(|e| anyhow!("mcp server {name:?}: {e}"))?;
            if !command.is_empty() || !args.is_empty() || !env.is_empty() || sandbox {
                bail!(
                    "mcp server {name:?}: command, args, env and sandbox belong to a stdio server"
                );
            }
        }
        _ => bail!("mcp server {name:?}: transport must be stdio or http"),
    }
    let mut used = std::collections::BTreeSet::new();
    for (k, v) in &env {
        if !valid_ref_name(k) {
            bail!("mcp server {name:?}: env name {k:?} is not a legal variable name");
        }
        match parse_ref(v) {
            Some(("", _)) if looks_secret(k) => bail!(
                "mcp server {name:?}: env {k} looks like a secret: use env:NAME or cred:KEY, not a literal"
            ),
            Some(("vault" | "vulnetix", _)) => {
                bail!(
                    "mcp server {name:?}: env {k}: vault/vulnetix references are for http headers only"
                )
            }
            Some(("cred", r)) => {
                used.insert(r.to_string());
            }
            Some(_) => {}
            None => bail!("mcp server {name:?}: env {k}: {v:?} is not a legal reference"),
        }
        if has_control(&v.replace(['\t', '\n', '\r'], "")) || v.len() > 4096 {
            bail!("mcp.env.{k} holds a control character or is too long");
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for (k, v) in &headers {
        if k.is_empty() || k.contains([' ', '\t', '\r', '\n', ':']) {
            bail!("mcp server {name:?}: header name {k:?} is not valid");
        }
        if !seen.insert(k.to_ascii_lowercase()) {
            bail!("mcp.headers names {k:?} twice (header names are not case sensitive)");
        }
        match parse_ref(v) {
            Some(("", _)) if looks_secret(k) => bail!(
                "mcp server {name:?}: header {k} looks like a secret: use env:NAME, cred:KEY or vault:NAME, not a literal"
            ),
            Some(("vulnetix", _)) => {
                bail!("mcp server {name:?}: header {k}: vulnetix:cli is not imported")
            }
            Some(("cred", r)) => {
                used.insert(r.to_string());
            }
            Some(_) => {}
            None => bail!("mcp server {name:?}: header {k}: {v:?} is not a legal reference"),
        }
    }
    if secrets.len() > 16 {
        bail!("mcp server {name:?}: secrets binds too many credentials");
    }
    for (k, vn) in &secrets {
        if !valid_ref_name(k) || !valid_ref_name(vn) {
            bail!("mcp server {name:?}: secrets: names must be variable-shaped");
        }
        if !used.contains(k) {
            bail!("mcp server {name:?}: secrets binds {k:?} but no cred:{k} value uses it");
        }
    }
    Ok(name.to_string())
}

/// `(kind, target)`: kind "" for a literal; `None` for a bad target.
fn parse_ref(v: &str) -> Option<(&'static str, &str)> {
    if v == "vulnetix:cli" {
        return Some(("vulnetix", ""));
    }
    for (p, k) in [("env:", "env"), ("cred:", "cred"), ("vault:", "vault")] {
        if let Some(r) = v.strip_prefix(p) {
            return valid_ref_name(r).then_some((k, r));
        }
    }
    Some(("", v))
}

/// https (or loopback http), no credentials, fragment, backslash, space
/// or secret-looking query parameter.
pub fn check_url(raw: &str) -> Result<(), String> {
    let (scheme, rest) = raw
        .split_once("://")
        .ok_or("an http server needs an http(s) url")?;
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if host_port.is_empty() {
        return Err("an http server needs an http(s) url".into());
    }
    if host_port.contains('@') {
        return Err("the url must not hold credentials".into());
    }
    if raw.contains(['\\', ' ', '#']) {
        return Err("the url must not hold a fragment, a backslash or a space".into());
    }
    if let Some((_, q)) = rest.split_once('?') {
        for kv in q.split('&') {
            let k = kv.split('=').next().unwrap_or_default();
            if looks_secret(k) {
                return Err(format!(
                    "the url's query parameter {k:?} looks like a secret: send it in a header as a reference"
                ));
            }
        }
    }
    let host = host_port
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(host_port);
    match scheme {
        "https" => Ok(()),
        "http" if matches!(host, "localhost" | "127.0.0.1" | "[::1]") => Ok(()),
        "http" => Err("an http url must be https, or loopback".into()),
        _ => Err("an http server needs an http(s) url".into()),
    }
}

/// Validates canonical bytes of a library kind. Returns `(name, description)`.
pub fn library(kind: &str, doc: &[u8], files: &[BundleFile]) -> Result<(String, String)> {
    match kind {
        "skill" | "command" | "prompt" => {
            if doc.len() > MAX_MD_BYTES {
                bail!(
                    "the document is {} bytes; the most is {MAX_MD_BYTES}",
                    doc.len()
                );
            }
            let text = std::str::from_utf8(doc).map_err(|_| anyhow!("not valid UTF-8"))?;
            if canonical::markdown(text)? != text {
                bail!("the document is not in canonical form");
            }
            let m = if kind == "prompt" {
                prompt(text)?
            } else {
                skill_or_command(text, kind == "command")?
            };
            Ok((m.name, m.description))
        }
        "hook" => hook(doc, files),
        "mcp" => Ok((mcp(doc)?, String::new())),
        k => bail!("{k:?} is not a library kind search imports"),
    }
}

// ---- the draft every converter fills ----------------------------------------------

/// What a converter returns before hashing, licence and scan.
#[derive(Clone, Debug)]
pub struct Draft {
    pub target: Target,
    pub name: String,
    pub description: String,
    pub doc: Vec<u8>,
    pub files: Vec<BundleFile>,
    pub profile: Option<kls_store::vault::AgentProfile>,
    pub extra_skills: Vec<Converted>,
    pub notes: Vec<Note>,
    pub flags: Vec<Flag>,
    /// The front-matter `license` of the item (step 1 of detection).
    pub licence_field: Option<String>,
}

impl Draft {
    pub fn new(target: Target) -> Self {
        Self {
            target,
            name: String::new(),
            description: String::new(),
            doc: vec![],
            files: vec![],
            profile: None,
            extra_skills: vec![],
            notes: vec![],
            flags: vec![],
            licence_field: None,
        }
    }

    pub fn flag(&mut self, f: Flag) {
        if !self.flags.contains(&f) {
            self.flags.push(f);
        }
    }

    pub fn note(&mut self, n: Note) {
        if !self.notes.contains(&n) {
            self.notes.push(n);
        }
    }
}

/// Hashes, validates (a refusal becomes a `Refused` flag), detects the
/// licence and scans.
pub fn finish(d: Draft, src: &Fetched, opts: &Opts) -> Converted {
    let mut flags = d.flags;
    let mut notes = d.notes;
    let files_ref: Vec<(&str, &str)> = d
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.sha256.as_str()))
        .collect();
    let sha256 = match d.target {
        Target::Library { kind: "hook" } => canonical::bundle_hash(&d.doc, &files_ref),
        _ => canonical::hash(&d.doc),
    };
    if flags.iter().all(|f| !f.refused()) {
        let check = match d.target {
            Target::Library { kind } => library(kind, &d.doc, &d.files).map(|_| ()),
            Target::Agent => Ok(()),
        };
        if let Err(e) = check {
            notes.push(Note::warning("validate", e.to_string()));
            flags.push(Flag::Refused(format!("invalid: {e}")));
        }
    }
    let hint = d.licence_field.as_deref().or(opts.licence_hint.as_deref());
    let lic = licence::detect(src, hint);
    match lic.class {
        LicenceClass::Permissive => {}
        LicenceClass::Unknown => flags.push(Flag::LicenceUnknown),
        _ => flags.push(Flag::LicenceRestricted(lic.spdx.clone())),
    }
    flags.extend(src.flags.iter().cloned());
    let source_files = src
        .files
        .iter()
        .map(|(p, b)| SourceFile {
            path: p.clone(),
            sha256: canonical::hash(b),
            url: src.file_urls.get(p).cloned().unwrap_or_default(),
        })
        .collect();
    let mut c = Converted {
        target: d.target,
        name: d.name,
        description: d.description,
        doc: d.doc,
        sha256,
        files: d.files,
        profile: d.profile,
        extra_skills: d.extra_skills,
        notes,
        flags,
        licence: lic,
        upstream: src.upstream.clone(),
        source_files,
    };
    let scanned = crate::scan::scan(&c);
    c.flags = dedupe(scanned);
    c
}

pub fn dedupe(flags: Vec<Flag>) -> Vec<Flag> {
    let mut out: Vec<Flag> = Vec::new();
    for f in flags {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// The converter for a type and ecosystem.
pub fn convert_any(
    item_type: ItemType,
    eco: Ecosystem,
    src: &Fetched,
    opts: &Opts,
) -> Result<Converted> {
    if src.files.is_empty() {
        bail!("nothing was fetched for this item");
    }
    let d = match item_type {
        ItemType::Skill => super::skill::convert(eco, src, opts)?,
        ItemType::Command => super::command::convert(eco, src, opts)?,
        ItemType::Agent => super::agent::convert(eco, src, opts)?,
        ItemType::Hook => super::hook::convert(eco, src, opts)?,
        ItemType::Mcp => super::mcp::convert(eco, src, opts)?,
        ItemType::Prompt => super::prompt::convert(eco, src, opts)?,
    };
    Ok(finish(d, src, opts))
}

/// Re-validates an edited canonical document (the editable Converted tab):
/// canonicalises it, recomputes the hash, re-scans. Upstream, source files,
/// licence and notes are kept. An agent is re-read from its profile
/// Markdown.
pub fn from_edited(c: &Converted, edited: &str) -> Result<Converted> {
    let mut out = c.clone();
    out.flags
        .retain(|f| !matches!(f, Flag::Refused(r) if r.starts_with("invalid: ")));
    match c.target {
        Target::Library { kind } => {
            let doc = match kind {
                "hook" | "mcp" => canonical::json(edited.as_bytes(), canonical::MAX_JSON_DEPTH)?,
                _ => canonical::markdown(edited)?.into_bytes(),
            };
            let (name, desc) = library(kind, &doc, &c.files)?;
            out.name = name;
            out.description = desc;
            let files: Vec<(&str, &str)> = c
                .files
                .iter()
                .map(|f| (f.path.as_str(), f.sha256.as_str()))
                .collect();
            out.sha256 = if kind == "hook" {
                canonical::bundle_hash(&doc, &files)
            } else {
                canonical::hash(&doc)
            };
            out.doc = doc;
        }
        Target::Agent => {
            let doc = canonical::markdown(edited)?;
            let p = super::agent::parse_profile_md(&doc)?;
            out.name = p.name.clone();
            out.description = p.description.clone();
            out.sha256 = canonical::hash(doc.as_bytes());
            out.doc = doc.into_bytes();
            out.profile = Some(p);
        }
    }
    // Content flags are recomputed; converter, licence and upstream flags stay.
    out.flags.retain(|f| {
        !matches!(
            f,
            Flag::ShellExpansion
                | Flag::FileInclude
                | Flag::PipeToShell
                | Flag::RemoteUrl(_)
                | Flag::SecretLike
                | Flag::PromptInjection
                | Flag::LongBase64
        )
    });
    out.flags = dedupe(crate::scan::scan(&out));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hookdoc(extra: &str) -> Vec<u8> {
        canonical::json(
            format!(r#"{{"name":"pix","hooks":{{"PreToolUse":[{{"matcher":"Bash|Edit","hooks":[{{"type":"command","command":"vulnetix agent hook","timeout":30}}]}}]}}{extra}}}"#)
                .as_bytes(),
            8,
        )
        .unwrap()
    }

    #[test]
    fn hook_rules() {
        assert_eq!(hook(&hookdoc(""), &[]).unwrap().0, "pix");
        assert!(
            hook(&hookdoc(r#","run":"x""#), &[])
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );
        let bad = |s: &str| {
            hook(&canonical::json(s.as_bytes(), 8).unwrap(), &[])
                .unwrap_err()
                .to_string()
        };
        assert!(bad(r#"{"name":"p","hooks":{}}"#).contains("at least one event"));
        assert!(
            bad(
                r#"{"name":"p","hooks":{"OnSave":[{"hooks":[{"type":"command","command":"a"}]}]}}"#
            )
            .contains("not an event")
        );
        assert!(
            bad(r#"{"name":"p","hooks":{"Stop":[{"hooks":[{"type":"prompt","command":"a"}]}]}}"#)
                .contains("must be command")
        );
        assert!(bad(r#"{"name":"p","hooks":{"Stop":[{"hooks":[{"type":"command","command":"a","timeout":601}]}]}}"#).contains("from 1 to 600"));
        assert!(bad(r#"{"name":"p","hooks":{"Stop":[{"hooks":[{"type":"command","command":"a","timeout":1.5}]}]}}"#).contains("whole number"));
        assert!(bad(r#"{"name":"p","hooks":{"UserPromptSubmit":[{"matcher":"x","hooks":[{"type":"command","command":"a"}]}]}}"#).contains("nothing to match"));
        assert!(
            bad(r#"{"name":"P","hooks":{"Stop":[{"hooks":[{"type":"command","command":"a"}]}]}}"#)
                .contains("not valid")
        );
        assert!(
            bad(r#"{"name":"p","hooks":{"Stop":[{"hooks":[{"type":"command","command":"a "}]}]}}"#)
                .contains("space")
        );
        let f = BundleFile {
            path: "../x.sh".into(),
            data: b"echo".to_vec(),
            sha256: canonical::hash(b"echo"),
        };
        assert!(hook(&hookdoc(""), &[f]).is_err());
    }

    #[test]
    fn mcp_rules() {
        let ok = |s: &str| mcp(&canonical::json(s.as_bytes(), 8).unwrap());
        assert_eq!(
            ok(r#"{"name":"github","command":"npx","args":["-y","x@1"]}"#).unwrap(),
            "github"
        );
        assert!(ok(r#"{"name":"g","command":"npx","env":{"API_TOKEN":"abc"}}"#).is_err());
        assert!(ok(r#"{"name":"g","command":"npx","env":{"API_TOKEN":"cred:API_TOKEN"}}"#).is_ok());
        assert!(ok(r#"{"name":"g","transport":"http","url":"http://example.com/mcp"}"#).is_err());
        assert!(ok(r#"{"name":"g","transport":"http","url":"https://example.com/mcp"}"#).is_ok());
        assert!(ok(r#"{"name":"clef","command":"x"}"#).is_err());
        assert!(ok(r#"{"name":"g","command":"x","run":1}"#).is_err());
    }

    #[test]
    fn markdown_rules() {
        let s =
            "---\nname: pdf\ndescription: Reads PDFs\nallowed-tools: Read Grep\n---\n\nSteps.\n";
        assert_eq!(library("skill", s.as_bytes(), &[]).unwrap().0, "pdf");
        assert!(library("skill", b"---\nname: pdf\n---\n\nx\n", &[]).is_err());
        assert!(
            library(
                "skill",
                b"---\nname: pdf\ndescription: d\nmodel: x\n---\n\nx\n",
                &[]
            )
            .is_err()
        );
        assert!(
            library(
                "skill",
                b"---\nname: belai-x\ndescription: d\n---\n\nx\n",
                &[]
            )
            .is_err()
        );
        assert!(library("skill", b"---\nname: pdf\ndescription: d\n---\n", &[]).is_err());
        let c = "---\nname: review\ndescription: Reviews\nargument-hint: [pr]\n---\n\nReview $ARGUMENTS\n";
        assert!(library("command", c.as_bytes(), &[]).is_ok());
        assert!(library("skill", c.as_bytes(), &[]).is_err());
        let p = "---\nname: rules\ndescription: House rules\norder: 10\nenabled: false\n---\n\nBe kind.\n";
        assert!(library("prompt", p.as_bytes(), &[]).is_ok());
        assert!(library("prompt", b"---\nname: rules\norder: 1000\n---\n\nx\n", &[]).is_err());
        assert!(library("prompt", b"---\nname: rules\nglobs: x\n---\n\nx\n", &[]).is_err());
    }
}
