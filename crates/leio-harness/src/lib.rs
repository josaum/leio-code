//! LEIO-Harness: agent execution runtime — leases, worktrees, semantic bus, gates.
//!
//! The library behind the `leio-harness` binary, exposed so a Rust host (a
//! substrate service, a build orchestrator) can embed the same machinery
//! in-process instead of shelling out:
//!
//! - [`lease`] — atomic, file-locked lane leases with heartbeat and expiry.
//! - [`worktree`] — fail-closed git worktree create / commit / retire.
//! - [`bus`] / [`bus_client`] — Arrow Flight `do_exchange` semantic bus
//!   (embedding rows, cosine match, merge gate, GEPA evolve) and its client.
//! - [`agents`] — work-shape → model routing and agent argv templates.
//! - [`gepa`] / [`sigreg`] / [`improvement`] — vector evolution, isotropy
//!   collapse detection, and the collab-beats-baseline verdict.
//! - [`orchestrator`] / [`integrate`] / [`gate`] / [`merge`] — the `day`
//!   lane runner and the integration + improvement gates.
//! - [`process`] / [`acp`] / [`arrow_events`] — supervised runs, the ACP
//!   stdio proxy, and zero-copy Arrow event streams.
//! - [`receipts`] — native check that a lane's own output resolves the
//!   evidence it cites (LEIO query ids, Reference Provider id shapes), opt-in via
//!   `DaySpec.require_receipts_check`.
//! - [`model`] — the serde types shared across the above.
//!
//! The binary (`src/main.rs`) is a thin clap front end over these modules;
//! nothing here depends on it.
// Rust guideline compliant 2026-02-21

pub mod acp;
pub mod agents;
pub mod arrow_events;
pub mod bus;
pub mod bus_client;
/// In-process `leio-code` indexing and code-view publication. Behind the
/// `codeview` feature (default on) so a host that only needs leases, the bus
/// and the gates can embed the harness without the full code-intelligence
/// dependency.
#[cfg(feature = "codeview")]
pub mod codeview;
pub mod delivery;
pub mod embed;
pub mod gate;
pub mod gepa;
pub mod improvement;
pub mod integrate;
pub mod lease;
pub mod managed_bus;
pub mod manifest;
pub mod merge;
pub mod model;
pub mod orchestrator;
pub mod process;
pub mod receipts;
pub mod shm;
pub mod sigreg;
pub mod tui;
pub mod workflow;
pub mod worktree;
