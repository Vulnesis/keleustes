//! agent → `AgentProfile` / belai profile Markdown (§3.3): single mode,
//! supervised, tools only from the fixed table (read-only fallback).
//!
//! Never carried: `permissionMode`, `hooks`, `mcpServers`, endpoints, env
//! names, and every A2A or peer setting (`a2a`, `peers`, `allow_agents`,
//! `crew`, `subagents`, `Task(<agent>)` bindings, worker blocks,
//! schedules): WORKSTREAMS rule 4.

use super::canonical::{self, Json, one_line};
use super::frontmatter::{self, Doc, list, scalar};
use super::validate::Draft;
use super::{Note, NoteKind, Opts, Target, primary, stem};
use crate::scan::Flag;
use crate::source::{Ecosystem, Fetched};
use anyhow::{Result, anyhow, bail};
use kls_store::vault::AgentProfile;
use std::collections::BTreeMap;

/// Keys that would give an imported agent an A2A or peer path.
pub const A2A_KEYS: [&str; 16] = [
    "a2a",
    "peers",
    "peer",
    "allow_agents",
    "allowagents",
    "allowed_agents",
    "crew",
    "crews",
    "subagents",
    "sub_agents",
    "agents",
    "worker",
    "workers",
    "schedule",
    "cron",
    "kanban",
];

const NEVER: [&str; 8] = [
    "permissionmode",
    "permission_mode",
    "hooks",
    "mcpservers",
    "mcp_servers",
    "mcp",
    "env",
    "endpoint",
];

const PROMPT_MAX: usize = 32 << 10;

fn is_a2a(k: &str) -> bool {
    let n = k.to_ascii_lowercase().replace('-', "_");
    A2A_KEYS.contains(&n.as_str()) || n.starts_with("a2a")
}

fn is_never(k: &str) -> bool {
    let n = k.to_ascii_lowercase().replace('-', "_");
    NEVER.contains(&n.as_str()) || n.contains("endpoint") || n.contains("base_url")
}

/// A finished profile: the profile, its Markdown, notes and flags.
type Built = (AgentProfile, Vec<u8>, Vec<Note>, Vec<Flag>);

/// What every dialect feeds the profile builder.
#[derive(Default)]
struct Builder {
    name: String,
    display: String,
    description: String,
    prompt: String,
    tools: Vec<String>,
    tools_set: bool,
    provider: String,
    alias: String,
    meta: BTreeMap<String, String>,
    notes: Vec<Note>,
    flags: Vec<Flag>,
}

impl Builder {
    fn tools(&mut self, field: &str, names: &[String]) {
        self.tools_set = true;
        let (kept, dropped) = super::tools::map_all(names);
        for t in kept {
            if !self.tools.contains(&t) {
                self.tools.push(t);
            }
        }
        if !dropped.is_empty() {
            self.notes.push(Note::new(
                NoteKind::Dropped,
                field,
                format!("tools with no match: {}", dropped.join(", ")),
            ));
            if dropped.iter().any(|t| t.contains('(')) {
                self.notes.push(Note::warning(
                    field,
                    "Task-to-named-agent binding dropped: no A2A path for imported agents",
                ));
            }
        }
    }

    fn model(&mut self, field: &str, m: &str) {
        let m = m.trim();
        match m.to_ascii_lowercase().as_str() {
            "" => {}
            "inherit" => self
                .notes
                .push(Note::mapped(field, "inherit: routing picks")),
            "sonnet" | "opus" | "haiku" => {
                self.provider = "anthropic".into();
                self.alias = m.to_ascii_lowercase();
                self.notes.push(Note::mapped(
                    field,
                    format!(
                        "provider anthropic; alias {} kept, routing picks the model",
                        self.alias
                    ),
                ));
            }
            _ => {
                self.meta.insert("model".into(), one_line(m));
                self.notes.push(Note::metadata(
                    field,
                    "kept in metadata; routing picks the model",
                ));
            }
        }
    }

    /// Drops what an imported agent must never carry; true when handled.
    fn guard_key(&mut self, k: &str) -> bool {
        if is_a2a(k) {
            self.notes.push(Note::warning(
                k,
                "A2A / peer setting dropped: imported agents are single-mode and supervised",
            ));
            return true;
        }
        if is_never(k) {
            self.notes.push(Note::warning(
                k,
                "not carried (permissions, hooks, MCP, env, endpoints)",
            ));
            return true;
        }
        false
    }

