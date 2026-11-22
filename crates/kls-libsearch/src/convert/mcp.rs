//! mcp → belai MCP (§3.5): `name (≤32 [A-Za-z0-9_-]), transport
//! stdio|http, command, args, env | url, headers, tools?, timeout_ms?`.
//!
//! Secret-looking env and header values become `cred:NAME`; versions are
//! always pinned; `${CLAUDE_PLUGIN_ROOT}` / `${extensionPath}` configs and
//! plain-http remotes are refused (they need the plugin's files or would
//! send traffic in the clear).

use super::canonical::{self, Json, mcp_slug};
use super::validate::{Draft, looks_secret, valid_ref_name};
use super::{Note, Opts, Target, primary};
use crate::scan::Flag;
use crate::source::{Ecosystem, Fetched};
use anyhow::{Result, anyhow};

/// A `cred:` reference name for an env or header name.
fn cred_name(n: &str) -> String {
    let mut s: String = n
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() || s.as_bytes()[0].is_ascii_digit() {
        s.insert(0, '_');
    }
    canonical::clip(&s, 64).to_string()
}

/// `${VAR}` / `$VAR` → `env:VAR`.
fn env_ref(v: &str) -> Option<String> {
    let inner = v
        .strip_prefix("${")
        .and_then(|x| x.strip_suffix('}'))
        .or_else(|| v.strip_prefix('$'))?;
    valid_ref_name(inner).then(|| format!("env:{inner}"))
}

struct Out {
    doc: Json,
    notes: Vec<Note>,
    flags: Vec<Flag>,
}

impl Out {
    fn new(name: &str) -> Self {
        let mut doc = Json::obj();
        doc.set("name", Json::s(name));
        Self {
            doc,
            notes: vec![],
            flags: vec![],
        }
    }

    /// A value for a secret-or-not variable.
    fn value(
        &mut self,
        field: &str,
        key: &str,
        secret: bool,
        v: Option<&str>,
        required: bool,
    ) -> Option<String> {
        if secret || looks_secret(key) {
            let r = format!("cred:{}", cred_name(key));
            self.notes.push(Note::mapped(field, format!("{key} → {r}")));
            return Some(r);
        }
        match v.map(str::trim).filter(|s| !s.is_empty()) {
            Some(v) => Some(env_ref(v).unwrap_or_else(|| v.to_string())),
            None => {
                if required {
                    self.notes.push(Note::warning(
                        field,
                        format!("{key} is required: set it on the host"),
                    ));
                }
                None
            }
        }
    }

    fn refuse_roots(&mut self, raw: &str) {
        for root in [
            "${CLAUDE_PLUGIN_ROOT}",
            "$CLAUDE_PLUGIN_ROOT",
            "${extensionPath}",
            "$extensionPath",
        ] {
            if raw.contains(root) {
                self.flags.push(Flag::Refused(format!(
                    "the config uses {root}: it needs the plugin's files"
                )));
                return;
            }
        }
    }
}

