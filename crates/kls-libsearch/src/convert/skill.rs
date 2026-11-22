//! skill → belai Skill (§3.1): `name, description, license, compatibility,
//! metadata, allowed-tools, disable-model-invocation`. A belai skill is one
//! document: supporting files are not carried.

use super::canonical::{self, one_line};
use super::frontmatter::{self, Doc, list, scalar, yaml_scalar};
use super::validate::Draft;
use super::{Note, NoteKind, Opts, Target, cut_description, name_for, primary};
use crate::scan::Flag;
use crate::source::{Ecosystem, Fetched};
use anyhow::{Result, anyhow};
use serde_json::{Map, Value};

/// The description belai keeps for search listings.
pub const DESCRIPTION_MAX: usize = 300;
/// One-line metadata cap.
pub const METADATA_MAX: usize = 1024;

const DROPPED_CLAUDE: [&str; 7] = [
    "argument-hint",
    "user-invocable",
    "model",
    "context",
    "agent",
    "hooks",
    "paths",
];

pub fn convert(eco: Ecosystem, src: &Fetched, opts: &Opts) -> Result<Draft> {
    let path = primary(src, &["SKILL.md", "skill.md"])
        .ok_or_else(|| anyhow!("no SKILL.md in the fetched files"))?
        .to_string();
    let (text, hidden) = super::text(src, &path)?;
    let doc = frontmatter::parse(&text);
    let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let mut d = from_doc(eco, &doc, src, dir, opts)?;
    if hidden {
        d.flag(Flag::HiddenText);
        d.note(Note::warning("text", "hidden characters were removed"));
    }
    // Supporting files: not carried.
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    let extra = src
        .files
        .keys()
        .filter(|k| k.starts_with(&prefix) && **k != path)
        .filter(|k| !crate::git::always_keep(k) && !k.ends_with("agents/openai.yaml"))
        .count() as u32;
    if extra > 0 {
        d.flag(Flag::FilesNotCarried(extra));
        d.note(Note::new(
            NoteKind::Dropped,
            "files",
            format!("{extra} supporting file(s) not carried"),
        ));
    }
    if doc.body.contains("scripts/") || doc.body.contains("references/") {
        d.note(Note::warning(
            "body",
            "the instructions link scripts/ or references/, which are not carried",
        ));
    }
    Ok(d)
}