    fn finish(mut self, eco: Ecosystem, opts: &Opts) -> Result<Built> {
        let src_name = opts.rename.clone().unwrap_or(self.name.clone());
        let mut name = canonical::slug(&src_name);
        if name.is_empty() {
            name = "imported-agent".into();
        }
        if name != src_name {
            if self.display.is_empty() {
                self.display = one_line(&src_name);
            }
            self.notes.push(Note::mapped(
                "name",
                format!("{} → {name}", one_line(&src_name)),
            ));
        }
        let (prompt, hidden) = canonical::clean_text(self.prompt.trim());
        if hidden {
            self.flags.push(Flag::HiddenText);
        }
        let mut prompt = prompt.trim().to_string();
        if prompt.len() > PROMPT_MAX {
            prompt = canonical::clip(&prompt, PROMPT_MAX).trim().to_string();
            self.notes
                .push(Note::warning("system_prompt", "cut at 32 KiB"));
        }
        if prompt.is_empty() {
            self.flags
                .push(Flag::Refused("the definition holds no instructions".into()));
        }
        let mut description = canonical::clip(&one_line(&self.description), 300).to_string();
        if description.is_empty() {
            description = format!("Imported {} agent", eco.label());
        }
        if self.tools.is_empty() {
            self.tools = super::tools::READ_ONLY
                .iter()
                .map(|s| s.to_string())
                .collect();
            self.notes.push(Note::warning(
                "tools",
                if self.tools_set {
                    "no tool maps to belai's: read-only set (Read, Grep, Glob)"
                } else {
                    "no tools named: read-only set (Read, Grep, Glob)"
                },
            ));
        }
        let mutating: Vec<&String> = self
            .tools
            .iter()
            .filter(|t| super::tools::MUTATING.contains(&t.as_str()))
            .collect();
        if !mutating.is_empty() {
            self.notes.push(Note::warning(
                "tools",
                format!(
                    "changes things: {}",
                    mutating
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        self.meta
            .insert("source.format".into(), eco.as_str().into());
        let display = canonical::clip(&one_line(&self.display), 80).to_string();
        let doc = profile_md(
            &name,
            &display,
            &description,
            &self.tools,
            &self.provider,
            &self.meta,
            &prompt,
        );
        let mut meta = serde_json::Map::new();
        meta.insert(
            "source".into(),
            serde_json::Value::String(eco.as_str().into()),
        );
        if !self.alias.is_empty() {
            meta.insert(
                "model_alias".into(),
                serde_json::Value::String(self.alias.clone()),
            );
        }
        for (k, v) in &self.meta {
            if k != "source.format" {
                meta.insert(k.clone(), serde_json::Value::String(v.clone()));
            }
        }
        let profile = AgentProfile {
            name,
            display_name: display,
            description,
            prompt,
            provider: self.provider.clone(),
            model: String::new(),
            tools: self.tools.clone(),
            meta_json: serde_json::Value::Object(meta).to_string(),
            ..Default::default()
        };
        Ok((
            profile,
            canonical::markdown(&doc)?.into_bytes(),
            self.notes,
            self.flags,
        ))
    }
}

/// belai `MarshalMarkdown` of a single-mode, supervised profile: each key
/// as Go `json.Marshal` writes it (HTML escaped), then the prompt.
pub fn profile_md(
    name: &str,
    display: &str,
    description: &str,
    tools: &[String],
    provider: &str,
    meta: &BTreeMap<String, String>,
    prompt: &str,
) -> String {
    let js = |v: &Json| {
        let mut s = String::new();
        canonical::write_json(v, true, &mut s);
        s
    };
    let mut b = String::from("---\n");
    b.push_str(&format!("name: {}\n", js(&Json::s(name))));
    if !display.is_empty() {
        b.push_str(&format!("display_name: {}\n", js(&Json::s(display))));
    }
    b.push_str(&format!("description: {}\n", js(&Json::s(description))));
    if !tools.is_empty() {
        b.push_str(&format!(
            "tools: {}\n",
            js(&Json::Arr(
                tools.iter().map(|t| Json::s(t.as_str())).collect()
            ))
        ));
    }
    b.push_str("mode: \"single\"\n");
    b.push_str("autonomy: \"supervised\"\n");
    if !provider.is_empty() {
        b.push_str(&format!("provider: {}\n", js(&Json::s(provider))));
    }
    if !meta.is_empty() {
        let m = Json::Obj(
            meta.iter()
                .map(|(k, v)| (k.clone(), Json::s(v.as_str())))
                .collect(),
        );
        b.push_str(&format!("metadata: {}\n", js(&m)));
    }
    b.push_str("---\n");
    b.push_str(prompt.trim());
    b.push('\n');
    b
}

/// Reads a profile Markdown back (the edited Converted tab). Only the keys
/// [`profile_md`] writes are accepted: an edit cannot add A2A, schedules,
/// workers or other modes.
pub fn parse_profile_md(doc: &str) -> Result<AgentProfile> {
    let (front, body) = frontmatter::split(doc);
    let front = front.ok_or_else(|| anyhow!("a profile starts with a --- front-matter line"))?;
    let mut p = AgentProfile::default();
    let mut meta = serde_json::Map::new();
    for line in front.lines().filter(|l| !l.trim().is_empty()) {
        let (k, v) = line
            .split_once(':')
            .ok_or_else(|| anyhow!("malformed front matter line {line:?}"))?;
        let v: serde_json::Value =
            serde_json::from_str(v.trim()).map_err(|_| anyhow!("{k}: the value is not JSON"))?;
        let s = || {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow!("{k} must be a string"))
        };
        match k.trim() {
            "name" => p.name = s()?,
            "display_name" => p.display_name = s()?,
            "description" => p.description = s()?,
            "provider" => p.provider = s()?,
            "model" => p.model = s()?,
            "tools" => {
                let names: Vec<String> = v
                    .as_array()
                    .ok_or_else(|| anyhow!("tools must be a list"))?
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect();
                let (kept, dropped) = super::tools::map_all(&names);
                if !dropped.is_empty() {
                    bail!("tools outside the fixed table: {}", dropped.join(", "));
                }
                p.tools = kept;
            }
            "mode" if v.as_str() == Some("single") => {}
            "autonomy" if v.as_str() == Some("supervised") => {}
            "mode" | "autonomy" => bail!("imported agents are single-mode and supervised"),
            "metadata" => {
                if let Some(o) = v.as_object() {
                    meta = o.clone();
                }
            }
            other => bail!("{other:?} cannot be set on an imported agent"),
        }
    }
    if !super::validate::valid_name(&p.name) {
        bail!("name {:?} is not valid", p.name);
    }
    p.prompt = body.trim().to_string();
    if p.prompt.is_empty() {
        bail!("the profile has no instructions");
    }
    if p.tools.is_empty() {
        p.tools = super::tools::READ_ONLY
            .iter()
            .map(|s| s.to_string())
            .collect();
    }
    meta.remove("source.format");
    p.meta_json = serde_json::Value::Object(meta).to_string();
    Ok(p)
}

fn md_agent(b: &mut Builder, eco: Ecosystem, doc: &Doc, file_stem: &str) {
    for (k, v) in &doc.fields {
        if b.guard_key(k) {
            continue;
        }
        match k.as_str() {
            "name" if eco != Ecosystem::OpenCode => {
                if let Some(n) = scalar(v) {
                    if eco == Ecosystem::Copilot {
                        b.display = n.clone();
                    }
                    b.name = n;
                }
            }
            "description" => b.description = scalar(v).unwrap_or_default(),
            "tools" => match v {
                serde_norway::Value::Mapping(m) => {
                    // opencode: {write: false, bash: true}.
                    let on: Vec<String> = m
                        .iter()
                        .filter(|(_, x)| matches!(x, serde_norway::Value::Bool(true)))
                        .filter_map(|(k, _)| scalar(k))
                        .collect();
                    b.tools(k, &on);
                }
                v => b.tools(k, &list(v)),
            },
            "model" => {
                let m = scalar(v).unwrap_or_default();
                b.model(k, &m);
            }
            "color" => b.notes.push(Note::dropped("color")),
            "permission" | "temperature" | "memory" => b.notes.push(Note::dropped(k)),
            "mode" if eco == Ecosystem::OpenCode => {
                if let Some(m) = scalar(v) {
                    b.meta.insert("opencode.mode".into(), m);
                }
            }
            "skills" => b.notes.push(Note::dropped("skills")),
            other => b.notes.push(Note::dropped(other)),
        }
    }
    if b.name.is_empty() {
        b.name = file_stem.to_string();
    }
    b.prompt = doc.body.clone();
}

// ---- Hermes and OpenClaw (Claws) profiles -------------------------------------
//
// Ports of belai `agentimport/hermes.go` and `claws.go`. What belai keeps in
// metadata for its own use (subagents, memory search, packages) is named in
// notes here instead: an imported agent carries no A2A path, schedule,
// MCP server, memory or package (WORKSTREAMS rule 4).

/// Caps for an exported profile archive (belai `agentimport` limits).
pub const ARCHIVE_LIMITS: crate::git::Limits = crate::git::Limits {
    files: 256,
    file_bytes: 1024 * 1024,
    kept_bytes: 8 * 1024 * 1024,
    expanded_bytes: 64 * 1024 * 1024,
    depth: 6,
};

/// At most this many names in one note.
const NOTE_NAMES: usize = 12;

fn capped(mut v: Vec<String>, n: usize) -> String {
    let more = v.len().saturating_sub(n);
    v.truncate(n);
    let mut s = v.join(", ");
    if more > 0 {
        s.push_str(&format!(" and {more} more"));
    }
    s
}

/// The shallowest path whose base name is `name` (case-insensitive).
fn find_file<'a>(src: &'a Fetched, name: &str) -> Option<&'a str> {
    src.files
        .keys()
        .filter(|k| {
            k.rsplit('/')
                .next()
                .is_some_and(|b| b.eq_ignore_ascii_case(name))
        })
        .min_by_key(|k| (k.matches('/').count(), k.len()))
        .map(|k| k.as_str())
}

fn yaml_map(src: &Fetched, path: &str) -> Result<serde_norway::Mapping> {
    let (t, _) = super::text(src, path)?;
    match serde_norway::from_str::<serde_norway::Value>(&t) {
        Ok(serde_norway::Value::Mapping(m)) => Ok(m),
        Ok(serde_norway::Value::Null) => Ok(Default::default()),
        Ok(_) => bail!("{path}: not a mapping"),
        Err(e) => bail!("{path}: {e}"),
    }
}

fn get<'a>(m: &'a serde_norway::Mapping, path: &[&str]) -> Option<&'a serde_norway::Value> {
    let mut v = m.get(path.first()?)?;
    for k in &path[1..] {
        v = v.as_mapping()?.get(k)?;
    }
    Some(v)
}