/// From a registry `server.json` (or a list entry wrapping it).
fn from_registry(v: &Json, opts: &Opts) -> Out {
    let s = v.get("server").unwrap_or(v);
    let full = s.str_of("name").unwrap_or("server").to_string();
    let last = full.rsplit('/').next().unwrap_or(&full);
    let name = mcp_slug(opts.rename.as_deref().unwrap_or(last));
    let mut o = Out::new(&name);
    o.notes
        .push(Note::mapped("name", format!("{full} → {name}")));
    let mut done = false;
    let pkgs = s
        .get("packages")
        .and_then(Json::as_arr)
        .cloned()
        .unwrap_or_default();
    // Prefer npm, then pypi, then oci.
    for want in ["npm", "pypi", "oci"] {
        if done {
            break;
        }
        for p in &pkgs {
            if p.str_of("registryType").or(p.str_of("registry_type")) != Some(want) {
                continue;
            }
            let id = p
                .str_of("identifier")
                .or(p.str_of("name"))
                .unwrap_or_default()
                .to_string();
            let ver = p
                .str_of("version")
                .or(s.str_of("version"))
                .unwrap_or_default()
                .to_string();
            if id.is_empty() {
                continue;
            }
            if ver.is_empty() || ver == "latest" {
                o.notes.push(Note::warning(
                    "version",
                    "no exact version published; not pinned",
                ));
            }
            let (cmd, mut args): (&str, Vec<String>) = match want {
                "npm" => (
                    "npx",
                    vec![
                        "-y".into(),
                        if ver.is_empty() {
                            id.clone()
                        } else {
                            format!("{id}@{ver}")
                        },
                    ],
                ),
                "pypi" => (
                    "uvx",
                    vec![if ver.is_empty() {
                        id.clone()
                    } else {
                        format!("{id}=={ver}")
                    }],
                ),
                _ => {
                    let last_seg = id.rsplit('/').next().unwrap_or(&id);
                    let image =
                        if last_seg.contains(':') || last_seg.contains('@') || ver.is_empty() {
                            id.clone()
                        } else {
                            format!("{id}:{ver}")
                        };
                    (
                        "docker",
                        vec!["run".into(), "-i".into(), "--rm".into(), image],
                    )
                }
            };
            for a in p
                .get("packageArguments")
                .and_then(Json::as_arr)
                .into_iter()
                .flatten()
            {
                let val = a
                    .str_of("value")
                    .or(a.str_of("default"))
                    .unwrap_or_default();
                match a.str_of("type") {
                    Some("named") => {
                        if let Some(n) = a.str_of("name") {
                            args.push(n.to_string());
                        }
                        if !val.is_empty() {
                            args.push(val.to_string());
                        }
                    }
                    _ if !val.is_empty() => args.push(val.to_string()),
                    _ => o.notes.push(Note::warning(
                        "packageArguments",
                        format!(
                            "{} has no value: add it by hand",
                            a.str_of("valueHint")
                                .or(a.str_of("name"))
                                .unwrap_or("an argument")
                        ),
                    )),
                }
            }
            if p.get("runtimeArguments").is_some() {
                o.notes.push(Note::dropped("runtimeArguments"));
            }
            let mut env = Json::obj();
            for e in p
                .get("environmentVariables")
                .and_then(Json::as_arr)
                .into_iter()
                .flatten()
            {
                let Some(k) = e.str_of("name") else { continue };
                if !valid_ref_name(k) {
                    o.notes.push(Note::dropped(&format!("env {k}")));
                    continue;
                }
                let secret = e.get("isSecret").and_then(Json::as_bool).unwrap_or(false);
                let req = e.get("isRequired").and_then(Json::as_bool).unwrap_or(false);
                if let Some(v) = o.value(
                    "env",
                    k,
                    secret,
                    e.str_of("default").or(e.str_of("value")),
                    req,
                ) {
                    env.set(k, Json::s(v));
                }
            }
            o.doc.set("transport", Json::s("stdio"));
            o.doc.set("command", Json::s(cmd));
            o.doc
                .set("args", Json::Arr(args.into_iter().map(Json::Str).collect()));
            if env.as_obj().is_some_and(|m| !m.is_empty()) {
                o.doc.set("env", env);
            }
            o.notes
                .push(Note::mapped("packages", format!("{want} package {id}")));
            done = true;
            break;
        }
    }
    for p in &pkgs {
        if let Some(t) = p.str_of("registryType")
            && !matches!(t, "npm" | "pypi" | "oci")
        {
            o.notes.push(Note::warning(
                "packages",
                format!("{t} packages are not supported"),
            ));
        }
    }
    if !done {
        for r in s
            .get("remotes")
            .and_then(Json::as_arr)
            .into_iter()
            .flatten()
        {
            let t = r.str_of("type").unwrap_or_default();
            if !matches!(t, "streamable-http" | "sse" | "http") {
                continue;
            }
            let url = r.str_of("url").unwrap_or_default();
            remote(&mut o, url, r.get("headers"));
            done = true;
            break;
        }
    }
    if !done {
        o.flags
            .push(Flag::Refused("no supported package or remote".into()));
    }
    o
}

fn remote(o: &mut Out, url: &str, headers: Option<&Json>) {
    if !url.starts_with("https://") {
        o.flags.push(Flag::Refused(format!(
            "the remote URL is not https ({})",
            canonical::clip(url, 80)
        )));
    }
    o.doc.set("transport", Json::s("http"));
    o.doc.set("url", Json::s(url));
    let mut hs = Json::obj();
    match headers {
        Some(Json::Arr(list)) => {
            for h in list {
                let Some(k) = h.str_of("name") else { continue };
                let secret = h.get("isSecret").and_then(Json::as_bool).unwrap_or(false);
                let req = h.get("isRequired").and_then(Json::as_bool).unwrap_or(false);
                if let Some(v) = o.value(
                    "headers",
                    k,
                    secret,
                    h.str_of("value").or(h.str_of("default")),
                    req,
                ) {
                    hs.set(k, Json::s(v));
                }
            }
        }
        Some(Json::Obj(m)) => {
            for (k, v) in m {
                if let Some(v) = o.value("headers", k, false, v.as_str(), false) {
                    hs.set(k, Json::s(v));
                }
            }
        }
        _ => {}
    }
    if hs.as_obj().is_some_and(|m| !m.is_empty()) {
        o.doc.set("headers", hs);
    }
}

