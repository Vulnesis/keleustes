//! prompt (rules and instruction docs) → belai Prompt (§3.6): `name,
//! description?, order 0–999, enabled`. belai prompts are global: globs
//! and `applyTo` are dropped.

use super::canonical::{self, one_line};
use super::frontmatter::{self, scalar, yaml_scalar};
use super::validate::{Draft, MAX_PROMPT_DESCRIPTION};
use super::{Note, Opts, Target, cut_description, first_line, name_for, primary, stem};
use crate::scan::Flag;
use crate::source::{Ecosystem, Fetched};
use anyhow::{Result, anyhow};

const KNOWN: [&str; 9] = [
    ".cursorrules",
    ".clinerules",
    ".roorules",
    ".windsurfrules",
    "AGENTS.md",
    "CLAUDE.md",
    "GEMINI.md",
    "copilot-instructions.md",
    ".github/copilot-instructions.md",
];

pub fn convert(eco: Ecosystem, src: &Fetched, opts: &Opts) -> Result<Draft> {
    let path = primary(src, &KNOWN)
        .or_else(|| {
            src.files
                .keys()
                .find(|k| k.ends_with(".mdc") || k.ends_with(".md") || !k.contains('.'))
                .map(|k| k.as_str())
        })
        .ok_or_else(|| anyhow!("no rules file in the fetched files"))?
        .to_string();
    let (text, hidden) = super::text(src, &path)?;
    let doc = frontmatter::parse(&text);
    let mut d = Draft::new(Target::Library { kind: "prompt" });
    let mut notes = Vec::new();
    if hidden {
        d.flag(Flag::HiddenText);
        notes.push(Note::warning("text", "hidden characters were removed"));
    }
    let base = path.rsplit('/').next().unwrap_or(&path);
    let raw_name = if !src.name_hint.is_empty() {
        src.name_hint.clone()
    } else if base.starts_with('.') {
        // `.cursorrules` in `rules/react/` → `react-cursorrules`.
        let parent = path.rsplit('/').nth(1).unwrap_or("");
        if parent.is_empty() {
            base.trim_start_matches('.').to_string()
        } else {
            format!("{parent}-{}", base.trim_start_matches('.'))
        }
    } else {
        stem(&path, &[".instructions.md", ".mdc", ".md"])
    };
    d.name = name_for(&raw_name, opts, &mut notes);
    let mut desc = String::new();
    let mut enabled = true;
    for (k, v) in &doc.fields {
        match k.as_str() {
            "description" => desc = scalar(v).map(|s| one_line(&s)).unwrap_or_default(),
            "alwaysApply" => {
                if matches!(v, serde_norway::Value::Bool(false))
                    || scalar(v).as_deref() == Some("false")
                {
                    enabled = false;
                    notes.push(Note::mapped("alwaysApply", "false → enabled: false"));
                }
            }
            "globs" | "applyTo" => notes.push(Note::new(
                super::NoteKind::Dropped,
                k,
                "belai prompts are global",
            )),
            "name" => {}
            other => notes.push(Note::dropped(other)),
        }
    }
    if desc.is_empty() {
        desc = first_line(&doc.body);
    }
    d.description = cut_description(&desc, MAX_PROMPT_DESCRIPTION, &mut notes);
    let mut fields: Vec<(&str, String)> = vec![("name", d.name.clone())];
    if !d.description.is_empty() {
        fields.push(("description", d.description.clone()));
    }
    if !enabled {
        fields.push(("enabled", "false".into()));
    }
    let _ = (eco, yaml_scalar);
    // belai's prompt front matter is `key: value` lines: keep the
    // description plain (quotes would be part of the value).
    let md = frontmatter::compose(&fields, &doc.body);
    d.doc = canonical::markdown(&md)?.into_bytes();
    d.notes = notes;
    Ok(d)
}