fn get_str(m: &serde_norway::Mapping, path: &[&str]) -> String {
    get(m, path).and_then(scalar).unwrap_or_default()
}

fn keys_of(v: &serde_norway::Value) -> Vec<String> {
    v.as_mapping()
        .map(|m| m.keys().filter_map(scalar).collect())
        .unwrap_or_default()
}

/// An exported profile (`.tar.gz` / `.tgz`) in place of the profile dir:
/// read with [`crate::git::extract_archive`] when no `SOUL.md` / `CLAW.md`
/// was fetched. A path escape refuses the import; skipped entries are noted.
fn profile_archive(eco: Ecosystem, src: &Fetched, b: &mut Builder) -> Option<Fetched> {
    if !matches!(eco, Ecosystem::Hermes | Ecosystem::OpenClaw)
        || find_file(src, "SOUL.md").is_some()
        || find_file(src, "CLAW.md").is_some()
    {
        return None;
    }
    let (path, gz) = src
        .files
        .iter()
        .find(|(k, _)| k.ends_with(".tar.gz") || k.ends_with(".tgz"))?;
    match crate::git::extract_archive(gz, ARCHIVE_LIMITS) {
        Ok(t) => {
            b.notes
                .push(Note::mapped(path, format!("{} files read", t.files.len())));
            if !t.skipped.is_empty() {
                b.notes.push(Note::new(
                    NoteKind::Dropped,
                    "files",
                    format!(
                        "{} file(s) not read: links, special and credential files, files over 1 MiB",
                        t.skipped.len()
                    ),
                ));
            }
            if t.truncated {
                b.notes.push(Note::warning(
                    path,
                    "archive cut at its caps (256 files, 8 MiB)",
                ));
            }
            Some(Fetched {
                files: t.files,
                name_hint: src.name_hint.clone(),
                upstream: src.upstream.clone(),
                licence_files: src.licence_files.clone(),
                licence_hint: src.licence_hint.clone(),
                file_urls: src.file_urls.clone(),
                ..Default::default()
            })
        }
        Err(e) => {
            b.flags.push(Flag::Refused(format!("{path}: {e}")));
            None
        }
    }
}