/// From an `mcpServers` map (`.mcp.json`, aitmpl, gemini-extension.json).
fn from_servers(v: &Json, hint: &str, eco: Ecosystem, opts: &Opts, raw: &str) -> Out {
    let servers = v
        .get("mcpServers")
        .or(v.get("servers"))
        .and_then(Json::as_obj)
        .cloned()
        .unwrap_or_default();
    let pick = servers
        .iter()
        .find(|(k, _)| !hint.is_empty() && (k.as_str() == hint || mcp_slug(k) == mcp_slug(hint)))
        .or_else(|| servers.iter().next());
    let Some((key, s)) = pick else {
        let mut o = Out::new(&mcp_slug(hint));
        o.flags
            .push(Flag::Refused("no server config: open the README".into()));
        return o;
    };
    let name = mcp_slug(opts.rename.as_deref().unwrap_or(key));
    let mut o = Out::new(if name.is_empty() { "server" } else { &name });
    if servers.len() > 1 {
        o.notes.push(Note::metadata(
            "mcpServers",
            format!("{} servers; {key} imported", servers.len()),
        ));
    }
    if name != *key {
        o.notes
            .push(Note::mapped("name", format!("{key} → {name}")));
    }
    // Refuse only when the chosen server uses a plugin root.
    let mut chosen = String::new();
    canonical::write_json(s, false, &mut chosen);
    o.refuse_roots(if chosen.is_empty() { raw } else { &chosen });
    let typ = s
        .str_of("type")
        .or(s.str_of("transport"))
        .unwrap_or_default();
    let url = s
        .str_of("httpUrl")
        .or(s.str_of("url"))
        .or(s.str_of("serverUrl"));
    if let Some(u) = url.filter(|_| typ != "stdio") {
        remote(&mut o, u, s.get("headers"));
    } else if let Some(cmd) = s.str_of("command") {
        o.doc.set("transport", Json::s("stdio"));
        o.doc.set("command", Json::s(cmd));
        if let Some(a) = s.get("args").and_then(Json::as_arr) {
            o.doc.set(
                "args",
                Json::Arr(a.iter().filter_map(|x| x.as_str()).map(Json::s).collect()),
            );
        }
        let mut env = Json::obj();
        for (k, v) in s.get("env").and_then(Json::as_obj).into_iter().flatten() {
            if !valid_ref_name(k) {
                o.notes.push(Note::dropped(&format!("env {k}")));
                continue;
            }
            if let Some(val) = o.value("env", k, false, v.as_str(), false) {
                env.set(k, Json::s(val));
            }
        }
        // Gemini: settings[].sensitive → cred:.
        for st in v
            .get("settings")
            .and_then(Json::as_arr)
            .into_iter()
            .flatten()
        {
            if let Some(ev) = st.str_of("envVar").filter(|e| valid_ref_name(e)) {
                let sens = st.get("sensitive").and_then(Json::as_bool).unwrap_or(false);
                if let Some(val) = o.value("settings", ev, sens, None, !sens) {
                    env.set(ev, Json::s(val));
                }
            }
        }
        if env.as_obj().is_some_and(|m| !m.is_empty()) {
            o.doc.set("env", env);
        }
        if let Some(a) = o.doc.get("args").and_then(Json::as_arr)
            && a.iter()
                .filter_map(Json::as_str)
                .any(|x| x.contains("@latest"))
        {
            o.notes.push(Note::warning("args", "@latest is not pinned"));
        }
    } else {
        o.flags
            .push(Flag::Refused("the server has no command or URL".into()));
    }
    if let Some(t) = s
        .get("timeout")
        .and_then(Json::as_i64)
        .filter(|t| (0..=600_000).contains(t))
    {
        o.doc.set("timeout_ms", Json::Num(t.to_string()));
    }
    if let Some(Json::Arr(t)) = s.get("tools").or(s.get("includeTools")) {
        o.doc.set(
            "tools",
            Json::Arr(t.iter().filter_map(|x| x.as_str()).map(Json::s).collect()),
        );
    }
    for k in s.as_obj().into_iter().flat_map(|m| m.keys()) {
        match k.as_str() {
            "type" | "transport" | "url" | "httpUrl" | "serverUrl" | "headers" | "command"
            | "args" | "env" | "timeout" | "tools" | "includeTools" => {}
            "trust" => o.notes.push(Note::warning("trust", "never carried")),
            other => o.notes.push(Note::dropped(other)),
        }
    }
    let _ = eco;
    o
}

