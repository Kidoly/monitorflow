//! Update campaigns (ADR-0033): `apply_updates` commands (ADR-0031) for a
//! customer's servers, frozen at creation into ordered waves, queued only
//! inside maintenance windows, and stopped when a wave fails beyond its
//! threshold.
//!
//! A campaign is a `draft` until started, `scheduled` until its first wave
//! is queued, then `running`; users can pause, resume and stop it, and it
//! ends `completed`, `stopped` or `failed`.
//!
//! A campaign may pin mirror snapshots (ADR-0038): each server is pointed
//! at them before its update and gets its previous sources back after.

mod model;
mod pin;
pub mod query;
mod runner;
mod store;

pub use model::{
    plan_waves, Action, CampaignState, FailureThreshold, FailureUnit, LocalTime, PinnedMirror,
    PlanError, SourcesMode, SourcesPin, UpdateSpec, WaveSpec, Window, MAX_PACKAGES, MAX_TARGETS,
    MAX_WAVES, MAX_WINDOWS,
};
pub use pin::write_pins;
pub use runner::{Runner, Step, MIN_WINDOW_LEFT, POLL};
pub use store::{Actor, CampaignError, CampaignRow, CampaignStore, NewCampaign};
