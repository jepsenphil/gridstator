//! Native Gridstator operations shared by the CLI, controller, and worker.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUILD_ID: &str = env!("GRIDSTATOR_BUILD_ID");
pub const SOURCE_REVISION: &str = env!("GRIDSTATOR_SOURCE_REVISION");
pub mod bridge;
pub mod cli;
pub mod client;
pub mod controller;
pub mod db;
pub mod discovery;
pub mod executor;
pub mod install;
pub mod local_worker;
pub mod packaging;
pub mod pki;
pub mod protocol;
pub mod release;
pub mod reports;
pub mod scheduler;
pub mod services;
pub mod state;
pub mod transfer;
pub mod updates;
pub mod worker;
pub mod workspace;