pub fn convert(eco: Ecosystem, src: &Fetched, opts: &Opts) -> Result<Draft> {
    let path = primary(
        src,
        &[
            "server.json",
            ".mcp.json",
            "gemini-extension.json",
            "mcp.json",
        ],
    )
    .or_else(|| {
        src.files
            .keys()
            .find(|k| k.ends_with(".json"))
            .map(|k| k.as_str())
    })
    .ok_or_else(|| anyhow!("no MCP config in the fetched files"))?
    .to_string();
    let (text, hidden) = super::text(src, &path)?;
    let v = canonical::parse_value(text.as_bytes()).map_err(|e| anyhow!("{path}: {e}"))?;
    let registry = eco == Ecosystem::McpRegistry
        || v.get("server").is_some()
        || v.get("packages").is_some()
        || v.get("remotes").is_some();
    let mut o = if registry {
        from_registry(&v, opts)
    } else if v.get("mcpServers").is_some() || v.get("servers").is_some() {
        from_servers(&v, &src.name_hint, eco, opts, &text)
    } else if v.get("discoveryOnly").and_then(Json::as_bool) == Some(true) {
        // A marketplace entry with no published config (Cline, §3.5): a
        // manual draft to complete from the README in the Converted tab. Its
        // empty command fails the validator, so it stays refused until the
        // person fills it in; the edit is validated again.
        let hint = if src.name_hint.is_empty() {
            "server"
        } else {
            &src.name_hint
        };
        let mut o = Out::new(&mcp_slug(opts.rename.as_deref().unwrap_or(hint)));
        o.doc.set("transport", Json::s("stdio"));
        o.doc.set("command", Json::s(""));
        o.doc.set("args", Json::Arr(vec![]));
        let url = v.str_of("githubUrl").unwrap_or_default();
        o.notes.push(Note::warning(
            "draft",
            format!("no server config is published: set command and args from the README {url}"),
        ));
        o
    } else {
        let mut o = Out::new(&mcp_slug(if src.name_hint.is_empty() {
            "server"
        } else {
            &src.name_hint
        }));
        o.flags
            .push(Flag::Refused("no server config: open the README".into()));
        o
    };
    if hidden {
        o.flags.push(Flag::HiddenText);
    }
    let mut d = Draft::new(Target::Library { kind: "mcp" });
    d.name = o.doc.str_of("name").unwrap_or_default().to_string();
    d.description = v
        .get("server")
        .unwrap_or(&v)
        .str_of("description")
        .map(|s| canonical::clip(&canonical::one_line(s), 300).to_string())
        .unwrap_or_default();
    if o.doc.str_of("transport") == Some("stdio") {
        let mut argv = o.doc.str_of("command").unwrap_or_default().to_string();
        for a in o
            .doc
            .get("args")
            .and_then(Json::as_arr)
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
        {
            argv.push(' ');
            argv.push_str(a);
        }
        o.notes.push(Note::mapped("argv", argv));
    }
    d.doc = o.doc.canonical();
    d.notes = o.notes;
    for f in o.flags {
        d.flag(f);
    }
    Ok(d)
}

#[cfg(test)]
mod cline_draft_tests {
    use super::*;
    use crate::source::ItemType;

    /// §3.5: a Cline marketplace entry has no server config. The preview is
    /// a manual draft (stdio, empty command) that stays refused until the
    /// person completes it from the README; the completed edit validates.
    #[test]
    fn a_cline_entry_becomes_a_manual_draft() {
        let mut src = Fetched {
            name_hint: "PDF Reader".into(),
            ..Default::default()
        };
        let entry = br#"{"discoveryOnly":true,"name":"PDF Reader","githubUrl":"https://github.com/o/pdf","note":"README setup"}"#;
        src.files
            .insert("cline-entry.json".into(), bytes::Bytes::from_static(entry));
        src.primary = Some("cline-entry.json".into());
        let c = super::super::validate::convert_any(
            ItemType::Mcp,
            Ecosystem::Cline,
            &src,
            &Opts::default(),
        )
        .unwrap();
        let doc = String::from_utf8(c.doc.clone()).unwrap();
        assert!(
            doc.contains(r#""transport":"stdio""#) && doc.contains(r#""command":"""#),
            "{doc}"
        );
        assert!(
            c.refused().is_some_and(|r| r.starts_with("invalid: ")),
            "{:?}",
            c.flags
        );
        assert!(c.notes.iter().any(|n| n.text.contains("github.com/o/pdf")));
        let edited = doc
            .replace(r#""command":"""#, r#""command":"npx""#)
            .replace(r#""args":[]"#, r#""args":["-y","@o/pdf@1.0.0"]"#);
        let done = super::super::validate::from_edited(&c, &edited).unwrap();
        assert!(done.refused().is_none(), "{:?}", done.flags);
    }
}