/// Credential stores (`auth.json`, `.env`, keys) are dropped before any
/// converter reads the files, so they reach no profile, note, node or log.
fn without_credentials(src: &Fetched, b: &mut Builder) -> Option<Fetched> {
    let is_cred = |k: &str| crate::git::credential_name(k.rsplit('/').next().unwrap_or(k));
    let n = src.files.keys().filter(|k| is_cred(k)).count();
    if n == 0 {
        return None;
    }
    b.notes.push(Note::new(
        NoteKind::Dropped,
        "files",
        format!("{n} credential file(s) never read"),
    ));
    let mut out = src.clone();
    out.files.retain(|k, _| !is_cred(k));
    out.file_urls.retain(|k, _| !is_cred(k));
    if out.primary.as_deref().is_some_and(is_cred) {
        out.primary = None;
    }
    Some(out)
}

/// A file's text as the prompt; hidden characters flagged.
fn read_prompt(b: &mut Builder, src: &Fetched, path: &str) -> Result<String> {
    let (t, hidden) = super::text(src, path)?;
    if hidden {
        b.flags.push(Flag::HiddenText);
    }
    Ok(t)
}

/// Hermes: `SOUL.md` is the prompt, `profile.yaml` the description,
/// `config.yaml` the model, `distribution.yaml` the version, `skills/` the
/// bundled skills. `mcp.json`, `cron/`, `memories/` and credential stores
/// are never taken (named in notes only).
fn hermes(b: &mut Builder, src: &Fetched) -> Result<()> {
    if let Some(p) = find_file(src, "profile.yaml") {
        for (k, v) in &yaml_map(src, p)? {
            let Some(k) = scalar(k) else { continue };
            if b.guard_key(&k) {
                continue;
            }
            match k.as_str() {
                "name" => b.name = scalar(v).unwrap_or_default(),
                "description" => b.description = scalar(v).unwrap_or_default(),
                "display_name" => b.display = scalar(v).unwrap_or_default(),
                "tools" | "toolsets" => b.tools("tools", &list(v)),
                "model" => {
                    let m = scalar(v).unwrap_or_default();
                    b.model("model", &m)
                }
                _ => b.notes.push(Note::dropped(&k)),
            }
        }
    }
    if let Some(s) = find_file(src, "SOUL.md") {
        let t = read_prompt(b, src, s)?;
        b.prompt = frontmatter::parse(&t).body;
        b.notes
            .push(Note::mapped("SOUL.md", "became the system prompt"));
    }
    if b.name.is_empty() {
        b.name = src.name_hint.clone();
    }
    if let Some(p) = find_file(src, "distribution.yaml")
        && let Ok(d) = yaml_map(src, p)
    {
        let v = get_str(&d, &["version"]);
        if !v.is_empty() {
            b.meta.insert("hermes.version".into(), one_line(&v));
            b.notes.push(Note::metadata(
                "distribution.yaml",
                "kept in metadata as hermes.version",
            ));
        }
    }
    if let Some(p) = find_file(src, "config.yaml") {
        let cfg = yaml_map(src, p)?;
        // `model: x`, or `model: {default|name|model: x, provider: y}`.
        let (model, provider) = match get(&cfg, &["model"]) {
            Some(serde_norway::Value::Mapping(m)) => {
                let pick = ["default", "name", "model"]
                    .iter()
                    .map(|k| get_str(m, &[k]))
                    .find(|s| !s.is_empty())
                    .unwrap_or_default();
                (pick, get_str(m, &["provider"]))
            }
            Some(v) => (scalar(v).unwrap_or_default(), get_str(&cfg, &["provider"])),
            None => (String::new(), String::new()),
        };
        if !model.is_empty() {
            let joined = if provider.is_empty() || model.contains('/') {
                model
            } else {
                format!("{provider}/{model}")
            };
            b.model("config.yaml model", &joined);
        }
        let keys: Vec<String> = cfg
            .keys()
            .filter_map(scalar)
            .filter(|k| k != "model" && k != "provider")
            .collect();
        if !keys.is_empty() {
            b.notes.push(Note::new(
                NoteKind::Dropped,
                "config.yaml",
                format!(
                    "settings with no profile field: {}",
                    capped(keys, NOTE_NAMES)
                ),
            ));
        }
    }
    if find_file(src, "mcp.json").is_some() {
        b.notes.push(Note::new(
            NoteKind::Dropped,
            "mcp.json",
            "MCP servers are the person's own settings, never taken from a definition",
        ));
    }
    let count = |dir: &str| {
        src.files
            .keys()
            .filter(|k| k.starts_with(dir) || k.contains(&format!("/{dir}")))
            .count()
    };
    let (cron, mem) = (count("cron/"), count("memories/"));
    if cron > 0 {
        b.notes.push(Note::warning(
            "cron/",
            format!("{cron} scheduled job file(s) not imported"),
        ));
    }
    if mem > 0 {
        b.notes.push(Note::new(
            NoteKind::Dropped,
            "memories/",
            format!("{mem} memory file(s) are the person's own, not imported"),
        ));
    }

    Ok(())
}

