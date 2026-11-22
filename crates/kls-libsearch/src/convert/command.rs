//! command → belai Command (§3.2): `name, description (required, ≤1024),
//! argument-hint, allowed-tools, license, compatibility, metadata,
//! disable-model-invocation`.
//!
//! Nested names (`a/b.md`, `gcs/sync.toml`) become `a.b`: Claude shows
//! them as `a:b`, but a belai name has no `:`.

use super::canonical::{self, one_line};
use super::frontmatter::{self, list, yaml_scalar};
use super::validate::{Draft, MAX_SKILL_DESCRIPTION};
use super::{Note, Opts, Target, cut_description, first_line, name_for, primary};
use crate::scan::Flag;
use crate::source::{Ecosystem, Fetched};
use anyhow::{Result, anyhow};

/// The command name from its path below a `commands`/`prompts`/`workflows`
/// folder: `a/b.md` → `a.b`.
pub fn name_from_path(path: &str) -> String {
    let mut p = path;
    for marker in ["commands/", "command/", "prompts/", "workflows/"] {
        if let Some(i) = p.rfind(marker) {
            p = &p[i + marker.len()..];
            break;
        }
    }
    let p = p
        .strip_suffix(".prompt.md")
        .or_else(|| p.strip_suffix(".md"))
        .or_else(|| p.strip_suffix(".toml"))
        .unwrap_or(p);
    p.replace('/', ".")
}

pub fn convert(eco: Ecosystem, src: &Fetched, opts: &Opts) -> Result<Draft> {
    let path = primary(src, &[])
        .or_else(|| {
            src.files
                .keys()
                .find(|k| k.ends_with(".md") || k.ends_with(".toml"))
                .map(|k| k.as_str())
        })
        .ok_or_else(|| anyhow!("no command file in the fetched files"))?
        .to_string();
    let (text, hidden) = super::text(src, &path)?;
    let mut d = Draft::new(Target::Library { kind: "command" });
    let mut notes = Vec::new();
    if hidden {
        d.flag(Flag::HiddenText);
        notes.push(Note::warning("text", "hidden characters were removed"));
    }
    let raw_name = if src.name_hint.is_empty() {
        name_from_path(&path)
    } else {
        src.name_hint.replace([':', '/'], ".")
    };
    let mut fields: Vec<(&str, String)> = Vec::new();
    let mut desc = String::new();
    let mut hint = String::new();
    let mut tools: Vec<String> = Vec::new();
    let body;
    if path.ends_with(".toml") || eco == Ecosystem::GeminiCli && !path.ends_with(".md") {
        // Gemini CLI: TOML with `description` and `prompt`.
        let v: toml::Value = toml::from_str(&text).map_err(|e| anyhow!("{path}: not TOML: {e}"))?;
        let t = v
            .as_table()
            .ok_or_else(|| anyhow!("{path}: not a TOML table"))?;
        desc = t
            .get("description")
            .and_then(|x| x.as_str())
            .map(one_line)
            .unwrap_or_default();
        let prompt = t
            .get("prompt")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("{path}: no prompt"))?;
        let (p, _) = canonical::clean_text(prompt);
        if p.contains("{{args}}") {
            notes.push(Note::mapped("prompt", "{{args}} → $ARGUMENTS"));
        }
        body = p.replace("{{args}}", "$ARGUMENTS");
        notes.push(Note::mapped("prompt", "TOML prompt became the body"));
        for k in t.keys() {
            if k != "description" && k != "prompt" {
                notes.push(Note::dropped(k));
            }
        }
    } else {
        let doc = frontmatter::parse(&text);
        for (k, v) in &doc.fields {
            match k.as_str() {
                "description" => {
                    desc = frontmatter::scalar(v)
                        .map(|s| one_line(&s))
                        .unwrap_or_default()
                }
                "argument-hint" | "argument_hint" => {
                    hint = frontmatter::scalar(v)
                        .map(|s| one_line(&s))
                        .or_else(|| match v {
                            serde_norway::Value::Sequence(_) => {
                                Some(format!("[{}]", list(v).join(", ")))
                            }
                            _ => None,
                        })
                        .unwrap_or_default()
                }
                "allowed-tools" => tools = list(v),
                "tools" if eco == Ecosystem::Copilot => {
                    let (kept, dropped) = super::tools::map_all(&list(v));
                    if !dropped.is_empty() {
                        notes.push(Note::new(
                            super::NoteKind::Dropped,
                            "tools",
                            format!("no match: {}", dropped.join(", ")),
                        ));
                    }
                    if !kept.is_empty() {
                        notes.push(Note::mapped("tools", "mapped to allowed-tools"));
                    }
                    tools = kept;
                }
                "license" => {
                    if let Some(l) = frontmatter::scalar(v) {
                        d.licence_field = Some(one_line(&l));
                        fields.push(("license", yaml_scalar(canonical::clip(&one_line(&l), 128))));
                    }
                }
                "compatibility" => {
                    if let Some(c) = frontmatter::scalar(v) {
                        fields.push((
                            "compatibility",
                            yaml_scalar(canonical::clip(&one_line(&c), 500)),
                        ));
                    }
                }
                "disable-model-invocation" => {
                    if let Some(b) = frontmatter::scalar(v).filter(|b| b == "true" || b == "false")
                    {
                        fields.push(("disable-model-invocation", b));
                    }
                }
                "name" => {}
                other => notes.push(Note::dropped(other)),
            }
        }
        body = doc.body;
    }
    d.name = name_for(&raw_name, opts, &mut notes);
    if desc.is_empty() {
        desc = first_line(&body);
        if !desc.is_empty() {
            notes.push(Note::mapped(
                "description",
                "from the first heading or line",
            ));
        }
    }
    if desc.is_empty() {
        desc = format!("Imported {} command", eco.label());
    }
    d.description = cut_description(&desc, MAX_SKILL_DESCRIPTION, &mut notes);
    let mut out: Vec<(&str, String)> = vec![
        ("name", d.name.clone()),
        ("description", yaml_scalar(&d.description)),
    ];
    if !hint.is_empty() {
        out.push(("argument-hint", yaml_scalar(canonical::clip(&hint, 256))));
    }
    if !tools.is_empty() {
        out.push(("allowed-tools", yaml_scalar(&tools.join(" "))));
    }
    out.extend(fields);
    let md = frontmatter::compose(&out, &body);
    d.doc = canonical::markdown(&md)?.into_bytes();
    d.notes = notes;
    Ok(d)
}
