//! Keleustes workflow engine (M3): the workflow graph, validation, dry run,
//! versioning, the run executor, the cron scheduler, built-in templates,
//! approvals and the checkpoints queue.
//!
//! The UI and other areas call [`ops`] and [`templates`] with a `Core`;
//! [`start`] runs the scheduler and executor for the life of the process.

pub mod catalogue;
pub mod conflicts;
pub mod cron;
pub mod decision;
pub mod defaults;
pub mod defaults_cli;
pub mod delivery;
pub mod dryrun;
pub mod exec;
pub mod forge;
pub mod graph;
pub mod guard;
pub mod ops;
pub mod route;
pub mod templates;
pub mod triggers;
pub mod validate;
pub mod walker;

pub use dryrun::{DryRun, dry_run};
pub use exec::{
    Board, CoreBoard, Engine, EngineConfig, EngineHandle, Forge, GateResult, Integrations,
    StepContext, start, start_with,
};
pub use forge::GitHubForge;
pub use graph::{Edge, Graph, Node};
pub use validate::{Invalid, Issue, Level, Mode, validate};
