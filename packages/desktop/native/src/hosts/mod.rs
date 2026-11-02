//! Native Hosts surface: the shared connection grammar, the client's own
//! saved-host registry, the SSH stdio tunnel, and the host connection runtime.
//! The daemon-side registrations vertical (`paseo.hosts`) rides the active
//! connection's session channel.

pub mod connection;
pub mod endpoints;
pub mod offer;
pub mod profile;
pub mod registry;
pub mod runtime;
pub mod ssh;
pub mod target;

pub use connection::{
    HostConnection, SshConnectionInput, connection_from_listen, connection_from_target,
    create_remote_ssh_host_connection, grammar_for_connection, order_hosts_local_first,
    relay_connection_from_offer,
};
pub use offer::{ConnectionOffer, parse_connection_offer_from_url};
pub use profile::{
    HostProfile, UpsertInput, normalize_stored_host_profile, upsert_host_connection_in_profiles,
};
pub use registry::{PLAINTEXT_CREDENTIAL_LIMITATION, load, registry_path, save, state_root};
pub use runtime::{
    ConnectInput, HostClient, HostPhase, HostRuntimeEvent, IdentityMismatch, daemon_registrations,
};
pub use target::{HostTarget, parse_host_target};
