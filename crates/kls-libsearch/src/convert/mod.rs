//! Converters: foreign definitions → the Keleustes/belai format (§3 of
//! `docs/gaps/library-search.md`). Pure: no IO, no clock.

pub mod agent;
pub mod canonical;
pub mod command;
pub mod frontmatter;
pub mod hook;
pub mod mcp;
pub mod prompt;
pub mod skill;
pub mod tools;
pub mod validate;

use crate::licence::Licence;
use crate::scan::Flag;
use crate::source::{Ecosystem, Fetched, ItemType, Upstream};
use serde::{Deserialize, Serialize};

/// Where a converted item goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Target {
    /// `library_items` of this kind: "skill" | "command" | "prompt" | "hook" | "mcp".
    Library { kind: &'static str },
    /// `agent_profiles`.
    Agent,
}

impl Target {
    /// The provenance `item_kind`: the library kind, or `agent`.
    pub fn kind(self) -> &'static str {
        match self {
            Target::Library { kind } => kind,
            Target::Agent => "agent",
        }
    }

    pub fn for_type(t: ItemType) -> Self {
        match t {
            ItemType::Agent => Target::Agent,
            ItemType::Skill => Target::Library { kind: "skill" },
            ItemType::Command => Target::Library { kind: "command" },
            ItemType::Hook => Target::Library { kind: "hook" },
            ItemType::Mcp => Target::Library { kind: "mcp" },
            ItemType::Prompt => Target::Library { kind: "prompt" },
        }
    }
}

/// belai scan vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    Mapped,
    Metadata,
    Dropped,
    Warning,
}

impl NoteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NoteKind::Mapped => "mapped",
            NoteKind::Metadata => "metadata",
            NoteKind::Dropped => "dropped",
            NoteKind::Warning => "warning",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub kind: NoteKind,
    pub field: String,
    pub text: String,
}

impl Note {
    pub fn new(kind: NoteKind, field: &str, text: impl Into<String>) -> Self {
        Self {
            kind,
            field: field.to_string(),
            text: text.into(),
        }
    }
    pub fn dropped(field: &str) -> Self {
        Self::new(NoteKind::Dropped, field, "")
    }
    pub fn warning(field: &str, text: impl Into<String>) -> Self {
        Self::new(NoteKind::Warning, field, text)
    }
    pub fn mapped(field: &str, text: impl Into<String>) -> Self {
        Self::new(NoteKind::Mapped, field, text)
    }
    pub fn metadata(field: &str, text: impl Into<String>) -> Self {
        Self::new(NoteKind::Metadata, field, text)
    }
}

/// One file of a hook bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleFile {
    pub path: String,
    pub data: Vec<u8>,
    /// sha256 lower hex of `data`.
    pub sha256: String,
}

/// A source file as provenance records it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFile {
    pub path: String,
    pub sha256: String,
    pub url: String,
}

/// A converted item, ready for review.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Converted {
    pub target: Target,
    pub name: String,
    pub description: String,
    /// Canonical bytes (belai rules). For an agent: the profile Markdown.
    pub doc: Vec<u8>,
    /// libitem hash, or `HashBundle` for hooks.
    pub sha256: String,
    /// Hooks only.
    pub files: Vec<BundleFile>,
    /// `Target::Agent`.
    pub profile: Option<kls_store::vault::AgentProfile>,
    /// Skills bundled with an agent or plugin.
    pub extra_skills: Vec<Converted>,
    pub notes: Vec<Note>,
    pub flags: Vec<Flag>,
    pub licence: Licence,
    /// The fetched upstream (commit, version, sha256 over source files, URLs).
    pub upstream: Upstream,
    /// Per source file: path, sha256, URL.
    pub source_files: Vec<SourceFile>,
}

impl Converted {
    /// The canonical document as text (every kind's doc is UTF-8).
    pub fn doc_text(&self) -> String {
        String::from_utf8_lossy(&self.doc).into_owned()
    }

    /// Flags the review must tick.
    pub fn review_flags(&self) -> Vec<&Flag> {
        self.flags.iter().filter(|f| f.needs_review()).collect()
    }

