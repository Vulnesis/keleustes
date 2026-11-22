//! Keleustes library search: discover agent profiles, skills, commands,
//! hooks, MCP servers and prompts in public places without credentials,
//! convert them to the Keleustes/belai format and scan them for risk.
//!
//! This crate is pure plus IO: no egui and no `kls-core`. The domain
//! (kill switch, audit, provenance, import) is `kls_core::libsearch`.
//! Spec: `docs/gaps/library-search.md`.
//!
//! Nothing fetched is ever executed, sourced or installed here.

pub mod cache;
pub mod catalogue;
pub mod convert;
pub mod fetch;
pub mod git;
pub mod index;
pub mod licence;
mod licence_text;
pub mod scan;
mod scan_rules;
pub mod source;
pub mod sources;

pub use cache::Cache;
pub use convert::{BundleFile, Converted, Note, NoteKind, Opts, SourceFile, Target, convert};
pub use fetch::{
    Class, Ctx, FakeFetcher, Fetcher, FixedGate, Gate, GhApi, HttpFetcher, Limited, Req, Resp,
};
pub use git::{Limits, Tree};
pub use index::{Facets, Index, SourceState};
pub use licence::{Licence, LicenceClass, LicencePolicy};
pub use scan::{Flag, scan};
pub use source::{
    Ecosystem, Fetched, Hit, ItemType, Locator, Mode, Query, Refresh, Source, Trust, Upstream,
};
