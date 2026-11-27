//! Every forge, not only github.com (docs/gaps/forges.md): kinds and
//! connections ([`kind`]), each CLI's sign-in ([`auth`]), the local CLI
//! ([`cli`]), the dialect translations ([`api`], [`gitlab`], [`gitea`]), the
//! capability table ([`cap`]), conflict commits over git ([`push`]), the
//! attribution block on every write ([`attribution`]) and the
//! audit and kill-switch guard ([`guard`]).

pub mod api;
pub mod attribution;
pub mod auth;
pub mod cap;
pub mod cli;
pub mod gitea;
pub mod gitlab;
pub mod guard;
pub mod kind;
pub mod propose;
pub mod push;
pub mod server;

pub use kind::{
    CliKind, Connection, Dialect, ForgeKind, PullRef, RoutePref, normalize_host, pull_ref_of,
    valid_forge_name, valid_key,
};

#[cfg(test)]
mod tests;