    /// The first `Refused` reason, if any.
    pub fn refused(&self) -> Option<String> {
        self.flags.iter().find_map(|f| match f {
            Flag::Refused(r) => Some(r.clone()),
            _ => None,
        })
    }
}

/// Converter options.
#[derive(Clone, Debug, Default)]
pub struct Opts {
    /// Use this name instead of the slugged source name.
    pub rename: Option<String>,
    /// Licence hint from the index (step 4 of detection).
    pub licence_hint: Option<String>,
}

/// Converts one fetched item. Pure. The result's `flags` already include
/// [`crate::scan::scan`] and the licence flags.
pub fn convert(
    item_type: ItemType,
    eco: Ecosystem,
    src: &Fetched,
    opts: &Opts,
) -> anyhow::Result<Converted> {
    validate::convert_any(item_type, eco, src, opts)
}

/// Re-validates an edited canonical document (the editable Converted tab).
pub use validate::from_edited;

// ---- helpers the converters share -------------------------------------------

/// The main file: `Fetched.primary`, else the first of `names` present
/// (exact path, then any path ending in `/<name>`), else the only file.
pub(crate) fn primary<'a>(src: &'a Fetched, names: &[&str]) -> Option<&'a str> {
    if let Some(p) = &src.primary
        && src.files.contains_key(p)
    {
        return Some(p.as_str());
    }
    for n in names {
        if let Some((k, _)) = src.files.get_key_value(*n) {
            return Some(k.as_str());
        }
    }
    for n in names {
        let suffix = format!("/{n}");
        if let Some(k) = src
            .files
            .keys()
            .filter(|k| k.ends_with(&suffix))
            .min_by_key(|k| k.len())
        {
            return Some(k.as_str());
        }
    }
    let docs: Vec<&String> = src
        .files
        .keys()
        .filter(|k| !crate::git::always_keep(k))
        .collect();
    (docs.len() == 1).then(|| docs[0].as_str())
}

/// UTF-8 text of a file, cleaned (CRLF, hidden characters); the flag says
/// whether hidden characters were removed.
pub(crate) fn text(src: &Fetched, path: &str) -> anyhow::Result<(String, bool)> {
    let b = src
        .files
        .get(path)
        .ok_or_else(|| anyhow::anyhow!("{path} was not fetched"))?;
    let s = std::str::from_utf8(b).map_err(|_| anyhow::anyhow!("{path} is not UTF-8 text"))?;
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);
    Ok(canonical::clean_text(s))
}

/// The file stem of a path (`a/b.agent.md` → `b`, with `suffixes` cut).
pub(crate) fn stem(path: &str, suffixes: &[&str]) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    for s in suffixes {
        if let Some(x) = base.strip_suffix(s)
            && !x.is_empty()
        {
            return x.to_string();
        }
    }
    base.rsplit_once('.')
        .map(|(a, _)| a)
        .filter(|a| !a.is_empty())
        .unwrap_or(base)
        .to_string()
}

/// The item name: `opts.rename`, else `raw` slugged; a mapped note when the
/// slug differs from what the source wrote.
pub(crate) fn name_for(raw: &str, opts: &Opts, notes: &mut Vec<Note>) -> String {
    let src = opts.rename.as_deref().unwrap_or(raw);
    let n = canonical::slug(src);
    if n != src {
        notes.push(Note::mapped(
            "name",
            format!("{} → {n}", canonical::one_line(src)),
        ));
    }
    n
}

/// The first Markdown heading or non-empty line, as one line.
pub(crate) fn first_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && *l != "---")
        .map(|l| canonical::one_line(l.trim_start_matches('#').trim()))
        .unwrap_or_default()
}

/// Cuts a one-line description to `max` bytes with a warning note.
pub(crate) fn cut_description(s: &str, max: usize, notes: &mut Vec<Note>) -> String {
    let s = canonical::one_line(s);
    if s.len() > max {
        notes.push(Note::warning(
            "description",
            format!("cut at {max} bytes (was {})", s.len()),
        ));
        return canonical::clip(&s, max).trim_end().to_string();
    }
    s
}