/// OpenClaw / Claws: `CLAW.md` front matter (`schemaVersion`, `agent`,
/// `workspace`, `packages`, `mcpServers`, `cronJobs`) and body; `SOUL.md`
/// (the workspace bootstrap file, else the root one) when the body is empty.
fn claws(b: &mut Builder, src: &Fetched) -> Result<()> {
    let Some(claw) = find_file(src, "CLAW.md").map(str::to_string) else {
        // A SOUL.md alone still gives the instructions.
        if let Some(s) = find_file(src, "SOUL.md") {
            b.prompt = read_prompt(b, src, s)?;
            b.notes
                .push(Note::mapped("SOUL.md", "used as the instructions"));
        }
        if b.name.is_empty() {
            b.name = src.name_hint.clone();
        }

        return Ok(());
    };
    let (t, hidden) = super::text(src, &claw)?;
    if hidden {
        b.flags.push(Flag::HiddenText);
    }
    let doc = frontmatter::parse(&t);
    let mut unknown = Vec::new();
    let mut bootstrap = String::new();
    for (k, v) in &doc.fields {
        match k.as_str() {
            "schemaVersion" => {
                let s = scalar(v).unwrap_or_default();
                if !s.is_empty() && s != "1" {
                    b.notes.push(Note::warning(
                        k,
                        format!(
                            "schema version {} is not 1; unknown fields are named below",
                            one_line(&s)
                        ),
                    ));
                }
            }
            "agent" => {
                let Some(m) = v.as_mapping() else { continue };
                for (ak, av) in m {
                    let Some(ak) = scalar(ak) else { continue };
                    let field = format!("agent.{ak}");
                    match ak.as_str() {
                        "id" => b.name = scalar(av).unwrap_or_default(),
                        "name" => b.display = scalar(av).unwrap_or_default(),
                        "description" => b.description = scalar(av).unwrap_or_default(),
                        "model" => match av {
                            serde_norway::Value::Mapping(mm) => {
                                let p = get_str(mm, &["primary"]);
                                b.model("agent.model.primary", &p);
                                let fb = get(mm, &["fallbacks"]).map(list).unwrap_or_default();
                                if !fb.is_empty() {
                                    b.meta.insert(
                                        "claws.model.fallbacks".into(),
                                        one_line(&fb.join(", ")),
                                    );
                                    b.notes.push(Note::metadata(
                                        "agent.model.fallbacks",
                                        "kept in metadata as claws.model.fallbacks",
                                    ));
                                }
                            }
                            other => {
                                let p = scalar(other).unwrap_or_default();
                                b.model("agent.model", &p);
                            }
                        },
                        "tools" => match av {
                            serde_norway::Value::Mapping(tm) => {
                                let mut allow = get(tm, &["allow"]).map(list).unwrap_or_default();
                                allow.extend(get(tm, &["alsoAllow"]).map(list).unwrap_or_default());
                                let deny = get(tm, &["deny"]).map(list).unwrap_or_default();
                                let (denied, _) = super::tools::map_all(&deny);
                                b.tools("agent.tools.allow", &allow);
                                b.tools.retain(|t| !denied.contains(t));
                                if !deny.is_empty() {
                                    b.notes.push(Note::mapped(
                                        "agent.tools.deny",
                                        format!("removed: {}", capped(deny, NOTE_NAMES)),
                                    ));
                                }
                                if get(tm, &["fs"]).is_some() {
                                    b.notes.push(Note::warning(
                                        "agent.tools.fs",
                                        "file-system limits upstream; not enforced",
                                    ));
                                }
                            }
                            other => b.tools(&field, &list(other)),
                        },
                        "memory" => b.notes.push(Note::new(
                            NoteKind::Dropped,
                            &field,
                            "memory settings are the person's own, not imported",
                        )),
                        o if b.guard_key(o) => {}
                        _ => b.notes.push(Note::dropped(&field)),
                    }
                }
            }
            "workspace" => {
                if let Some(m) = v.as_mapping() {
                    bootstrap = get_str(m, &["bootstrapFiles", "SOUL.md", "source"]);
                }
                b.notes.push(Note::new(
                    NoteKind::Dropped,
                    k,
                    "workspace files are not copied",
                ));
            }
            "packages" => {
                let refs: Vec<String> = match v {
                    serde_norway::Value::Sequence(s) => s
                        .iter()
                        .filter_map(|x| x.as_mapping())
                        .map(|m| {
                            format!(
                                "{}:{}@{}",
                                get_str(m, &["kind"]),
                                get_str(m, &["ref"]),
                                get_str(m, &["version"])
                            )
                        })
                        .map(|s| one_line(&s))
                        .collect(),
                    _ => vec![],
                };
                b.notes.push(Note::new(
                    NoteKind::Dropped,
                    k,
                    format!(
                        "no package installed from a definition: {}",
                        capped(refs, NOTE_NAMES)
                    ),
                ));
            }
            // Names only: an MCP entry's args or env may hold secrets.
            "mcp" | "mcpServers" => b.notes.push(Note::metadata(
                k,
                format!("MCP entries named in notes only: {}", capped(keys_of(v), 8)),
            )),
            "cron" | "schedule" | "cronJobs" => {
                let items: Vec<String> = match v {
                    serde_norway::Value::Sequence(s) => s
                        .iter()
                        .map(|x| match x.as_mapping() {
                            Some(m) => format!(
                                "{} {}",
                                get_str(m, &["id"]),
                                get_str(m, &["schedule", "cron"])
                            ),
                            None => scalar(x).unwrap_or_default(),
                        })
                        .map(|s| one_line(s.trim()))
                        .collect(),
                    other => vec![one_line(&scalar(other).unwrap_or_default())],
                };
                b.notes.push(Note::warning(
                    k,
                    format!(
                        "{} schedule(s) named in notes only, never carried: {}",
                        items.len(),
                        capped(items, 8)
                    ),
                ));
            }
            "description" => b.description = scalar(v).unwrap_or_default(),
            "tools" => b.tools("tools", &list(v)),
            other if b.guard_key(other) => {}
            other => unknown.push(other.to_string()),
        }
    }
    if !unknown.is_empty() {
        b.notes.push(Note::new(
            NoteKind::Dropped,
            "front matter",
            format!("keys not read: {}", capped(unknown, 10)),
        ));
    }
    b.prompt = doc.body.clone();
    if b.prompt.trim().is_empty() {
        let dir = claw
            .rsplit_once('/')
            .map(|(d, _)| format!("{d}/"))
            .unwrap_or_default();
        // The bootstrap path is untrusted: a clean relative path only.
        let boot = (!bootstrap.is_empty())
            .then(|| crate::git::clean_path(bootstrap.trim_start_matches("./")))
            .and_then(|r| match r {
                Ok(p) => Some(format!("{dir}{p}")),
                Err(why) => {
                    b.notes.push(Note::warning(
                        "workspace.bootstrapFiles.SOUL.md",
                        format!("source not read: {why}"),
                    ));
                    None
                }
            })
            .filter(|p| src.files.contains_key(p));
        if let Some(p) = boot {
            b.prompt = read_prompt(b, src, &p)?;
            b.notes.push(Note::mapped(
                "workspace.bootstrapFiles.SOUL.md",
                "used as the instructions",
            ));
        } else if let Some(s) = Some(format!("{dir}SOUL.md")).filter(|s| src.files.contains_key(s))
        {
            // Only the SOUL.md beside CLAW.md.
            b.prompt = read_prompt(b, src, &s)?;
            b.notes
                .push(Note::mapped("SOUL.md", "used as the instructions"));
        }
    } else {
        b.notes.push(Note::mapped(
            "body",
            "the CLAW.md body became the system prompt",
        ));
    }
    if b.name.is_empty() {
        b.name = src.name_hint.clone();
    }
    Ok(())
}

