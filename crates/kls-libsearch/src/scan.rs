//! Risk flags. Heuristics only flag; the review step shows the text and
//! the person ticks each flag that [`Flag::needs_review`]. Nothing imported
//! is ever sent to a model.

use crate::convert::Converted;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Flag {
    /// Any hook; any MCP stdio command.
    RunsCode,
    /// Claude ``!`…` ``, Gemini `!{…}`.
    ShellExpansion,
    /// `@path` / `@{…}`.
    FileInclude,
    /// `curl|wget … | sh/bash`, `iwr|iex`.
    PipeToShell,
    /// First non-allowlisted URL host in the body or commands.
    RemoteUrl(String),
    /// Bidi / zero-width / tag characters (stripped).
    HiddenText,
    /// Key-shaped literals.
    SecretLike,
    /// "ignore previous instructions", role-swap markers, hidden HTML comments.
    PromptInjection,
    /// 200 or more base64 characters in a row.
    LongBase64,
    /// ClawHub isSuspicious / moderation, aitmpl skillspector != SAFE.
    Upstream(String),
    /// Hook dialect converted (Gemini / Copilot / Cursor).
    ProtocolMismatch,
    FilesNotCarried(u32),
    /// Hook first word not a bundled file.
    NeedsProgram(String),
    LicenceUnknown,
    LicenceRestricted(String),
    /// Refused outright: marketplace `source:"command"`, `${PLUGIN_ROOT}`
    /// MCP, path escape, plain-http MCP remote.
    Refused(String),
}

impl Flag {
    /// Whether the review must tick it (informational flags need not).
    pub fn needs_review(&self) -> bool {
        !matches!(self, Flag::FilesNotCarried(_) | Flag::Refused(_))
    }

    pub fn refused(&self) -> bool {
        matches!(self, Flag::Refused(_))
    }

    /// A stable key for acks (`RemoteUrl:example.com`).
    pub fn key(&self) -> String {
        match self {
            Flag::RemoteUrl(s) => format!("RemoteUrl:{s}"),
            Flag::Upstream(s) => format!("Upstream:{s}"),
            Flag::FilesNotCarried(n) => format!("FilesNotCarried:{n}"),
            Flag::NeedsProgram(s) => format!("NeedsProgram:{s}"),
            Flag::LicenceRestricted(s) => format!("LicenceRestricted:{s}"),
            Flag::Refused(s) => format!("Refused:{s}"),
            other => format!("{other:?}"),
        }
    }

    /// The flag's name (`ShellExpansion`).
    pub fn name(&self) -> &'static str {
        match self {
            Flag::RunsCode => "RunsCode",
            Flag::ShellExpansion => "ShellExpansion",
            Flag::FileInclude => "FileInclude",
            Flag::PipeToShell => "PipeToShell",
            Flag::RemoteUrl(_) => "RemoteUrl",
            Flag::HiddenText => "HiddenText",
            Flag::SecretLike => "SecretLike",
            Flag::PromptInjection => "PromptInjection",
            Flag::LongBase64 => "LongBase64",
            Flag::Upstream(_) => "Upstream",
            Flag::ProtocolMismatch => "ProtocolMismatch",
            Flag::FilesNotCarried(_) => "FilesNotCarried",
            Flag::NeedsProgram(_) => "NeedsProgram",
            Flag::LicenceUnknown => "LicenceUnknown",
            Flag::LicenceRestricted(_) => "LicenceRestricted",
            Flag::Refused(_) => "Refused",
        }
    }

    /// The detail after the name (`example.com`), if any.
    pub fn detail(&self) -> String {
        match self {
            Flag::RemoteUrl(s)
            | Flag::Upstream(s)
            | Flag::NeedsProgram(s)
            | Flag::LicenceRestricted(s)
            | Flag::Refused(s) => s.clone(),
            Flag::FilesNotCarried(n) => n.to_string(),
            _ => String::new(),
        }
    }
}

/// Every flag for a converted item: content heuristics over the doc and
/// bundle files, plus the converter's and the licence's flags.
pub fn scan(c: &Converted) -> Vec<Flag> {
    crate::scan_rules::scan(c)
}
