//! Automated remediation (architecture §8): what the server does with the
//! commands an AI provider proposes for an incident.

pub mod approval;
pub mod audit;
pub mod context;
pub mod redact;
pub mod risk;
pub mod runner;
pub mod snapshot;
pub mod switch;