/// Converts one parsed SKILL.md (also used for skills bundled with agents).
pub fn from_doc(eco: Ecosystem, doc: &Doc, src: &Fetched, dir: &str, opts: &Opts) -> Result<Draft> {
    let mut d = Draft::new(Target::Library { kind: "skill" });
    let mut notes = Vec::new();
    let dir_name = dir.rsplit('/').next().unwrap_or(dir);
    let raw_name = doc
        .str("name")
        .or_else(|| (!src.name_hint.is_empty()).then(|| src.name_hint.clone()))
        .or_else(|| (!dir_name.is_empty()).then(|| dir_name.to_string()))
        .unwrap_or_else(|| "imported-skill".into());
    d.name = name_for(&raw_name, opts, &mut notes);
    if d.name.starts_with("belai-") {
        d.name = format!("x-{}", d.name);
        notes.push(Note::warning(
            "name",
            "belai- names are reserved; prefixed x-",
        ));
    }
    let mut desc = doc
        .str("description")
        .map(|s| one_line(&s))
        .unwrap_or_default();
    let mut meta = Map::new();
    if let Some(w) = doc.str("when_to_use") {
        let w = one_line(&w);
        if desc.len() + 1 + w.len() <= DESCRIPTION_MAX {
            desc = if desc.is_empty() {
                w
            } else {
                format!("{desc} {w}")
            };
            notes.push(Note::mapped("when_to_use", "appended to the description"));
        } else {
            meta.insert("when_to_use".into(), Value::String(w));
            notes.push(Note::metadata("when_to_use", "kept in metadata"));
        }
    }
    if desc.is_empty() {
        desc = super::first_line(&doc.body);
        notes.push(Note::mapped("description", "from the first line"));
    }
    if desc.is_empty() {
        desc = format!("Imported {} skill", eco.label());
    }
    d.description = cut_description(&desc, DESCRIPTION_MAX, &mut notes);
    let mut fields: Vec<(&str, String)> = vec![
        ("name", d.name.clone()),
        ("description", yaml_scalar(&d.description)),
    ];
    if let Some(l) = doc.str("license") {
        let l = one_line(&l);
        d.licence_field = Some(l.clone());
        fields.push(("license", yaml_scalar(canonical::clip(&l, 128))));
    }
    if let Some(c) = doc.str("compatibility") {
        fields.push((
            "compatibility",
            yaml_scalar(canonical::clip(&one_line(&c), 500)),
        ));
    }
    // Ecosystem metadata.
    for k in doc.keys().map(str::to_string).collect::<Vec<_>>() {
        let v = doc.get(&k).cloned().unwrap_or(serde_norway::Value::Null);
        match k.as_str() {
            "name"
            | "description"
            | "license"
            | "compatibility"
            | "when_to_use"
            | "allowed-tools"
            | "disable-model-invocation" => {}
            "metadata" => match &v {
                serde_norway::Value::Mapping(m) => {
                    for (mk, mv) in m {
                        let Some(mk) = scalar(mk) else { continue };
                        if mk == "hermes" {
                            if let serde_norway::Value::Mapping(h) = mv {
                                for (hk, hv) in h {
                                    if let Some(hk) = scalar(hk)
                                        && matches!(hk.as_str(), "tags" | "category")
                                    {
                                        meta.insert(hk, frontmatter::yaml_to_json(hv));
                                    }
                                }
                            }
                        } else {
                            meta.insert(mk, frontmatter::yaml_to_json(mv));
                        }
                    }
                    notes.push(Note::metadata("metadata", "kept as one line"));
                }
                serde_norway::Value::String(s) => {
                    // OpenClaw writes inline JSON.
                    match serde_json::from_str::<Value>(s) {
                        Ok(Value::Object(o)) => {
                            for (ok, ov) in o {
                                meta.insert(ok, ov);
                            }
                        }
                        _ => {
                            meta.insert("note".into(), Value::String(one_line(s)));
                        }
                    }
                    notes.push(Note::metadata("metadata", "kept as one line"));
                }
                _ => {}
            },
            "version" | "author" | "platforms" | "tags" | "category" => {
                meta.insert(k.clone(), frontmatter::yaml_to_json(&v));
                notes.push(Note::metadata(&k, "kept in metadata"));
            }
            "homepage" => notes.push(Note::mapped("homepage", "kept in provenance")),
            _ if k == "required_environment_variables"
                || k.starts_with("requires")
                || k.starts_with("fallback_for")
                || k == "config" =>
            {
                notes.push(Note::metadata(
                    &k,
                    format!(
                        "named in notes only, never env: {}",
                        frontmatter::to_json_line(&v)
                    ),
                ));
            }
            _ if DROPPED_CLAUDE.contains(&k.as_str()) => {
                notes.push(Note::dropped(&k));
            }
            _ => notes.push(Note::dropped(&k)),
        }
    }
    // OpenClaw requires.bins / env (inside metadata.openclaw).
    if let Some(Value::Object(oc)) = meta.get("openclaw")
        && let Some(Value::Object(req)) = oc.get("requires")
    {
        for key in ["bins", "env", "anyBins"] {
            if let Some(v) = req.get(key) {
                notes.push(Note::warning(&format!("requires.{key}"), v.to_string()));
            }
        }
    }
    // Codex UI metadata.
    let oy = if dir.is_empty() {
        "agents/openai.yaml".to_string()
    } else {
        format!("{dir}/agents/openai.yaml")
    };
    if let Some(t) = src.text(&oy)
        && let Ok(serde_norway::Value::Mapping(m)) =
            serde_norway::from_str::<serde_norway::Value>(t)
        && let Some(serde_norway::Value::Mapping(i)) = m.get("interface")
    {
        for (k, v) in i {
            let Some(k) = scalar(k) else { continue };
            if matches!(k.as_str(), "display_name" | "short_description") {
                meta.insert(k, frontmatter::yaml_to_json(v));
            } else {
                notes.push(Note::dropped(&format!("interface.{k}")));
            }
        }
        notes.push(Note::metadata(
            "agents/openai.yaml",
            "interface kept in metadata",
        ));
    }
    if !meta.is_empty() {
        let mut line = serde_json::to_string(&Value::Object(meta.clone())).unwrap_or_default();
        while line.len() > METADATA_MAX && !meta.is_empty() {
            let last = meta.keys().next_back().cloned().unwrap_or_default();
            meta.remove(&last);
            notes.push(Note::new(
                NoteKind::Dropped,
                &format!("metadata.{last}"),
                format!("over {METADATA_MAX} bytes"),
            ));
            line = serde_json::to_string(&Value::Object(meta.clone())).unwrap_or_default();
        }
        if !meta.is_empty() {
            fields.push(("metadata", yaml_scalar(&line)));
        }
    }
    if let Some(t) = doc.get("allowed-tools") {
        let tools = list(t);
        if !tools.is_empty() {
            fields.push(("allowed-tools", yaml_scalar(&tools.join(" "))));
        }
    }
    if let Some(b) = doc.str("disable-model-invocation")
        && matches!(b.as_str(), "true" | "false")
    {
        fields.push(("disable-model-invocation", b));
    }
    let body = doc.body.trim_start_matches('\n');
    let md = frontmatter::compose(&fields, body);
    d.doc = canonical::markdown(&md)?.into_bytes();
    d.notes = notes;
    Ok(d)
}
