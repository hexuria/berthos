//! Berthos node: fail-closed eligibility and loopback HTTP.
//!
//! Isolation is the product. This crate never drives the host desktop and
//! never accepts `class=laptop` as a public node. Payments are not implemented
//! here.

#![forbid(unsafe_code)]

pub mod action;
pub mod config;
pub mod eligibility;
pub mod guest;
pub mod http;
pub mod pairing;
pub mod probes;
pub mod view;

pub use action::{action_argv, argv_targets_host, GuestOp, MINIMAL_PNG, PNG_MAGIC};
pub use config::{NodeConfig, NodeConfigFile};
pub use eligibility::{eligible_private_facts, evaluate, evaluate_at};
pub use guest::{
    inspect_isolation, isolated_run_args, DockerGuest, GuestHandle, GuestIsolation, GuestRuntime,
    GuestSpec, MemoryGuest,
};
pub use http::{new_state, new_state_with_report, reject_if_bind_all, router, serve, NodeState};
pub use probes::{
    default_facts_config, docker_available, labeled_guest_image_ready, observe, simulate,
    SimulateCase,
};
pub use view::{view_bind_is_loopback, LoopbackView};

use std::net::IpAddr;

/// Node-level errors.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    /// Listener was asked to bind every interface.
    #[error("bind-all rejected ({0}); node HTTP must listen on loopback")]
    BindAllRejected(IpAddr),
    /// IO / listen failure.
    #[error("node io: {0}")]
    Io(#[from] std::io::Error),
    /// Config.
    #[error(transparent)]
    Config(#[from] config::ConfigError),
}

/// Directory used for node/client files. Override with `BERTHOS_HOME`.
pub fn berthos_home() -> std::path::PathBuf {
    if let Ok(raw) = std::env::var("BERTHOS_HOME") {
        return std::path::PathBuf::from(raw);
    }
    let base = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(base).join(".berthos")
}