pub fn convert(eco: Ecosystem, src: &Fetched, opts: &Opts) -> Result<Draft> {
    let mut b = Builder::default();
    let expanded;
    let src = match profile_archive(eco, src, &mut b) {
        Some(x) => {
            expanded = x;
            &expanded
        }
        None => src,
    };
    let cleaned;
    let src = match without_credentials(src, &mut b) {
        Some(x) => {
            cleaned = x;
            &cleaned
        }
        None => src,
    };
    let path = primary(
        src,
        &[
            ".roomodes",
            "CLAW.md",
            "SOUL.md",
            "profile.yaml",
            "AGENT.md",
        ],
    )
    .or_else(|| {
        src.files
            .keys()
            .find(|k| k.ends_with(".md") || k.ends_with(".json"))
            .map(|k| k.as_str())
    })
    .ok_or_else(|| anyhow!("no agent definition in the fetched files"))?
    .to_string();
    let base = path.rsplit('/').next().unwrap_or(&path).to_string();
    let file_stem = stem(&path, &[".agent.md", ".chatmode.md"]);
    let hint = (!src.name_hint.is_empty()).then(|| src.name_hint.clone());
    match eco {
        Ecosystem::Roo
            if base == ".roomodes"
                || base.ends_with(".roomodes")
                || path.ends_with(".json")
                || path.ends_with(".yaml")
                || path.ends_with(".yml") =>
        {
            let (text, hidden) = super::text(src, &path)?;
            if hidden {
                b.flags.push(Flag::HiddenText);
            }
            let v: serde_json::Value = serde_json::from_str(&text)
                .or_else(|_| serde_norway::from_str::<serde_json::Value>(&text))
                .map_err(|e| anyhow!("{path}: not JSON or YAML: {e}"))?;
            let modes = v
                .get("customModes")
                .and_then(|m| m.as_array())
                .ok_or_else(|| anyhow!("{path}: no customModes"))?;
            let pick = hint
                .as_deref()
                .and_then(|h| {
                    modes
                        .iter()
                        .find(|m| m.get("slug").and_then(|s| s.as_str()) == Some(h))
                })
                .or_else(|| modes.first())
                .ok_or_else(|| anyhow!("{path}: customModes is empty"))?;
            if modes.len() > 1 {
                b.notes.push(Note::metadata(
                    "customModes",
                    format!("{} modes; one imported", modes.len()),
                ));
            }
            let g = |k: &str| {
                pick.get(k)
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            b.name = g("slug");
            b.display = g("name");
            b.description = if g("whenToUse").is_empty() {
                g("description")
            } else {
                g("whenToUse")
            };
            let role = g("roleDefinition");
            let ci = g("customInstructions");
            b.prompt = if ci.is_empty() {
                role
            } else {
                format!("{role}\n\n{ci}")
            };
            b.tools_set = true;
            if let Some(groups) = pick.get("groups").and_then(|x| x.as_array()) {
                for gr in groups {
                    let (gname, opt) = match gr {
                        serde_json::Value::String(s) => (s.clone(), None),
                        serde_json::Value::Array(a) => (
                            a.first()
                                .and_then(|x| x.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            a.get(1).cloned(),
                        ),
                        _ => continue,
                    };
                    let add: &[&str] = match gname.as_str() {
                        "read" => &["Read", "Grep", "Glob"],
                        "edit" => &["Edit", "Write"],
                        "command" => &["Bash"],
                        "browser" => &["WebFetch"],
                        "mcp" => {
                            b.notes.push(Note::dropped("groups.mcp"));
                            &[]
                        }
                        other => {
                            b.notes.push(Note::dropped(&format!("groups.{other}")));
                            &[]
                        }
                    };
                    for t in add {
                        if !b.tools.iter().any(|x| x == t) {
                            b.tools.push(t.to_string());
                        }
                    }
                    if let Some(o) = opt
                        && let Some(re) = o.get("fileRegex")
                    {
                        b.notes.push(Note::warning(
                            "fileRegex",
                            format!("{gname} limited to {re} upstream; not enforced"),
                        ));
                    }
                }
            }
            for k in pick.as_object().into_iter().flat_map(|o| o.keys()) {
                if !matches!(
                    k.as_str(),
                    "slug"
                        | "name"
                        | "roleDefinition"
                        | "whenToUse"
                        | "description"
                        | "customInstructions"
                        | "groups"
                ) && !b.guard_key(k)
                {
                    b.notes.push(Note::dropped(k));
                }
            }
        }
        Ecosystem::LobeHub => {
            let (text, hidden) = super::text(src, &path)?;
            if hidden {
                b.flags.push(Flag::HiddenText);
            }
            let v: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| anyhow!("{path}: not JSON: {e}"))?;
            let s = |p: &[&str]| {
                let mut x = &v;
                for k in p {
                    x = match x.get(k) {
                        Some(y) => y,
                        None => return String::new(),
                    };
                }
                x.as_str().unwrap_or_default().to_string()
            };
            b.name = s(&["identifier"]);
            b.display = s(&["meta", "title"]);
            b.description = s(&["meta", "description"]);
            b.prompt = s(&["config", "systemRole"]);
            let av = s(&["meta", "avatar"]);
            if !av.is_empty() {
                b.notes.push(Note::dropped(&format!("avatar {av}")));
            }
            if let Some(tags) = v.pointer("/meta/tags").and_then(|t| t.as_array()) {
                let t: Vec<&str> = tags.iter().filter_map(|x| x.as_str()).collect();
                b.meta.insert("tags".into(), t.join(" "));
            }
            if let Some(c) = v.get("config").and_then(|c| c.as_object()) {
                for k in c.keys().filter(|k| *k != "systemRole") {
                    if !b.guard_key(k) {
                        b.notes.push(Note::dropped(&format!("config.{k}")));
                    }
                }
            }
            b.notes.push(Note::mapped(
                "tools",
                "LobeHub agents get the read-only set",
            ));
        }
        Ecosystem::Hermes => hermes(&mut b, src)?,
        Ecosystem::OpenClaw => claws(&mut b, src)?,
        _ => {
            let (t, hidden) = super::text(src, &path)?;
            if hidden {
                b.flags.push(Flag::HiddenText);
            }
            let doc = frontmatter::parse(&t);
            md_agent(&mut b, eco, &doc, &file_stem);
        }
    }
    if let Some(h) = hint
        && b.name.is_empty()
    {
        b.name = h;
    }
    let (profile, doc, mut notes, flags) = b.finish(eco, opts)?;
    let mut d = Draft::new(Target::Agent);
    d.name = profile.name.clone();
    d.description = profile.description.clone();
    d.doc = doc;
    // Bundled skills: `skills/<name>/SKILL.md` beside the agent.
    let dir = path
        .rsplit_once('/')
        .map(|(d, _)| format!("{d}/"))
        .unwrap_or_default();
    let skill_paths: Vec<String> = src
        .files
        .keys()
        .filter(|k| k.starts_with(&format!("{dir}skills/")) && k.ends_with("/SKILL.md"))
        .cloned()
        .collect();
    for sp in skill_paths {
        let (t, _) = super::text(src, &sp)?;
        let sdoc = frontmatter::parse(&t);
        let sdir = sp.trim_end_matches("/SKILL.md");
        let sub = Fetched {
            files: src
                .files
                .iter()
                .filter(|(k, _)| *k == &sp || crate::git::always_keep(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            upstream: src.upstream.clone(),
            licence_files: src.licence_files.clone(),
            licence_hint: src.licence_hint.clone(),
            file_urls: src.file_urls.clone(),
            primary: Some(sp.clone()),
            ..Default::default()
        };
        match super::skill::from_doc(eco, &sdoc, &sub, sdir, &Opts::default()) {
            Ok(sd) => {
                let sc = super::validate::finish(sd, &sub, &Opts::default());
                notes.push(Note::mapped(
                    &sp,
                    format!("skill {} imported alongside", sc.name),
                ));
                d.extra_skills.push(sc);
            }
            Err(e) => notes.push(Note::new(
                NoteKind::Dropped,
                &sp,
                format!("skill not imported: {e}"),
            )),
        }
    }
    d.profile = Some(profile);
    d.notes = notes;
    for f in flags {
        d.flag(f);
    }
    Ok(d)
}
