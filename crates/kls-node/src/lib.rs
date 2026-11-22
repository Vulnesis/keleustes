//! Keleustes node server: local CA, join tokens, and the gRPC link that
//! belai nodes keep open.

pub mod attention;
pub mod board;
pub mod fleet;
pub mod forge;
pub mod gateway;
pub mod host;
pub mod jobs;
pub mod library;
pub mod lost;
pub mod pki;
pub mod schedules;
pub mod screen;
pub mod server;
pub mod sessions;
pub mod status;
pub mod teleport;
pub mod token;

pub use pki::Pki;
pub use server::{Hub, run};

use anyhow::Result;
use kls_core::Core;
use std::sync::Arc;

/// Loads (or creates) the PKI under `<data>/ca` and builds the hub.
pub async fn hub(core: Core) -> Result<Hub> {
    let pki = Pki::load_or_create(&core.data_dir().join("ca"), &core.server_id().await?)?;
    let hub = Hub::new(core, Arc::new(pki));
    // The control broadcast runs with the node server (`run`), not in every
    // process that opens a hub.
    gateway::install(&hub);
    Ok(hub)
}

#[cfg(test)]
mod tests;
